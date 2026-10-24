//! OFDBox Folio 的单向排版引擎。
//!
//! 输入 [`ofdbox_folio_ir::FlowDocument`] 与字体提供者，输出
//! [`ofdbox_folio_ir::FixedDocument`]。布局是 Flow 到 Fixed 的唯一通路，不提供
//! 反向转换，也不做渲染或文件 IO。
//!
//! A1 里程碑实现单栏排版：段落（缩进、行距、对齐与两端对齐）、行内图片、块级
//! 图片、分页符、页码字段与标题大纲。表格与自动目录留给 A2，规则见
//! `docs/layout-contract.md`。

mod font;
mod linebreak;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ofdbox_folio_ir::{
    Align, Block, Border, ColumnWidth, Diagnostic, DiagnosticCode, Element, FieldKind, FillRule,
    FixedDocument, FlowDocument, FontAsset, FontProvider, Group, HeaderFooter, HeaderFooterSet,
    ImageAsset, ImageBlock, ImageElement, Inline, LineCap, LineJoin, LineSpacing, LinkArea,
    LinkTarget, Matrix, OutlineItem, Page, PageInfo, PageSetup, Paint, ParaStyle, Paragraph, Path,
    PathElement, Point, Rect, Rgba, Rotation, Row, Scene, Size, StrokePaint, StrokeStyle, TabAlign,
    TabLeader, TabStop, Table, TextElement, TextItem, TextLayer, TextLine, TextRun, TextStyle,
    Twips,
};

use font::{FontKey, FontStore};
use linebreak::{LineSpan, Piece};

/// 布局引擎版本，写进 `EngineInfo`。
pub const LAYOUT_VERSION: &str = "layout-0.1";

/// 没有显式制表位时，制表符默认前进的步长（毫米）。
const DEFAULT_TAB_STEP: f64 = 12.5;

/// 布局错误。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LayoutError {
    /// 文档没有分节。
    #[error("文档没有分节，无法排版")]
    NoSection,
}

/// 布局选项。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutOptions {
    /// 图片没有声明 DPI 时使用的分辨率。
    pub default_image_dpi: f64,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            default_image_dpi: 96.0,
        }
    }
}

/// 把流式文档排版为分页文档。
pub fn layout(
    document: &FlowDocument,
    fonts: &mut dyn FontProvider,
) -> Result<FixedDocument, LayoutError> {
    layout_with(document, fonts, LayoutOptions::default())
}

/// 带选项的排版入口。
///
/// 引用「总页数」字段时先排一遍拿页数；含自动目录时先排一遍拿标题页码，再代入
/// 重排，直到目录页码稳定（最多 4 遍）。
pub fn layout_with(
    document: &FlowDocument,
    fonts: &mut dyn FontProvider,
    options: LayoutOptions,
) -> Result<FixedDocument, LayoutError> {
    if document.sections.is_empty() {
        return Err(LayoutError::NoSection);
    }
    let needs_toc = uses_toc(document);
    let needs_total = uses_total_pages(document);
    if !needs_toc && !needs_total {
        let (fixed, _) = Engine::new(document, options).run(fonts, None, None);
        return Ok(fixed);
    }

    // 第一遍：目录内容为空，只用来拿标题页码与总页数。
    let mut total_pages = None;
    let mut toc_entries: Option<Vec<TocEntry>> = None;
    let mut last = None;
    for _ in 0..4 {
        let (fixed, headings) =
            Engine::new(document, options).run(fonts, total_pages, toc_entries.clone());
        let next_entries: Option<Vec<TocEntry>> = needs_toc.then(|| {
            headings
                .iter()
                .map(|(level, title, page, _)| (*level, title.clone(), *page))
                .collect()
        });
        let next_total = needs_total.then_some(fixed.pages.len());
        let stable = next_entries == toc_entries && next_total == total_pages;
        total_pages = next_total;
        toc_entries = next_entries;
        last = Some(fixed);
        if stable {
            break;
        }
    }
    Ok(last.expect("至少排过一遍"))
}

/// 文档是否含自动目录。
fn uses_toc(document: &FlowDocument) -> bool {
    fn blocks_use_toc(blocks: &[Block]) -> bool {
        blocks.iter().any(|block| match block {
            Block::Toc(_) => true,
            Block::Table(table) => table
                .rows
                .iter()
                .any(|row| row.cells.iter().any(|cell| blocks_use_toc(&cell.blocks))),
            _ => false,
        })
    }
    document
        .sections
        .iter()
        .any(|section| blocks_use_toc(&section.blocks))
}

/// 文档是否引用总页数字段。
fn uses_total_pages(document: &FlowDocument) -> bool {
    document.sections.iter().any(|section| {
        let areas = [&section.headers, &section.footers]
            .into_iter()
            .flat_map(|set| [&set.default, &set.first, &set.even])
            .flatten();
        section.blocks.iter().any(block_uses_total_pages)
            || areas
                .into_iter()
                .any(|area| area.blocks.iter().any(block_uses_total_pages))
    })
}

fn block_uses_total_pages(block: &Block) -> bool {
    match block {
        Block::Paragraph(paragraph) => paragraph.runs.iter().any(inline_uses_total_pages),
        Block::Table(table) => table.rows.iter().any(|row| {
            row.cells
                .iter()
                .any(|cell| cell.blocks.iter().any(block_uses_total_pages))
        }),
        _ => false,
    }
}

fn inline_uses_total_pages(inline: &Inline) -> bool {
    match inline {
        Inline::Field(field) => matches!(field.kind, FieldKind::TotalPages),
        Inline::Link(link) => link.runs.iter().any(inline_uses_total_pages),
        _ => false,
    }
}

/// 毫米为单位的四边边距。
#[derive(Clone, Copy, Debug)]
struct MarginsMm {
    top: f64,
    right: f64,
    bottom: f64,
    left: f64,
}

/// 当前页累积的内容。
struct PageBuffer {
    scene: Group,
    items: Vec<TextItem>,
    lines: Vec<TextLine>,
    links: Vec<LinkArea>,
}

impl PageBuffer {
    fn new() -> Self {
        Self {
            scene: Group::default(),
            items: Vec::new(),
            lines: Vec::new(),
            links: Vec::new(),
        }
    }
}

impl Default for PageBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// 一个已测量的行内图片。
#[derive(Clone, Copy, Debug)]
struct MeasuredImage {
    /// Flow 资源表中的图片 ID。
    asset: ofdbox_folio_ir::AssetId,
    /// 显示宽度，毫米。
    width: f64,
    /// 显示高度，毫米。
    height: f64,
}

/// 段落测量结果：断行单元、链接区间、行内图片。
type Measured = (
    Vec<Piece>,
    Vec<(usize, usize, LinkTarget)>,
    Vec<MeasuredImage>,
);

/// 文本样式的去重键。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct StyleKey {
    family: String,
    size: i64,
    bold: bool,
    italic: bool,
    color: [u8; 4],
    underline: bool,
    strike: bool,
    superscript: bool,
    subscript: bool,
}

impl StyleKey {
    fn of(style: &TextStyle) -> Self {
        Self {
            family: style.family.name.clone(),
            size: style.size.0,
            bold: style.bold,
            italic: style.italic,
            color: style.color.to_rgba8(),
            underline: style.underline,
            strike: style.strike,
            superscript: style.superscript,
            subscript: style.subscript,
        }
    }
}

/// 一次布局过程。
struct Engine<'a> {
    document: &'a FlowDocument,
    options: LayoutOptions,
    fonts: FontStore,
    font_assets: HashMap<FontKey, Arc<FontAsset>>,
    styles: Vec<TextStyle>,
    style_index: HashMap<StyleKey, usize>,
    fixed: FixedDocument,
    page_width: f64,
    page_height: f64,
    margins: MarginsMm,
    page: PageBuffer,
    cursor: f64,
    /// 当前排版框；正文是版心，表格单元格与页眉页脚会临时换框。
    frame: Frame,
    /// 是否在框内排版：框内不自动分页。
    in_frame: bool,
    /// 当前页是否有正文内容（只有页眉的页不产出）。
    page_body: bool,
    /// 当前页是否已经开始排正文。
    page_started: bool,
    /// 当前分节的页序，用于「首页不同」。
    section_page: usize,
    /// 分栏数。
    column_count: u16,
    /// 栏间距（毫米）。
    column_gap: f64,
    /// 当前栏下标。
    column_index: usize,
    /// 页眉距顶边。
    header_distance: f64,
    /// 页脚距底边。
    footer_distance: f64,
    /// 当前分节的页眉。
    headers: HeaderFooterSet,
    /// 当前分节的页脚。
    footers: HeaderFooterSet,
    /// 两遍布局时已知的总页数。
    total_pages: Option<usize>,
    /// 上一遍排版收集到的目录条目。
    toc_entries: Option<Vec<TocEntry>>,
    headings: Vec<(u8, String, usize, f64)>,
    /// 字体缺字形的字符计数，最后汇总成一条诊断。
    missing_chars: usize,
}

