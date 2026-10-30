English | [简体中文](README.zh-CN.md)

# ofdbox-folio

`ofdbox-folio` is the facade crate of [OFDBox Folio](../../README.md): one reader for OFD, PDF and DOCX, with page caching, selection, search and hit testing over Fixed IR.

## Quick Start

OFD, PDF and DOCX open through the same `Document` API; the format is probed from the file content unless `OpenOptions::format` says otherwise. OFD and PDF stay lazy, while DOCX is read into Flow IR and laid out eagerly, so all of its pages already exist once it is open:

```rust
use ofdbox_folio::{Document, OpenOptions};

let mut document = Document::open_path("input.ofd", OpenOptions::default())?;
let page = document.page(0)?;
println!("{} x {} mm", page.display_size().width, page.display_size().height);

// PDF: the same call, the format is detected from the leading bytes.
let mut document = Document::open_path("input.pdf", OpenOptions::default())?;
let page = document.page(0)?;

// DOCX: read into Flow IR and laid out eagerly on open, so every page already exists.
let mut document = Document::open_path("input.docx", OpenOptions::default())?;
let page = document.page(0)?;
```

The bundled examples exercise the same API end to end:

```bash
cargo run -p ofdbox-folio --example ofd_to_png -- input.ofd output.png 0 96
cargo run -p ofdbox-folio --example ofd_search -- input.ofd query
```

## Unified API

Every format adapter implements `DocumentSource`, and `Document` drives any source through one page cache:

- **`Document`** — `open_path` / `open_bytes` to open, then `info`, `metadata`, `page_count`, `outline`, `attachments`, `page_info`, `page`, `page_text`, `text_position`, `prefetch`, `prefetch_around`, `cache_stats` and `clear_cache`.
- **`DocumentSource`** — the contract every format implements: `info`, `page_info`, `load_page`, `load_attachment`, plus optional `font_names` and `clear_caches`. `FixedDocumentSource` adapts a laid-out `FixedDocument` to the same contract.
- **Laid-out documents are sources** — `Document::from_fixed` wraps a `FixedDocument` directly; `Document::from_flow` lays out a `FlowDocument` first (requires the `layout` feature). Both take `ReaderOptions` and reuse the cache, selection, search and hit-testing APIs.
- **Cache** — one `CacheOptions` per document, set through `OpenOptions::cache` or `ReaderOptions::cache`; see [Reader Cache](#reader-cache).
- **Search, selection and hit testing** — `search` / `search_all`, `selection_text`, `selection_rects` and `hit_test` read the text layer and return page-space geometry; see [Cross-Page Selection and Search](#cross-page-selection-and-search).

```rust
use ofdbox_folio::{
    ir::{FixedDocument, SourceFormat},
    Document, ReaderOptions,
};

// Wrap layout output so it reuses the same reader APIs as native OFD/PDF.
let fixed = FixedDocument::default(); // any laid-out document, e.g. from `ofdbox_folio::layout`
let mut document = Document::from_fixed(fixed, SourceFormat::Docx, ReaderOptions::default());
document.page(0)?;
```

## Loading Model

- **Metadata first (OFD/PDF)**: opening a native document parses only metadata, the page table, outlines and resource indexes; `DocumentSource::load_page()` parses a single page on demand.
- **DOCX is eager**: opening DOCX parses `word/document.xml` into Flow IR, runs layout with the supplied `FontProvider`, and wraps the resulting `FixedDocument` in `FixedDocumentSource`; all pages exist right after open.
- **Layout output is a source**: `Document::from_fixed` and `Document::from_flow` expose the same cache, selection, search and hit-testing APIs for laid-out documents.
- **Immutable pages**: a `Page` can be cached and shared across threads; OFD/PDF never build all pages at once, while DOCX pages already exist after layout.
- **Shared resources**: fonts and images belong to the document, not to a page, so loading another page does not reload them.
- **Local damage**: a page that fails to load shows a placeholder and keeps its diagnostics; the rest of the document stays readable.
- **Untrusted input**: every format parser returns errors instead of panicking, bounded by limits on page count, decompression, image pixels and recursion depth.

## Performance Model

- **Lazy pages (OFD/PDF)**: first-screen time is independent of the total page count; memory is determined by the cache budget and the visible pages.
- **Separated resources**: fonts and images are shared by content; images are decoded at the target scale tier, so thumbnails do not trigger full-resolution decoding.
- **Render-list cache**: a flat render-command list is generated the first time a page is drawn and reused for scrolling and zooming.
- **Independent text layer**: drawing order is not reading order; the text layer stores line boxes, baselines and character mappings, so selection and search do not re-parse the page.
- **Eager DOCX layout**: DOCX is parsed into Flow IR and fully laid out when opened; page count and layout cost are paid up front, and the page cache then caches laid-out pages. Progressive first-page-first layout is not implemented yet.

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

The cache is charged by page-structure bytes and evicts in LRU order; `max_bytes = 0` disables it. Fonts and images are document-level shared resources accounted separately, so the budget bounds page structure rather than the whole document footprint.

## Cross-Page Selection and Search

```rust
use ofdbox_folio::{ir::Point, Document, OpenOptions, SearchQuery, Selection, TextPosition};

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

let position = document.hit_test(0, Point::new(12.0, 18.0))?; // page coordinates, in millimetres
```

Search loads pages on demand and puts them into the cache; when a selection spans pages, the text is joined with newlines and pages are not re-parsed in between. Hit rectangles and selection rectangles are both in page space, so the caller only has to map them to its viewport.

## Error Isolation

- **Page-local failure**: a damaged page does not abort document loading; that page shows a placeholder and the document keeps working.
- **Diagnostics are kept**: the reason for the failure stays available, so the caller can show the rest of the document and report the damaged page.
- **No collateral damage**: one bad page does not invalidate the cache or force other pages to be re-parsed.
- **Bounded by contract**: page-level failures return errors rather than panicking, and the limits on page count, decompression, image pixels and recursion depth still apply.

## See Also

- [Architecture](../../docs/architecture.md) — how the facade relates to IR, layout, rendering and the format adapters.
- [Layout contract](../ofdbox-folio-layout/CONTRACT.md) — the Flow → Fixed contract shared by layout and readers.
- [OFD adapter](../ofdbox-folio-ofd/README.md) — OFD probing, reading and writing.
- [PDF adapter](../ofdbox-folio-pdf/README.md) — PDF parsing, fonts and writing.
- [DOCX adapter](../ofdbox-folio-docx/README.md) — DOCX read/write and the Flow IR mapping.
- [Render](../ofdbox-folio-render/README.md) — CPU/GPU bitmap rendering of Fixed IR.
