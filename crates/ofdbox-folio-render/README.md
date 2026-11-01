English | [简体中文](README.zh-CN.md)

# ofdbox-folio-render

Bitmap rendering for [`ofdbox-folio`](../../README.md): it rasterizes Fixed IR pages and scenes on the CPU and, behind the optional `wgpu` feature, on the GPU.

## Public Entry Points

- `CpuRenderer` (`new()` / `with_defaults()`) draws a `Page` or `Scene` with `render_page()` / `render_scene()` into an `RasterTarget`; `render_scene_region()` refreshes one tile using `RenderRegion`. `clear_caches()`, `report()` and `take_report()` expose cache control and per-image diagnostics.
- `RenderOptions` sets the page background, the maximum decoded image pixels and the decoded-image cache byte budget.
- `decode_image()` and `detect_format()` handle PNG, JPEG, BMP, JBIG2 and TIFF bytes; `DecodedImage` can apply a grayscale mask.
- With the `wgpu` feature (facade feature `gpu`): `WgpuContext`, `WgpuTarget`, `WgpuCapabilities` and `WgpuBackend`; without it, `WgpuBackend` is a placeholder whose `draw()` returns `Unsupported`.

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

### GPU Backend

The `wgpu` feature of `ofdbox-folio-render` (facade `gpu`) provides the compute backend; `gpu` is off by default.

Solid fills, strokes, linear gradients, clipping, atlas-backed text and RGBA images.

The backend batches all fill/stroke paths of a scene into one compute dispatch, all
glyphs into one instanced text draw, and all image quads into one render pass. Text
glyphs are rasterized into an R8 atlas; images are decoded once and cached as GPU
textures. `WgpuBackend::render_page` is the page-level entry point,
`render_page_region` renders a tile of a page for incremental refresh, and
`examples/wgpu_compare.rs` in the facade renders a page with both backends and
reports the pixel difference.

## Known Limitations

- Radial gradients and blend modes are not yet implemented in the GPU backend.
- GPU text uses solid-color atlas glyphs; synthetic bold is approximated by
  stamping offset glyphs, and gradient text fills fall back to the first color
  stop.
- Paths are drawn before all images and text; images and text keep their relative
  scene order.
- Clip paths clip text and image quads to the clip bounding box with a scissor
  rect; non-rectangular clip paths are therefore approximate for glyphs/images.
- Use the CPU renderer when you need the full feature set.
- The GPU backend exists only with the `wgpu` feature; the default build exposes a placeholder `WgpuBackend` that reports `Unsupported`.

## See Also

- [Architecture](../../docs/architecture.md) · [架构](../../docs/architecture.zh-CN.md)
- [ofdbox-folio-ir](../ofdbox-folio-ir) — the `Page`, `Scene`, paint, image and font types consumed here.
- [ofdbox-folio-ofd](../ofdbox-folio-ofd) · [ofdbox-folio-pdf](../ofdbox-folio-pdf) — the adapters that produce the pages this crate renders.
- [ofdbox-folio](../ofdbox-folio) — the facade exposing rendering behind the `cpu` and `gpu` features.
