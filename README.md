English | [简体中文](README.zh-CN.md)

# ofdbox-folio

OFDBox Folio is a page-centric multi-format document engine: it reads OFD, PDF and DOCX, unifies them into Flow IR / Fixed IR, and handles layout, rendering and writing.

## Features

- Open OFD, PDF and DOCX through one reader: OFD/PDF load pages on demand, DOCX is read into Flow IR and laid out eagerly.
- Build Flow IR with one creation API: write DOCX directly, or write OFD/PDF/bitmaps after layout.
- Fixed IR as the unified read model: lazy pages, text selection/search, page cache, incremental rendering.
- Wrap any laid-out `FixedDocument` back into the reader (`FixedDocumentSource`) to reuse caching, selection and search.
- Deterministic output for the same input, font bytes and engine version.
- CPU renderer plus an optional wgpu backend; OFD/PDF writers with font subsetting.

The engine is organized around a single flow:

```text
Creation API ──┐
               │
DOCX read ─────┤
               ├─► Flow IR ──► DOCX write
                      │
                      │ layout (one-way)
                      ▼
                  Fixed IR ──┬─► bitmap rendering
                      ▲      ├─► OFD write
                      │      └─► PDF write
OFD / PDF read ───────┘
```

Flow IR is the semantic, editable model (twips, no absolute coordinates); Fixed IR is the immutable, cacheable page model (millimetres, origin top-left, y down). Layout is the only downward path; OFD/PDF reading bypasses it, and DOCX writing consumes Flow IR directly.

## Architecture

```text
ofdbox-folio                façade: format probing, Reader, creation API, convenience conversion
├─ ofdbox-folio-ofd        OFD read/write        → ofdbox-core
├─ ofdbox-folio-pdf        PDF read/write        → lopdf
├─ ofdbox-folio-docx       DOCX read/write       → zip + quick-xml
├─ ofdbox-folio-layout     Flow → Fixed, one-way layout
├─ ofdbox-folio-render     Fixed → bitmap, CPU/GPU rendering and caching
└─ ofdbox-folio-ir         Flow IR, Fixed IR, geometry, resources, text layer, Reader contracts
```

Format adapters, layout and rendering depend on `ofdbox-folio-ir`; `ofdbox-folio-ir` depends on no format library or rendering backend.

Details: [architecture](docs/architecture.md) · [reader](docs/reader.md) · [format adapters](docs/formats.md) · [layout contract](docs/layout-contract.md).

## Quick Start

```bash
cargo test --workspace
```

Read a page (OFD, PDF and DOCX use the same API):

```rust
use ofdbox_folio::{Document, OpenOptions};

let mut document = Document::open_path("input.ofd", OpenOptions::default())?;
let page = document.page(0)?;

// DOCX is read into Flow IR and laid out on open.
let mut document = Document::open_path("input.docx", OpenOptions::default())?;
```

Build Flow IR and write DOCX, or lay it out and write OFD:

```rust
use ofdbox_folio::{
    docx::{DocxWriteOptions, write_flow},
    ir::{Block, FlowDocument, Inline, ParaStyle, Paragraph, TextRun, TextStyle},
    layout::layout,
    ofd::{OfdWriteOptions, write_fixed},
};

let mut flow = FlowDocument::new(Default::default());
let mut paragraph = Paragraph::new(ParaStyle::default());
paragraph.runs.push(Inline::Text(TextRun::new("Hello, world", TextStyle::default())));
flow.push(Block::Paragraph(paragraph));

let docx = write_flow(&flow, &DocxWriteOptions::default())?;

let mut fonts = MyFontProvider::new();
let fixed = layout(&flow, &mut fonts)?;
let ofd = write_fixed(&fixed, &OfdWriteOptions::default())?;
```

Render or convert from the command line:

```bash
cargo run -p ofdbox-folio --example ofd_to_png -- input.ofd output.png 0 96
cargo run -p ofdbox-folio --example docx_to_ofd -- input.docx output.ofd
```

## Guarantees & Limits

- One-way: only Flow → Fixed; Fixed IR keeps no back-references to Flow.
- Format-neutral: OFD/PDF/DOCX types never appear in IR, layout or rendering interfaces.
- Immutable pages: a `Page` is cacheable and shareable; OFD/PDF documents are not fully built on open (DOCX is laid out eagerly).
- Units: Flow uses twips, Fixed uses millimetres, origin top-left, y down.
- Determinism: no clock, randomness or environment reads; same input + fonts + options + version → identical output.
- Failures are returned as errors or recorded as diagnostics; parsers do not panic on untrusted input.

Known limits:

- OFD without embedded fonts needs a caller-provided `FontProvider`; otherwise text is kept in the text layer but not drawn.
- OFD template pages are flattened into page content.
- The page cache accounts for page structure only; fonts and images are shared document resources.
- The GPU backend covers solid fills, strokes, linear gradients, clipping and text outlines; images, radial gradients and blend modes are CPU-only.
- OFD pattern/mesh fills are not written yet; PDF tiling patterns and JPEG2000 are not read yet.
- DOCX is laid out eagerly when opened, so its page count and layout cost are paid up front; the page cache then caches laid-out pages instead of deferring parsing.
- DOCX layout approximates Word (no line-by-line fidelity); table and header/footer fidelity is still being hardened.

## Status & Roadmap

| Phase | Content | State |
|---|---|---|
| R1 Reader kernel | IR, OFD reading, CPU rendering, text layer, page cache | done |
| R2 Flow documents | Flow IR, layout (columns/tables/headers/footers/TOC), DOCX read/write, creation API | in progress |
| R3 PDF hardening | fonts/CMaps, patterns, transparency, encryption, malformed files | in progress |
| R4 Document capabilities | annotations/forms, attachments, outline editing, GPU | planned |

## Verification

```bash
cargo test --workspace
```

Acceptance highlights: generated OFD is strictly readable and byte-identical after a strict rewrite; layout digests are stable across platforms; reader memory stays bounded as page count grows; selection/search never re-parse pages; a damaged page does not affect others.

## Docs & License

- [Architecture](docs/architecture.md) · [Reader](docs/reader.md) · [Format adapters](docs/formats.md) · [Layout contract](docs/layout-contract.md)
- 中文文档：[架构](docs/architecture.zh-CN.md) · [Reader](docs/reader.zh-CN.md) · [格式适配](docs/formats.zh-CN.md) · [布局契约](docs/layout-contract.zh-CN.md)
- [中文 README](README.zh-CN.md)

Dual-licensed under MIT or Apache-2.0, at your option: [LICENSE-MIT](LICENSE-MIT) · [LICENSE-APACHE](LICENSE-APACHE).
