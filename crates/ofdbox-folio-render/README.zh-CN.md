[English](README.md) | 简体中文

# ofdbox-folio-render

[`ofdbox-folio`](../../README.zh-CN.md) 的位图渲染：在 CPU 上栅格化 Fixed IR 页面与场景，并可通过可选的 `wgpu` feature 在 GPU 上渲染。

## Public Entry Points

- `CpuRenderer`（`new()` / `with_defaults()`）用 `render_page()` / `render_scene()` 把 `Page` 或 `Scene` 画进 `RasterTarget`；`render_scene_region()` 借助 `RenderRegion` 刷新单个分块。`clear_caches()`、`report()` 与 `take_report()` 提供缓存控制与逐图诊断。
- `RenderOptions` 设置页面背景色、单图最大解码像素与解码图片缓存的字节预算。
- `decode_image()` 与 `detect_format()` 处理 PNG、JPEG、BMP、JBIG2 与 TIFF 字节；`DecodedImage` 可应用灰度蒙版。
- 启用 `wgpu` feature（门面 feature `gpu`）时提供 `WgpuContext`、`WgpuTarget`、`WgpuCapabilities` 与 `WgpuBackend`；未启用时 `WgpuBackend` 是占位类型，`draw()` 返回 `Unsupported`。

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

CPU 渲染器基于 `vello_cpu`，只消费 Fixed IR，不感知源文档格式。它把 Fixed IR 场景（路径、文本与图片）按指定 DPI 画进 RGBA8 目标，支持整页与分块区域渲染以便增量刷新，并按字节预算缓存解码后的图片，超预算后按插入顺序淘汰。

### GPU Backend

`ofdbox-folio-render` 的 `wgpu` feature（门面 `gpu`）提供 compute 后端，`gpu` 默认关闭。

实色填充、描边、线性渐变、裁剪与文字轮廓。

## Known Limitations

- GPU 后端尚未实现图片、径向渐变与混合模式。
- 需要完整特性时使用 CPU 渲染器。
- GPU 后端仅在启用 `wgpu` feature 时存在；默认构建暴露占位 `WgpuBackend`，其调用返回 `Unsupported`。

## See Also

- [Architecture](../../docs/architecture.md) · [架构](../../docs/architecture.zh-CN.md)
- [ofdbox-folio-ir](../ofdbox-folio-ir) — 本 crate 消费的 `Page`、`Scene`、绘制、图片与字体类型。
- [ofdbox-folio-ofd](../ofdbox-folio-ofd) · [ofdbox-folio-pdf](../ofdbox-folio-pdf) — 产出本 crate 所渲染页面的适配器。
- [ofdbox-folio](../ofdbox-folio) — 在 `cpu` 与 `gpu` feature 后暴露渲染能力的门面。
