//! 流式文档模型（Flow IR）。
//!
//! Flow IR 只描述语义和样式，不含绝对坐标。长度统一使用 twip；布局阶段
//! 把它换算为毫米并计算分页。DOCX 适配器与创建 API 都直接构造本模型。

use std::sync::Arc;

use crate::asset::AssetId;
use crate::geometry::Point;
use crate::meta::{AttachmentInfo, Metadata};
use crate::paint::Rgba;
use crate::units::Twips;

/// 流式文档。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlowDocument {
    /// 文档元数据。
    pub metadata: Metadata,
    /// 命名样式表。
    pub styles: StyleSheet,
    /// 分节列表；至少应有一个分节。
    pub sections: Vec<Section>,
    /// 图片等文档级资源。
    pub resources: crate::asset::ResourceStore,
    /// 附件。
    pub attachments: Vec<Attachment>,
    /// 脚注与尾注。
    pub notes: Vec<Note>,
    /// 导入或构建过程中产生的诊断。
    pub report: crate::meta::Report,
}

impl FlowDocument {
    /// 创建带默认 A4 分节的文档。
    pub fn new(metadata: Metadata) -> Self {
        Self {
            metadata,
            styles: StyleSheet::default(),
            sections: vec![Section::default()],
            resources: crate::asset::ResourceStore::new(),
            attachments: Vec::new(),
            notes: Vec::new(),
            report: crate::meta::Report::new(),
        }
    }

    /// 向最后一个分节追加块；没有分节时创建默认分节。
    pub fn push(&mut self, block: Block) {
        if self.sections.is_empty() {
            self.sections.push(Section::default());
        }
        if let Some(section) = self.sections.last_mut() {
            section.blocks.push(block);
        }
    }

    /// 当前分节的可变引用。
    pub fn current_section_mut(&mut self) -> &mut Section {
        if self.sections.is_empty() {
            self.sections.push(Section::default());
        }
        let index = self.sections.len() - 1;
        &mut self.sections[index]
    }
}

/// 一个分节。
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// 页面设置。
    pub setup: PageSetup,
    /// 页眉。
    pub headers: HeaderFooterSet,
    /// 页脚。
    pub footers: HeaderFooterSet,
    /// 正文块。
    pub blocks: Vec<Block>,
}

impl Section {
    /// 创建空分节。
    pub fn new(setup: PageSetup) -> Self {
        Self {
            setup,
            headers: HeaderFooterSet::default(),
            footers: HeaderFooterSet::default(),
            blocks: Vec::new(),
        }
    }
}

impl Default for Section {
    fn default() -> Self {
        Self::new(PageSetup::default())
    }
}

/// 页面设置，单位 twip。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageSetup {
    /// 页面宽度。
    pub width: Twips,
    /// 页面高度。
    pub height: Twips,
    /// 页边距。
    pub margins: Margins,
    /// 页眉到页面顶边的距离。
    pub header_distance: Twips,
    /// 页脚到页面底边的距离。
    pub footer_distance: Twips,
    /// 分栏数；1 表示单栏。
    pub columns: u16,
    /// 栏间距。
    pub column_gap: Twips,
}

impl PageSetup {
    /// A4 页面，页边距 1 英寸。
    pub fn a4() -> Self {
        Self {
            width: Twips(11906),
            height: Twips(16838),
            margins: Margins::uniform(Twips(1440)),
            header_distance: Twips(720),
            footer_distance: Twips(720),
            columns: 1,
            column_gap: Twips::from_mm(8.0),
        }
    }
}

impl Default for PageSetup {
    fn default() -> Self {
        Self::a4()
    }
}

/// 四边边距，单位 twip。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Margins {
    /// 上边距。
    pub top: Twips,
    /// 右边距。
    pub right: Twips,
    /// 下边距。
    pub bottom: Twips,
    /// 左边距。
    pub left: Twips,
}

impl Margins {
    /// 四边使用相同边距。
    pub const fn uniform(value: Twips) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }
}