impl<'a> Engine<'a> {
    fn new(document: &'a FlowDocument, options: LayoutOptions) -> Self {
        Self {
            document,
            options,
            fonts: FontStore::default(),
            font_assets: HashMap::new(),
            styles: Vec::new(),
            style_index: HashMap::new(),
            fixed: FixedDocument {
                metadata: document.metadata.clone(),
                ..FixedDocument::default()
            },
            page_width: 0.0,
            page_height: 0.0,
            margins: MarginsMm {
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
                left: 0.0,
            },
            page: PageBuffer::new(),
            cursor: 0.0,
            frame: Frame::default(),
            in_frame: false,
            page_body: false,
            page_started: false,
            section_page: 0,
            column_count: 1,
            column_gap: 0.0,
            column_index: 0,
            header_distance: 0.0,
            footer_distance: 0.0,
            headers: HeaderFooterSet::default(),
            footers: HeaderFooterSet::default(),
            total_pages: None,
            toc_entries: None,
            headings: Vec::new(),
            missing_chars: 0,
        }
    }

    fn run(
        mut self,
        provider: &mut dyn FontProvider,
        total_pages: Option<usize>,
        toc_entries: Option<Vec<TocEntry>>,
    ) -> (FixedDocument, Vec<(u8, String, usize, f64)>) {
        self.prepare_fonts(provider);
        self.total_pages = total_pages;
        self.toc_entries = toc_entries;
        for section in &self.document.sections {
            self.begin_section(&section.setup, &section.headers, &section.footers);
            for block in &section.blocks {
                self.place_block(block);
            }
            self.finish_page();
        }
        if self.fixed.pages.is_empty() {
            self.begin_section(
                &self
                    .document
                    .sections
                    .first()
                    .map(|section| section.setup)
                    .unwrap_or_default(),
                &HeaderFooterSet::default(),
                &HeaderFooterSet::default(),
            );
            self.finish_page();
        }
        if self.missing_chars > 0 {
            self.diagnostic(
                DiagnosticCode::MissingFont,
                format!("有 {} 个字符在当前字体中缺少字形", self.missing_chars),
            );
        }

        self.fixed.outline = self.build_outline();
        self.fixed.engine.layout_version = LAYOUT_VERSION;
        (self.fixed, self.headings)
    }

    // ---- 字体 ----

    /// 收集字符集并逐个字体解析一次度量。
    ///
    /// 表格单元格与页眉页脚里的文字同样要预解析；自动目录与字段用的样式不在 IR
    /// 的 run 里（排版时才生成文字），因此把这些样式的字体按全文字符集预解析，
    /// 否则目录条目会因为「字体没准备」而画成空框。
    fn prepare_fonts(&mut self, provider: &mut dyn FontProvider) {
        let mut wanted: HashMap<FontKey, HashSet<char>> = HashMap::new();
        let mut fallbacks: HashMap<String, Vec<String>> = HashMap::new();
        for section in &self.document.sections {
            collect_block_chars(&section.blocks, self, &mut wanted, &mut fallbacks);
            for set in [&section.headers, &section.footers] {
                for area in [&set.default, &set.first, &set.even].into_iter().flatten() {
                    collect_block_chars(&area.blocks, self, &mut wanted, &mut fallbacks);
                }
            }
        }

        // 全文用到的字符，供「排版时才生成文字」的样式兜底；目录标题与页码是
        // 排版时生成的，也要算进去。
        let mut generated: HashSet<char> = "0123456789".chars().collect();
        for section in &self.document.sections {
            collect_toc_text(&section.blocks, &mut generated);
        }
        let all_chars: HashSet<char> = wanted
            .values()
            .flatten()
            .copied()
            .chain(generated.iter().copied())
            .collect();
        let mut extra_styles: Vec<TextStyle> = vec![self.document.styles.default_character.clone()];
        for section in &self.document.sections {
            collect_toc_styles(&section.blocks, &mut extra_styles);
        }
        for style in extra_styles {
            let style = self.resolve_char_style(&style, None);
            let key = FontKey::new(&style.family.name, style.bold, style.italic);
            fallbacks
                .entry(style.family.name.clone())
                .or_insert_with(|| style.family.fallback.clone());
            wanted
                .entry(key)
                .or_default()
                .extend(all_chars.iter().copied());
        }

        for (key, chars) in &wanted {
            let chain = fallbacks.get(&key.family).cloned().unwrap_or_default();
            self.fonts.prepare(provider, key, &chain, chars);
        }
    }

    fn collect_chars(
        &self,
        runs: &[Inline],
        wanted: &mut HashMap<FontKey, HashSet<char>>,
        fallbacks: &mut HashMap<String, Vec<String>>,
    ) {
        for inline in runs {
            match inline {
                Inline::Text(run) => {
                    let style = self.resolve_char_style(&run.style, run.style_id.as_deref());
                    let key = FontKey::new(&style.family.name, style.bold, style.italic);
                    fallbacks
                        .entry(style.family.name.clone())
                        .or_insert_with(|| style.family.fallback.clone());
                    wanted.entry(key).or_default().extend(run.text.chars());
                }
                Inline::Link(link) => self.collect_chars(&link.runs, wanted, fallbacks),
                Inline::Field(field) => {
                    let style = self.resolve_char_style(&TextStyle::default(), None);
                    let key = FontKey::new(&style.family.name, style.bold, style.italic);
                    fallbacks
                        .entry(style.family.name.clone())
                        .or_insert_with(|| style.family.fallback.clone());
                    wanted.entry(key).or_default().extend("0123456789".chars());
                    if let Some(text) = &field.cached_text {
                        wanted
                            .entry(FontKey::new(&style.family.name, style.bold, style.italic))
                            .or_default()
                            .extend(text.chars());
                    }
                }
                _ => {}
            }
        }
    }

    /// 取（必要时登记）Fixed IR 里的字体资源。
    fn font_asset(&mut self, key: &FontKey) -> Option<Arc<FontAsset>> {
        if let Some(asset) = self.font_assets.get(key) {
            return Some(Arc::clone(asset));
        }
        let asset = self.fonts.get(key)?.asset();
        let id = self.fixed.resources.add_font_asset(asset);
        let stored = self.fixed.resources.font(id)?.clone();
        self.font_assets.insert(key.clone(), Arc::clone(&stored));
        Some(stored)
    }

    // ---- 样式 ----

    /// 解析段落样式：命名样式为底，直接格式覆盖非默认值。
    fn resolve_paragraph_style(&self, style: &ParaStyle) -> ParaStyle {
        let mut resolved = self
            .named_paragraph_style(style.style_id.as_deref())
            .unwrap_or_else(|| self.document.styles.default_paragraph.clone());
        apply_paragraph_direct(&mut resolved, style);
        resolved.style_id = style.style_id.clone();
        resolved
    }

    fn named_paragraph_style(&self, id: Option<&str>) -> Option<ParaStyle> {
        let id = id?;
        let mut current = self.document.styles.paragraph.iter().find(|n| n.id == id)?;
        let mut chain = Vec::new();
        for _ in 0..8 {
            chain.push(current);
            let Some(parent) = current.based_on.as_deref() else {
                break;
            };
            let Some(next) = self
                .document
                .styles
                .paragraph
                .iter()
                .find(|named| named.id == parent)
            else {
                break;
            };
            if chain.iter().any(|item| item.id == next.id) {
                break;
            }
            current = next;
        }
        // 从默认样式出发，按「父 → 子」顺序叠非默认值，子样式缺省字段继承父样式。
        let mut resolved = self.document.styles.default_paragraph.clone();
        for named in chain.into_iter().rev() {
            apply_paragraph_direct(&mut resolved, &named.props);
        }
        Some(resolved)
    }

    /// 解析字符样式：命名字符样式为底，直接格式覆盖非默认值。
    fn resolve_char_style(&self, style: &TextStyle, style_id: Option<&str>) -> TextStyle {
        let mut resolved = self
            .named_char_style(style_id)
            .unwrap_or_else(|| self.document.styles.default_character.clone());
        apply_text_direct(&mut resolved, style);
        resolved
    }

