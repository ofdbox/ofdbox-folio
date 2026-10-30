English | [简体中文](README.zh-CN.md)

# ofdbox-folio-ir

`ofdbox-folio-ir` is the format-neutral core of OFDBox Folio. It defines Flow IR and Fixed IR, the geometry, path, paint and resource types that every other crate shares, the text layer, and the `DocumentSource` reader contract. The crate depends on no document-format library and performs no I/O, so the OFD, PDF and DOCX adapters, the layout engine and the renderer can all agree on one data model. The crate map and the full set of contracts are in the [architecture document](../../docs/architecture.md).

## Role in the Workspace

```text
ofdbox-folio-ofd     ──► ofdbox-folio-ir
ofdbox-folio-pdf     ──► ofdbox-folio-ir
ofdbox-folio-docx    ──► ofdbox-folio-ir
ofdbox-folio-layout  ──► ofdbox-folio-ir
ofdbox-folio-render  ──► ofdbox-folio-ir
```

The dependency direction never reverses: `ofdbox-folio-ir` has no format dependency, no rendering backend and no file-system access, and the `ofdbox-folio` façade sits on top of all of them.

## The Two IRs

### Flow IR

`FlowDocument` is the semantic, editable document model: sections, paragraphs and inline runs, headings, tables, images, fields, tables of contents, headers and footers, footnotes and endnotes. It records intent — a heading, a centered paragraph, a page break — rather than positions, and it never carries absolute coordinates.

### Fixed IR

`FixedDocument` is the immutable, cacheable page model produced by layout or by reading a fixed-layout format. A `Page` carries:

- `Scene`: the page content tree (`Text`, `Image`, `Path`, `Group`, `Clip`) consumed by rendering and writing;
- `TextLayer`: text in logical reading order plus character/glyph-cluster mapping, for selection, copying and search;
- `PageInfo`: media and crop box, rotation and page labels;
- link areas and template layers.

The document keeps what pages share (`resources`, `templates`, `outline`, `metadata`, `report`, `engine`), while pages stay independently loadable and cacheable. `FixedDocument` is layout's complete output; `Page` is the reader's unit of loading.

## Units and Coordinates

| Model | Length unit | Representation |
| --- | --- | --- |
| Flow IR | twip (1/1440 inch) | integer `Twips` |
| Fixed IR | millimetre | `f64` |

- Everything in Flow IR is a twip: font size, indents, line spacing, margins and page size. `Twips` is an integer newtype, and `from_pt`/`from_mm` quantise on the way in so that round-trips through points or millimetres do not accumulate drift.
- Fixed IR is millimetres: the origin is the top-left corner of the page, x points right and y points down. Glyph coordinates are relative to the baseline, and `Glyph::advance` is the step to the next glyph.
- The boundary converts once, `mm = twip * 25.4 / 1440`. Layout computes in millimetre floats and rounds to `1e-4 mm` when writing back to the IR, and OFD output is quantised to `0.001 mm`.

```rust
use ofdbox_folio_ir::Twips;

// 1440 twips == 25.4 mm == 1 inch; the IR stores integers only.
assert_eq!(Twips::from_mm(25.4), Twips::new(1440));
assert_eq!(Twips::from_pt(12.0), Twips::new(240));
```

## Reader Contracts

`DocumentSource` is the trait every format adapter implements; the façade owns caching, selection, search and hit-testing on top of it:

- `info()` returns document-level `DocumentInfo`: format, metadata, page count, outline and attachment metadata;
- `page_info(index)` returns page geometry without loading page content;
- `load_page(index)` parses and returns one `Page`;
- `load_attachment(id)` returns attachment bytes;
- `font_names()` and `clear_caches()` are optional hooks with empty defaults.

Opening a document parses only metadata, the page table, outlines and resource indexes; `load_page` parses a single page on demand, and the reader keeps a byte-budgeted LRU cache. Damage to one page affects only that page: the reader shows a placeholder and keeps the diagnostics.

`FixedDocumentSource` implements the same trait for an already laid-out `FixedDocument`:

```rust
use ofdbox_folio_ir::{DocumentSource, FixedDocumentSource, SourceFormat};

// Bridge layout output back into the reader stack.
let mut source = FixedDocumentSource::new(fixed, SourceFormat::Docx);
let info = source.info();
let page = source.load_page(0)?;
```

That bridge is what lets a DOCX package, or a document built through the creation API, use the same page cache, selection, search and hit-testing APIs as native OFD and PDF files. `document()` borrows the wrapped fixed document, and `into_inner()` returns it.

`DocumentSource` does not currently require `Send`, because the `ofdbox-core` package reader is single-threaded; the bound can be tightened later.

## Determinism

- Parsing and layout read no clock, no random numbers and no environment variables, and never depend on `HashMap` iteration order.
- Same Flow input + same font bytes + same options + same engine version produce an identical `FixedDocument` and identical output bytes.
- `FixedDocument.engine` carries `EngineInfo`: the layout engine version and the content fingerprints of the fonts that participated (`FontFingerprint`), so a result can be attributed to an engine build and to exact font bytes.
- Reader load timing may vary, but page content does not change because of it.
- There is no logging and no printing: anything unrepresentable becomes a `Report` diagnostic, and failures return `Result` errors.

## Format-Neutral Constraint

- OFD, PDF, DOCX, ZIP and XML types never appear in the public API; the crate's only dependency is `thiserror`.
- Dependency leaks are forbidden in public signatures: `ofdbox_core`, `lopdf`, `quick_xml`, `zip`, `rustybuzz`, `ttf_parser`, `vello` and `wgpu` types stay inside their own crates.
- The crate defines types, validation and text algorithms only: no file or network access, no rendering, no layout.
- Fixed IR keeps no back-references to Flow IR, so a page remains valid without the document it came from.
- Fonts are injected through `FontProvider`; the crate never scans system fonts, and its TrueType helpers (`extract_truetype_face`, `subset_truetype`) do not expose a font library's types.

## See Also

- [Architecture](../../docs/architecture.md): the crate map, the two IRs and the key contracts.
- [Layout contract](../ofdbox-folio-layout/CONTRACT.md): the rules that turn Flow IR into Fixed IR.
- [Project README](../../README.md): goals, status and usage.

## License

Dual-licensed under MIT or Apache-2.0, at your option: [LICENSE-MIT](../../LICENSE-MIT) · [LICENSE-APACHE](../../LICENSE-APACHE).