/// 默认、首页和偶数页页眉页脚。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaderFooterSet {
    /// 默认页眉或页脚。
    pub default: Option<HeaderFooter>,
    /// 首页页眉或页脚。
    pub first: Option<HeaderFooter>,
    /// 偶数页页眉或页脚。
    pub even: Option<HeaderFooter>,
}

/// 一页页眉或页脚内容。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaderFooter {
    /// 内容块。
    pub blocks: Vec<Block>,
}

/// 流式文档的块。
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Block {
    /// 段落。
    Paragraph(Paragraph),
    /// 表格。
    Table(Table),
    /// 块级图片。
    Image(ImageBlock),
    /// 自动目录。
    Toc(TocBlock),
    /// 分页符。
    PageBreak,
    /// 浮动对象：脱离正文流、按页面坐标定位。
    Floating(Box<FloatingObject>),
}

/// 浮动对象的绕排方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WrapMode {
    /// 不绕排：正文照原样排，对象覆盖在上面。
    #[default]
    None,
    /// 上下型：正文排在对象上方与下方。
    TopBottom,
    /// 四周型：与对象重叠的段落让到对象一侧（整段让开，不逐行绕排）。
    Square,
}

/// 浮动对象位置的参考基准。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PositionFrom {
    /// 相对页面左上角。
    Page,
    /// 相对版心（正文区域）左上角。
    #[default]
    Margin,
    /// 相对当前栏左上角（单栏时等同版心）。
    Column,
    /// 相对锚定段落（排版时按版心处理，属于已知近似）。
    Paragraph,
}

/// 一个浮动对象。
#[derive(Clone, Debug, PartialEq)]
pub struct FloatingObject {
    /// 左上角位置，单位毫米。
    pub position: Point,
    /// 水平/垂直位置的参考基准。
    pub position_from: (PositionFrom, PositionFrom),
    /// 宽度；None 表示占满版心。
    pub width: Option<Twips>,
    /// 内容。
    pub block: Box<Block>,
    /// 绕排方式。
    pub wrap: WrapMode,
}

impl FloatingObject {
    /// 创建浮动对象（位置相对版心左上角）。
    pub fn new(position: Point, block: Block) -> Self {
        Self {
            position,
            position_from: (PositionFrom::Margin, PositionFrom::Margin),
            width: None,
            block: Box::new(block),
            wrap: WrapMode::None,
        }
    }
}

/// 段落。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Paragraph {
    /// 段落样式。
    pub style: ParaStyle,
    /// 行内内容。
    pub runs: Vec<Inline>,
    /// 标题层级；`Some` 时会进入目录与大纲。
    pub heading_level: Option<u8>,
    /// 可选锚点。
    pub anchor: Option<crate::asset::AnchorId>,
}

impl Paragraph {
    /// 创建空段落。
    pub fn new(style: ParaStyle) -> Self {
        Self {
            style,
            runs: Vec::new(),
            heading_level: None,
            anchor: None,
        }
    }

    /// 创建只有一个文本 run 的段落。
    pub fn plain(text: impl Into<String>, style: TextStyle) -> Self {
        Self {
            style: ParaStyle::default(),
            runs: vec![Inline::Text(TextRun::new(text, style))],
            heading_level: None,
            anchor: None,
        }
    }
}

/// 段落样式。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParaStyle {
    /// 水平对齐。
    pub align: Align,
    /// 行距。
    pub line_spacing: LineSpacing,
    /// 段前间距。
    pub space_before: Twips,
    /// 段后间距。
    pub space_after: Twips,
    /// 首行缩进。
    pub first_line_indent: Twips,
    /// 左缩进。
    pub left_indent: Twips,
    /// 右缩进。
    pub right_indent: Twips,
    /// 与下一段保持同页。
    pub keep_with_next: bool,
    /// 段内各行保持同页。
    pub keep_lines_together: bool,
    /// 制表位（相对版心左边界），按 `position` 升序使用。
    pub tabs: Vec<TabStop>,
    /// 命名样式 ID。
    pub style_id: Option<String>,
}

/// 一个制表位。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabStop {
    /// 位置：相对版心左边界，单位 twip。
    pub position: Twips,
    /// 对齐方式。
    pub align: TabAlign,
    /// 前导符。
    pub leader: TabLeader,
}

