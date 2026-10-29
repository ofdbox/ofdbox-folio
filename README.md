English | [简体中文](README.zh-CN.md)

# ofdbox-folio

OFDBox Folio is a page-centric multi-format document engine: it reads OFD, PDF and DOCX, unifies them into Flow IR / Fixed IR, and takes care of layout, rendering and writing.

**Current status: the IR contract is frozen; OFD → image, Fixed IR → OFD/PDF writing, PDF → Fixed IR, Reader page caching, cross-page selection and search, and DOCX → Flow reading with single-column layout are all working.** 211 of the 217 OFD files in the corpus are readable (6 source files are corrupted). After round-tripping the readable corpus through OFD → Fixed IR → OFD and rendering the first page, 189 files are pixel-identical to the source, and 203 of the 211 have fewer than 0.2% differing pixels, with an average MAE of 0.011. PDF reading and writing are integrated: 200 of the 211 PDFs exported by the old pipeline can be matched to an OFD source, with an average of 1.5% differing pixels on the first page, mostly from text being outlined and pixel rounding during export; representative samples of a PDF-only round trip (read → write → re-render) differ by less than 0.6%. The wgpu backend (solid paths, strokes, linear gradients, clipping, text outlines) is integrated. DOCX reading and single-column layout are integrated; table layout, header/footer pagination and DOCX writing are not yet integrated.

## Goals

- Read OFD and PDF, obtaining Fixed IR pages directly.
- Read DOCX to obtain Flow IR, then run layout to obtain Fixed IR.
- Build Flow IR with a single creation API, and either write DOCX directly or write OFD, PDF and bitmaps through layout.
- Use Fixed IR as the unified read model, supporting lazy loading, text selection/search, page caching and incremental rendering.
- The same input, the same font bytes and the same engine version produce a deterministic FixedDocument.

## Non-Goals

- No reverse conversion from Fixed IR to Flow IR.
- No promise that DOCX layout matches Word line by line; the layout goal is correct structure, reading order and readability.
- No attempt at full PDF feature coverage in the first version (Type3, transparency groups, encryption, complex CMaps etc. are handled in stages).
- No editing API for FixedDocument; the reader does not take on document editing duties.
- No leaking of OFD/PDF/DOCX format types into the IR, layout or rendering interfaces.

## Crate Architecture

```text
ofdbox-folio                façade: format probing, Reader, creation API, convenience conversion
├─ ofdbox-folio-ofd        OFD read/write        → ofdbox-core
├─ ofdbox-folio-pdf        PDF read/write        → lopdf
├─ ofdbox-folio-docx       DOCX read/write       → zip + quick-xml
├─ ofdbox-folio-layout     Flow → Fixed, one-way layout
├─ ofdbox-folio-render     Fixed → bitmap, CPU/GPU rendering and caching
└─ ofdbox-folio-ir         Flow IR, Fixed IR, geometry, resources, text layer, Reader contracts
```

The dependency direction is fixed: the format adapters, layout and rendering all depend on `ofdbox-folio-ir`; `ofdbox-folio-ir` depends on no format library or rendering backend.

| crate | Public responsibility | Key constraints |
|---|---|---|
| `ofdbox-folio-ir` | Flow IR, Fixed IR, geometry/paths/pens, resources, `TextLayer`, `DocumentSource`, diagnostics | No format dependencies, no I/O |
| `ofdbox-folio-layout` | `layout()`, `LayoutSession`, `FontProvider`, layout options and errors | Only Flow → Fixed; reads no clock/random numbers/system fonts |
| `ofdbox-folio-render` | `CpuRenderer`, render targets, tiles, glyph and image caches | Consumes Fixed IR only, unaware of the source format |
| `ofdbox-folio-ofd` | `OfdDocument` (`DocumentSource`), `write_fixed()` | Does not expose `ofdbox_core` types publicly |
| `ofdbox-folio-pdf` | `PdfDocument` (`DocumentSource`), `write_fixed()` | Does not expose `lopdf` types publicly |
| `ofdbox-folio-docx` | `read_flow()`/`read_flow_path()` → Flow, `probe()`; `write_docx()` to come | Does not touch Fixed IR |
| `ofdbox-folio` | `open_bytes()`/`open_path()`, `Document`, creation API, convenience conversions | Unified errors, caching and format dispatch |

## How It Works

### Main Flow

```text
Creation API ───┐
                ├──► Flow IR ──────► DOCX write
DOCX read ──────┘       │
                        │ layout (one-way)
                        ▼
                    Fixed IR ─────┬──► bitmap rendering
                        ▲         ├──► OFD write
                        │         └──► PDF write
                        │
OFD / PDF read ─────────┘
```

