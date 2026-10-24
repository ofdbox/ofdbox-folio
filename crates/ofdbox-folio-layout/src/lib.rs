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
    Align, Block, Diagnostic, DiagnosticCode, Element, FieldKind, FixedDocument, FlowDocument,
    FontAsset, FontProvider, Group, ImageAsset, ImageBlock, ImageElement, Inline, LineSpacing,
    LinkArea, LinkTarget, Matrix, OutlineItem, Page, PageInfo, PageSetup, Paint, ParaStyle,
    Paragraph, Point, Rect, Rotation, Scene, Size, TextElement, TextItem, TextLayer, TextLine,
    TextStyle, Twips,
};

use font::{FontKey, FontStore};
use linebreak::{LineSpan, Piece};

/// 布局引擎版本，写进 `EngineInfo`。
pub const LAYOUT_VERSION: &str = "layout-0.1";

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
/// 文档引用「总页数」字段时执行两遍：第一遍得到页数，第二遍代入。
pub fn layout_with(
    document: &FlowDocument,
    fonts: &mut dyn FontProvider,
    options: LayoutOptions,
) -> Result<FixedDocument, LayoutError> {
    if document.sections.is_empty() {
        return Err(LayoutError::NoSection);
    }
    let first = Engine::new(document, options).run(fonts, None);
    if !uses_total_pages(document) {
        return Ok(first);
    }
    let total = first.pages.len();
    Ok(Engine::new(document, options).run(fonts, Some(total)))
}

/// 文档是否引用总页数字段。
fn uses_total_pages(document: &FlowDocument) -> bool {
    document
        .sections
        .iter()
        .any(|section| section.blocks.iter().any(block_uses_total_pages))
}

fn block_uses_total_pages(block: &Block) -> bool {
    let Block::Paragraph(paragraph) = block else {
        return false;
    };
    paragraph.runs.iter().any(inline_uses_total_pages)
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

    fn is_empty(&self) -> bool {
        self.scene.children.is_empty() && self.items.is_empty()
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
            headings: Vec::new(),
            missing_chars: 0,
        }
    }

    fn run(mut self, provider: &mut dyn FontProvider, total_pages: Option<usize>) -> FixedDocument {
        self.prepare_fonts(provider);
        for section in &self.document.sections {
            self.begin_section(&section.setup);
            for block in &section.blocks {
                self.place_block(block, total_pages);
            }
            self.finish_page();
        }
        if self.fixed.pages.is_empty() {
            let setup = self
                .document
                .sections
                .first()
                .map(|section| section.setup)
                .unwrap_or_default();
            self.begin_section(&setup);
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
        self.fixed
    }

    // ---- 字体 ----

    /// 收集字符集并逐个字体解析一次度量。
    fn prepare_fonts(&mut self, provider: &mut dyn FontProvider) {
        let mut wanted: HashMap<FontKey, HashSet<char>> = HashMap::new();
        let mut fallbacks: HashMap<String, Vec<String>> = HashMap::new();
        for section in &self.document.sections {
            for block in &section.blocks {
                let Block::Paragraph(paragraph) = block else {
                    continue;
                };
                self.collect_chars(&paragraph.runs, &mut wanted, &mut fallbacks);
            }
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

    fn begin_section(&mut self, setup: &PageSetup) {
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
        self.cursor = self.margins.top;
    }

    fn content_width(&self) -> f64 {
        (self.page_width - self.margins.left - self.margins.right).max(1.0)
    }

    fn bottom(&self) -> f64 {
        self.page_height - self.margins.bottom
    }

    fn page_number(&self) -> usize {
        self.fixed.pages.len() + 1
    }

    fn new_page(&mut self) {
        self.finish_page();
        self.cursor = self.margins.top;
    }

    /// 结束当前页；空页不产出。
    fn finish_page(&mut self) {
        if self.page.is_empty() {
            return;
        }
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

    fn warn(&mut self, message: impl Into<String>) {
        self.fixed
            .report
            .push(Diagnostic::warning(DiagnosticCode::Geometry, message));
    }

    fn diagnostic(&mut self, code: DiagnosticCode, message: impl Into<String>) {
        self.fixed.report.push(Diagnostic::warning(code, message));
    }

    // ---- 块 ----

    fn place_block(&mut self, block: &Block, total_pages: Option<usize>) {
        match block {
            Block::Paragraph(paragraph) => self.place_paragraph(paragraph, total_pages),
            Block::Image(image) => self.place_image_block(image),
            Block::PageBreak => self.new_page(),
            Block::Table(_) => self.warn("表格排版尚未实现（A2）"),
            Block::Toc(_) => self.warn("自动目录尚未实现（A2）"),
            _ => self.warn("未知块类型，已跳过"),
        }
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
        let style = self.resolve_paragraph_style(&paragraph.style);
        let page_number = self.page_number();
        let (pieces, links, images) = self.measure_paragraph(paragraph, page_number, total_pages);
        let content = self.content_width();
        let indent_left = style.left_indent.to_mm().max(0.0);
        let indent_right = style.right_indent.to_mm().max(0.0);
        let width = (content - indent_left - indent_right).max(1.0);
        let first_width = (width - style.first_line_indent.to_mm()).max(1.0);
        let lines = linebreak::break_lines(&pieces, first_width, width);

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
            let height = self.line_height(&style, &pieces, line);
            if self.cursor + height > self.bottom() + 1e-6 && !self.page.is_empty() {
                self.new_page();
            }
            let baseline = self.cursor + 0.8 * self.max_size(&pieces, line);
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
        let mut x = self.margins.left + indent_left + first_indent;
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
        let line_width = (x - self.margins.left - indent_left).max(0.0);
        let bbox = Rect::new(
            self.margins.left + indent_left,
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
        if height > self.bottom() - self.cursor && !self.page.is_empty() {
            self.new_page();
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
        let x = self.margins.left
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

    fn build_outline(&mut self) -> Vec<OutlineItem> {
        let mut roots: Vec<OutlineItem> = Vec::new();
        // 父链：每项是（层级，在 roots 中的路径）。
        let mut path: Vec<(u8, usize)> = Vec::new();
        for (level, title, page, y) in std::mem::take(&mut self.headings) {
            while path.last().is_some_and(|(parent, _)| *parent >= level) {
                path.pop();
            }
            let item = OutlineItem {
                title,
                destination: ofdbox_folio_ir::Destination {
                    page,
                    point: Point::new(0.0, y),
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
