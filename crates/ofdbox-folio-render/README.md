English | [简体中文](README.zh-CN.md)

# ofdbox-folio-render

Bitmap rendering for [`ofdbox-folio`](../../README.md): it rasterizes Fixed IR pages and scenes on the CPU and, behind the optional `wgpu` feature, on the GPU.

## Public Entry Points

- `RenderCacheStats` reports cached fonts, glyphs, images and image bytes for all three renderers; the CPU and tiny-skia `cache_stats()` and wgpu `render_cache_stats()` return it.
- `RenderPlan::from_scene()` resolves a Fixed IR scene into a backend-neutral command list; `PageSnapshot::from_page()` adds the text layer, links and page info for the parse/render thread boundary; `DevicePlan` culls a plan to one DPI/tile; `PageSnapshotCache` and `DevicePlanCache` provide page/tile reuse. with explicit layer/clip commands; `CpuRenderer` and `TinySkiaRenderer` expose `render_plan()` / `render_plan_region()` (and the older `render_prepared*` aliases); the wgpu backend consumes it for path, text and image draws and caches the current page's prepared scene.
- `CpuRenderer` (`new()` / `with_defaults()`) draws a `Page` or `Scene` with `render_page()` / `render_scene()` into an `RasterTarget`; `render_scene_region()` refreshes one tile using `RenderRegion`. `clear_caches()`, `report()` and `take_report()` expose cache control and per-image diagnostics.
- `RenderOptions` sets the page background, the maximum decoded image pixels and the decoded-image cache byte budget.
- `decode_image()`, `decode_image_tier()` and `detect_format()` handle PNG, JPEG, BMP, JBIG2 and TIFF bytes; `DecodedImage` can apply a grayscale mask.
- With the `wgpu` feature (facade feature `gpu`): `WgpuContext` (`new()` uses empty `InstanceFlags`; `new_from_env()` is the opt-in diagnostic path), `WgpuContext::from_device_queue()`, `WgpuTarget`, `WgpuCapabilities`, `WgpuOptions` (image cache bytes, tile cache entries, prepared scenes, glyph entries, atlas size and atlas layers), `WgpuCacheStats` and `WgpuBackend` (`new()` or `from_shared()`); without it, `WgpuBackend` is a placeholder whose `draw()` returns `Unsupported`.

### Example

```rust
use ofdbox_folio::render::{WgpuBackend, WgpuContext, WgpuTarget};

let backend = WgpuBackend::new(WgpuContext::new_blocking()?)?;
let target = WgpuTarget::new(backend.context(), page.display_size(), 144.0)?;
backend.render_to_target(&page.content, &target)?;
let rgba = target.read_rgba(backend.context())?;
```

## Coverage

### CPU Renderer

The CPU renderer is built on `vello_cpu` and consumes Fixed IR only; it is unaware of the source document format. It draws Fixed IR scenes (paths, text and images) into RGBA8 targets at a chosen DPI, supports whole-page and tile-sized region rendering for incremental display, and caches decoded images under a byte budget, evicting entries in insertion order once it is exceeded.

### tiny-skia Backend

The optional `tiny-skia` feature provides `TinySkiaRenderer`, an alternative
software renderer built on `tiny-skia`. It supports solid and gradient path
fills/strokes, glyph outlines, RGBA images with masks, clip paths and group
opacity, and exposes the same page/tile rendering API as `CpuRenderer`.

Enable it with:

```text
cargo build -p ofdbox-folio-render --features tiny-skia
```

### GPU Backend

The `wgpu` feature of `ofdbox-folio-render` (facade `gpu`) provides the compute backend; `gpu` is off by default.

Solid fills, strokes, linear gradients, clipping, atlas-backed text and RGBA images.

`WgpuTarget::from_texture()` wraps a caller-owned `Rgba8Unorm` texture (storage + render attachment usage) so rendering can complete without `read_rgba()`; `texture()` and `view()` expose the backing target; `copy_to_texture()` performs the GPU-side copy into a display texture; `with_transparent_background()` / `with_background()` control the clear color used before wgpu rendering. Embedded renderers should share the host device with `WgpuContext::from_device_queue` + `WgpuBackend::from_shared` instead of creating a second GPU device. `WgpuContext::new()` deliberately disables backend validation/debug layers; wgpu 27 can otherwise enable the D3D12 debug layer in debug builds and reset the GPU in processes that already own a display device.

The backend groups consecutive path elements into source-ordered runs, rasterizes
each run into a transparent layer and composites it at the right position, so
paths, text and images follow the scene tree order. Glyphs are batched into one
instanced text draw, and all image quads share one render pass. Text glyphs are
rasterized into a four-layer R8 atlas array and new glyph bitmaps are uploaded
through one staging buffer per frame and copied in the main command encoder; images are decoded once and cached as GPU textures. `WgpuBackend::render_page` is the page-level entry point,
`render_page_region` renders a tile of a page for incremental refresh, and
`examples/wgpu_compare.rs` in the facade renders a page with both backends and
reports the pixel difference.

## Known Limitations

- GPU path radial gradients and blend modes are not yet implemented; text
  radial gradients are supported.
- The tiny-skia backend currently draws text fills only: text strokes and
  synthetic bold text are not yet implemented.
- GPU text supports solid, linear-gradient and radial-gradient fills; synthetic
  bold is rendered by dilating the glyph coverage bitmap.
- Text and image quads are clipped per fragment against the clip path edges; a
  nested clip uses the innermost clip path.
- Radial text gradients support the two-point conical form plus optional
  ellipse squash/rotation.
- Use the CPU renderer when you need the full feature set.
- The GPU backend exists only with the `wgpu` feature; the default build exposes a placeholder `WgpuBackend` that reports `Unsupported`.

## See Also

- [Architecture](../../docs/architecture.md) · [架构](../../docs/architecture.zh-CN.md)
- [ofdbox-folio-ir](../ofdbox-folio-ir) — the `Page`, `Scene`, paint, image and font types consumed here.
- [ofdbox-folio-ofd](../ofdbox-folio-ofd) · [ofdbox-folio-pdf](../ofdbox-folio-pdf) — the adapters that produce the pages this crate renders.
- [ofdbox-folio](../ofdbox-folio) — the facade exposing rendering behind the `cpu` and `gpu` features.