    fn named_char_style(&self, id: Option<&str>) -> Option<TextStyle> {
        let id = id?;
        let mut current = self
            .document
            .styles
            .character
            .iter()
            .find(|named| named.id == id)?;
        let mut chain = Vec::new();
        for _ in 0..8 {
            chain.push(current);
            let Some(parent) = current.based_on.as_deref() else {
                break;
            };
            let Some(next) = self
                .document
                .styles
                .character
                .iter()
                .find(|named| named.id == parent)
            else {
                break;
            };
            if chain.iter().any(|item| item.id == next.id) {
                break;
            }
            current = next;
        }
        let mut resolved = self.document.styles.default_character.clone();
        for named in chain.into_iter().rev() {
            apply_text_direct(&mut resolved, &named.props);
        }
        Some(resolved)
    }

    /// 登记并去重文本样式，返回样式下标。
    fn intern_style(&mut self, style: &TextStyle) -> usize {
        let key = StyleKey::of(style);
        if let Some(index) = self.style_index.get(&key) {
            return *index;
        }
        let index = self.styles.len();
        self.styles.push(style.clone());
        self.style_index.insert(key, index);
        index
    }

    // ---- 页面 ----

    fn begin_section(
        &mut self,
        setup: &PageSetup,
        headers: &HeaderFooterSet,
        footers: &HeaderFooterSet,
    ) {
        self.finish_page();
        self.page_width = setup.width.to_mm();
        self.page_height = setup.height.to_mm();
        self.margins = MarginsMm {
            top: setup.margins.top.to_mm(),
            right: setup.margins.right.to_mm(),
            bottom: setup.margins.bottom.to_mm(),
            left: setup.margins.left.to_mm(),
        };
        if self.page_width <= 0.0 || self.page_height <= 0.0 {
            self.page_width = 210.0;
            self.page_height = 297.0;
            self.warn("页面尺寸无效，按 A4 处理");
        }
        self.column_count = setup.columns.max(1);
        self.column_gap = setup.column_gap.to_mm().max(0.0);
        self.column_index = 0;
        self.header_distance = setup.header_distance.to_mm().max(0.0);
        self.footer_distance = setup.footer_distance.to_mm().max(0.0);
        self.headers = headers.clone();
        self.footers = footers.clone();
        self.section_page = 0;
        self.in_frame = false;
        self.page_body = false;
        self.page_started = false;
        self.frame = self.column_frame(0);
        self.cursor = self.frame.top;
    }

    /// 版心框。
    /// 整页版心框（页眉页脚用，横跨所有栏）。
    fn page_frame(&self) -> Frame {
        Frame {
            left: self.margins.left,
            width: (self.page_width - self.margins.left - self.margins.right).max(1.0),
            top: self.margins.top,
            bottom: self.page_height - self.margins.bottom,
        }
    }

    /// 第 index 栏的框。
    fn column_frame(&self, index: usize) -> Frame {
        let page = self.page_frame();
        let count = f64::from(self.column_count.max(1));
        let gap = self.column_gap;
        let width = ((page.width - gap * (count - 1.0)) / count).max(1.0);
        Frame {
            left: page.left + (width + gap) * index as f64,
            width,
            top: page.top,
            bottom: page.bottom,
        }
    }

    /// 切到下一栏；没有下一栏时返回 alse。
    fn next_column(&mut self) -> bool {
        let count = usize::from(self.column_count.max(1));
        if self.column_index + 1 >= count {
            return false;
        }
        self.column_index += 1;
        self.frame = self.column_frame(self.column_index);
        self.cursor = self.frame.top;
        true
    }

    /// 正文流换下一段容器：先换栏，最后一栏再换页。
    fn new_flow(&mut self) {
        if self.next_column() {
            return;
        }
        self.new_page();
        self.start_page_body();
    }

    fn content_width(&self) -> f64 {
        self.frame.width.max(1.0)
    }

    fn bottom(&self) -> f64 {
        self.frame.bottom
    }

    fn page_number(&self) -> usize {
        self.fixed.pages.len() + 1
    }

    fn new_page(&mut self) {
        self.finish_page();
        self.page = PageBuffer::new();
        self.column_index = 0;
        self.frame = self.column_frame(0);
        self.cursor = self.frame.top;
        self.page_body = false;
        self.page_started = false;
        self.section_page += 1;
    }

    /// 结束当前页；没有正文的页不产出。
    fn finish_page(&mut self) {
        if !self.page_body {
            return;
        }
        self.layout_footer();
        let size = Size::new(self.page_width, self.page_height);
        let scene = Scene {
            size,
            root: std::mem::take(&mut self.page.scene),
            diagnostics: Vec::new(),
        };
        let index = self.fixed.pages.len();
        let mut page = Page::new(
            PageInfo {
                media_box: Rect::new(0.0, 0.0, size.width, size.height),
                crop_box: Rect::new(0.0, 0.0, size.width, size.height),
                rotation: Rotation::None,
                label: None,
            },
            scene,
        );
        page.text = TextLayer {
            page: index,
            items: std::mem::take(&mut self.page.items),
            lines: std::mem::take(&mut self.page.lines),
        };
        page.links = std::mem::take(&mut self.page.links);
        self.fixed.pages.push(page);
    }