impl TabStop {
    /// 创建一个制表位。
    pub fn new(position: Twips, align: TabAlign, leader: TabLeader) -> Self {
        Self {
            position,
            align,
            leader,
        }
    }
}

/// 制表位对齐方式：决定制表符之后那截文字相对制表位怎么放。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TabAlign {
    /// 左对齐：文字从制表位开始。
    #[default]
    Left,
    /// 居中：文字以制表位为中心。
    Center,
    /// 右对齐：文字以制表位为右边界。
    Right,
    /// 小数点对齐：小数点落在制表位上；没有小数点时按右对齐。
    Decimal,
}

/// 制表位前导符：填充制表符留下的空白。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TabLeader {
    /// 不填充。
    #[default]
    None,
    /// 点线（目录常用）。
    Dot,
    /// 虚线。
    Dash,
    /// 实线。
    Line,
}

/// 水平对齐。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Align {
    /// 左对齐。
    #[default]
    Left,
    /// 居中。
    Center,
    /// 右对齐。
    Right,
    /// 两端对齐。
    Justify,
}

/// 行距。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub enum LineSpacing {
    /// 单倍行距。
    #[default]
    Single,
    /// 多倍行距。
    Multiple(f64),
    /// 固定行距。
    Exact(Twips),
    /// 最小行距。
    AtLeast(Twips),
}

/// 行内内容。
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Inline {
    /// 文本。
    Text(TextRun),
    /// 行内图片。
    Image(InlineImage),
    /// 字段。
    Field(Field),
    /// 超链接。
    Link(Link),
    /// 脚注或尾注引用。
    Note(NoteRef),
}

/// 一段带样式的文本。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextRun {
    /// 文本内容。
    pub text: String,
    /// 文本样式。
    pub style: TextStyle,
    /// 命名字符样式 ID。
    pub style_id: Option<String>,
}

impl TextRun {
    /// 创建文本 run。
    pub fn new(text: impl Into<String>, style: TextStyle) -> Self {
        Self {
            text: text.into(),
            style,
            style_id: None,
        }
    }
}

/// 文本样式。
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// 字体栈。
    pub family: FontFamily,
    /// 字号。
    pub size: Twips,
    /// 粗体。
    pub bold: bool,
    /// 斜体。
    pub italic: bool,
    /// 下划线。
    pub underline: bool,
    /// 删除线。
    pub strike: bool,
    /// 文本颜色。
    pub color: Rgba,
    /// 上标。
    pub superscript: bool,
    /// 下标。
    pub subscript: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            family: FontFamily::default(),
            size: Twips::from_pt(12.0),
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            color: Rgba::BLACK,
            superscript: false,
            subscript: false,
        }
    }
}

/// 字体家族与回退链。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontFamily {
    /// 首选字体家族名。
    pub name: String,
    /// 按顺序回退的字体家族名。
    pub fallback: Vec<String>,
}

impl FontFamily {
    /// 创建字体家族。
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fallback: Vec::new(),
        }
    }
}

/// 块级图片。
#[derive(Clone, Debug, PartialEq)]
pub struct ImageBlock {
    /// 图片资源 ID。
    pub image: AssetId,
    /// 显示尺寸；`None` 表示使用固有尺寸。
    pub size: Option<(Twips, Twips)>,
    /// 水平对齐。
    pub align: Align,
    /// 替代文本。
    pub alt: Option<String>,
}

impl ImageBlock {
    /// 创建块级图片。
    pub fn new(image: AssetId) -> Self {
        Self {
            image,
            size: None,
            align: Align::Left,
            alt: None,
        }
    }
}

/// 行内图片。
#[derive(Clone, Debug, PartialEq)]
pub struct InlineImage {
    /// 图片资源 ID。
    pub image: AssetId,
    /// 显示尺寸；`None` 表示使用固有尺寸。
    pub size: Option<(Twips, Twips)>,
    /// 替代文本。
    pub alt: Option<String>,
}

impl InlineImage {
    /// 创建行内图片。
    pub fn new(image: AssetId) -> Self {
        Self {
            image,
            size: None,
            alt: None,
        }
    }
}

