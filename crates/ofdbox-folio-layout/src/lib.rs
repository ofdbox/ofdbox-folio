//! The one-way typesetting engine of OFDBox Folio.
//!
//! It takes [`ofdbox_folio_ir::FlowDocument`] and a font provider as input and
//! outputs [`ofdbox_folio_ir::FixedDocument`]. Layout is the only path from Flow
//! to Fixed; no reverse conversion is provided, and no rendering or file IO is
//! performed.
//!
//! The engine covers paragraphs (indent, line spacing, alignment and
//! justification), inline/block/floating images, tables, headers and footers,
//! footnotes and endnotes, automatic tables of contents, page breaks, page
//! number fields and heading outlines. The frozen rules live in
//! `crates/ofdbox-folio-layout/CONTRACT.md`.

mod font;
mod linebreak;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ofdbox_folio_ir::{
    Align, Block, Border, ColumnWidth, Diagnostic, DiagnosticCode, Element, FieldKind, FillRule,
    FixedDocument, FlowDocument, FontAsset, FontProvider, Group, HeaderFooter, HeaderFooterSet,
    ImageAsset, ImageBlock, ImageElement, Inline, LineCap, LineJoin, LineSpacing, LinkArea,
    LinkTarget, Matrix, NoteKind, NoteRef, OutlineItem, Page, PageInfo, PageSetup, Paint,
    ParaStyle, Paragraph, Path, PathElement, Point, Rect, Rgba, Rotation, Row, Scene, Size,
    StrokePaint, StrokeStyle, TabAlign, TabLeader, TabStop, Table, TextElement, TextItem,
    TextLayer, TextLine, TextRun, TextStyle, Twips,
};

use font::{FontKey, FontStore};
use linebreak::{LineSpan, Piece};

/// Layout engine version, written into `EngineInfo`.
pub const LAYOUT_VERSION: &str = "layout-0.1";

/// Default step (millimeters) a tab advances by when no explicit tab stop is
/// declared.
const DEFAULT_TAB_STEP: f64 = 12.5;

/// Left indent of the note area (millimeters).
const NOTE_INDENT: f64 = 8.0;

/// Gap between notes (millimeters).
const NOTE_GAP: f64 = 1.6;

/// Exclusion area of square wrapping (page coordinates, millimeters).
#[derive(Clone, Copy, Debug)]
struct FloatExclusion {
    /// Top boundary.
    top: f64,
    /// Bottom boundary.
    bottom: f64,
    /// Left boundary.
    left: f64,
    /// Right boundary.
    right: f64,
}

/// A note waiting to be typeset.
struct PendingNote {
    /// The reference itself.
    note: NoteRef,
    /// Body blocks with the number already prepended.
    blocks: Vec<Block>,
    /// Typeset height, millimeters.
    height: f64,
}

/// Prepend the number to the first paragraph of the note body; footnotes use
/// `1)`, endnotes use `(1)`.
fn with_note_number(kind: NoteKind, blocks: &[Block], number: usize) -> Vec<Block> {
    let marker = match kind {
        NoteKind::Footnote => format!("{number}) "),
        NoteKind::Endnote => format!("({number}) "),
    };
    let mut blocks = blocks.to_vec();
    match blocks.first_mut() {
        Some(Block::Paragraph(paragraph)) => {
            let style = paragraph
                .runs
                .iter()
                .find_map(|inline| match inline {
                    Inline::Text(run) => Some(run.style.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            paragraph
                .runs
                .insert(0, Inline::Text(TextRun::new(marker, style)));
        }
        _ => blocks.insert(
            0,
            Block::Paragraph(Paragraph::plain(
                marker,
                TextStyle {
                    size: Twips::from_pt(9.0),
                    ..TextStyle::default()
                },
            )),
        ),
    }
    blocks
}

/// Layout error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LayoutError {
    /// The document has no sections.
    #[error("the document has no sections and cannot be laid out")]
    NoSection,
}

/// Layout options.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutOptions {
    /// Resolution used when an image declares no DPI.
    pub default_image_dpi: f64,
    /// Heading text of the endnotes page.
    pub endnote_title: String,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            default_image_dpi: 96.0,
            endnote_title: "Endnotes".to_owned(),
        }
    }
}

/// Typeset a flow document into a paginated document.
pub fn layout(
    document: &FlowDocument,
    fonts: &mut dyn FontProvider,
) -> Result<FixedDocument, LayoutError> {
    layout_with(document, fonts, LayoutOptions::default())
}

/// Typesetting entry point with options.
///
/// When the total-pages field is referenced, one pass is made first to obtain
/// the page count; when an automatic table of contents is present, one pass is
/// made first to obtain heading page numbers, which are then fed back into a
/// re-layout until the TOC page numbers stabilize (at most 4 passes).
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

    // First pass: the TOC content is empty; this pass only provides heading
    // page numbers and the total page count.
    let mut total_pages = None;
    let mut toc_entries: Option<Vec<TocEntry>> = None;
    let mut last = None;
    for _ in 0..4 {
        let (fixed, headings) =
            Engine::new(document, options.clone()).run(fonts, total_pages, toc_entries.clone());
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
    Ok(last.expect("laid out at least once"))
}

/// Whether the document contains an automatic table of contents.
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

/// Whether the document references the total-pages field.
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

/// Four-sided margins in millimeters.
#[derive(Clone, Copy, Debug)]
struct MarginsMm {
    top: f64,
    right: f64,
    bottom: f64,
    left: f64,
}

/// Content accumulated for the current page.
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

/// A measured inline image.
#[derive(Clone, Copy, Debug)]
struct MeasuredImage {
    /// Image ID in the Flow resource table.
    asset: ofdbox_folio_ir::AssetId,
    /// Display width, millimeters.
    width: f64,
    /// Display height, millimeters.
    height: f64,
}

/// Paragraph measurement result: line breaking pieces, link ranges, inline
/// images.
type Measured = (
    Vec<Piece>,
    Vec<(usize, usize, LinkTarget)>,
    Vec<MeasuredImage>,
);

/// Deduplication key for text styles.
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