    /// 快照当前页，用于回滚一次失败的放置。
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            children: self.page.scene.children.len(),
            items: self.page.items.len(),
            lines: self.page.lines.len(),
            links: self.page.links.len(),
            cursor: self.cursor,
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.page.scene.children.truncate(snapshot.children);
        self.page.items.truncate(snapshot.items);
        self.page.lines.truncate(snapshot.lines);
        self.page.links.truncate(snapshot.links);
        self.cursor = snapshot.cursor;
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.fixed
            .report
            .push(Diagnostic::warning(DiagnosticCode::Geometry, message));
    }

    fn diagnostic(&mut self, code: DiagnosticCode, message: impl Into<String>) {
        self.fixed.report.push(Diagnostic::warning(code, message));
    }

    // ---- 块 ----

    fn place_block(&mut self, block: &Block) {
        // 一页的第一块正文之前才排页眉，避免空页也带页眉。
        if !self.page_body && !self.in_frame && !matches!(block, Block::PageBreak) {
            self.start_page_body();
        }
        match block {
            Block::Paragraph(paragraph) => self.place_paragraph(paragraph, self.total_pages),
            Block::Image(image) => self.place_image_block(image),
            Block::PageBreak => self.new_page(),
            Block::Table(table) => self.place_table(table),
            Block::Toc(toc) => self.place_toc(toc),
            _ => self.warn("未知块类型，已跳过"),
        }
    }

    /// 本页开始排正文：标记并排页眉。
    fn start_page_body(&mut self) {
        self.page_body = true;
        self.begin_body();
    }

    /// 正文开始时排页眉。
    fn begin_body(&mut self) {
        if self.page_started {
            return;
        }
        self.page_started = true;
        self.layout_header();
    }

    /// 当前页该用哪一套页眉页脚：首页不同 → 默认；偶数页 → 偶数页。
    fn pick_footer(
        set: &HeaderFooterSet,
        first_page: bool,
        page_index: usize,
    ) -> Option<&HeaderFooter> {
        if first_page && set.first.is_some() {
            return set.first.as_ref();
        }
        if page_index % 2 == 1 && set.even.is_some() {
            return set.even.as_ref();
        }
        set.default.as_ref()
    }

    fn layout_header(&mut self) {
        let first_page = self.section_page == 0;
        let Some(header) = Self::pick_footer(&self.headers, first_page, self.section_page) else {
            return;
        };
        let blocks = header.blocks.clone();
        if blocks.is_empty() {
            return;
        }
        let width = self.page_frame().width;
        let saved = (self.frame, self.cursor, self.in_frame);
        self.in_frame = true;
        self.frame = Frame {
            left: self.margins.left,
            width,
            top: self.header_distance,
            bottom: self.margins.top,
        };
        self.cursor = self.header_distance;
        for block in &blocks {
            self.place_block(block);
        }
        if self.cursor > self.margins.top + 0.5 {
            self.warn("页眉超出上边距，可能与正文重叠");
        }
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
    }

    fn layout_footer(&mut self) {
        let first_page = self.section_page == 0;
        let Some(footer) = Self::pick_footer(&self.footers, first_page, self.section_page) else {
            return;
        };
        let blocks = footer.blocks.clone();
        if blocks.is_empty() {
            return;
        }
        let width = self.page_frame().width;
        // 先量高度，再贴着「底边 - 页脚距离」的上方落版。
        let height = self.measure_blocks(&blocks, width);
        let bottom = (self.page_height - self.footer_distance).max(self.margins.bottom);
        let top = (bottom - height).max(0.0);
        let saved = (self.frame, self.cursor, self.in_frame);
        self.in_frame = true;
        self.frame = Frame {
            left: self.margins.left,
            width,
            top,
            bottom,
        };
        self.cursor = top;
        for block in &blocks {
            self.place_block(block);
        }
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
    }

    // ---- 自动目录 ----

    /// 排自动目录：标题 + 每条「条目 ⇥ 页码」的段落，页码落在右制表位上。
    ///
    /// 页码来自上一遍排版的结果；目录自身占位会让页码整体后移，`layout_with`
    /// 会反复代入直到稳定。条目文字与页码之间用制表符分开，右制表位放在版心
    /// 右边界，`TocStyle::dot_leader` 打开时画点线前导符。
    fn place_toc(&mut self, toc: &ofdbox_folio_ir::TocBlock) {
        if self.in_frame {
            self.warn("目录放在表格单元格里，已跳过");
            return;
        }
        let Some(entries) = self.toc_entries.clone() else {
            // 第一遍排版没有条目，等后续遍次。
            return;
        };
        let entries: Vec<TocEntry> = entries
            .into_iter()
            .filter(|(level, title, _)| *level <= toc.max_level.max(1) && !title.trim().is_empty())
            .collect();
        if entries.is_empty() {
            self.warn("自动目录没有可用标题");
            return;
        }

        if let Some(title) = &toc.title
            && !title.trim().is_empty()
        {
            let mut heading = Paragraph::plain(title.clone(), toc.style.title_style.clone());
            heading.heading_level = None;
            self.place_paragraph(&heading, self.total_pages);
        }

        let content = self.content_width();
        let page_number_style = toc.style.page_number_style.clone();
        let leader = if toc.style.dot_leader {
            TabLeader::Dot
        } else {
            TabLeader::None
        };
        for (level, title, page) in entries {
            let indent = toc
                .style
                .indent_per_level
                .to_mm()
                .max(0.0)
                .mul_add(f64::from(level.saturating_sub(1)), 0.0);
            let mut paragraph = Paragraph::new(ParaStyle {
                left_indent: Twips::from_mm(indent),
                tabs: vec![TabStop::new(
                    Twips::from_mm(content),
                    TabAlign::Right,
                    leader,
                )],
                ..ParaStyle::default()
            });
            paragraph.runs.push(Inline::Text(TextRun::new(
                title,
                toc.style.entry_style.clone(),
            )));
            paragraph
                .runs
                .push(Inline::Text(TextRun::new("\t", page_number_style.clone())));
            // 大纲目标是 0 基页序，目录里写 1 基页码。
            paragraph.runs.push(Inline::Text(TextRun::new(
                (page + 1).to_string(),
                page_number_style.clone(),
            )));
            self.place_paragraph(&paragraph, self.total_pages);
        }
    }

    /// 在临时页里量一组块的高度（不产出任何元素）。
    fn measure_blocks(&mut self, blocks: &[Block], width: f64) -> f64 {
        let saved_page = std::mem::take(&mut self.page);
        let saved = (self.frame, self.cursor, self.in_frame);
        self.in_frame = true;
        self.frame = Frame {
            left: 0.0,
            width,
            top: 0.0,
            bottom: f64::MAX,
        };
        self.cursor = 0.0;
        for block in blocks {
            self.place_block(block);
        }
        let height = self.cursor;
        self.page = saved_page;
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
        height
    }

    // ---- 表格 ----

    fn place_table(&mut self, table: &Table) {
        if table.rows.is_empty() || table.columns.is_empty() {
            self.warn("表格没有行或列，已跳过");
            return;
        }
        let widths = self.table_widths(table);
        let header_rows = usize::try_from(table.header_rows)
            .unwrap_or(0)
            .min(table.rows.len());
        let page_bottom = self.bottom();
        let mut index = 0_usize;
        // 行内拆分的续排位置：`Some` 表示这一行还没排完。
        let mut resume: Option<Vec<Option<CellContinuation>>> = None;
        let mut guard = 0_usize;
        while index < table.rows.len() {
            guard += 1;
            if guard > table.rows.len() * 8 + 16 {
                self.warn("表格分页次数异常，已停止排这张表");
                break;
            }
            let snapshot = self.snapshot();
            let top = self.cursor;
            let fragment = self.layout_row(
                table,
                &widths,
                index,
                top,
                page_bottom,
                resume.as_deref(),
                false,
            );
            let complete = fragment.continuation.iter().all(Option::is_none);
            if complete && fragment.bottom <= page_bottom + 1e-6 {
                // 本页放得下整行。
                self.cursor = fragment.bottom;
                index += 1;
                resume = None;
                continue;
            }
            if !fragment.placed_any {
                // 一行都没有落下：整行挪到下一页，重排表头后再试。
                self.restore(snapshot);
                self.new_flow();
                self.repeat_header_rows(table, &widths, header_rows, page_bottom);
                let retry =
                    self.layout_row(table, &widths, index, self.cursor, page_bottom, None, true);
                if retry.bottom > page_bottom + 1e-6 {
                    self.diagnostic(
                        DiagnosticCode::Geometry,
                        "表格行高于可用高度，已按行拆分或允许溢出",
                    );
                }
                self.cursor = retry.bottom;
                index += 1;
                resume = None;
                continue;
            }
            // 行内拆分：本页保留已排的部分，剩余内容续到下一页。
            self.cursor = fragment.bottom;
            self.new_flow();
            self.repeat_header_rows(table, &widths, header_rows, page_bottom);
            if complete {
                // 只是内边距/固定行高溢出，行本身已经排完。
                index += 1;
                resume = None;
            } else {
                self.diagnostic(DiagnosticCode::Geometry, "表格行跨页，已按行续排");
                resume = Some(fragment.continuation);
            }
        }
    }

    /// 跨页时重排表头行。
    fn repeat_header_rows(&mut self, table: &Table, widths: &[f64], count: usize, limit: f64) {
        for header in 0..count {
            let fragment = self.layout_row(table, widths, header, self.cursor, limit, None, true);
            self.cursor = fragment.bottom;
        }
    }

    /// 表格列宽（毫米）。
    fn table_widths(&self, table: &Table) -> Vec<f64> {
        let total = self.content_width();
        let mut widths = vec![0.0_f64; table.columns.len()];
        let mut fixed = 0.0;
        let mut weights = 0_u32;
        for (index, column) in table.columns.iter().enumerate() {
            match column.width {
                ColumnWidth::Twips(twips) => {
                    let width = twips.to_mm().max(0.0);
                    widths[index] = width;
                    fixed += width;
                }
                ColumnWidth::Weight(weight) => weights += u32::from(weight.max(1)),
                _ => weights += 1,
            }
        }
        if weights > 0 {
            let remaining = (total - fixed).max(0.0);
            for (index, column) in table.columns.iter().enumerate() {
                let weight = match column.width {
                    ColumnWidth::Weight(weight) => u32::from(weight.max(1)),
                    ColumnWidth::Twips(_) => 0,
                    _ => 1,
                };
                if weight > 0 {
                    widths[index] = remaining * f64::from(weight) / f64::from(weights);
                }
            }
        } else if fixed > total && fixed > 0.0 {
            let scale = total / fixed;
            for width in &mut widths {
                *width *= scale;
            }
        }
        widths
    }

    /// 排一行（可能只排一部分），返回本页占用的底部与每格的续排位置。
    ///
    /// `limit` 是本页可用的下边界：单元格内容排到边界就停，剩余部分通过
    /// `CellContinuation` 交给下一页续排。`resume` 是本行在上一页留下的续排位置，
    /// `force` 表示即使一行都放不下也要硬排一行（防止死循环）。
    #[allow(clippy::too_many_arguments)]
    fn layout_row(
        &mut self,
        table: &Table,
        widths: &[f64],
        index: usize,
        top: f64,
        limit: f64,
        resume: Option<&[Option<CellContinuation>]>,
        force: bool,
    ) -> RowFragment {
        let row: &Row = &table.rows[index];
        let padding_top = table.cell_padding.top.to_mm().max(0.0);
        let padding_bottom = table.cell_padding.bottom.to_mm().max(0.0);
        let padding_left = table.cell_padding.left.to_mm().max(0.0);
        let padding_right = table.cell_padding.right.to_mm().max(0.0);
        let saved_frame = self.frame;
        let saved_cursor = self.cursor;
        let saved_in_frame = self.in_frame;
        self.in_frame = true;

        let mut x = saved_frame.left;
        let mut max_bottom = top;
        let mut placed_any = false;
        let mut continuation = vec![None; row.cells.len()];
        for (column, cell) in row.cells.iter().enumerate() {
            let width = widths.get(column).copied().unwrap_or(0.0);
            if width <= 0.0 {
                continue;
            }
            let inner_left = x + padding_left;
            let inner_width = (width - padding_left - padding_right).max(1.0);
            let resume_at = resume.and_then(|items| items.get(column).copied().flatten());
            let outcome = self.layout_cell(
                cell,
                Frame {
                    left: inner_left,
                    width: inner_width,
                    top: top + padding_top,
                    bottom: limit,
                },
                top + padding_top,
                resume_at,
                force,
            );
            placed_any |= outcome.placed_any;
            max_bottom = max_bottom.max(outcome.bottom + padding_bottom);
            continuation[column] = outcome.continuation;
            x += width;
        }

        self.frame = saved_frame;
        self.cursor = saved_cursor;
        self.in_frame = saved_in_frame;

        let complete = continuation.iter().all(Option::is_none);
        let mut height = (max_bottom - top).max(1.0);
        if complete && let Some(fixed) = row.height {
            height = height.max(fixed.to_mm());
        }
        if placed_any {
            self.draw_row_borders(table, widths, index, top, top + height);
        }
        RowFragment {
            bottom: top + height,
            continuation,
            placed_any,
        }
    }

    /// 在一个单元格的子框里排版，最多排到 `frame.bottom`。
    fn layout_cell(
        &mut self,
        cell: &ofdbox_folio_ir::Cell,
        frame: Frame,
        top: f64,
        resume: Option<CellContinuation>,
        force: bool,
    ) -> CellOutcome {
        let saved_frame = self.frame;
        let saved_cursor = self.cursor;
        let saved_in_frame = self.in_frame;
        self.frame = frame;
        self.cursor = top;
        self.in_frame = true;

        let mut block_index = resume.map(|item| item.block).unwrap_or(0);
        let mut line = resume.map(|item| item.line).unwrap_or(0);
        let mut placed_any = false;
        let mut continuation = None;
        while block_index < cell.blocks.len() {
            match &cell.blocks[block_index] {
                Block::Paragraph(paragraph) => {
                    // 只有续排的第一个块需要 force；后续块如果放不下就留给下一页。
                    let force_this = force && block_index == resume.map_or(0, |item| item.block);
                    let (next, total) =
                        self.place_paragraph_bounded(paragraph, line, frame.bottom, force_this);
                    placed_any |= next > line;
                    if next < total {
                        continuation = Some(CellContinuation {
                            block: block_index,
                            line: next,
                        });
                        break;
                    }
                    block_index += 1;
                    line = 0;
                }
                block => {
                    if self.cursor > frame.bottom + 1e-6 && placed_any {
                        continuation = Some(CellContinuation {
                            block: block_index,
                            line: 0,
                        });
                        break;
                    }
                    let before = self.cursor;
                    self.place_block(block);
                    placed_any |= self.cursor > before || matches!(block, Block::Paragraph(_));
                    block_index += 1;
                    line = 0;
                }
            }
        }

        let bottom = self.cursor;
        self.frame = saved_frame;
        self.cursor = saved_cursor;
        self.in_frame = saved_in_frame;
        CellOutcome {
            bottom,
            continuation,
            placed_any,
        }
    }

    /// 画一行里每个单元格的边框。
    fn draw_row_borders(
        &mut self,
        table: &Table,
        widths: &[f64],
        index: usize,
        top: f64,
        bottom: f64,
    ) {
        let rows = table.rows.len();
        let columns = widths.len();
        let left = self.frame.left;
        let cell_count = table.rows[index].cells.len().min(columns);
        let mut x = left;
        for (column, width) in widths.iter().copied().take(cell_count).enumerate() {
            let is_first_row = index == 0;
            let is_last_row = index + 1 == rows;
            let is_first_column = column == 0;
            let is_last_column = column + 1 == cell_count;
            let horizontal = if is_first_row || is_last_row {
                table.borders.outer.or(table.borders.horizontal)
            } else {
                table.borders.horizontal.or(table.borders.outer)
            };
            let vertical = if is_first_column || is_last_column {
                table.borders.outer.or(table.borders.vertical)
            } else {
                table.borders.vertical.or(table.borders.outer)
            };
            if let Some(border) = horizontal {
                self.line(Point::new(x, top), Point::new(x + width, top), border);
                self.line(Point::new(x, bottom), Point::new(x + width, bottom), border);
            }
            if let Some(border) = vertical {
                self.line(Point::new(x, top), Point::new(x, bottom), border);
                self.line(
                    Point::new(x + width, top),
                    Point::new(x + width, bottom),
                    border,
                );
            }
            x += width;
        }
    }

    /// 画一条表格边框线。
    fn line(&mut self, from: Point, to: Point, border: Border) {
        if border.width.to_mm() <= 0.0 {
            return;
        }
        let mut path = Path::new();
        path.move_to(from);
        path.line_to(to);
        self.page.scene.children.push(Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: None,
            stroke: Some(StrokePaint {
                paint: Paint::Color(border.color),
                style: StrokeStyle {
                    width: border.width.to_mm().max(0.05),
                    cap: LineCap::Butt,
                    join: LineJoin::Miter,
                    ..StrokeStyle::default()
                },
            }),
            rule: FillRule::NonZero,
        }));
    }

    /// 段前间距：页首丢弃。
    fn advance_space(&mut self, space: Twips) {
        let space = space.to_mm().max(0.0);
        if space <= 0.0 {
            return;
        }
        if self.cursor <= self.margins.top + 1e-6 {
            return;
        }
        self.cursor += space;
    }

    fn place_paragraph(&mut self, paragraph: &Paragraph, total_pages: Option<usize>) {
        let laid = self.lay_out_paragraph(paragraph, total_pages);
        let LaidParagraph {
            style,
            pieces,
            links,
            images,
            lines,
        } = laid;

        self.advance_space(style.space_before);

        if paragraph.heading_level.is_some() {
            let title = paragraph_text(paragraph);
            if !title.trim().is_empty() {
                let level = paragraph.heading_level.unwrap_or(1).max(1);
                let page = self.fixed.pages.len();
                self.headings.push((level, title, page, self.cursor));
            }
        }

        let line_count = lines.len();
        for (index, line) in lines.iter().enumerate() {
            let size = self.max_size(&pieces, line);
            // 行内图片底部贴基线，行盒必须按图片高度加高，否则图片会盖住上一行。
            let image_height = line_image_height(&pieces, line, &images);
            let height = self
                .line_height(&style, &pieces, line)
                .max(image_height + 0.2 * size);
            if self.cursor + height > self.bottom() + 1e-6 && !self.in_frame && self.page_body {
                self.new_flow();
            }
            let baseline = self.cursor + (0.8 * size).max(image_height);
            let first = index == 0;
            self.emit_line(
                &style,
                &pieces,
                line,
                first,
                index + 1 == line_count,
                baseline,
                &links,
                &images,
            );
            self.cursor += height;
        }
        self.cursor += style.space_after.to_mm().max(0.0);
    }

    /// 测量一个段落：样式、断行单元与行区间。
    fn lay_out_paragraph(
        &mut self,
        paragraph: &Paragraph,
        total_pages: Option<usize>,
    ) -> LaidParagraph {
        let style = self.resolve_paragraph_style(&paragraph.style);
        let page_number = self.page_number();
        let (pieces, links, images) = self.measure_paragraph(paragraph, page_number, total_pages);
        let content = self.content_width();
        let indent_left = style.left_indent.to_mm().max(0.0);
        let indent_right = style.right_indent.to_mm().max(0.0);
        let width = (content - indent_left - indent_right).max(1.0);
        let first_width = (width - style.first_line_indent.to_mm()).max(1.0);
        let lines = linebreak::break_lines(&pieces, first_width, width);
        LaidParagraph {
            style,
            pieces,
            links,
            images,
            lines,
        }
    }

    /// 在框内排段落，最多排到下边界 limit；返回 (下一个未排的行, 总行数)。
    ///
    /// orce 为真时至少排一行，保证拆分能推进。
    fn place_paragraph_bounded(
        &mut self,
        paragraph: &Paragraph,
        start_line: usize,
        limit: f64,
        force: bool,
    ) -> (usize, usize) {
        let LaidParagraph {
            style,
            pieces,
            links,
            images,
            lines,
        } = self.lay_out_paragraph(paragraph, self.total_pages);
        let line_count = lines.len();
        if start_line == 0 {
            self.advance_space(style.space_before);
        }
        let mut index = start_line;
        while index < line_count {
            let line = &lines[index];
            let size = self.max_size(&pieces, line);
            let image_height = line_image_height(&pieces, line, &images);
            let height = self
                .line_height(&style, &pieces, line)
                .max(image_height + 0.2 * size);
            let fits = self.cursor + height <= limit + 1e-6;
            if !fits && !(force && index == start_line) {
                break;
            }
            let baseline = self.cursor + (0.8 * size).max(image_height);
            self.emit_line(
                &style,
                &pieces,
                line,
                index == 0,
                index + 1 == line_count,
                baseline,
                &links,
                &images,
            );
            self.cursor += height;
            index += 1;
        }
        if index >= line_count {
            self.cursor += style.space_after.to_mm().max(0.0);
        }
        (index, line_count)
    }

    /// 一段的行高。
    fn line_height(&self, style: &ParaStyle, pieces: &[Piece], line: &LineSpan) -> f64 {
        let size = self.max_size(pieces, line);
        let single = 1.2 * size;
        let base = match style.line_spacing {
            LineSpacing::Single => single,
            LineSpacing::Multiple(factor) if factor.is_finite() && factor > 0.0 => single * factor,
            LineSpacing::Multiple(_) => single,
            LineSpacing::Exact(value) => value.to_mm().max(0.1),
            LineSpacing::AtLeast(value) => value.to_mm().max(single),
            _ => single,
        };
        if base <= 0.0 { single } else { base }
    }

    fn max_size(&self, pieces: &[Piece], line: &LineSpan) -> f64 {
        pieces[line.start..line.end]
            .iter()
            .filter_map(|piece| self.styles.get(piece.style))
            .map(|style| style.size.to_mm())
            .fold(0.0_f64, f64::max)
            .max(1.0)
    }

    // ---- 测量 ----

    fn measure_paragraph(
        &mut self,
        paragraph: &Paragraph,
        page_number: usize,
        total_pages: Option<usize>,
    ) -> Measured {
        let mut pieces = Vec::new();
        let mut links = Vec::new();
        let mut images = Vec::new();
        self.measure_inlines(
            &paragraph.runs,
            page_number,
            total_pages,
            &mut pieces,
            &mut links,
            &mut images,
        );
        (pieces, links, images)
    }

    #[allow(clippy::too_many_arguments)]
    fn measure_inlines(
        &mut self,
        inlines: &[Inline],
        page_number: usize,
        total_pages: Option<usize>,
        pieces: &mut Vec<Piece>,
        links: &mut Vec<(usize, usize, LinkTarget)>,
        images: &mut Vec<MeasuredImage>,
    ) {
        for inline in inlines {
            match inline {
                Inline::Text(run) => {
                    let style = self.resolve_char_style(&run.style, run.style_id.as_deref());
                    let index = self.intern_style(&style);
                    self.push_text(&run.text, &style, index, pieces);
                }
                Inline::Link(link) => {
                    let start = pieces.len();
                    self.measure_inlines(
                        &link.runs,
                        page_number,
                        total_pages,
                        pieces,
                        links,
                        images,
                    );
                    if pieces.len() > start {
                        links.push((start, pieces.len(), link.target.clone()));
                    }
                }
                Inline::Field(field) => {
                    let style = self.resolve_char_style(&TextStyle::default(), None);
                    let index = self.intern_style(&style);
                    let text = match &field.kind {
                        FieldKind::PageNumber => page_number.to_string(),
                        FieldKind::TotalPages => total_pages
                            .map(|total| total.to_string())
                            .unwrap_or_default(),
                        _ => field.cached_text.clone().unwrap_or_default(),
                    };
                    self.push_text(&text, &style, index, pieces);
                }
                Inline::Image(image) => {
                    let style = self.resolve_char_style(&TextStyle::default(), None);
                    let index = self.intern_style(&style);
                    match self.image_size_of(image.image, image.size) {
                        Some((width, height)) => {
                            images.push(MeasuredImage {
                                asset: image.image,
                                width,
                                height,
                            });
                            pieces.push(Piece::object(width, index, images.len() - 1));
                        }
                        None => {
                            self.diagnostic(DiagnosticCode::Image, "行内图片缺少资源，已跳过");
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn push_text(&mut self, text: &str, style: &TextStyle, index: usize, pieces: &mut Vec<Piece>) {
        let key = FontKey::new(&style.family.name, style.bold, style.italic);
        let size = style.size.to_mm();
        for ch in text.chars() {
            if ch == '\n' {
                pieces.push(Piece::text('\n', 0.0, index));
                continue;
            }
            if ch == '\t' {
                // 制表符的宽度要等到知道当前 x 之后才能定，测量阶段记 0。
                pieces.push(Piece::text('\t', 0.0, index));
                continue;
            }
            let (width, missing) = self
                .fonts
                .get(&key)
                .map(|data| (data.advance(ch) * size, data.is_missing(ch)))
                .unwrap_or((0.5 * size, true));
            if missing {
                self.missing_chars += 1;
            }
            pieces.push(Piece::text(ch, width, index));
        }
    }

    fn image_size_of(
        &mut self,
        id: ofdbox_folio_ir::AssetId,
        size: Option<(Twips, Twips)>,
    ) -> Option<(f64, f64)> {
        if let Some((width, height)) = size {
            return Some((width.to_mm(), height.to_mm()));
        }
        let asset = self.document.resources.image(id)?;
        let dpi = asset
            .dpi
            .map(|(x, _)| x)
            .unwrap_or(self.options.default_image_dpi);
        let dpi_x = if dpi.is_finite() && dpi > 0.0 {
            dpi
        } else {
            self.options.default_image_dpi
        };
        let dpi_y = asset
            .dpi
            .map(|(_, y)| y)
            .filter(|y| y.is_finite() && *y > 0.0)
            .unwrap_or(dpi_x);
        Some((
            f64::from(asset.pixel_width) / dpi_x * 25.4,
            f64::from(asset.pixel_height) / dpi_y * 25.4,
        ))
    }

    // ---- 放置 ----

    #[allow(clippy::too_many_arguments)]
    fn emit_line(
        &mut self,
        style: &ParaStyle,
        pieces: &[Piece],
        line: &LineSpan,
        first: bool,
        last: bool,
        baseline: f64,
        links: &[(usize, usize, LinkTarget)],
        images: &[MeasuredImage],
    ) {
        if line.is_empty() {
            return;
        }
        let indent_left = style.left_indent.to_mm().max(0.0);
        let first_indent = if first {
            style.first_line_indent.to_mm()
        } else {
            0.0
        };
        let mut x = self.frame.left + indent_left + first_indent;
        let width = self.content_width() - indent_left - style.right_indent.to_mm().max(0.0);
        let natural: f64 = pieces[line.start..line.end]
            .iter()
            .map(|piece| piece.width)
            .sum();
        let mut extra = vec![0.0_f64; line.len()];

        // 两端对齐：最后一行按左对齐；优先拉伸空格，没有空格时均摊到字间。
        if style.align == Align::Justify && !last && width > natural + 1e-6 {
            let slack = width - natural;
            let spaces: Vec<usize> = (line.start..line.end)
                .filter(|index| pieces[*index].is_space())
                .collect();
            if !spaces.is_empty() {
                let each = slack / spaces.len() as f64;
                for index in spaces {
                    extra[index - line.start] = each;
                }
            } else if line.len() > 1 {
                let each = slack / (line.len() - 1) as f64;
                for slot in extra.iter_mut().take(line.len() - 1) {
                    *slot = each;
                }
            }
        } else {
            match style.align {
                Align::Center => x += (width - natural).max(0.0) / 2.0,
                Align::Right => x += (width - natural).max(0.0),
                _ => {}
            }
        }

        let line_start_item = self.page.items.len() as u32;
        let mut positions = Vec::with_capacity(line.len());
        for (offset, index) in (line.start..line.end).enumerate() {
            positions.push(x);
            let piece = pieces[index];
            if piece.is_tab() {
                // 制表符把 x 推到下一个制表位，并按需画前导符。
                x = self.place_tab(style, pieces, line, index, x, baseline);
                continue;
            }
            let advance = piece.width + extra[offset];
            if let Some(object) = piece.object {
                if let Some(image) = images.get(object) {
                    self.place_image(image, x, baseline, piece.width);
                }
            } else {
                self.place_text_piece(piece, x, baseline);
            }
            x += advance;
        }
        let line_end_item = self.page.items.len() as u32;

        // 行框与基线。
        let line_width = (x - self.frame.left - indent_left).max(0.0);
        let bbox = Rect::new(
            self.frame.left + indent_left,
            baseline - self.max_size(pieces, line),
            line_width,
            self.max_size(pieces, line) * 1.2,
        );
        let mut bbox = bbox;
        for item in &self.page.items[line_start_item as usize..line_end_item as usize] {
            bbox = union(bbox, item.bbox);
        }
        if line_end_item > line_start_item {
            self.page.lines.push(TextLine {
                items: line_start_item..line_end_item,
                bbox,
                baseline,
            });
        }

        // 链接区域。
        for (start, end, target) in links {
            let from = (*start).max(line.start);
            let to = (*end).min(line.end);
            if from >= to {
                continue;
            }
            let left = positions[from - line.start];
            let right = positions[(to - 1) - line.start] + pieces[to - 1].width;
            self.page.links.push(LinkArea {
                rect: Rect::new(
                    left,
                    baseline - self.max_size(pieces, line),
                    right - left,
                    self.max_size(pieces, line) * 1.2,
                ),
                target: target.clone(),
            });
        }
    }

    /// 制表符落版：把 x 推到下一个制表位，并按需画前导符。
    ///
    /// 制表位之后的文字宽度在测量阶段是已知的（除制表符本身），所以右/居中/
    /// 小数点对齐都能在这里一次算出来：先量「制表符到行尾」的宽度，再据此
    /// 决定起点，最后用前导符填满剩下的空隙。
    fn place_tab(
        &mut self,
        style: &ParaStyle,
        pieces: &[Piece],
        line: &LineSpan,
        index: usize,
        x: f64,
        baseline: f64,
    ) -> f64 {
        // 只对齐到本行末尾，行尾空白不参与。
        let mut remaining = 0.0;
        let mut decimal_width = None;
        for piece in &pieces[index + 1..line.end] {
            let piece = *piece;
            if piece.is_tab() {
                break;
            }
            if decimal_width.is_none() && piece.ch == '.' {
                // 小数点左侧的宽度：把小数点本身挪到制表位上。
                decimal_width = Some(remaining);
            }
            remaining += piece.width;
        }

        let left = self.frame.left;
        let default_step = DEFAULT_TAB_STEP;
        let stop = style
            .tabs
            .iter()
            .filter(|stop| stop.position.to_mm() > x - left + 0.01)
            .min_by(|a, b| a.position.cmp(&b.position));
        let (target, align, leader) = match stop {
            Some(stop) => (
                left + stop.position.to_mm().max(0.0),
                stop.align,
                stop.leader,
            ),
            None => {
                // 没有显式制表位：按默认步进推到下一个格点。
                let offset = x - left;
                let next = ((offset / default_step).floor() + 1.0) * default_step;
                (left + next, TabAlign::Left, TabLeader::None)
            }
        };

        let start = match align {
            TabAlign::Left => target,
            TabAlign::Right => target - remaining,
            TabAlign::Center => target - remaining / 2.0,
            TabAlign::Decimal => match decimal_width {
                Some(width) => target - width,
                None => target - remaining,
            },
            _ => target,
        }
        .max(x);

        if start > x + 0.4 {
            let color = self
                .styles
                .get(pieces[index].style)
                .map(|style| style.color)
                .unwrap_or(Rgba::BLACK);
            self.draw_leader(leader, x, start, baseline, color);
        }
        start
    }

    /// 画制表位前导符。
    fn draw_leader(&mut self, leader: TabLeader, from: f64, to: f64, baseline: f64, color: Rgba) {
        let style = match leader {
            TabLeader::None => return,
            TabLeader::Dot => StrokeStyle {
                width: 0.35,
                cap: LineCap::Round,
                join: LineJoin::Round,
                dash: vec![0.01, 1.1],
                dash_offset: 0.0,
                ..StrokeStyle::default()
            },
            TabLeader::Dash => StrokeStyle {
                width: 0.25,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                dash: vec![1.5, 1.0],
                dash_offset: 0.0,
                ..StrokeStyle::default()
            },
            TabLeader::Line => StrokeStyle {
                width: 0.25,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                dash: Vec::new(),
                dash_offset: 0.0,
                ..StrokeStyle::default()
            },
            _ => return,
        };
        let mut path = Path::new();
        path.move_to(Point::new(from, baseline));
        path.line_to(Point::new(to, baseline));
        self.page.scene.children.push(Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: None,
            stroke: Some(StrokePaint {
                paint: Paint::Color(color),
                style,
            }),
            rule: FillRule::NonZero,
        }));
    }

    fn place_text_piece(&mut self, piece: Piece, x: f64, baseline: f64) {
        let Some(style) = self.styles.get(piece.style).cloned() else {
            return;
        };
        let key = FontKey::new(&style.family.name, style.bold, style.italic);
        let Some(font) = self.font_asset(&key) else {
            self.diagnostic(
                DiagnosticCode::MissingFont,
                format!("字体缺失，跳过字符 {:?} 的绘制", piece.ch),
            );
            return;
        };
        let size = style.size.to_mm();
        let glyph = self
            .fonts
            .get(&key)
            .map(|data| data.glyph(piece.ch))
            .unwrap_or(0);
        let run = Arc::new(ofdbox_folio_ir::GlyphRun::new(
            piece.ch.to_string(),
            vec![ofdbox_folio_ir::Glyph {
                id: glyph,
                x: 0.0,
                y: 0.0,
                advance: piece.width,
            }],
            vec![ofdbox_folio_ir::Cluster {
                text_start: 0,
                text_end: 1,
                glyph_start: 0,
                glyph_end: 1,
            }],
        ));
        let transform = Matrix::translate(x, baseline);
        let element = TextElement {
            font: Arc::clone(&font),
            font_size: size,
            transform,
            run: Arc::clone(&run),
            fill: Some(Paint::Color(style.color)),
            stroke: None,
        };
        self.page.scene.children.push(Element::Text(element));
        let bbox = text_bbox(&run, size, transform);
        self.page.items.push(TextItem {
            run,
            transform,
            bbox,
            font,
            font_size: size,
        });
    }

    /// 放置行内图片：底边贴基线。
    fn place_image(&mut self, image: &MeasuredImage, x: f64, baseline: f64, width: f64) {
        let Some(stored) = self.register_image(image.asset) else {
            return;
        };
        let height = image.height.max(0.1);
        let width = width.min(image.width).max(0.1);
        let transform = Matrix::scale(
            width / f64::from(stored.pixel_width.max(1)),
            height / f64::from(stored.pixel_height.max(1)),
        )
        .then(Matrix::translate(x, baseline - height));
        self.page.scene.children.push(Element::Image(ImageElement {
            image: stored,
            rect: Rect::default(),
            transform,
            alpha: 1.0,
        }));
    }

    /// 把 Flow 的图片登记进 Fixed 的资源表，返回带 ID 的句柄。
    fn register_image(&mut self, id: ofdbox_folio_ir::AssetId) -> Option<Arc<ImageAsset>> {
        let (format, encoded, pixel_width, pixel_height, dpi, mask) = {
            let source = self.document.resources.image(id)?;
            (
                source.format,
                Arc::clone(&source.encoded),
                source.pixel_width,
                source.pixel_height,
                source.dpi,
                source.mask.clone(),
            )
        };
        let mut asset = ImageAsset::new(format, encoded, pixel_width, pixel_height);
        if let Some((x, y)) = dpi {
            asset = asset.with_dpi(x, y);
        }
        if let Some(mask) = mask {
            asset = asset.with_mask(mask);
        }
        let id = self.fixed.resources.add_image(asset);
        self.fixed.resources.image(id).cloned()
    }

    fn place_image_block(&mut self, image: &ImageBlock) {
        let Some((mut width, mut height)) = self.image_size_of(image.image, image.size) else {
            self.diagnostic(DiagnosticCode::Image, "块级图片缺少资源，已跳过");
            return;
        };
        let content = self.content_width();
        if width > content {
            let scale = content / width;
            width = content;
            height *= scale;
            self.diagnostic(DiagnosticCode::Approximation, "图片超宽，已等比缩放到版心");
        }
        if height > self.bottom() - self.cursor && !self.in_frame && self.page_body {
            self.new_flow();
        }
        let available = self.bottom() - self.cursor;
        if height > available {
            let scale = available / height;
            height = available.max(1.0);
            width *= scale;
            self.diagnostic(
                DiagnosticCode::Approximation,
                "图片超高，已等比缩放到当前页",
            );
        }
        let Some(stored) = self.register_image(image.image) else {
            return;
        };
        let x = self.frame.left
            + match image.align {
                Align::Center => (content - width) / 2.0,
                Align::Right => content - width,
                _ => 0.0,
            };
        let transform = Matrix::scale(
            width / f64::from(stored.pixel_width.max(1)),
            height / f64::from(stored.pixel_height.max(1)),
        )
        .then(Matrix::translate(x, self.cursor));
        self.page.scene.children.push(Element::Image(ImageElement {
            image: stored,
            rect: Rect::default(),
            transform,
            alpha: 1.0,
        }));
        self.cursor += height;
    }

    // ---- 大纲 ----

    fn build_outline(&self) -> Vec<OutlineItem> {
        let mut roots: Vec<OutlineItem> = Vec::new();
        // 父链：每项是（层级，在 roots 中的路径）。
        let mut path: Vec<(u8, usize)> = Vec::new();
        for (level, title, page, y) in &self.headings {
            let level = *level;
            while path.last().is_some_and(|(parent, _)| *parent >= level) {
                path.pop();
            }
            let item = OutlineItem {
                title: title.clone(),
                destination: ofdbox_folio_ir::Destination {
                    page: *page,
                    point: Point::new(0.0, *y),
                },
                children: Vec::new(),
            };
            if let Some((_, index)) = path.last() {
                let parent = element_at(&mut roots, &path, *index);
                if let Some(parent) = parent {
                    parent.children.push(item);
                    let index = parent.children.len() - 1;
                    path.push((level, index));
                    continue;
                }
            }
            roots.push(item);
            path.push((level, roots.len() - 1));
        }
        roots
    }
}

/// 沿路径取出可变的条目。
fn element_at<'b>(
    roots: &'b mut [OutlineItem],
    path: &[(u8, usize)],
    index: usize,
) -> Option<&'b mut OutlineItem> {
    let mut current: &mut OutlineItem = roots.get_mut(index)?;
    for (_, next) in path.iter().skip(1) {
        current = current.children.get_mut(*next)?;
    }
    Some(current)
}

/// 把段落直接格式（相对默认值发生变化的部分）叠加到已解析样式上。
///
/// 继承链上无法区分「显式写成默认值」与「没写」，因此默认值一律视为未设置。
fn apply_paragraph_direct(base: &mut ParaStyle, over: &ParaStyle) {
    let direct = ParaStyle::default();
    if over.align != direct.align {
        base.align = over.align;
    }
    if over.line_spacing != direct.line_spacing {
        base.line_spacing = over.line_spacing;
    }
    if over.space_before != direct.space_before {
        base.space_before = over.space_before;
    }
    if over.space_after != direct.space_after {
        base.space_after = over.space_after;
    }
    if over.first_line_indent != direct.first_line_indent {
        base.first_line_indent = over.first_line_indent;
    }
    if over.left_indent != direct.left_indent {
        base.left_indent = over.left_indent;
    }
    if over.right_indent != direct.right_indent {
        base.right_indent = over.right_indent;
    }
    if over.keep_with_next != direct.keep_with_next {
        base.keep_with_next = over.keep_with_next;
    }
    if over.keep_lines_together != direct.keep_lines_together {
        base.keep_lines_together = over.keep_lines_together;
    }
    if over.tabs != direct.tabs {
        base.tabs = over.tabs.clone();
    }
}

/// 把字符直接格式叠加到已解析样式上，规则同 [`apply_paragraph_direct`]。
fn apply_text_direct(base: &mut TextStyle, over: &TextStyle) {
    let direct = TextStyle::default();
    if over.family != direct.family {
        base.family = over.family.clone();
    }
    if over.size != direct.size {
        base.size = over.size;
    }
    if over.bold != direct.bold {
        base.bold = over.bold;
    }
    if over.italic != direct.italic {
        base.italic = over.italic;
    }
    if over.underline != direct.underline {
        base.underline = over.underline;
    }
    if over.strike != direct.strike {
        base.strike = over.strike;
    }
    if over.color != direct.color {
        base.color = over.color;
    }
    if over.superscript != direct.superscript {
        base.superscript = over.superscript;
    }
    if over.subscript != direct.subscript {
        base.subscript = over.subscript;
    }
}

/// 收集目录标题里排版时才出现的字符。
fn collect_toc_text(blocks: &[Block], chars: &mut HashSet<char>) {
    for block in blocks {
        match block {
            Block::Toc(toc) => {
                if let Some(title) = &toc.title {
                    chars.extend(title.chars());
                }
            }
            Block::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        collect_toc_text(&cell.blocks, chars);
                    }
                }
            }
            _ => {}
        }
    }
}

