//! Flow document model (Flow IR).
//!
//! Flow IR describes only semantics and styles, without absolute coordinates. Lengths
//! are uniformly in twips; the layout stage converts them to millimeters and computes
//! pagination. The DOCX adapter and the authoring API both build this model directly.

use std::sync::Arc;

use crate::asset::AssetId;
use crate::geometry::Point;
use crate::meta::{AttachmentInfo, Metadata};
use crate::paint::Rgba;
use crate::units::Twips;

/// A flow document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlowDocument {
    /// Document metadata.
    pub metadata: Metadata,
    /// Named style sheet.
    pub styles: StyleSheet,
    /// Section list; there should be at least one section.
    pub sections: Vec<Section>,
    /// Document-level resources such as images.
    pub resources: crate::asset::ResourceStore,
    /// Attachments.
    pub attachments: Vec<Attachment>,
    /// Footnotes and endnotes.
    pub notes: Vec<Note>,
    /// Diagnostics produced during import or construction.
    pub report: crate::meta::Report,
}

impl FlowDocument {
    /// Create a document with a default A4 section.
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

    /// Append a block to the last section; create a default section if there is none.
    pub fn push(&mut self, block: Block) {
        if self.sections.is_empty() {
            self.sections.push(Section::default());
        }
        if let Some(section) = self.sections.last_mut() {
            section.blocks.push(block);
        }
    }

    /// Mutable access to the current section.
    pub fn current_section_mut(&mut self) -> &mut Section {
        if self.sections.is_empty() {
            self.sections.push(Section::default());
        }
        let index = self.sections.len() - 1;
        &mut self.sections[index]
    }
}

/// A section.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// Page setup.
    pub setup: PageSetup,
    /// Headers.
    pub headers: HeaderFooterSet,
    /// Footers.
    pub footers: HeaderFooterSet,
    /// Body blocks.
    pub blocks: Vec<Block>,
}

impl Section {
    /// Create an empty section.
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

/// Page setup, in twips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageSetup {
    /// Page width.
    pub width: Twips,
    /// Page height.
    pub height: Twips,
    /// Page margins.
    pub margins: Margins,
    /// Distance from the header to the top edge of the page.
    pub header_distance: Twips,
    /// Distance from the footer to the bottom edge of the page.
    pub footer_distance: Twips,
    /// Number of columns; 1 means a single column.
    pub columns: u16,
    /// Spacing between columns.
    pub column_gap: Twips,
}

impl PageSetup {
    /// A4 page with 1-inch margins.
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

/// Margins for all four sides, in twips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Margins {
    /// Top margin.
    pub top: Twips,
    /// Right margin.
    pub right: Twips,
    /// Bottom margin.
    pub bottom: Twips,
    /// Left margin.
    pub left: Twips,
}

impl Margins {
    /// Use the same margin on all four sides.
    pub const fn uniform(value: Twips) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }
}

/// Default, first-page, and even-page headers and footers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaderFooterSet {
    /// Default header or footer.
    pub default: Option<HeaderFooter>,
    /// First-page header or footer.
    pub first: Option<HeaderFooter>,
    /// Even-page header or footer.
    pub even: Option<HeaderFooter>,
}

/// The header or footer content of one page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaderFooter {
    /// Content blocks.
    pub blocks: Vec<Block>,
}

/// A block in a flow document.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Block {
    /// A paragraph.
    Paragraph(Paragraph),
    /// A table.
    Table(Table),
    /// A block-level image.
    Image(ImageBlock),
    /// An automatic table of contents.
    Toc(TocBlock),
    /// A page break.
    PageBreak,
    /// A floating object: detached from the body flow and positioned by page coordinates.
    Floating(Box<FloatingObject>),
}

/// The text wrapping mode of a floating object.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WrapMode {
    /// No wrapping: body text flows as usual and the object overlaps it.
    #[default]
    None,
    /// Top and bottom: body text flows above and below the object.
    TopBottom,
    /// Square: paragraphs overlapping the object make way to one side of it (the whole paragraph moves aside, without line-by-line wrapping).
    Square,
}

