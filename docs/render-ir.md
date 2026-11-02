# Render IR

`RenderPlan` is the page-space display list between Fixed IR and the raster
backends. `PageSnapshot` is the immutable boundary between the document parser
and the renderer.

```text
Document (thread-confined)
  └─ Page
       └─ RenderPlan (page-space commands)
            └─ PageSnapshot { RenderPlan + TextLayer + links + info }
                 ├─ UI: text, search, selection
                 └─ Renderer: raster
```

## Invariants

1. A `RenderPlan` contains only commands that can be rasterized directly in page
   space.
2. A `RenderPlan` is independent of DPI, tile origin and backend.
3. A `RenderPlan` is immutable after construction.
4. `PageSnapshot` is `Send + Sync`; `Document` and `Page` are not required to be.
5. Backends must not walk the semantic `Scene` tree to discover content.
6. `DevicePlan` (a future per-DPI/tile cache) is backend-internal and is not part
   of the public IR.

## Commands

- `PushLayer` / `PopLayer`: clip and group opacity.
- `FillPath` / `StrokePath`: resolved path, transform, paint and fill rule.
- `DrawImage`: image resource, local rect, transform and alpha.
- `DrawText`: font, size, transform, glyph run, fill and stroke.

Commands carry references to shared resources (`Arc<Path>`, `Arc<FontAsset>`,
`Arc<ImageAsset>`, `Arc<GlyphRun>`), never parser-owned state. Every drawing
command also carries page-space bounds; renderers skip commands outside the
current tile before flattening or rasterizing.

## Thread Boundary

`PageSnapshot::from_page` consumes a `Page` on the parser thread and builds the
plan there. Only the resulting snapshot crosses to the UI or a render worker.
This avoids requiring `DocumentSource: Send` or `Page: Send`.

## Planned Layers

- `RenderPlan`: page-space commands and bounds (current); tile culling is implemented.
- `DevicePlan`: per-DPI/tile command visibility (current; CPU and tiny-skia consume it).
- `DevicePlan`: optional per-DPI/tile culling and precomputed device data.
- Backend caches: glyph outlines, atlases, decoded images and textures.

`DevicePlan` must not leak backend types into the public API.

## Migration Notes

- `PreparedScene`/`PreparedCommand`/`PreparedClip` remain aliases for
  `RenderPlan`/`RenderCommand`/`RenderClip`.
- `CpuRenderer` and `TinySkiaRenderer` expose both `render_prepared*` and
  `render_plan*` entry points during the transition.
- The wgpu backend consumes the same plan for path, text and image draws.
