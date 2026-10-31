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

mod blocks;
mod floating;
mod font;
mod linebreak;
mod notes;
mod outline;
mod pages;
mod styles;
mod tables;
mod toc;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ofdbox_folio_ir::{
    Align, Block, Border, ColumnWidth, Diagnostic, DiagnosticCode, Element, FieldKind, FillRule,
    FixedDocument, FlowDocument, FontAsset, FontProvider, Group, HeaderFooter, HeaderFooterSet,
    ImageAsset, ImageBlock, ImageElement, Inline, LineCap, LineJoin, LineSpacing, LinkArea,
    LinkTarget, Matrix, NoteKind, NoteRef, OutlineItem, Page, PageInfo, PageSetup, Paint,
    ParaStyle, Paragraph, Path, PathElement, Point, Rect, Rgba, Rotation, Row, Scene, Severity,
    Size, StrokePaint, StrokeStyle, TabAlign, TabLeader, TabStop, Table, TextElement, TextItem,
    TextLayer, TextLine, TextRun, TextStyle, Twips, VerticalAlign,
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

/// Planned placement of one floating object, produced by a probe pass and
/// consumed by the next pass.
#[derive(Clone, Debug)]
struct FloatPlan {
    /// Index of the floating object in document traversal order.
    index: usize,
    /// Page the object is placed on (0-based).
    page: usize,
    /// Page coordinates and size in millimeters.
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    /// Wrapping mode.
    wrap: ofdbox_folio_ir::WrapMode,
    /// Whether the object is painted behind the body text.
    behind_text: bool,
    /// Object content, kept so the next pass can draw it without re-reading.
    object: ofdbox_folio_ir::FloatingObject,
}

/// Result of one layout pass for floating objects.
struct FloatPassResult {
    /// Recorded placements.
    plan: Vec<FloatPlan>,
    /// Whether a replay pass is required (an object was deferred or overlaps
    /// text that was already placed before its anchor).
    replay_needed: bool,
}

/// A floating object deferred to the next page during a probe pass.
#[derive(Clone, Debug)]
struct PendingFloat {
    index: usize,
    object: ofdbox_folio_ir::FloatingObject,
}

/// Geometry returned by placing a floating object.
enum FloatPlacement {
    Placed {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    Deferred,
}

/// Heading outline entry: (level, title, page index, y).
type HeadingEntry = (u8, String, usize, f64);

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
    let needs_floats = uses_floating(document);
    if !needs_toc && !needs_total && !needs_floats {
        let (fixed, _, _) = Engine::new(document, options).run(fonts, None, None, None);
        return Ok(fixed);
    }

    // First pass: the TOC content is empty; this pass only provides heading
    // page numbers, the total page count, and a plan for the floating objects.
    let mut total_pages = None;
    let mut toc_entries: Option<Vec<TocEntry>> = None;
    let mut float_plan: Option<Vec<FloatPlan>> = None;
    let mut last = None;
    let mut stable = false;
    for _ in 0..4 {
        let (fixed, headings, result) = Engine::new(document, options.clone()).run(
            fonts,
            total_pages,
            toc_entries.clone(),
            float_plan.clone(),
        );
        let next_plan = result.plan;
        let next_entries: Option<Vec<TocEntry>> = needs_toc.then(|| {
            headings
                .iter()
                .map(|(level, title, page, _)| (*level, title.clone(), *page))
                .collect()
        });
        let next_total = needs_total.then_some(fixed.pages.len());
        let floats_stable = if !needs_floats {
            true
        } else if float_plan.is_none() {
            // Probe pass: no replay is needed when no object was deferred and
            // no object overlaps text placed before its anchor.
            !result.replay_needed
        } else {
            !result.replay_needed
                && float_plan
                    .as_ref()
                    .is_some_and(|previous| same_float_plan(previous, &next_plan))
        };
        stable = next_entries == toc_entries && next_total == total_pages && floats_stable;
        total_pages = next_total;
        toc_entries = next_entries;
        float_plan = Some(next_plan);
        last = Some(fixed);
        if stable {
            break;
        }
    }
    let mut fixed = last.expect("laid out at least once");
    if needs_floats && !stable {
        fixed.report.diagnostics.push(Diagnostic::new(
            Severity::Warning,
            DiagnosticCode::Geometry,
            "floating object placement did not stabilize after 4 passes",
        ));
    }
    Ok(fixed)
}

/// Whether two float plans describe the same placement (object content is
/// ignored).
fn same_float_plan(a: &[FloatPlan], b: &[FloatPlan]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(left, right)| {
            left.index == right.index
                && left.page == right.page
                && (left.x - right.x).abs() < 0.01
                && (left.y - right.y).abs() < 0.01
                && (left.width - right.width).abs() < 0.01
                && (left.height - right.height).abs() < 0.01
                && left.wrap == right.wrap
                && left.behind_text == right.behind_text
        })
}