/// The reference basis for the position of a floating object.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PositionFrom {
    /// Relative to the top-left corner of the page.
    Page,
    /// Relative to the top-left corner of the text area (body region).
    #[default]
    Margin,
    /// Relative to the top-left corner of the current column (identical to the text area for a single column).
    Column,
    /// Relative to the anchoring paragraph (treated as the text area during layout; this is a known approximation).
    Paragraph,
}

/// A floating object.
#[derive(Clone, Debug, PartialEq)]
pub struct FloatingObject {
    /// Top-left position, in millimeters.
    pub position: Point,
    /// Reference basis for the horizontal/vertical position.
    pub position_from: (PositionFrom, PositionFrom),
    /// Width; `None` means span the whole text area.
    pub width: Option<Twips>,
    /// Content.
    pub block: Box<Block>,
    /// Wrapping mode.
    pub wrap: WrapMode,
}

impl FloatingObject {
    /// Create a floating object (position relative to the top-left corner of the text area).
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

/// A paragraph.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Paragraph {
    /// Paragraph style.
    pub style: ParaStyle,
    /// Inline content.
    pub runs: Vec<Inline>,
    /// Heading level; when `Some`, the paragraph enters the table of contents and outline.
    pub heading_level: Option<u8>,
    /// Optional anchor.
    pub anchor: Option<crate::asset::AnchorId>,
}

impl Paragraph {
    /// Create an empty paragraph.
    pub fn new(style: ParaStyle) -> Self {
        Self {
            style,
            runs: Vec::new(),
            heading_level: None,
            anchor: None,
        }
    }

    /// Create a paragraph with a single text run.
    pub fn plain(text: impl Into<String>, style: TextStyle) -> Self {
        Self {
            style: ParaStyle::default(),
            runs: vec![Inline::Text(TextRun::new(text, style))],
            heading_level: None,
            anchor: None,
        }
    }
}

/// Paragraph style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParaStyle {
    /// Horizontal alignment.
    pub align: Align,
    /// Line spacing.
    pub line_spacing: LineSpacing,
    /// Space before the paragraph.
    pub space_before: Twips,
    /// Space after the paragraph.
    pub space_after: Twips,
    /// First-line indent.
    pub first_line_indent: Twips,
    /// Left indent.
    pub left_indent: Twips,
    /// Right indent.
    pub right_indent: Twips,
    /// Keep with the next paragraph on the same page.
    pub keep_with_next: bool,
    /// Keep all lines of the paragraph on the same page.
    pub keep_lines_together: bool,
    /// Tab stops (relative to the left edge of the text area), used in ascending `position` order.
    pub tabs: Vec<TabStop>,
    /// Named style ID.
    pub style_id: Option<String>,
}

/// A tab stop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabStop {
    /// Position: relative to the left edge of the text area, in twips.
    pub position: Twips,
    /// Alignment.
    pub align: TabAlign,
    /// Leader.
    pub leader: TabLeader,
}

impl TabStop {
    /// Create a tab stop.
    pub fn new(position: Twips, align: TabAlign, leader: TabLeader) -> Self {
        Self {
            position,
            align,
            leader,
        }
    }
}

/// Tab stop alignment: determines how the text after the tab character is placed relative to the tab stop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TabAlign {
    /// Left-aligned: the text starts at the tab stop.
    #[default]
    Left,
    /// Centered: the text is centered on the tab stop.
    Center,
    /// Right-aligned: the text ends at the tab stop.
    Right,
    /// Decimal-aligned: the decimal point falls on the tab stop; without a decimal point, right-aligned.
    Decimal,
}

/// Tab stop leader: fills the blank space left by the tab character.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TabLeader {
    /// No fill.
    #[default]
    None,
    /// Dotted line (common in tables of contents).
    Dot,
    /// Dashed line.
    Dash,
    /// Solid line.
    Line,
}

/// Horizontal alignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Align {
    /// Left-aligned.
    #[default]
    Left,
    /// Centered.
    Center,
    /// Right-aligned.
    Right,
    /// Justified.
    Justify,
}

/// Line spacing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub enum LineSpacing {
    /// Single spacing.
    #[default]
    Single,
    /// Multiple spacing.
    Multiple(f64),
    /// Exact spacing.
    Exact(Twips),
    /// Minimum spacing.
    AtLeast(Twips),
}