/// 可解析字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// 字段类型。
    pub kind: FieldKind,
    /// 源文件中的缓存结果。
    pub cached_text: Option<String>,
}

impl Field {
    /// 创建字段。
    pub fn new(kind: FieldKind) -> Self {
        Self {
            kind,
            cached_text: None,
        }
    }
}

/// 字段类型。
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FieldKind {
    /// 当前页码。
    PageNumber,
    /// 总页数。
    TotalPages,
    /// 当前日期。
    Date,
    /// 引用书签。
    Ref(crate::asset::AnchorId),
}

/// 注释类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoteKind {
    /// 脚注：排在引用所在页的页底。
    Footnote,
    /// 尾注：排在文档末尾。
    Endnote,
}

/// 注释引用。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NoteRef {
    /// 注释类型。
    pub kind: NoteKind,
    /// 文档内唯一的注释 ID。
    pub id: u32,
}

impl NoteRef {
    /// 创建引用。
    pub fn new(kind: NoteKind, id: u32) -> Self {
        Self { kind, id }
    }
}

/// 一条注释的内容。
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    /// 注释 ID，与 [NoteRef::id] 对应。
    pub id: u32,
    /// 注释类型。
    pub kind: NoteKind,
    /// 注释正文。
    pub blocks: Vec<Block>,
}

impl Note {
    /// 创建注释。
    pub fn new(kind: NoteKind, id: u32, blocks: Vec<Block>) -> Self {
        Self { id, kind, blocks }
    }
}

/// 超链接。
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// 链接目标。
    pub target: LinkTarget,
    /// 链接文本或其他行内内容。
    pub runs: Vec<Inline>,
}

impl Link {
    /// 创建超链接。
    pub fn new(target: LinkTarget, runs: Vec<Inline>) -> Self {
        Self { target, runs }
    }
}

/// 链接目标。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LinkTarget {
    /// 外部 URI。
    Uri(String),
    /// 文档内锚点。
    Anchor(crate::asset::AnchorId),
}

/// 自动目录块。
#[derive(Clone, Debug, PartialEq)]
pub struct TocBlock {
    /// 收集的最大标题层级。
    pub max_level: u8,
    /// 目录标题；`None` 表示不显示标题。
    pub title: Option<String>,
    /// 目录样式。
    pub style: TocStyle,
}

impl Default for TocBlock {
    fn default() -> Self {
        Self {
            max_level: 3,
            title: None,
            style: TocStyle::default(),
        }
    }
}

/// 目录样式。
#[derive(Clone, Debug, PartialEq)]
pub struct TocStyle {
    /// 目录标题样式。
    pub title_style: TextStyle,
    /// 条目文本样式。
    pub entry_style: TextStyle,
    /// 页码样式。
    pub page_number_style: TextStyle,
    /// 是否使用点线引导。
    pub dot_leader: bool,
    /// 每级缩进。
    pub indent_per_level: Twips,
}

impl Default for TocStyle {
    fn default() -> Self {
        Self {
            title_style: TextStyle::default(),
            entry_style: TextStyle::default(),
            page_number_style: TextStyle::default(),
            dot_leader: true,
            indent_per_level: Twips::from_pt(12.0),
        }
    }
}

/// 表格。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    /// 列定义。
    pub columns: Vec<Column>,
    /// 行。
    pub rows: Vec<Row>,
    /// 前多少行是跨页重复的表头。
    pub header_rows: u32,
    /// 表格边框。
    pub borders: TableBorders,
    /// 单元格内边距。
    pub cell_padding: Margins,
}

impl Table {
    /// 创建空表格。
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            header_rows: 0,
            borders: TableBorders::default(),
            cell_padding: Margins::default(),
        }
    }
}

/// 列宽定义。
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum ColumnWidth {
    /// 固定宽度。
    Twips(Twips),
    /// 按剩余宽度分配权重。
    Weight(u16),
}

/// 表格列。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Column {
    /// 列宽。
    pub width: ColumnWidth,
}

/// 表格行。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    /// 单元格。
    pub cells: Vec<Cell>,
    /// 可选固定行高。
    pub height: Option<Twips>,
    /// 是否表头行。
    pub is_header: bool,
}