- Flow IR is on top and Fixed IR at the bottom; layout is the only downward path between them.
- DOCX writing consumes Flow IR directly, bypassing layout and Fixed IR.
- OFD/PDF reading produces Fixed IR directly, bypassing Flow IR and layout.
- Fixed IR is the common entry point for bitmap rendering, OFD writing and PDF writing.
- When reading, opening a document parses only metadata, the page table, outlines and resource indexes; `DocumentSource::load_page()` parses a single page on demand, and the Reader uses a byte-budgeted LRU cache. Damage to a single page affects only that page: the reader shows a placeholder and keeps the diagnostics.

### The Two IRs

**Flow IR**: a semantic, editable flow document model. Paragraphs, inline runs, headings, tables, images, fields, tables of contents, sections, headers and footers. Lengths are integer twips and no absolute coordinates are involved.

**Fixed IR**: immutable, cacheable fixed-layout pages. The origin is top-left, x points right, y points down, and lengths are in millimeters. Each page contains:

- `Scene`: the semantic page content tree, consumed by rendering and writing;
- `TextLayer`: text in logical reading order plus character/glyph-cluster mapping, for selection, copying and search;
- `PageInfo`: media/crop box, rotation, page labels;
- link regions and template layer references.

`FixedDocument` is the complete output of layout; `Page` is the reader's unit of loading. Both share the same content tree but build it differently: layout produces it all at once, while the reader parses it on demand.

### Layout (A1)

`ofdbox_folio::layout()` lays Flow IR out into Fixed IR, with the rules frozen in `docs/layout-contract.md`:

- **Line breaking**: breaks are allowed at CJK boundaries and spaces, and line-start/line-end prohibition rules pull punctuation back; trailing spaces do not consume line width.
- **Paragraphs**: first-line/left/right indents, single/multiple/fixed/minimum line spacing, space before/after, left/center/right and justified alignment (justification stretches spaces first, and distributes the extra space between characters when there are none).
- **Line height and font fallback**: single line height uses the font's `hhea` ascent/descent/lineGap, and the baseline is positioned by ascent; when the primary font lacks a glyph, the character falls back to a fallback font one by one (mixed Chinese/English runs no longer show blanks).
- **Pagination and columns**: pages break by line, and space before a paragraph is dropped at the top of a page; PageSetup::columns supports equal-width multi-column layout (fill the left column before flowing into the right one, and break the page only when the last column is full); when the "total pages" field is referenced, layout runs twice — the first pass takes the page count, the second substitutes it.
- **Images**: block-level images use their declared size or a DPI conversion; oversized ones are scaled proportionally and a diagnostic is recorded; inline images are unbreakable units, sitting on the baseline at the bottom.
- **Fonts**: `FontProvider` resolves bytes by "family + bold/italic", metrics come from `hmtx`; missing glyphs are recorded as diagnostics and laid out with the `.notdef` width.
- **Outlines**: `heading_level` is registered during layout, and an `OutlineItem` tree is generated by level; the target page and y coordinate come from the first line of the heading.
- **Tables**: fixed/weighted column widths, cell sub-frame layout (column width minus padding), page breaks by row with the header row repeated, and per-cell borders; when a row is taller than the space left on the page, it is split by row and continued.
- **Headers and footers**: headers lay out downward from above the top margin, and footers are measured first and then pinned to the bottom; different first page / even pages are supported, and `PAGE`/`NUMPAGES` share the same page numbering as the body.
- **Floating objects**: Block::Floating is positioned absolutely in page coordinates, and supports three wrapping modes: no wrap, top-and-bottom, and square (the whole paragraph steps aside).
- **Footnotes and endnotes**: Inline::Note is numbered in order of appearance; footnotes reserve space at the bottom of the referencing page and draw a separator line, while endnotes are collected at the end of the document on a page of their own.
- **Tab stops**: a declared stop beyond the text width is clipped to the right edge, and content that does not fit in the remaining space is laid out immediately after (both record diagnostics); `ParaStyle::tabs` supports left/center/right/decimal alignment with dot, dash and solid leaders; when laying out, a tab jumps to the next tab stop at the current x, and without an explicit stop it advances on a 12.5 mm grid.
- **Automatic table of contents**: `Block::Toc` expands into paragraphs of "entry + tab + page number", with the page number landing on a right tab stop at the right edge of the text area and carrying a dot leader; a dry run first collects the page numbers, which are then substituted iteratively until stable.
- **Styles**: named styles inherit along the `based_on` chain, and direct formatting overrides non-default fields.

### System Fonts