/// Inline content.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Inline {
    /// Text.
    Text(TextRun),
    /// An inline image.
    Image(InlineImage),
    /// A field.
    Field(Field),
    /// A hyperlink.
    Link(Link),
    /// A footnote or endnote reference.
    Note(NoteRef),
}

/// A run of styled text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextRun {
    /// Text content.
    pub text: String,
    /// Text style.
    pub style: TextStyle,
    /// Named character style ID.
    pub style_id: Option<String>,
}

impl TextRun {
    /// Create a text run.
    pub fn new(text: impl Into<String>, style: TextStyle) -> Self {
        Self {
            text: text.into(),
            style,
            style_id: None,
        }
    }
}

/// Text style.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// Font stack.
    pub family: FontFamily,
    /// Font size.
    pub size: Twips,
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
    /// Strikethrough.
    pub strike: bool,
    /// Text color.
    pub color: Rgba,
    /// Superscript.
    pub superscript: bool,
    /// Subscript.
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

/// A font family and its fallback chain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontFamily {
    /// Preferred font family name.
    pub name: String,
    /// Font family names to fall back to, in order.
    pub fallback: Vec<String>,
}

impl FontFamily {
    /// Create a font family.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fallback: Vec::new(),
        }
    }
}

/// A block-level image.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageBlock {
    /// Image resource ID.
    pub image: AssetId,
    /// Display size; `None` means use the intrinsic size.
    pub size: Option<(Twips, Twips)>,
    /// Horizontal alignment.
    pub align: Align,
    /// Alternative text.
    pub alt: Option<String>,
}

impl ImageBlock {
    /// Create a block-level image.
    pub fn new(image: AssetId) -> Self {
        Self {
            image,
            size: None,
            align: Align::Left,
            alt: None,
        }
    }
}

/// An inline image.
#[derive(Clone, Debug, PartialEq)]
pub struct InlineImage {
    /// Image resource ID.
    pub image: AssetId,
    /// Display size; `None` means use the intrinsic size.
    pub size: Option<(Twips, Twips)>,
    /// Alternative text.
    pub alt: Option<String>,
}

impl InlineImage {
    /// Create an inline image.
    pub fn new(image: AssetId) -> Self {
        Self {
            image,
            size: None,
            alt: None,
        }
    }
}

/// A resolvable field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// Field kind.
    pub kind: FieldKind,
    /// Cached result from the source file.
    pub cached_text: Option<String>,
}

impl Field {
    /// Create a field.
    pub fn new(kind: FieldKind) -> Self {
        Self {
            kind,
            cached_text: None,
        }
    }
}

/// Field kind.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FieldKind {
    /// Current page number.
    PageNumber,
    /// Total page count.
    TotalPages,
    /// Current date.
    Date,
    /// Reference to a bookmark.
    Ref(crate::asset::AnchorId),
}

/// Note kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoteKind {
    /// Footnote: placed at the bottom of the page containing the reference.
    Footnote,
    /// Endnote: placed at the end of the document.
    Endnote,
}

/// A note reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NoteRef {
    /// Note kind.
    pub kind: NoteKind,
    /// Note ID unique within the document.
    pub id: u32,
}

impl NoteRef {
    /// Create a reference.
    pub fn new(kind: NoteKind, id: u32) -> Self {
        Self { kind, id }
    }
}

/// The content of a note.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    /// Note ID, corresponding to [NoteRef::id].
    pub id: u32,
    /// Note kind.
    pub kind: NoteKind,
    /// Note body.
    pub blocks: Vec<Block>,
}

impl Note {
    /// Create a note.
    pub fn new(kind: NoteKind, id: u32, blocks: Vec<Block>) -> Self {
        Self { id, kind, blocks }
    }
}

/// A hyperlink.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// Link target.
    pub target: LinkTarget,
    /// Link text or other inline content.
    pub runs: Vec<Inline>,
}

impl Link {
    /// Create a hyperlink.
    pub fn new(target: LinkTarget, runs: Vec<Inline>) -> Self {
        Self { target, runs }
    }
}

/// A link target.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LinkTarget {
    /// An external URI.
    Uri(String),
    /// An anchor within the document.
    Anchor(crate::asset::AnchorId),
}

