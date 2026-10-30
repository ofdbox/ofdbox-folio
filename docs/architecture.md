English | [简体中文](architecture.zh-CN.md)

# Architecture

This document describes how OFDBox Folio is put together; for goals, current status and usage, see the [README](../README.md).

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

The dependency direction is fixed: the format adapters, layout and rendering all depend on `ofdbox-folio-ir`, while `ofdbox-folio-ir` depends on no format library or rendering backend.

| crate | Public responsibility | Key constraints |
|---|---|---|
| `ofdbox-folio-ir` | Flow IR, Fixed IR, geometry/paths/pens, resources, `TextLayer`, `DocumentSource`, `FixedDocumentSource`, diagnostics | No format dependencies, no I/O |
| `ofdbox-folio-layout` | `layout()`, `LayoutSession`, `FontProvider`, layout options and errors | Only Flow → Fixed; reads no clock/random numbers/system fonts |
| `ofdbox-folio-render` | `CpuRenderer`, render targets, tiles, glyph and image caches | Consumes Fixed IR only, unaware of the source format |
| `ofdbox-folio-ofd` | `OfdDocument` (`DocumentSource`), `write_fixed()` | Does not expose `ofdbox_core` types publicly |
| `ofdbox-folio-pdf` | `PdfDocument` (`DocumentSource`), `write_fixed()` | Does not expose `lopdf` types publicly |
| `ofdbox-folio-docx` | `read_flow()`/`read_flow_path()` → Flow, `write_flow()` → DOCX, `probe()` | Does not touch Fixed IR |
| `ofdbox-folio` | `Document::open_bytes()`/`open_path()`, `from_fixed()`/`from_flow()`, creation API, convenience conversions | Unified errors, caching and format dispatch (DOCX → Flow → layout) |

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

- Flow IR sits on top and Fixed IR at the bottom; layout is the only downward path between them.
- DOCX writing consumes Flow IR directly, bypassing both layout and Fixed IR.
- OFD/PDF reading produces Fixed IR directly, bypassing Flow IR and layout.
- Fixed IR is the common entry point for bitmap rendering, OFD writing and PDF writing.
- Opening a document parses only metadata, the page table, outlines and resource indexes; `DocumentSource::load_page()` parses one page on demand, and the Reader keeps a byte-budgeted LRU cache. Damage to a single page affects only that page: the reader shows a placeholder and keeps the diagnostics. See [reader.md](reader.md) for page caching, selection and search.
- The same Reader dispatches DOCX: it reads the package into Flow IR, lays it out eagerly with the supplied `FontProvider`, and wraps the resulting `FixedDocument` in `FixedDocumentSource`, so DOCX pages use the same cache, selection and search APIs. `Document::from_fixed`/`from_flow` wrap layout output the same way.

### The Two IRs

**Flow IR** is a semantic, editable flow model: paragraphs, inline runs, headings, tables, images, fields, tables of contents, sections, headers and footers. Lengths are integer twips and no absolute coordinates are involved.

**Fixed IR** holds immutable, cacheable fixed-layout pages. The origin is top-left, x points right, y points down, and lengths are in millimeters. Each page carries:

- `Scene`: the semantic page content tree, consumed by rendering and writing;
- `TextLayer`: text in logical reading order plus character/glyph-cluster mapping, for selection, copying and search;
- `PageInfo`: media/crop box, rotation and page labels;
- link regions and template layer references.

`FixedDocument` is layout's complete output; `Page` is the reader's unit of loading. Both share the same content tree but build it differently: layout produces it in one pass, while the reader parses it on demand.

### Layout (A1)

`ofdbox_folio::layout()` lays Flow IR out into Fixed IR, with the rules frozen in [layout-contract.md](layout-contract.md):

- **Line breaking**: breaks are allowed at CJK boundaries and spaces, line-start/line-end prohibition rules pull punctuation back, and trailing spaces do not consume line width.
- **Paragraphs**: first-line, left and right indents; single/multiple/fixed/minimum line spacing; space before/after; left, center, right and justified alignment, where justification stretches spaces first and shares the extra space between characters when there are none.
- **Line height and font fallback**: single line height uses the font's `hhea` ascent/descent/lineGap with the baseline placed by ascent; a missing glyph falls back one character at a time to a fallback font.
- **Pagination and columns**: pages break by line, space before a paragraph is dropped at the top of a page, and `PageSetup::columns` supports equal-width multi-column flow that breaks only when the last column is full; when total pages are referenced, layout runs twice to count and then substitute.
- **Images**: block-level images use their declared size or a DPI conversion, oversized ones are scaled proportionally with a diagnostic, and inline images are unbreakable units sitting on the baseline.
- **Fonts**: `FontProvider` resolves bytes by family plus bold/italic, metrics come from `hmtx`, and missing glyphs are recorded as diagnostics and laid out at the `.notdef` width.
- **Outlines**: `heading_level` is registered during layout and an `OutlineItem` tree is generated by level, with the target page and y taken from the heading's first line.
- **Tables**: fixed/weighted column widths, cell sub-frame layout (column width minus padding), row-wise page breaks with the header row repeated, per-cell borders, and continued rows when one is taller than the remaining space.
- **Headers and footers**: headers lay out downward from above the top margin and footers are measured then pinned to the bottom; first-page/even-page variants are supported, and `PAGE`/`NUMPAGES` share the body's page numbering.
- **Floating objects**: `Block::Floating` is positioned absolutely in page coordinates and supports no-wrap, top-and-bottom and square wrapping, where the whole paragraph steps aside.
- **Footnotes and endnotes**: `Inline::Note` is numbered in order of appearance; footnotes reserve space at the bottom of the referencing page and draw a separator line, while endnotes collect at the end of the document on a page of their own.
- **Tab stops**: a stop beyond the text width is clipped to the right edge and content that does not fit the remaining space is laid out immediately after, both with diagnostics; `ParaStyle::tabs` supports left/center/right/decimal alignment with dot, dash or solid leaders, and a tab jumps to the next stop at the current x or advances on a 12.5 mm grid.
- **Automatic table of contents**: `Block::Toc` expands into "entry + tab + page number" paragraphs, with the number on a right tab stop at the right edge of the text area and a dot leader; page numbers are collected in a dry run and substituted iteratively until stable.
- **Styles**: named styles inherit along the `based_on` chain, and direct formatting overrides non-default fields.