/// One layout run.
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
    /// Current layout frame; body text uses the text area, while table cells and
    /// headers/footers temporarily switch it.
    frame: Frame,
    /// Whether layout is inside a frame: no automatic pagination inside a frame.
    in_frame: bool,
    /// Whether the current page has body content (pages with only a header are
    /// not emitted).
    page_body: bool,
    /// Whether the current page has started laying out body text.
    page_started: bool,
    /// Page ordinal within the current section, used for "different first page".
    section_page: usize,
    /// Number of columns.
    column_count: u16,
    /// Gap between columns (millimeters).
    column_gap: f64,
    /// Current column index.
    column_index: usize,
    /// Distance from the header to the top edge.
    header_distance: f64,
    /// Distance from the footer to the bottom edge.
    footer_distance: f64,
    /// Header of the current section.
    headers: HeaderFooterSet,
    /// Footer of the current section.
    footers: HeaderFooterSet,
    /// Total page count already known during multi-pass layout.
    total_pages: Option<usize>,
    /// TOC entries collected during the previous layout pass.
    toc_entries: Option<Vec<TocEntry>>,
    /// Note numbering: (kind, ID) → ordinal.
    note_numbers: HashMap<(NoteKind, u32), usize>,
    /// Footnote counter.
    next_footnote: usize,
    /// Endnote counter.
    next_endnote: usize,
    /// Footnotes pending for this page.
    page_notes: Vec<PendingNote>,
    /// Height occupied by the bottom note area; the body bottom boundary must
    /// move up accordingly.
    bottom_inset: f64,
    /// Endnotes waiting to be laid out at the end of the document.
    endnotes: Vec<PendingNote>,
    /// Square wrapping exclusion areas on this page.
    float_exclusions: Vec<FloatExclusion>,
    headings: Vec<(u8, String, usize, f64)>,
    /// Count of characters missing glyphs in their font, summarized into one
    /// diagnostic at the end.
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
            note_numbers: HashMap::new(),
            next_footnote: 0,
            next_endnote: 0,
            page_notes: Vec::new(),
            bottom_inset: 0.0,
            endnotes: Vec::new(),
            float_exclusions: Vec::new(),
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
            self.place_blocks(&section.blocks);
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
        if !self.endnotes.is_empty() {
            self.flush_endnotes();
        }
        if self.missing_chars > 0 {
            self.diagnostic(
                DiagnosticCode::MissingFont,
                format!(
                    "{} characters are missing glyphs in the current font",
                    self.missing_chars
                ),
            );
        }

        self.fixed.outline = self.build_outline();
        self.fixed.engine.layout_version = LAYOUT_VERSION;
        (self.fixed, self.headings)
    }

    // ---- Fonts ----

    /// Collect the character set and parse the metrics of each font once.
    ///
    /// Text in table cells and headers/footers must be pre-resolved as well;
    /// the styles used by the automatic TOC and fields do not appear in the IR
    /// runs (their text is generated at layout time), so the fonts of those
    /// styles are pre-resolved against the document-wide character set.
    /// Otherwise TOC entries would be drawn as empty boxes because "the font was
    /// not prepared".
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
        // Footnote and endnote bodies are only laid out at the page bottom /
        // document end, so their fonts must be prepared as well; otherwise the
        // subset would not contain those glyphs.
        for note in &self.document.notes {
            collect_block_chars(&note.blocks, self, &mut wanted, &mut fallbacks);
        }

        // Characters used document-wide, as a fallback for styles whose text is
        // "generated at layout time"; the TOC title and page numbers are
        // generated at layout time, so they count too.
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
        // Per-glyph fallback: switch to a generic font when the primary font
        // lacks a glyph, so that whole runs of CJK text do not turn blank.
        self.fonts.prepare_fallback(provider, &all_chars);
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

    /// Fetch (registering if necessary) the font resource in the Fixed IR.
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

    /// Resource of the fallback font (used for per-glyph fallback), cached under
    /// a fixed key.
    fn fallback_asset(&mut self) -> Option<Arc<FontAsset>> {
        let key = FontKey::new("", false, false);
        if let Some(asset) = self.font_assets.get(&key) {
            return Some(Arc::clone(asset));
        }
        let asset = self.fonts.fallback()?.asset();
        let id = self.fixed.resources.add_font_asset(asset);
        let stored = self.fixed.resources.font(id)?.clone();
        self.font_assets.insert(key, Arc::clone(&stored));
        Some(stored)
    }

    // ---- Styles ----

    /// Resolve a paragraph style: the named style is the base and direct
    /// formatting overrides non-default values.
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
        // Start from the default style, then layer non-default values in
        // parent → child order; fields left unset by a child inherit from its
        // parent.
        let mut resolved = self.document.styles.default_paragraph.clone();
        for named in chain.into_iter().rev() {
            apply_paragraph_direct(&mut resolved, &named.props);
        }
        Some(resolved)
    }

    /// Resolve a character style: the named character style is the base and
    /// direct formatting overrides non-default values.
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

    /// Register and deduplicate a text style, returning the style index.
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

    // ---- Pages ----

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
            self.warn("invalid page size; falling back to A4");
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
        self.float_exclusions.clear();
        self.frame = self.column_frame(0);
        self.cursor = self.frame.top;
    }

    /// Text area frame.
    /// Full-page text area frame (used by headers and footers, spanning all
    /// columns).
    fn page_frame(&self) -> Frame {
        Frame {
            left: self.margins.left,
            width: (self.page_width - self.margins.left - self.margins.right).max(1.0),
            top: self.margins.top,
            bottom: self.page_height - self.margins.bottom,
        }
    }

    /// Frame of column `index`.
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

    /// Advance to the next column; returns false when there is none.
    fn next_column(&mut self) -> bool {
        let count = usize::from(self.column_count.max(1));
        if self.column_index + 1 >= count {
            return false;
        }
        self.column_index += 1;
        // Square-wrap exclusion areas belong to the current column container.
        self.float_exclusions.clear();
        self.frame = self.column_frame(self.column_index);
        self.cursor = self.frame.top;
        true
    }

    /// Move the body flow to the next container: first the next column, and on
    /// the last column a new page.
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
        self.bottom_inset = 0.0;
        // Square-wrap exclusion areas belong to the page where the floating
        // object was placed; they must not affect later pages.
        self.float_exclusions.clear();
        self.frame = self.column_frame(0);
        self.cursor = self.frame.top;
        self.page_body = false;
        self.page_started = false;
        self.section_page += 1;
    }

    /// End the current page; pages without body content are not emitted.
    fn finish_page(&mut self) {
        if !self.page_body {
            return;
        }
        let footer_top = self.layout_footer();
        let body_bottom = self.page_height - self.margins.bottom;
        self.emit_page_notes(footer_top.min(body_bottom));
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

    /// Snapshot the current page, used to roll back a failed placement.
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

    // ---- Blocks ----

    fn place_block(&mut self, block: &Block) {
        // Lay out the header only before the first body block of a page, so that
        // empty pages do not get a header either.
        if !self.page_body && !self.in_frame && !matches!(block, Block::PageBreak) {
            self.start_page_body();
        }
        match block {
            Block::Paragraph(paragraph) => self.place_paragraph(paragraph, self.total_pages),
            Block::Image(image) => self.place_image_block(image),
            Block::PageBreak => self.new_page(),
            Block::Table(table) => self.place_table(table),
            Block::Toc(toc) => self.place_toc(toc),
            Block::Floating(floating) => self.place_floating(floating),
            _ => self.warn("unknown block type; skipped"),
        }
    }

    /// Lay out a group of blocks in order, handling `keep_with_next`: a heading
    /// must not be left alone at the bottom of a page.
    fn place_blocks(&mut self, blocks: &[Block]) {
        let width = self.content_width();
        for (index, block) in blocks.iter().enumerate() {
            self.break_for_keep_with_next(blocks, index, width);
            self.place_block(block);
        }
    }

    /// Break to the next column / page early when the current block carries
    /// `keep_with_next` (possibly forming a chain) and it plus the start of the
    /// next block do not fit on this page.
    fn break_for_keep_with_next(&mut self, blocks: &[Block], index: usize, width: f64) {
        if self.in_frame || !self.page_body {
            return;
        }
        let mut needed = 0.0_f64;
        let mut cursor = index;
        while let Some(block) = blocks.get(cursor) {
            let keep =
                matches!(block, Block::Paragraph(paragraph) if paragraph.style.keep_with_next);
            if !keep {
                break;
            }
            needed += self.measure_blocks(std::slice::from_ref(block), width);
            cursor += 1;
        }
        if cursor == index {
            return;
        }
        let Some(next) = blocks.get(cursor) else {
            return;
        };
        needed += self.block_leading_height(next, width);
        let content = self.bottom() - self.frame.top;
        if needed <= content && self.cursor + needed > self.bottom() + 1e-6 {
            self.new_flow();
        }
    }

    /// Height needed for the "start" of a block: the first line for a paragraph,
    /// the header plus first row for a table, the whole block otherwise.
    fn block_leading_height(&mut self, block: &Block, width: f64) -> f64 {
        match block {
            Block::Paragraph(paragraph) => {
                let laid = self.lay_out_paragraph(paragraph, self.total_pages);
                let Some(line) = laid.lines.first() else {
                    return 0.0;
                };
                let line_box = self.line_box(&laid.style, &laid.pieces, &laid.images, line);
                laid.style.space_before.to_mm().max(0.0) + line_box.height
            }
            Block::Table(table) => {
                let mut head = table.clone();
                let rows = usize::try_from(table.header_rows).unwrap_or(0).max(1);
                head.rows.truncate(rows.min(table.rows.len()));
                self.measure_blocks(&[Block::Table(head)], width)
            }
            _ => self.measure_blocks(std::slice::from_ref(block), width),
        }
    }

    /// The page body starts here: mark it and lay out the header.
    fn start_page_body(&mut self) {
        self.page_body = true;
        self.begin_body();
    }

    /// Lay out the header when the body starts.
    fn begin_body(&mut self) {
        if self.page_started {
            return;
        }
        self.page_started = true;
        self.layout_header();
    }

    /// Which header/footer set this page uses: different first page → default;
    /// even page → even-page set.
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
            self.warn("header exceeds the top margin and may overlap the body");
        }
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
    }

    /// Lay out the footer and return the top y it occupies (the body bottom edge
    /// when there is no footer).
    fn layout_footer(&mut self) -> f64 {
        let body_bottom = self.page_height - self.margins.bottom;
        let first_page = self.section_page == 0;
        let Some(footer) = Self::pick_footer(&self.footers, first_page, self.section_page) else {
            return body_bottom;
        };
        let blocks = footer.blocks.clone();
        if blocks.is_empty() {
            return body_bottom;
        }
        let width = self.page_frame().width;
        // Measure the height first, then place it right above "bottom edge -
        // footer distance".
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
        top
    }

    // ---- Floating objects ----

    /// Lay out a floating object: position it absolutely in page coordinates,
    /// then affect the body cursor according to its wrap mode.
    ///
    /// It does not participate in the body flow and does not cause pagination;
    /// the page it lands on is "the page containing the current cursor".
    fn place_floating(&mut self, floating: &ofdbox_folio_ir::FloatingObject) {
        if self.in_frame {
            self.warn("floating object nested in a table/header; treated as normal content");
            self.place_block(&floating.block);
            return;
        }
        let page = self.page_frame();
        let width = floating
            .width
            .map(|width| width.to_mm())
            .unwrap_or(page.width)
            .clamp(1.0, page.width);
        // Convert the top-left corner according to the reference base: `page` is
        // relative to the page origin, `margin`/`column` relative to the text
        // area, and `paragraph` is currently treated as the text area (a known
        // approximation).
        let (from_h, from_v) = floating.position_from;
        let origin_x = match from_h {
            ofdbox_folio_ir::PositionFrom::Page => 0.0,
            _ => page.left,
        };
        let origin_y = match from_v {
            ofdbox_folio_ir::PositionFrom::Page => 0.0,
            _ => page.top,
        };
        let x = (origin_x + floating.position.x).clamp(0.0, page.left + page.width);
        let y = (origin_y + floating.position.y).max(0.0);
        let saved = (self.frame, self.cursor, self.in_frame);
        self.in_frame = true;
        self.frame = Frame {
            left: x,
            width,
            top: y,
            bottom: f64::MAX,
        };
        self.cursor = y;
        self.place_block(&floating.block);
        let bottom = self.cursor;
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;

        match floating.wrap {
            ofdbox_folio_ir::WrapMode::None => {
                // Floating on top; the body is unaffected.
            }
            ofdbox_folio_ir::WrapMode::TopBottom => {
                // The body continues below the object.
                if bottom > self.cursor {
                    self.cursor = bottom;
                }
            }
            ofdbox_folio_ir::WrapMode::Square => {
                // Record an exclusion area: paragraphs that vertically overlap it
                // move to the other side of the object.
                self.float_exclusions.push(FloatExclusion {
                    top: y,
                    bottom,
                    left: x,
                    right: x + width,
                });
            }
            _ => {}
        }
    }

    /// Floating areas the current paragraph must avoid (square wrapping).
    ///
    /// The whole paragraph yields as one unit: if the object is on the left half
    /// of the page the paragraph shifts right, if it is on the right half the
    /// right margin shrinks.
    fn floating_insets(&self, top: f64, bottom: f64) -> (f64, f64) {
        let page = self.page_frame();
        let mut left = 0.0_f64;
        let mut right = 0.0_f64;
        for exclusion in &self.float_exclusions {
            if exclusion.bottom <= top || exclusion.top >= bottom {
                continue;
            }
            let middle = page.left + page.width / 2.0;
            if exclusion.left + exclusion.right < 2.0 * middle {
                left = left.max(exclusion.right - page.left);
            } else {
                right = right.max(page.left + page.width - exclusion.left);
            }
        }
        (left, right)
    }

    // ---- Footnotes and endnotes ----

    /// Get (allocating if necessary) the note number, starting at 1.
    fn note_number(&mut self, note: NoteRef) -> usize {
        if let Some(number) = self.note_numbers.get(&(note.kind, note.id)) {
            return *number;
        }
        let number = match note.kind {
            NoteKind::Footnote => {
                self.next_footnote += 1;
                self.next_footnote
            }
            NoteKind::Endnote => {
                self.next_endnote += 1;
                self.next_endnote
            }
        };
        self.note_numbers.insert((note.kind, note.id), number);
        number
    }

    /// Find the note body.
    fn note_blocks(&self, note: NoteRef) -> Option<Vec<Block>> {
        self.document
            .notes
            .iter()
            .find(|item| item.id == note.id && item.kind == note.kind)
            .map(|item| item.blocks.clone())
    }

    /// Collect the note references on a line and reserve page-bottom space for
    /// them.
    fn reserve_notes_in_line(&mut self, pieces: &[Piece], line: &LineSpan) {
        for piece in &pieces[line.start..line.end] {
            let Some(note) = piece.note else {
                continue;
            };
            if self.page_notes.iter().any(|item| item.note == note)
                || self.endnotes.iter().any(|item| item.note == note)
            {
                continue;
            }
            let Some(blocks) = self.note_blocks(note) else {
                self.diagnostic(
                    DiagnosticCode::Unsupported,
                    format!("note {} has no body; skipped", note.id),
                );
                continue;
            };
            let number = self.note_number(note);
            let blocks = with_note_number(note.kind, &blocks, number);
            match note.kind {
                NoteKind::Footnote => {
                    let width = (self.page_frame().width - NOTE_INDENT).max(1.0);
                    let height = self.measure_blocks(&blocks, width) + NOTE_GAP;
                    self.page_notes.push(PendingNote {
                        note,
                        blocks,
                        height,
                    });
                    self.bottom_inset += height;
                    self.refresh_frame_bottom();
                }
                NoteKind::Endnote => self.endnotes.push(PendingNote {
                    note,
                    blocks,
                    height: 0.0,
                }),
            }
        }
    }

    /// Recompute the current frame's bottom boundary after the note area grows.
    ///
    /// When the body has already been pushed past the page bottom by the notes,
    /// `cursor` is not pulled back: that line is already placed, and later lines
    /// will move to the next column / page automatically because
    /// cursor + line height > bottom.
    fn refresh_frame_bottom(&mut self) {
        if self.in_frame {
            return;
        }
        self.frame.bottom = self.column_frame(self.column_index).bottom;
    }

    /// Lay out this page's footnotes at the bottom of the page.
    fn emit_page_notes(&mut self, bottom: f64) {
        let notes = std::mem::take(&mut self.page_notes);
        if notes.is_empty() {
            return;
        }
        let saved = (self.frame, self.cursor, self.in_frame);
        let width = self.page_frame().width;
        let total: f64 = notes
            .iter()
            .map(|note| note.height.max(NOTE_GAP * 2.0))
            .sum();
        let top = (bottom - total).max(0.0);
        self.draw_note_separator(top - NOTE_GAP / 2.0, width);
        self.layout_notes(&notes, top, width);
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
    }

    /// Lay out a list of notes (reused by endnotes); `top` is the top of the note
    /// area.
    fn emit_notes(&mut self, mut notes: Vec<PendingNote>, bottom: f64, separator: bool) {
        if notes.is_empty() {
            return;
        }
        let width = self.page_frame().width;
        let inner = (width - NOTE_INDENT).max(1.0);
        for note in &mut notes {
            if note.height <= 0.0 {
                note.height = self.measure_blocks(&note.blocks, inner) + NOTE_GAP;
            }
        }
        let total: f64 = notes.iter().map(|note| note.height).sum();
        let top = (bottom - total).max(0.0);
        if separator {
            self.draw_note_separator(top - NOTE_GAP / 2.0, width);
        }
        self.layout_notes(&notes, top, width);
    }

    /// The short rule above the note area.
    fn draw_note_separator(&mut self, y: f64, width: f64) {
        let mut path = Path::new();
        path.move_to(Point::new(self.margins.left, y));
        path.line_to(Point::new(self.margins.left + width / 3.0, y));
        self.page.scene.children.push(Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: None,
            stroke: Some(StrokePaint {
                paint: Paint::Color(Rgba::BLACK),
                style: StrokeStyle {
                    width: 0.2,
                    cap: LineCap::Butt,
                    join: LineJoin::Miter,
                    dash: Vec::new(),
                    dash_offset: 0.0,
                    ..StrokeStyle::default()
                },
            }),
            rule: FillRule::NonZero,
        }));
    }

    /// Lay out the note bodies downward starting at `top`.
    fn layout_notes(&mut self, notes: &[PendingNote], top: f64, width: f64) {
        let saved = (self.frame, self.cursor, self.in_frame);
        self.in_frame = true;
        self.frame = Frame {
            left: self.margins.left + NOTE_INDENT,
            width: (width - NOTE_INDENT).max(1.0),
            top,
            bottom: f64::MAX,
        };
        self.cursor = top;
        for note in notes {
            for block in &note.blocks {
                self.place_block(block);
            }
            self.cursor += NOTE_GAP;
        }
        self.frame = saved.0;
        self.cursor = saved.1;
        self.in_frame = saved.2;
    }

    /// Lay out the endnotes at the end of the document.
    fn flush_endnotes(&mut self) {
        let notes = std::mem::take(&mut self.endnotes);
        if notes.is_empty() {
            return;
        }
        self.new_page();
        self.start_page_body();
        let title = self.options.endnote_title.clone();
        let mut heading = Paragraph::plain(
            title,
            TextStyle {
                size: Twips::from_pt(14.0),
                ..TextStyle::default()
            },
        );
        heading.heading_level = None;
        self.place_paragraph(&heading, self.total_pages);
        self.emit_notes(notes, self.bottom(), false);
        self.finish_page();
    }

    // ---- Automatic table of contents ----

    /// Lay out the automatic TOC: a title plus one paragraph per "entry ⇥ page
    /// number", with the page number placed on a right tab stop.
    ///
    /// Page numbers come from the previous layout pass; the space the TOC itself
    /// occupies shifts them all, and `layout_with` keeps feeding them back until
    /// they stabilize. Entry text and page number are separated by a tab, the
    /// right tab stop sits at the right boundary of the text area, and a dotted
    /// leader is drawn when `TocStyle::dot_leader` is enabled.
    fn place_toc(&mut self, toc: &ofdbox_folio_ir::TocBlock) {
        if self.in_frame {
            self.warn("table of contents inside a table cell; skipped");
            return;
        }
        let Some(entries) = self.toc_entries.clone() else {
            // The first layout pass has no entries; wait for later passes.
            return;
        };
        let entries: Vec<TocEntry> = entries
            .into_iter()
            .filter(|(level, title, _)| *level <= toc.max_level.max(1) && !title.trim().is_empty())
            .collect();
        if entries.is_empty() {
            self.warn("automatic table of contents has no usable headings");
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
            // Outline targets are 0-based page ordinals; the TOC records 1-based
            // page numbers.
            paragraph.runs.push(Inline::Text(TextRun::new(
                (page + 1).to_string(),
                page_number_style.clone(),
            )));
            self.place_paragraph(&paragraph, self.total_pages);
        }
    }

    /// Measure the height of a group of blocks on a temporary page (emitting no
    /// elements).
    fn measure_blocks(&mut self, blocks: &[Block], width: f64) -> f64 {
        let saved_page = std::mem::take(&mut self.page);
        let saved = (self.frame, self.cursor, self.in_frame);
        // Measurement must not have side effects: the outline, footnote
        // reservations and float exclusion areas all have to be restored.
        let saved_headings = self.headings.len();
        let saved_notes = self.page_notes.len();
        let saved_inset = self.bottom_inset;
        let saved_exclusions = self.float_exclusions.len();
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
        self.headings.truncate(saved_headings);
        self.page_notes.truncate(saved_notes);
        self.bottom_inset = saved_inset;
        self.float_exclusions.truncate(saved_exclusions);
        height
    }

    // ---- Tables ----

    fn place_table(&mut self, table: &Table) {
        if table.rows.is_empty() || table.columns.is_empty() {
            self.warn("table has no rows or columns; skipped");
            return;
        }
        let widths = self.table_widths(table);
        let header_rows = usize::try_from(table.header_rows)
            .unwrap_or(0)
            .min(table.rows.len());
        let page_bottom = self.bottom();
        let mut index = 0_usize;
        // Continuation position for in-row splitting: `Some` means this row has
        // not been fully laid out yet.
        let mut resume: Option<Vec<Option<CellContinuation>>> = None;
        let mut guard = 0_usize;
        while index < table.rows.len() {
            guard += 1;
            if guard > table.rows.len() * 8 + 16 {
                self.warn("abnormal table pagination count; stopped laying out this table");
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
                // The whole row fits on this page.
                self.cursor = fragment.bottom;
                index += 1;
                resume = None;
                continue;
            }
            if !fragment.placed_any {
                // Not a single line was placed: move the whole row to the next
                // page, repeat the header and try again.
                self.restore(snapshot);
                self.new_flow();
                self.repeat_header_rows(table, &widths, header_rows, page_bottom);
                let retry =
                    self.layout_row(table, &widths, index, self.cursor, page_bottom, None, true);
                if retry.bottom > page_bottom + 1e-6 {
                    self.diagnostic(
                        DiagnosticCode::Geometry,
                        "table row is taller than the available height; split by row or allowed to overflow",
                    );
                }
                self.cursor = retry.bottom;
                index += 1;
                resume = None;
                continue;
            }
            // In-row splitting: keep the part laid out on this page and continue
            // the rest on the next page.
            self.cursor = fragment.bottom;
            self.new_flow();
            self.repeat_header_rows(table, &widths, header_rows, page_bottom);
            if complete {
                // Only padding / fixed row height overflows; the row itself is
                // already complete.
                index += 1;
                resume = None;
            } else {
                self.diagnostic(
                    DiagnosticCode::Geometry,
                    "table row spans pages; continued by row",
                );
                resume = Some(fragment.continuation);
            }
        }
    }

    /// Repeat the header rows when the table crosses a page.
    fn repeat_header_rows(&mut self, table: &Table, widths: &[f64], count: usize, limit: f64) {
        for header in 0..count {
            let fragment = self.layout_row(table, widths, header, self.cursor, limit, None, true);
            self.cursor = fragment.bottom;
        }
    }

    /// Table column widths (millimeters).
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

    /// Lay out one row (possibly only part of it) and return the bottom occupied
    /// on this page plus the continuation position of each cell.
    ///
    /// `limit` is the available bottom boundary on this page: cell content stops
    /// at the boundary and the remainder is handed to the next page through
    /// `CellContinuation`. `resume` is the continuation position this row left on
    /// the previous page, and `force` means one line must be laid out even if
    /// nothing fits (to prevent an infinite loop).
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
        // Column cursor: a merged cell occupies several columns according to its
        // span.
        let mut column = 0_usize;
        for (cell_index, cell) in row.cells.iter().enumerate() {
            let span = usize::try_from(cell.span.max(1)).unwrap_or(1);
            let width: f64 = widths.iter().skip(column).take(span).sum();
            if width <= 0.0 {
                column += span;
                continue;
            }
            let inner_left = x + padding_left;
            let inner_width = (width - padding_left - padding_right).max(1.0);
            let resume_at = resume.and_then(|items| items.get(cell_index).copied().flatten());
            // A vertically merged continuation cell has no content of its own; it
            // is covered by the starting cell above.
            if cell.v_merge == ofdbox_folio_ir::VMerge::Continue {
                x += width;
                column += span;
                continue;
            }
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
            continuation[cell_index] = outcome.continuation;
            x += width;
            column += span;
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

    /// Lay out inside a cell's sub-frame, up to `frame.bottom` at most.
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
                    // Only the first continued block needs `force`; later blocks
                    // are left for the next page when they do not fit.
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

    /// Draw the border of every cell in a row.
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
        let mut x = left;
        let mut column = 0_usize;
        for cell in &table.rows[index].cells {
            let span = usize::try_from(cell.span.max(1)).unwrap_or(1);
            let width: f64 = widths.iter().skip(column).take(span).sum();
            if width <= 0.0 {
                column += span;
                continue;
            }
            let is_first_row = index == 0;
            let is_last_row = index + 1 == rows;
            let is_first_column = column == 0;
            let is_last_column = column + span >= columns;
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
                // The top of a vertically merged continuation cell is covered by
                // the cell above, so no line is drawn there.
                if cell.v_merge != ofdbox_folio_ir::VMerge::Continue {
                    self.line(Point::new(x, top), Point::new(x + width, top), border);
                }
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
            column += span;
        }
    }

    /// Draw one table border line.
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

    /// Space before a paragraph: dropped at the top of a page.
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
            inset_left,
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

        // Square floating objects: paragraphs that vertically overlap the object
        // are re-laid out line by line (each line breaks against the width
        // available at that moment).
        if !self.in_frame && line_count > 0 {
            // Probe with a whole-paragraph height estimate: as soon as any part
            // falls in a wrap band, switch to the line-by-line path.
            let first_size = self.max_size(&pieces, &lines[0]);
            let estimate = line_count as f64 * (first_size * 1.2).max(1.0);
            let (probe_left, probe_right) =
                self.floating_insets(self.cursor, self.cursor + estimate);
            if probe_left > 0.0 || probe_right > 0.0 {
                self.place_paragraph_wrapped(&style, &pieces, &links, &images);
                return;
            }
        }

        // keep_lines_together: if the whole paragraph does not fit, move all of it
        // to the next column / page.
        if !self.in_frame && self.page_body && style.keep_lines_together && line_count > 0 {
            let block = Block::Paragraph(paragraph.clone());
            let total = self.measure_blocks(std::slice::from_ref(&block), self.content_width());
            let content = self.bottom() - self.frame.top;
            if total <= content && self.cursor + total > self.bottom() + 1e-6 {
                self.new_flow();
            }
        }

        let mut index = 0_usize;
        while index < line_count {
            // How many more lines still fit on this page.
            let fit = if self.in_frame {
                line_count - index
            } else {
                self.lines_that_fit(&style, &pieces, &images, &lines, index, self.bottom())
            };
            if fit == 0 && !self.in_frame && self.page_body {
                // Not even one line fits: move to the next column / page and
                // recompute.
                self.new_flow();
                continue;
            }
            let mut take = fit.max(1);
            let at_top = self.cursor <= self.frame.top + 0.01;
            // Widow: only 1 line fits on this page while more content follows,
            // and this is not the top of the page → move the whole paragraph.
            if !self.in_frame && self.page_body && take == 1 && index + 1 < line_count && !at_top {
                self.new_flow();
                continue;
            }
            // Orphan: only 1 line would be left after the page break → place one
            // fewer line here so the next page gets at least two.
            if !self.in_frame && self.page_body && take >= 2 && index + take + 1 == line_count {
                take -= 1;
            }
            for offset in 0..take {
                let line = &lines[index + offset];
                // The line box height and baseline are both driven by the font
                // metrics; images stretch the line box by their height.
                let line_box = self.line_box(&style, &pieces, &images, line);
                let baseline = self.cursor + line_box.ascent;
                let first = index + offset == 0;
                let last = index + offset + 1 == line_count;
                self.emit_line(
                    &style, &pieces, line, first, last, baseline, &links, &images, inset_left,
                );
                self.cursor += line_box.height;
            }
            index += take;
            if index < line_count && !self.in_frame && self.page_body {
                self.new_flow();
            }
        }
        self.cursor += style.space_after.to_mm().max(0.0);
    }

    /// Line-by-line wrapping: re-break each line against the width available at
    /// the current y, going around square floating objects.
    fn place_paragraph_wrapped(
        &mut self,
        style: &ParaStyle,
        pieces: &[Piece],
        links: &[(usize, usize, LinkTarget)],
        images: &[MeasuredImage],
    ) {
        let content = self.content_width();
        let mut from = 0_usize;
        let mut first = true;
        let mut guard = 0_usize;
        while from < pieces.len() {
            guard += 1;
            if guard > pieces.len() * 4 + 32 {
                break;
            }
            // Estimate the line height from the current font size to decide which
            // wrap band this line falls into.
            let estimate = self.max_size(
                pieces,
                &LineSpan {
                    start: from,
                    end: (from + 1).min(pieces.len()),
                },
            ) * 1.2;
            let (left, right) = self.floating_insets(self.cursor, self.cursor + estimate);
            let indent_left = style.left_indent.to_mm().max(0.0) + left;
            let indent_right = style.right_indent.to_mm().max(0.0) + right;
            let width = (content - indent_left - indent_right).max(1.0);
            let first_indent = if first {
                style.first_line_indent.to_mm()
            } else {
                0.0
            };
            let first_width = (width - first_indent).max(1.0);
            let lines = linebreak::break_lines(&pieces[from..], first_width, width);
            let Some(relative) = lines.first() else {
                break;
            };
            let span = LineSpan {
                start: from + relative.start,
                end: from + relative.end,
            };
            let line_box = self.line_box(style, pieces, images, &span);
            if self.cursor + line_box.height > self.bottom() + 1e-6 && self.page_body {
                self.new_flow();
                continue;
            }
            let baseline = self.cursor + line_box.ascent;
            let last = span.end >= pieces.len();
            self.emit_line(
                style, pieces, &span, first, last, baseline, links, images, left,
            );
            self.cursor += line_box.height;
            from = span.end;
            first = false;
        }
        self.cursor += style.space_after.to_mm().max(0.0);
    }

    /// How many lines, starting from line `start`, still fit entirely in the
    /// current frame.
    fn lines_that_fit(
        &self,
        style: &ParaStyle,
        pieces: &[Piece],
        images: &[MeasuredImage],
        lines: &[LineSpan],
        start: usize,
        bottom: f64,
    ) -> usize {
        let mut y = self.cursor;
        let mut count = 0;
        for line in &lines[start..] {
            let height = self.line_box(style, pieces, images, line).height;
            if y + height > bottom + 1e-6 {
                break;
            }
            y += height;
            count += 1;
        }
        count
    }

    /// Measure a paragraph: style, line breaking pieces and line ranges.
    fn lay_out_paragraph(
        &mut self,
        paragraph: &Paragraph,
        total_pages: Option<usize>,
    ) -> LaidParagraph {
        let style = self.resolve_paragraph_style(&paragraph.style);
        let page_number = self.page_number();
        let (pieces, links, images) = self.measure_paragraph(paragraph, page_number, total_pages);
        let content = self.content_width();
        // Break lines against the full width here; paragraphs that overlap a
        // floating object are re-laid out line by line by
        // `place_paragraph_wrapped`.
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
            inset_left: 0.0,
        }
    }

    /// Lay out a paragraph inside the frame, up to the bottom boundary `limit`;
    /// returns (next line not yet laid out, total line count).
    ///
    /// When `force` is true at least one line is laid out, so that splitting
    /// always makes progress.
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
            inset_left,
        } = self.lay_out_paragraph(paragraph, self.total_pages);
        let line_count = lines.len();
        if start_line == 0 {
            self.advance_space(style.space_before);
        }
        let mut index = start_line;
        while index < line_count {
            let line = &lines[index];
            // The line box height and baseline are both driven by the font
            // metrics; images stretch the line box by their height.
            let line_box = self.line_box(&style, &pieces, &images, line);
            let height = line_box.height;
            let fits = self.cursor + height <= limit + 1e-6;
            if !fits && !(force && index == start_line) {
                break;
            }
            let baseline = self.cursor + line_box.ascent;
            self.emit_line(
                &style,
                &pieces,
                line,
                index == 0,
                index + 1 == line_count,
                baseline,
                &links,
                &images,
                inset_left,
            );
            self.cursor += height;
            index += 1;
        }
        if index >= line_count {
            self.cursor += style.space_after.to_mm().max(0.0);
        }
        (index, line_count)
    }

    /// Height of one line: single spacing is computed from the font's
    /// ascent/descent/lineGap, and the multiplier and fixed line spacing adjust
    /// that base.
    fn line_height(&self, style: &ParaStyle, pieces: &[Piece], line: &LineSpan) -> f64 {
        let size = self.max_size(pieces, line);
        let (_, height_em) = self.font_metrics(pieces, line);
        let single = size * height_em;
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

    /// Vertical metrics of the line's primary font: (ascent, single line height),
    /// in em.
    fn font_metrics(&self, pieces: &[Piece], line: &LineSpan) -> (f64, f64) {
        // An empty line (an empty paragraph or a whitespace-only one) has no
        // pieces in its span; fall back to all pieces of the paragraph so the
        // line still uses the paragraph's font metrics.
        let range = if line.start < line.end {
            &pieces[line.start..line.end]
        } else {
            pieces
        };
        let mut best: Option<(f64, (f64, f64))> = None;
        for piece in range {
            let Some(style) = self.styles.get(piece.style) else {
                continue;
            };
            let size = style.size.to_mm();
            let key = FontKey::new(&style.family.name, style.bold, style.italic);
            let metrics = match self.fonts.get(&key) {
                Some(font) => (font.ascender.max(0.1), font.line_height()),
                // Fall back to empirical values when the font could not be
                // resolved, matching the old behavior.
                None => (0.8, 1.2),
            };
            if best.is_none_or(|(best_size, _)| size > best_size) {
                best = Some((size, metrics));
            }
        }
        best.map(|(_, metrics)| metrics).unwrap_or((0.8, 1.2))
    }

    /// Line box height and baseline offset (relative to the line box top) of one
    /// line.
    fn line_box(
        &self,
        style: &ParaStyle,
        pieces: &[Piece],
        images: &[MeasuredImage],
        line: &LineSpan,
    ) -> LineBox {
        let size = self.max_size(pieces, line);
        let image_height = line_image_height(pieces, line, images);
        let (ascent_em, _) = self.font_metrics(pieces, line);
        let height = self
            .line_height(style, pieces, line)
            .max(image_height + 0.2 * size);
        // Baseline: text is shifted down by the font ascent, and sits on the image
        // bottom when the line contains an image.
        let ascent = (size * ascent_em).max(image_height);
        let ascent = ascent.min((height - 0.1).max(0.0));
        LineBox { height, ascent }
    }

    fn max_size(&self, pieces: &[Piece], line: &LineSpan) -> f64 {
        // Empty lines fall back to all pieces of the paragraph, and a fully
        // empty paragraph falls back to the default text size so it still
        // occupies a normal line.
        let range = if line.start < line.end {
            &pieces[line.start..line.end]
        } else {
            pieces
        };
        let size = range
            .iter()
            .filter_map(|piece| self.styles.get(piece.style))
            .map(|style| style.size.to_mm())
            .fold(0.0_f64, f64::max);
        if size > 0.0 {
            size
        } else {
            TextStyle::default().size.to_mm()
        }
    }

    // ---- Measurement ----

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
                Inline::Note(note) => {
                    // Reference marker: a smaller number laid out as normal text;
                    // page-bottom space for the note area is reserved at
                    // placement time.
                    let mut style = self.resolve_char_style(&TextStyle::default(), None);
                    style.size = Twips(style.size.0 * 7 / 10);
                    let index = self.intern_style(&style);
                    let number = self.note_number(*note);
                    let text = number.to_string();
                    let start = pieces.len();
                    self.push_text(&text, &style, index, pieces);
                    if let Some(first) = pieces.get_mut(start) {
                        first.note = Some(*note);
                    }
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
                            self.diagnostic(
                                DiagnosticCode::Image,
                                "inline image has no resource; skipped",
                            );
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
                // A tab's width can only be determined once the current x is
                // known, so it is recorded as 0 during measurement.
                pieces.push(Piece::text('\t', 0.0, index));
                continue;
            }
            // Use the fallback font's glyph width when the primary font lacks the
            // character; only when both lack it is the character counted as
            // missing.
            let (width, missing) = match self.fonts.get(&key) {
                Some(data) if !data.is_missing(ch) => (data.advance(ch) * size, false),
                _ => match self.fonts.fallback() {
                    Some(fallback) if !fallback.is_missing(ch) => {
                        (fallback.advance(ch) * size, false)
                    }
                    _ => (0.5 * size, true),
                },
            };
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

    // ---- Placement ----

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
        inset_left: f64,
    ) {
        if line.is_empty() {
            return;
        }
        let indent_left = style.left_indent.to_mm().max(0.0) + inset_left;
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

        // Justification: the last line is left-aligned; spaces are stretched
        // first, and when there are none the slack is distributed between
        // characters.
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
                // A tab pushes x to the next tab stop and draws a leader when
                // needed.
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
        // Footnote references on this line must reserve page-bottom space on this
        // page.
        if !self.in_frame {
            self.reserve_notes_in_line(pieces, line);
        }

        // Line box and baseline.
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

        // Link areas.
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

    /// Tab placement: push x to the next tab stop and draw a leader when needed.
    ///
    /// The width of the text after the tab stop is already known from
    /// measurement (excluding the tab itself), so right / center / decimal
    /// alignment can all be computed here in one go: first measure the width
    /// from the tab to the end of the line, then decide the start position from
    /// it, and finally fill the remaining gap with the leader.
    fn place_tab(
        &mut self,
        style: &ParaStyle,
        pieces: &[Piece],
        line: &LineSpan,
        index: usize,
        x: f64,
        baseline: f64,
    ) -> f64 {
        // Only align up to the end of this line; trailing whitespace does not
        // participate.
        let mut remaining = 0.0;
        let mut decimal_width = None;
        for piece in &pieces[index + 1..line.end] {
            let piece = *piece;
            if piece.is_tab() {
                break;
            }
            if decimal_width.is_none() && piece.ch == '.' {
                // Width left of the decimal point: move the decimal point itself
                // onto the tab stop.
                decimal_width = Some(remaining);
            }
            remaining += piece.width;
        }

        let left = self.frame.left;
        // Body right edge: neither the tab stop nor the following content may
        // cross it.
        let right = left + self.content_width();
        let default_step = DEFAULT_TAB_STEP;
        let stop = style
            .tabs
            .iter()
            .filter(|stop| stop.position.to_mm() > x - left + 0.01)
            .min_by(|a, b| a.position.cmp(&b.position));
        let (target, align, leader) = match stop {
            Some(stop) => (
                (left + stop.position.to_mm().max(0.0)).min(right),
                stop.align,
                stop.leader,
            ),
            None => {
                // No explicit tab stop: advance by the default step to the next
                // grid position.
                let offset = x - left;
                let next = ((offset / default_step).floor() + 1.0) * default_step;
                ((left + next).min(right), TabAlign::Left, TabLeader::None)
            }
        };

        // Tab stop declared outside the body width: clip to the right edge and
        // record a diagnostic.
        if let Some(stop) = stop
            && stop.position.to_mm() > self.content_width() + 0.01
        {
            self.diagnostic(
                DiagnosticCode::Geometry,
                "tab stop exceeds the body width; clipped to the right edge",
            );
        }
        let mut start = match align {
            TabAlign::Left => target,
            TabAlign::Right => target - remaining,
            TabAlign::Center => target - remaining / 2.0,
            TabAlign::Decimal => match decimal_width {
                Some(width) => target - width,
                None => target - remaining,
            },
            _ => target,
        };
        // The content after the tab stop does not fit in the remaining space: stop
        // right-aligning and place it directly after the preceding text to avoid
        // overlap.
        if start < x {
            self.diagnostic(
                DiagnosticCode::Geometry,
                "content after the tab stop does not fit; placed directly after the preceding text",
            );
            start = x;
        }
        // Still out of bounds (tab stop declared outside the page margins): clip to
        // the body right edge.
        let start = start.min((right - remaining).max(x));

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

    /// Draw the tab leader.
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
        // Switch to the fallback font when the primary font lacks this glyph:
        // setting CJK text in an English style is very common in mixed
        // Chinese/English layout.
        let (use_fallback, glyph) = match self.fonts.get(&key) {
            Some(data) if !data.is_missing(piece.ch) => (false, data.glyph(piece.ch)),
            _ => match self.fonts.fallback() {
                Some(fallback) if !fallback.is_missing(piece.ch) => {
                    (true, fallback.glyph(piece.ch))
                }
                // Neither font has this glyph: still write .notdef so the text
                // layer stays complete.
                _ => (false, 0),
            },
        };
        let asset = if use_fallback {
            self.fallback_asset()
        } else {
            self.font_asset(&key)
        };
        let Some(font) = asset else {
            return;
        };
        let size = style.size.to_mm();
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

    /// Place an inline image: its bottom edge sits on the baseline.
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

    /// Register a Flow image into the Fixed resource table and return the handle
    /// carrying its ID.
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
            self.diagnostic(
                DiagnosticCode::Image,
                "block image has no resource; skipped",
            );
            return;
        };
        let content = self.content_width();
        if width > content {
            let scale = content / width;
            width = content;
            height *= scale;
            self.diagnostic(
                DiagnosticCode::Approximation,
                "image is wider than the text area; scaled proportionally to fit",
            );
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
                "image is taller than the page area; scaled proportionally to fit the current page",
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

    // ---- Outline ----

    fn build_outline(&self) -> Vec<OutlineItem> {
        let mut roots: Vec<OutlineItem> = Vec::new();
        // Parent chain: each entry is (level, path within `roots`).
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

/// Fetch a mutable entry along a path.
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

/// Overlay a paragraph's direct formatting (the parts that differ from the
/// defaults) onto an already resolved style.
///
/// On an inheritance chain it is impossible to tell "explicitly written as the
/// default" from "not written", so default values are always treated as unset.
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

/// Overlay character direct formatting onto an already resolved style, following
/// the same rules as [`apply_paragraph_direct`].
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

/// Collect the characters that only appear at layout time in the TOC title.
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

/// Collect the TOC styles (title / entry / page number) found in blocks, for font
/// pre-resolution.
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

/// Recursively collect the characters in blocks (including table cells).
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

/// Height of the tallest inline image on a line.
fn line_image_height(pieces: &[Piece], line: &LineSpan, images: &[MeasuredImage]) -> f64 {
    pieces[line.start..line.end]
        .iter()
        .filter_map(|piece| piece.object.and_then(|index| images.get(index)))
        .map(|image| image.height)
        .fold(0.0, f64::max)
}

/// TOC entry: level, title, page number.
type TocEntry = (u8, String, usize);

/// Horizontal frame of the current layout: table cells, headers and footers all
/// switch to a different frame.
#[derive(Clone, Copy, Debug, Default)]
struct Frame {
    /// Left boundary, millimeters.
    left: f64,
    /// Available width, millimeters.
    width: f64,
    /// Top boundary, millimeters.
    top: f64,
    /// Bottom boundary, millimeters.
    bottom: f64,
}

/// Line box of one line.
struct LineBox {
    /// Line box height (millimeters).
    height: f64,
    /// Baseline offset from the line box top (millimeters).
    ascent: f64,
}

/// A measured paragraph.
struct LaidParagraph {
    /// Resolved paragraph style.
    style: ParaStyle,
    /// Line breaking pieces.
    pieces: Vec<Piece>,
    /// Link ranges.
    links: Vec<(usize, usize, LinkTarget)>,
    /// Inline images.
    images: Vec<MeasuredImage>,
    /// Line ranges.
    lines: Vec<LineSpan>,
    /// Extra horizontal inset caused by floating objects.
    inset_left: f64,
}

/// Continuation position of one cell within a table row.
#[derive(Clone, Copy, Debug)]
struct CellContinuation {
    /// Index of the next block not yet laid out.
    block: usize,
    /// When that block is a paragraph, the number of lines already laid out.
    line: usize,
}

/// Layout result of one row (or a fragment of one).
#[derive(Clone, Debug)]
struct RowFragment {
    /// Bottom y occupied on this page.
    bottom: f64,
    /// Continuation position of each cell not yet complete; None means the cell is
    /// done.
    continuation: Vec<Option<CellContinuation>>,
    /// Whether any content was placed on this page.
    placed_any: bool,
}

/// Layout result of a cell.
#[derive(Clone, Copy, Debug)]
struct CellOutcome {
    /// y of the content bottom.
    bottom: f64,
    /// Continuation position when not complete.
    continuation: Option<CellContinuation>,
    /// Whether any content was placed.
    placed_any: bool,
}

/// Snapshot of one page's content, used to roll back when "it turns out not to
/// fit halfway through placement".
#[derive(Clone, Copy, Debug)]
struct Snapshot {
    /// Number of scene children.
    children: usize,
    /// Number of text items.
    items: usize,
    /// Number of lines.
    lines: usize,
    /// Number of links.
    links: usize,
    /// Vertical cursor.
    cursor: f64,
}

/// Plain text of a paragraph.
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

/// Page bounding box of a text item.
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