`ofdbox_folio::fonts::SystemFonts` is the only "font family name → local font file" resolution implementation (candidate table + cache + stable results), shared by the examples and the CLI; `used_fonts()` returns the files that were actually hit, and the CLI's `--report` writes them as `FONT <family> -> <file>`.

### DOCX Reading and Writing

`ofdbox_folio::docx::write_flow()` writes Flow IR into a DOCX package (`[Content_Types].xml`, `document.xml`, `styles.xml`, headers and footers, `footnotes.xml`/`endnotes.xml`, `media/*`, `docProps`): sections and page setup (including columns), paragraph and character formatting, tab stops and leaders, tables (borders/header rows/cell padding), inline and floating images (`wp:inline`/`wp:anchor`), headers and footers, `PAGE`/`NUMPAGES` fields, hyperlinks, footnote and endnote references, automatic TOC fields, bookmarks and outline levels. Attachments are written into the package as "manifest + original file parts" (Word does not display them, but this library can round-trip them). ZIP timestamps are fixed, so the same input produces the same bytes; write_flow_with_report additionally returns degradation diagnostics.

For a complete example of creating a document from scratch, see `examples/create_document.rs`: the same Flow IR writes both DOCX and OFD (heading, character formatting, dot-leader tab stops, tables, inline and floating images, footnotes, TOC fields, headers and footers with page-number fields; OFD goes through the layout engine with font subsetting):

```bash
cargo run -p ofdbox-folio --example create_document -- out.docx  # also produces out.ofd
```

### DOCX Reading

`ofdbox_folio::docx::read_flow()` reads DOCX into Flow IR:

- **Styles**: fields are resolved according to the OOXML cascade (`docDefaults` → default styles → paragraph/character style chain → direct formatting) and then flattened, so defaults written back explicitly also take effect; both `based_on` and theme fonts (`majorFont`/`minorFont`, `Hans` script) are resolved.
- **Blocks**: paragraphs, tables (column widths, header rows, borders and cell padding), inline images (display size taken from `wp:extent`), page breaks (before a paragraph and inside it), outline level → heading, table-of-contents content control → `Block::Toc`.
- **Lists**: numbering/bullets from `numbering.xml` become the marker of the paragraph's first run, paired with a hanging indent; private-use code points of the Symbol font are mapped to ordinary symbols.
- **Headers and footers**: `headerReference`/`footerReference` are read into `HeaderFooterSet`, and `PAGE`/`NUMPAGES` in `fldSimple` and in complex fields become `Field`.
- **Metadata**: `docProps/core.xml` and `app.xml`.
- **Degradation**: floating images are treated as inline, merged cells as ordinary cells, and VML/footnotes/symbol fonts etc. are recorded as `FlowDocument.report` diagnostics.

### Reader Performance Model

- **Lazy pages**: first-screen time is independent of the total page count; memory is determined by the cache budget and the visible pages.
- **Separated resources**: fonts and images are shared by content; images are decoded at the target scale tier, so thumbnails do not trigger full-resolution decoding.
- **Render-list cache**: a flat render-command list is generated the first time a page is drawn and reused for scrolling and zooming.
- **Independent text layer**: drawing order is not reading order; the text layer stores line boxes, baselines and character mappings, so selection/search does not re-parse the page.
- **Progressive layout**: after DOCX is parsed into Flow IR, pages are produced in order, showing the first page first, with total-page fields arriving later.
- **Error isolation**: a page-level error does not abort document loading.

### Text and Fonts

- Shaping is completed during layout; Fixed IR stores glyph IDs, positions, advances and character/glyph-cluster mappings.
- Reading OFD preserves `CGTransform`; reading PDF uses the `ToUnicode` CMap; DOCX uses logical order.
- Text that cannot be resolved to a font (Type3, missing fonts) degrades to paths, and the degraded range is recorded in the text layer.
- Fonts are supplied explicitly by the caller; system fonts are not scanned; a missing font produces an error or a fallback per policy and is written into the report.

### Determinism

- Flow IR uses twips and Fixed IR uses millimeters; boundaries are converted once, and OFD output is quantized to 0.001 mm.
- Layout reads no clock, random numbers or environment variables, and does not rely on HashMap iteration order.
- FixedDocument records the layout engine version and font content fingerprints.
- Same Flow + same font bytes + same options + same version → identical layout results.
- The reader's load timing may vary, but page content does not change because of it.

## Key Contracts