/// 收集块里的目录样式（标题/条目/页码），供字体预解析。
fn collect_toc_styles(blocks: &[Block], styles: &mut Vec<TextStyle>) {
    for block in blocks {
        match block {
            Block::Toc(toc) => {
                styles.push(toc.style.title_style.clone());
                styles.push(toc.style.entry_style.clone());
                styles.push(toc.style.page_number_style.clone());
            }
            Block::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        collect_toc_styles(&cell.blocks, styles);
                    }
                }
            }
            _ => {}
        }
    }
}

/// 递归收集块（含表格单元格）里的字符。
fn collect_block_chars(
    blocks: &[Block],
    engine: &Engine<'_>,
    wanted: &mut HashMap<FontKey, HashSet<char>>,
    fallbacks: &mut HashMap<String, Vec<String>>,
) {
    for block in blocks {
        match block {
            Block::Paragraph(paragraph) => {
                engine.collect_chars(&paragraph.runs, wanted, fallbacks);
            }
            Block::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        collect_block_chars(&cell.blocks, engine, wanted, fallbacks);
                    }
                }
            }
            _ => {}
        }
    }
}

/// 一行里最高的行内图片高度。
fn line_image_height(pieces: &[Piece], line: &LineSpan, images: &[MeasuredImage]) -> f64 {
    pieces[line.start..line.end]
        .iter()
        .filter_map(|piece| piece.object.and_then(|index| images.get(index)))
        .map(|image| image.height)
        .fold(0.0, f64::max)
}