/// Whether the document contains block-level floating objects.
fn uses_floating(document: &FlowDocument) -> bool {
    fn blocks_use_floating(blocks: &[Block]) -> bool {
        blocks.iter().any(|block| match block {
            Block::Floating(_) => true,
            Block::Table(table) => table.rows.iter().any(|row| {
                row.cells
                    .iter()
                    .any(|cell| blocks_use_floating(&cell.blocks))
            }),
            _ => false,
        })
    }
    document
        .sections
        .iter()
        .any(|section| blocks_use_floating(&section.blocks))
}

/// Whether a row directly contains a floating block in one of its cells.
fn row_has_floating(row: &Row) -> bool {
    row.cells.iter().any(|cell| {
        cell.blocks
            .iter()
            .any(|block| matches!(block, Block::Floating(_)))
    })
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
    /// Floating objects that did not fit on their anchor page and wait for the
    /// next page.
    pending_floats: Vec<PendingFloat>,
    /// Plans from the previous pass; `Some` means this is a replay pass.
    float_plan_in: Option<Vec<FloatPlan>>,
    /// Plans recorded during this pass.
    float_plan_out: Vec<FloatPlan>,
    /// Number of floating objects encountered so far in this pass.
    float_seen: usize,
    /// Whether a replay pass is needed.
    float_replay_needed: bool,
    /// Geometry of the objects pre-placed on the current page, keyed by index.
    applied_geometry: HashMap<usize, (usize, f64, f64, f64, f64)>,
    /// Foreground floating elements waiting to be appended at page end.
    front_elements: Vec<Element>,
    /// Rectangles of floating objects already placed on this page/column.
    placed_float_rects: Vec<(f64, f64, f64, f64)>,
    /// Whether an overlap diagnostic was already recorded for this container.
    float_overlap_reported: bool,
    /// Square-wrap exclusion areas inside the current frame (table cell/header).
    frame_exclusions: Vec<FloatExclusion>,
    /// First scene element of the current frame, used for behind z-order.
    frame_element_start: usize,
    /// Bottom of floating objects placed inside the current frame.
    frame_float_bottom: f64,
    /// Whether the row-float split diagnostic was already recorded.
    float_row_split_reported: bool,
    /// Whether the nested-table split diagnostic was already recorded.
    nested_table_split_reported: bool,
    /// Index of the first body element in `page.scene.children` (after the header).
    body_element_start: usize,
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
            pending_floats: Vec::new(),
            float_plan_in: None,
            float_plan_out: Vec::new(),
            float_seen: 0,
            float_replay_needed: false,
            applied_geometry: HashMap::new(),
            front_elements: Vec::new(),
            placed_float_rects: Vec::new(),
            float_overlap_reported: false,
            frame_exclusions: Vec::new(),
            frame_element_start: 0,
            frame_float_bottom: 0.0,
            float_row_split_reported: false,
            nested_table_split_reported: false,
            body_element_start: 0,
            headings: Vec::new(),
            missing_chars: 0,
        }
    }

    fn run(
        mut self,
        provider: &mut dyn FontProvider,
        total_pages: Option<usize>,
        toc_entries: Option<Vec<TocEntry>>,
        float_plan: Option<Vec<FloatPlan>>,
    ) -> (FixedDocument, Vec<HeadingEntry>, FloatPassResult) {
        self.prepare_fonts(provider);
        self.total_pages = total_pages;
        self.toc_entries = toc_entries;
        self.float_plan_in = float_plan;
        for section in &self.document.sections {
            self.begin_section(&section.setup, &section.headers, &section.footers);
            self.place_blocks(&section.blocks);
            if !self.pending_floats.is_empty() {
                self.new_page();
                self.start_page_body();
            }
            self.finish_page();
        }
        self.flush_unapplied_float_plans();
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
        (
            self.fixed,
            self.headings,
            FloatPassResult {
                plan: self.float_plan_out,
                replay_needed: self.float_replay_needed,
            },
        )
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
    /// When that block is a paragraph, the number of lines already laid out
    /// (or pieces, when the paragraph is laid out with local wrapping).
    line: usize,
    /// Whether `line` is a piece index from a wrapped paragraph.
    wrapped: bool,
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