impl Row {
    /// 创建行。
    pub fn new(cells: Vec<Cell>) -> Self {
        Self {
            cells,
            height: None,
            is_header: false,
        }
    }
}

/// 表格单元格。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    /// 单元格内容。
    pub blocks: Vec<Block>,
    /// 横向合并的列数（gridSpan），至少 1。
    pub span: u32,
    /// 纵向合并状态（Merge）。
    pub v_merge: VMerge,
}

impl Cell {
    /// 创建普通单元格。
    pub fn new(blocks: Vec<Block>) -> Self {
        Self {
            blocks,
            span: 1,
            v_merge: VMerge::None,
        }
    }

    /// 设置横向合并列数。
    pub fn with_span(mut self, span: u32) -> Self {
        self.span = span.max(1);
        self
    }

    /// 设置纵向合并状态。
    pub fn with_v_merge(mut self, v_merge: VMerge) -> Self {
        self.v_merge = v_merge;
        self
    }
}

/// 单元格的纵向合并状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VMerge {
    /// 不参与纵向合并。
    #[default]
    None,
    /// 合并组的起始单元格（Merge restart），内容写在这里。
    Restart,
    /// 合并组的延续单元格（Merge continue），内容为空、由上方单元格覆盖。
    Continue,
}

/// 边框线型。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BorderStyle {
    /// 实线。
    #[default]
    Solid,
    /// 虚线。
    Dashed,
    /// 点线。
    Dotted,
    /// 双线。
    Double,
}

/// 单条边框。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
    /// 线宽。
    pub width: Twips,
    /// 颜色。
    pub color: Rgba,
    /// 线型。
    pub style: BorderStyle,
}

impl Default for Border {
    fn default() -> Self {
        Self {
            width: Twips::from_pt(0.5),
            color: Rgba::BLACK,
            style: BorderStyle::Solid,
        }
    }
}

/// 表格边框集合。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TableBorders {
    /// 外边框。
    pub outer: Option<Border>,
    /// 水平内边框。
    pub horizontal: Option<Border>,
    /// 垂直内边框。
    pub vertical: Option<Border>,
}

/// 命名样式表。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyleSheet {
    /// 默认段落样式。
    pub default_paragraph: ParaStyle,
    /// 默认字符样式。
    pub default_character: TextStyle,
    /// 命名段落样式。
    pub paragraph: Vec<NamedStyle<ParaStyle>>,
    /// 命名字符样式。
    pub character: Vec<NamedStyle<TextStyle>>,
}

/// 一条命名样式。
#[derive(Clone, Debug, PartialEq)]
pub struct NamedStyle<T> {
    /// 样式 ID。
    pub id: String,
    /// 显示名称。
    pub name: String,
    /// 基于的父样式 ID。
    pub based_on: Option<String>,
    /// 样式属性。
    pub props: T,
}

/// 附件。
#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    /// 附件元数据。
    pub info: AttachmentInfo,
    /// 附件字节。
    pub bytes: Arc<[u8]>,
}

#[cfg(test)]
mod tests {
    use super::{Block, FlowDocument, Margins, PageSetup, Paragraph, TextStyle, Twips};
    use crate::meta::Metadata;

    #[test]
    fn new_document_has_a4_section() {
        let document = FlowDocument::new(Metadata::default());
        assert_eq!(document.sections.len(), 1);
        assert_eq!(document.sections[0].setup.width, PageSetup::a4().width);
    }

    #[test]
    fn push_creates_default_section_when_missing() {
        let mut document = FlowDocument::new(Metadata::default());
        document.sections.clear();
        document.push(Block::Paragraph(Paragraph::plain(
            "hello",
            TextStyle::default(),
        )));
        assert_eq!(document.sections.len(), 1);
        assert_eq!(document.sections[0].blocks.len(), 1);
    }

    #[test]
    fn uniform_margins_apply_to_all_sides() {
        let margins = Margins::uniform(Twips(720));
        assert_eq!(margins.top, margins.bottom);
        assert_eq!(margins.left, margins.right);
    }
}