/// 目录条目：层级、标题、页码。
type TocEntry = (u8, String, usize);

/// 当前排版的横向框：表格单元格、页眉页脚都会换一个框。
#[derive(Clone, Copy, Debug, Default)]
struct Frame {
    /// 左边界，毫米。
    left: f64,
    /// 可用宽度，毫米。
    width: f64,
    /// 顶边界，毫米。
    top: f64,
    /// 底边界，毫米。
    bottom: f64,
}

/// 测量好的段落。
struct LaidParagraph {
    /// 解析后的段落样式。
    style: ParaStyle,
    /// 断行单元。
    pieces: Vec<Piece>,
    /// 链接区间。
    links: Vec<(usize, usize, LinkTarget)>,
    /// 行内图片。
    images: Vec<MeasuredImage>,
    /// 行区间。
    lines: Vec<LineSpan>,
}

/// 表格行里某个单元格的续排位置。
#[derive(Clone, Copy, Debug)]
struct CellContinuation {
    /// 下一个未排的块下标。
    block: usize,
    /// 该块是段落时，已排完的行数。
    line: usize,
}

/// 一行（或行的一段）的排版结果。
#[derive(Clone, Debug)]
struct RowFragment {
    /// 本页占用的底部 y。
    bottom: f64,
    /// 每格未排完的续排位置；None 表示该格已排完。
    continuation: Vec<Option<CellContinuation>>,
    /// 本页是否落下过内容。
    placed_any: bool,
}

