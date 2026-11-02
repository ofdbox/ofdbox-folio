# Development Plan

This plan turns the architecture audit into incremental, testable work. The
guiding rule is to keep the existing crate boundaries and Fixed IR contract,
and to remove duplicated renderer semantics before adding new document
features.

## Goals

1. One renderer contract and one prepared scene pipeline.
2. One shared text/font/glyph stack.
3. A reader that can load and render off the UI thread.
4. Cache budgets that cover pages, fonts, glyphs, atlases and images.
5. Feature combinations and public documentation that match reality.

Non-goals for the first pass:

- rewriting Flow IR or Fixed IR;
- adding annotations, forms or color management before the renderer pipeline is
  shared;
- replacing vello_cpu, tiny-skia or wgpu;
- changing format-adapter public APIs.

## Status

- Phase 0: complete.
- Phase 1: in progress. `PreparedScene` is consumed by the CPU and tiny-skia
  renderers; wgpu and the prepared-scene cache still need to migrate.
- Phase 2: started. The wgpu image texture cache now has a 64 MiB byte budget;
  atlas/glyph eviction and configurable budgets remain.

## Phase 0: Feature matrix and contract correctness

Deliverables:

- `ofdbox_folio::render` is exported for every renderer feature independently:
  `cpu`, `tiny-skia`, `gpu27` and `gpu30`; no feature requires another renderer
  by accident.
- CI builds and tests `tiny-skia` independently from the default CPU feature.
- CI checks the facade with each renderer feature and without default features.
- Documentation states current backend capabilities accurately.

Acceptance:

```text
cargo check -p ofdbox-folio --no-default-features --features ofd,layout,tiny-skia
cargo check -p ofdbox-folio --no-default-features --features ofd,layout,gpu30
cargo test  -p ofdbox-folio-render --no-default-features --features tiny-skia
cargo test  -p ofdbox-folio-render --no-default-features --features wgpu30
```

## Phase 1: Shared prepared scene

Deliverables:

- A backend-neutral `PreparedScene` (display list) in `ofdbox-folio-render`:
  - page-space paths, glyph runs, images, clip stacks and paints;
  - bounds and a simple spatial index for region culling;
  - no backend types in its public surface.
- `prepare_scene(page_or_scene, dpi_bucket)` with a cache keyed by scene
  identity plus DPI bucket.
- `CpuRenderer` and `TinySkiaRenderer` consume the prepared scene for a first
  vertical slice: solid paths and images.
- `WgpuBackend` consumes the prepared text placements instead of walking the
  scene tree per tile.

Acceptance:

- Rendering the same page region from the prepared scene is pixel-identical to
  the current backend within the existing anti-aliasing tolerance.
- Two tiles of one page reuse one prepared scene; no second font-face parse or
  glyph lookup for the same page.
- A benchmark shows no regression at 48 DPI and fewer CPU cycles per tile.

## Phase 2: Shared text and resource management

Deliverables:

- `TextEngine` with a content-addressed font-face cache and a glyph cache keyed
  by `(font fingerprint, glyph id, pixel size, bold)`.
- Atlas manager with a byte budget, growth policy and eviction; atlas uploads
  are batched per render batch.
- Image cache with LRU eviction, DPI tiers and an explicit decoded-byte budget
  shared by all backends.
- Unified cache statistics exposed through the renderer API.

Acceptance:

- Switching between pages with different font subsets does not re-parse the
  same font file.
- A long paging session keeps renderer memory under the configured budget.
- Atlas exhaustion degrades to eviction, never to silently missing text.

## Phase 3: Reader concurrency

Deliverables:

- A `PageLoader` worker that owns the `DocumentSource`; the UI thread only sends
  requests and receives `Arc<Page>` results.
- Cancellable prefetch and search with progress callbacks.
- A placeholder page API for damaged pages, so callers do not need to special
  case every page error.
- `Document` becomes a thread-safe handle; `Page` stays immutable and cheap to
  clone.

Acceptance:

- Opening a large document and jumping to page N never blocks the UI thread on
  page parsing.
- Cancelling a search stops loading further pages.
- Damaged pages return a stable placeholder and a diagnostic without
  invalidating other pages.

## Phase 4: IR capabilities

Only after Phases 1-2:

- blend modes and soft masks;
- independent transparency groups;
- pattern/mesh fills and optional content groups;
- annotations, forms and signatures;
- color management (ICC/CMYK/spot colors).

Each capability needs one IR representation and one prepared-scene primitive;
backends may report it as unsupported through capabilities, but must not
silently reinterpret it.

## Phase 5: Quality gates

- Golden-image corpus for OFD, PDF and DOCX text, images, clips and gradients.
- Fuzz targets for OFD/PDF/DOCX package and page parsing.
- CI benchmark smoke test with a generous regression threshold.
- `cargo doc` link checks and a documentation test for feature combinations.

## Recommended order for the first iteration

1. Phase 0 (this branch).
2. Phase 1 vertical slice: `PreparedScene` for solid paths and images, then
   text.
3. Phase 2 atlas/cache manager.
4. Phase 3 reader worker.
5. Phase 4 capabilities.