1. **One-way**: only Flow → Fixed is allowed; Fixed IR keeps no back-references to Flow.
2. **Format-neutral**: OFD/PDF/DOCX/ZIP/XML types do not appear in the IR, layout or rendering interfaces.
3. **Immutable pages**: a `Page` can be cached and shared across threads; the document does not build all pages at once.
4. **Fixed units**: Flow uses twips, Fixed uses millimeters; the coordinate system has its origin at the top-left with y pointing down.
5. **Silence**: no logging, no printing; unrepresentable items go into the report, and failures return errors.
6. **No panics**: every format parser returns errors for untrusted input, bounded by limits on page count, decompression, image pixels and recursion depth.
7. **No dependency leaks**: `ofdbox_core`, `lopdf`, `quick_xml`, `zip`, `rustybuzz`, `ttf_parser`, `vello` and `wgpu` types must not appear in public signatures.

## API Sketch

The following is a structural sketch; the crates' public APIs are authoritative.

```rust
// Read: format probing + on-demand page access
let mut doc = ofdbox_folio::open_path("input.ofd", &OpenOptions::default())?;
let info = doc.info();
let page = doc.page(0)?;

// Create: build Flow IR
let mut builder = DocumentBuilder::new(Metadata::default());
builder.heading(1, "第一章");
builder.paragraph(
    ParagraphBuilder::new(ParaStyle::default())
        .span("正文", TextStyle::default())
        .build(),
);
let flow = builder.build()?;

// Write DOCX directly
let docx = ofdbox_folio::to_docx(&flow, &DocxWriteOptions::default())?;

// Write OFD after layout
let mut fonts = MyFontProvider::new();
let fixed = ofdbox_folio::layout(&flow, &mut fonts, &LayoutOptions::default())?;
let ofd = ofdbox_folio::to_ofd(&fixed, &OfdWriteOptions::default())?;
```

## OFD Writing

```rust
use ofdbox_folio::ir::FixedDocument;
use ofdbox_folio::ofd::{OfdWriteOptions, write_fixed};

let bytes = write_fixed(&document, &OfdWriteOptions::default())?;
```

The writer consumes Fixed IR and outputs an OFD 1.0 package (`Version="1.0"`): paths, text (with `CGTransform` and `DeltaX/DeltaY`), images, group clipping, transparency, axial/radial gradients, font names and embedded font bytes. The object `Boundary` covers the whole page, with the actual position carried by the `CTM`; line position deltas come directly from glyph-local coordinates, avoiding the deviation introduced by font metrics. Embedded fonts are subsetted as TrueType based on the characters actually used in the document (glyph IDs are preserved, only unused outlines are cleared, and composite glyph dependencies are kept along with them); when a font cannot be subsetted, it is embedded in full. By default a fixed ZIP timestamp and a content-digest `DocID` are used, so the same input produces the same bytes.

Known limitations: pattern and mesh fills are not yet written (they fall back to the first color stop), and group transparency is distributed to the alpha of child element colors, so independent layer semantics are not preserved.

## PDF Reading and Writing

```rust
use ofdbox_folio::pdf::{PdfDocument, PdfOptions, PdfReadOptions, write_fixed};

let mut source = PdfDocument::open_path("input.pdf", PdfReadOptions::default())?;
let page = source.load_page(0)?;
let bytes = write_fixed(&fixed, &PdfOptions::default())?;
```

Reading covers paths, text (Identity-H and simple fonts, `ToUnicode`, `Tr` render mode, character spacing and horizontal scaling), images (including CCITT G3/G4), `sh` shading and PatternType 2 gradient patterns, inline images, Form XObjects, clipping, transparency, page rotation, outlines and attachments, and encrypted PDFs (user or owner password); `PdfDocument` implements `DocumentSource` and provides page sizes, text layers, font names and resource tables. Writing covers paths, clipping, images, gradients, transparency groups, text (embedding subsetted TrueType fonts, degrading to glyph outlines on failure), metadata and bookmarks; JPEG is embedded as-is with `DCTDecode`, while PNG/JBIG2/BMP are decoded and embedded with `FlateDecode`.

Known limitations: PatternType 1 tiling patterns and JPEG2000 are not yet read; TIFF/CCITT image resources are still unsupported when writing (CCITT read from PDF is decoded to PNG first). An `/Encrypt` inlined directly in the trailer is first rewritten as an indirect object and forces an xref rebuild, so opening large encrypted files is noticeably slower the first time.

## GPU Backend

The `wgpu` feature of `ofdbox-folio-render` (facade `gpu`) provides the compute backend; `gpu` is off by default:

```rust
use ofdbox_folio::render::{WgpuBackend, WgpuContext, WgpuTarget};

let backend = WgpuBackend::new(WgpuContext::new_blocking()?)?;
let target = WgpuTarget::new(backend.context(), page.display_size(), 144.0)?;
backend.render_to_target(&page.content, &target)?;
let rgba = target.read_rgba(backend.context())?;
```