### System Fonts

`ofdbox_folio::fonts::SystemFonts` is the only "font family name → local font file" resolver (candidate table + cache + stable results), shared by the examples and the CLI. `used_fonts()` returns the files that were actually hit, and the CLI's `--report` writes them as `FONT <family> -> <file>`.

### DOCX Reading and Writing

`ofdbox_folio::docx::write_flow()` writes Flow IR into a DOCX package (`[Content_Types].xml`, `document.xml`, `styles.xml`, headers and footers, `footnotes.xml`/`endnotes.xml`, `media/*`, `docProps`), covering sections and page setup (including columns), paragraph and character formatting, tab stops and leaders, tables (borders/header rows/cell padding), inline and floating images (`wp:inline`/`wp:anchor`), headers and footers, `PAGE`/`NUMPAGES` fields, hyperlinks, footnote and endnote references, automatic TOC fields, bookmarks and outline levels. Attachments are stored as a manifest plus the original file parts (Word ignores them, but this library round-trips them), and ZIP timestamps are fixed, so the same input produces the same bytes; `write_flow_with_report` additionally returns degradation diagnostics.

`examples/create_document.rs` builds a document from scratch and writes both DOCX and OFD from the same Flow IR (headings, character formatting, dot-leader tab stops, tables, inline and floating images, footnotes, TOC fields, headers and footers with page-number fields; the OFD path goes through layout and font subsetting):

```bash
cargo run -p ofdbox-folio --example create_document -- out.docx  # also produces out.ofd
```

### DOCX Reading

`ofdbox_folio::docx::read_flow()` reads DOCX into Flow IR:

- **Styles**: fields are resolved through the OOXML cascade (`docDefaults` → default styles → paragraph/character style chain → direct formatting) and flattened so explicitly written defaults take effect; both `based_on` and theme fonts (`majorFont`/`minorFont`, `Hans` script) are resolved.
- **Blocks**: paragraphs, tables (column widths, header rows, borders, cell padding), inline images (display size from `wp:extent`), page breaks before and inside paragraphs, outline level → heading, and TOC content controls → `Block::Toc`.
- **Lists**: numbering/bullets from `numbering.xml` become the marker of the paragraph's first run with a hanging indent, and Symbol-font private-use code points map to ordinary symbols.
- **Headers and footers**: `headerReference`/`footerReference` become a `HeaderFooterSet`, and `PAGE`/`NUMPAGES` in `fldSimple` and complex fields become `Field`.
- **Metadata**: `docProps/core.xml` and `app.xml`.
- **Degradation**: floating images are treated as inline, merged cells as ordinary cells, and VML, footnotes, symbol fonts and the like are recorded as `FlowDocument.report` diagnostics.
- **Reader integration**: `Document::open_bytes` detects DOCX, calls `read_flow()`, lays the result out and wraps it in `FixedDocumentSource`; DOCX pages then use the same cache, selection, search and hit-testing APIs as native formats.

### Text and Fonts

- Shaping finishes during layout; Fixed IR stores glyph IDs, positions, advances and character/glyph-cluster mappings.
- OFD reading preserves `CGTransform`, PDF reading uses the `ToUnicode` CMap, and DOCX uses logical order.
- Text that cannot be resolved to a font (Type3, missing fonts) degrades to paths, and the degraded range is recorded in the text layer.
- Callers supply fonts explicitly; system fonts are not scanned, and a missing font produces an error or a fallback by policy, recorded in the report.

### Determinism

- Flow IR uses twips and Fixed IR millimeters; the boundary is converted once, and OFD output is quantized to 0.001 mm.
- Layout reads no clock, random numbers or environment variables, and does not rely on HashMap iteration order.
- `FixedDocument` records the layout engine version and font content fingerprints.
- Same Flow + same font bytes + same options + same version → identical layout results.
- Reader load timing may vary, but page content does not change because of it.

## Key Contracts

1. **One-way**: only Flow → Fixed is allowed; Fixed IR keeps no back-references to Flow.
2. **Format-neutral**: OFD/PDF/DOCX/ZIP/XML types do not appear in the IR, layout or rendering interfaces.
3. **Immutable pages**: a `Page` can be cached and shared across threads, and the document does not build all pages at once.
4. **Fixed units**: Flow uses twips, Fixed uses millimeters, and the coordinate system has its origin at the top-left with y pointing down.
5. **Silence**: no logging, no printing; unrepresentable items go into the report, and failures return errors.
6. **No panics**: every format parser returns errors for untrusted input, bounded by limits on page count, decompression, image pixels and recursion depth.
7. **No dependency leaks**: `ofdbox_core`, `lopdf`, `quick_xml`, `zip`, `rustybuzz`, `ttf_parser`, `vello` and `wgpu` types must not appear in public signatures.