/// An automatic table-of-contents block.
#[derive(Clone, Debug, PartialEq)]
pub struct TocBlock {
    /// Maximum heading level to collect.
    pub max_level: u8,
    /// Table-of-contents title; `None` means do not display a title.
    pub title: Option<String>,
    /// Table-of-contents style.
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

/// Table-of-contents style.
#[derive(Clone, Debug, PartialEq)]
pub struct TocStyle {
    /// Table-of-contents title style.
    pub title_style: TextStyle,
    /// Entry text style.
    pub entry_style: TextStyle,
    /// Page number style.
    pub page_number_style: TextStyle,
    /// Whether to use a dotted leader.
    pub dot_leader: bool,
    /// Indent per level.
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

/// A table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    /// Column definitions.
    pub columns: Vec<Column>,
    /// Rows.
    pub rows: Vec<Row>,
    /// How many leading rows are headers repeated across pages.
    pub header_rows: u32,
    /// Table borders.
    pub borders: TableBorders,
    /// Cell padding.
    pub cell_padding: Margins,
}

impl Table {
    /// Create an empty table.
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

/// A column width definition.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum ColumnWidth {
    /// A fixed width.
    Twips(Twips),
    /// A weight for distributing the remaining width.
    Weight(u16),
}

/// A table column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Column {
    /// Column width.
    pub width: ColumnWidth,
}

/// A table row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    /// Cells.
    pub cells: Vec<Cell>,
    /// Optional fixed row height.
    pub height: Option<Twips>,
    /// Whether this is a header row.
    pub is_header: bool,
}

impl Row {
    /// Create a row.
    pub fn new(cells: Vec<Cell>) -> Self {
        Self {
            cells,
            height: None,
            is_header: false,
        }
    }
}

/// A table cell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    /// Cell content.
    pub blocks: Vec<Block>,
    /// Number of columns spanned horizontally (gridSpan), at least 1.
    pub span: u32,
    /// Vertical merge state (`vMerge`).
    pub v_merge: VMerge,
}

impl Cell {
    /// Create a normal cell.
    pub fn new(blocks: Vec<Block>) -> Self {
        Self {
            blocks,
            span: 1,
            v_merge: VMerge::None,
        }
    }

    /// Set the number of horizontally spanned columns.
    pub fn with_span(mut self, span: u32) -> Self {
        self.span = span.max(1);
        self
    }

    /// Set the vertical merge state.
    pub fn with_v_merge(mut self, v_merge: VMerge) -> Self {
        self.v_merge = v_merge;
        self
    }
}

/// The vertical merge state of a cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VMerge {
    /// Not part of a vertical merge.
    #[default]
    None,
    /// The starting cell of a merge group (`vMerge restart`); the content is written here.
    Restart,
    /// A continuation cell of a merge group (`vMerge continue`); its content is empty and is covered by the cell above.
    Continue,
}

/// Border line style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BorderStyle {
    /// Solid line.
    #[default]
    Solid,
    /// Dashed line.
    Dashed,
    /// Dotted line.
    Dotted,
    /// Double line.
    Double,
}

/// A single border.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
    /// Line width.
    pub width: Twips,
    /// Color.
    pub color: Rgba,
    /// Line style.
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

/// A set of table borders.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TableBorders {
    /// Outer border.
    pub outer: Option<Border>,
    /// Horizontal inner borders.
    pub horizontal: Option<Border>,
    /// Vertical inner borders.
    pub vertical: Option<Border>,
}

/// A named style sheet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyleSheet {
    /// Default paragraph style.
    pub default_paragraph: ParaStyle,
    /// Default character style.
    pub default_character: TextStyle,
    /// Named paragraph styles.
    pub paragraph: Vec<NamedStyle<ParaStyle>>,
    /// Named character styles.
    pub character: Vec<NamedStyle<TextStyle>>,
}

/// A named style.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedStyle<T> {
    /// Style ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// ID of the parent style this is based on.
    pub based_on: Option<String>,
    /// Style properties.
    pub props: T,
}

/// An attachment.
#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    /// Attachment metadata.
    pub info: AttachmentInfo,
    /// Attachment bytes.
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