It currently covers solid fills, strokes, linear gradients, clipping and text outlines; images, radial gradients and blend modes are not yet implemented, so use the CPU renderer when you need the full feature set.

## Roadmap

| Phase | Content | Acceptance |
|---|---|---|
| R1 Reader kernel | IR, OFD reading, CPU rendering, text layer, page cache | OFD→image frames consistent, selection/search usable, page-level error isolation, wasm build |
| R2 Flow documents | Flow IR, layout (single column + tables + headers/footers + automatic TOC), DOCX reading, DOCX writing and the creation API | Word → OFD/PDF/bitmap pipeline working, deterministic layout |
| R3 PDF reading hardening | Fonts and CMaps, patterns/gradients, transparency, encryption, malformed-file tolerance | Real PDF corpus readable, degradation reported |
| R4 Document capabilities | Annotations/forms, attachments, outline editing, GPU backend | Reader feature set complete |

R1 is now complete for OFD/PDF reading and writing, CPU bitmaps plus the wgpu backend skeleton, Reader page caching and cross-page selection/search; the CLI has moved to `ofdbox-reader` and the old `ofdbox-render` has been removed. Next comes R2: Flow IR, layout and DOCX.

Currently known limitations:

- When an OFD does not embed fonts, rendering depends on the caller injecting a `FontProvider`; without one, the text layer still keeps the text and uses estimated advances, but no text is drawn.
- Template pages are currently flattened into page content; the `Page.templates` semantics have not yet been separated out.
- The page cache accounts only for page structure; fonts and images are document-level shared resources and are accounted separately.
- Text lines are grouped roughly by TextObject; merging lines across objects still needs work.

## Reader Cache

```rust
use ofdbox_folio::{CacheOptions, Document, OpenOptions};

let mut document = Document::open_path(
    "input.ofd",
    OpenOptions {
        cache: CacheOptions { max_bytes: 64 * 1024 * 1024 },
        ..OpenOptions::default()
    },
)?;

let page = document.page(0)?;      // parsed on first access and written to the cache
let page_again = document.page(0)?; // cache hit, returns the same Arc<Page>
document.prefetch(1)?;              // prefetch the next page
let stats = document.cache_stats();
```

The cache is charged by page-structure bytes and evicts in LRU order; `max_bytes = 0` disables it.

## Cross-Page Selection and Search

```rust
use ofdbox_folio::{Document, OpenOptions, SearchQuery, Selection, TextPosition};

let mut document = Document::open_path("input.ofd", OpenOptions::default())?;

let hits = document.search_all(&SearchQuery {
    text: "发票".to_owned(),
    case_sensitive: false,
    whole_word: false,
})?;
for hit in &hits {
    let page = hit.range.start.page;
    // hit.rects are rectangles in page space
}

let selection = Selection {
    anchor: TextPosition::new(0, 0, 2),
    focus: TextPosition::new(1, 0, 5),
};
let text = document.selection_text(&selection)?;
let rects = document.selection_rects(&selection)?;
```

Search loads pages on demand and puts them into the cache; when a selection spans pages, the text is joined with newlines and pages are not re-parsed in between.


## Quick Verification

```bash
cargo run -p ofdbox-folio --example ofd_to_png -- input.ofd output.png 0 96
cargo run -p ofdbox-folio --example ofd_search -- input.ofd query
```

## Testing and Acceptance

- Generated OFD must be strictly readable by `ofdbox-core`, and byte-identical after a strict rewrite.
- Layout results for the same input produce a stable digest, compared across platforms in CI.
- Reader memory stays bounded as the total page count grows; page turns reuse font, image and render caches.
- Text selection, copying and search do not re-parse pages.
- A single damaged page does not affect other pages.
- The existing OFD corpus is the baseline, complemented with PDF/DOCX corpora.

## Relationship to OFDBox

- `ofdbox-folio` is an independent repository under the `ofdbox` organization, released separately from the toolbox repository `ofdbox/ofdbox`.
- OFD structure parsing, cryptography and sealing temporarily depend on `ofdbox-core`, `ofdbox-crypto` and `ofdbox-seal`.
- The new engine takes over and will eventually replace the reading, rendering and writing capabilities of `ofdbox-render`; the old crate will be marked deprecated once migration is complete.
- Crates inside the new repository all use the `ofdbox-folio-` prefix, to avoid name clashes with old crates during the transition.

## License

This project is dual-licensed under MIT or Apache-2.0, at your option:

- [LICENSE-MIT](LICENSE-MIT)
- [LICENSE-APACHE](LICENSE-APACHE)