/// 单元格的排版结果。
#[derive(Clone, Copy, Debug)]
struct CellOutcome {
    /// 内容底部的 y。
    bottom: f64,
    /// 未排完时的续排位置。
    continuation: Option<CellContinuation>,
    /// 是否落下过内容。
    placed_any: bool,
}

/// 一页内容的快照，用于「放到一半发现放不下」时回滚。
#[derive(Clone, Copy, Debug)]
struct Snapshot {
    /// 场景子元素数量。
    children: usize,
    /// 文本项数量。
    items: usize,
    /// 行数量。
    lines: usize,
    /// 链接数量。
    links: usize,
    /// 竖直游标。
    cursor: f64,
}

/// 段落纯文本。
fn paragraph_text(paragraph: &Paragraph) -> String {
    let mut text = String::new();
    fn walk(inlines: &[Inline], text: &mut String) {
        for inline in inlines {
            match inline {
                Inline::Text(run) => text.push_str(&run.text),
                Inline::Link(link) => walk(&link.runs, text),
                Inline::Field(field) => {
                    if let Some(cached) = &field.cached_text {
                        text.push_str(cached);
                    }
                }
                _ => {}
            }
        }
    }
    walk(&paragraph.runs, &mut text);
    text
}

/// 文本项的页面包围盒。
fn text_bbox(run: &ofdbox_folio_ir::GlyphRun, size: f64, transform: Matrix) -> Rect {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for glyph in &run.glyphs {
        min_x = min_x.min(glyph.x);
        max_x = max_x.max(glyph.x + glyph.advance);
        min_y = min_y.min(glyph.y - size);
        max_y = max_y.max(glyph.y + size * 0.2);
    }
    if !min_x.is_finite() {
        return Rect::default();
    }
    transform.transform_rect(Rect::new(min_x, min_y, max_x - min_x, max_y - min_y))
}

fn union(a: Rect, b: Rect) -> Rect {
    if b.width <= 0.0 && b.height <= 0.0 {
        return a;
    }
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect::new(x, y, right - x, bottom - y)
}
