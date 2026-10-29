English | [简体中文](reader.zh-CN.md)

# Reader

The reader is the entry point for opening OFD, PDF and DOCX documents while keeping pages lazy, cached and selectable; see the [README](../README.md) for the full project overview.

## Loading Model

- **Metadata first**: opening a document parses only metadata, the page table, outlines and resource indexes; `DocumentSource::load_page()` parses a single page on demand.
- **Immutable pages**: a `Page` can be cached and shared across threads; the document never builds all pages at once.
- **Shared resources**: fonts and images belong to the document, not to a page, so loading another page does not reload them.
- **Local damage**: a page that fails to load shows a placeholder and keeps its diagnostics; the rest of the document stays readable.
- **Untrusted input**: every format parser returns errors instead of panicking, bounded by limits on page count, decompression, image pixels and recursion depth.

## Reader Performance Model

- **Lazy pages**: first-screen time is independent of the total page count; memory is determined by the cache budget and the visible pages.
- **Separated resources**: fonts and images are shared by content; images are decoded at the target scale tier, so thumbnails do not trigger full-resolution decoding.
- **Render-list cache**: a flat render-command list is generated the first time a page is drawn and reused for scrolling and zooming.
- **Independent text layer**: drawing order is not reading order; the text layer stores line boxes, baselines and character mappings, so selection/search does not re-parse the page.
- **Progressive layout**: after DOCX is parsed into Flow IR, pages are produced in order, showing the first page first, with total-page fields arriving later.
- **Error isolation**: a page-level error does not abort document loading.

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

Search loads pages on demand and puts them into the cache; when a selection spans pages, the text is joined with newlines and pages are not re-parsed in between. Hit rectangles and selection rectangles are both in page space, so the caller only has to map them to its viewport.

## Error Isolation

- **Page-local failure**: a damaged page does not abort document loading; that page shows a placeholder and the document keeps working.
- **Diagnostics are kept**: the reason for the failure stays available, so the caller can show the rest of the document and report the damaged page.
- **No collateral damage**: one bad page does not invalidate the cache or force other pages to be re-parsed.
- **Bounded by contract**: page-level failures return errors rather than panicking, and the limits on page count, decompression, image pixels and recursion depth still apply.

## Testing and Acceptance

- Reader memory stays bounded as the total page count grows; page turns reuse font, image and render caches.
- Text selection, copying and search do not re-parse pages.
- A single damaged page does not affect other pages.

## Quick Verification

The reader examples check the loading model end to end:

```bash
cargo run -p ofdbox-folio --example ofd_to_png -- input.ofd output.png 0 96
cargo run -p ofdbox-folio --example ofd_search -- input.ofd query
```

`ofd_to_png` renders page 0 at 96 DPI, and `ofd_search` runs a cross-page query through the same search API shown above.
