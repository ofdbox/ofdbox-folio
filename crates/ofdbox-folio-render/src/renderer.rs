//! CPU bitmap renderer built on `vello_cpu`.
//!
//! The renderer consumes only Fixed IR and is unaware of the source document
//! format. Page geometry is mapped to pixel coordinates via the device scale;
//! fonts and images are cached by resource ID.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ofdbox_folio_ir::{
    Diagnostic, DiagnosticCode, FillRule, FontAsset, GradientExtend, GradientStop, ImageElement,
    LineCap, LineJoin, Matrix, Page, Paint, Path, PathElement, PathSegment, RadialGradient, Report,
    Rgba, Scene, Size, StrokeStyle, TextElement,
};
use vello_cpu::kurbo::{
    Affine, Arc as KurboArc, BezPath, Point, Rect as KurboRect, Stroke, SvgArc, Vec2,
};
use vello_cpu::peniko::color::{AlphaColor, DynamicColor, Srgb};
use vello_cpu::peniko::{
    Blob, Brush, ColorStop, Extend, FontData, Gradient, ImageBrush, ImageSampler,
};
use vello_cpu::peniko::{Fill, ImageAlphaType};
use vello_cpu::{Glyph, ImageSource, PaintType, PixelMetadata, Pixmap, RenderContext, Resources};

use crate::cache_budget::{CacheBudget, SharedByteCache};
use crate::error::RenderError;
use crate::image::{decode_image, decode_image_tier};
use crate::options::RenderOptions;
use crate::prepared::{PreparedCommand, PreparedScene, scene_fingerprint};

/// Backend-neutral renderer cache statistics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderCacheStats {
    /// Cached parsed fonts.
    pub fonts: usize,
    /// Cached glyph outlines.
    pub glyphs: usize,
    /// Cached decoded images.
    pub images: usize,
    /// Decoded image bytes.
    pub image_bytes: u64,
    /// Whether a prepared scene is cached.
    pub prepared_scene: bool,
}

/// An RGBA8 pixel target.
#[derive(Clone, Debug)]
pub struct RasterTarget {
    width: u16,
    height: u16,
    dpi: f64,
    data: Vec<u8>,
}

/// A rectangle in whole-page device pixel coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PixelRect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl PixelRect {
    /// Creates a pixel rectangle.
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Right edge (exclusive).
    pub const fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    /// Bottom edge (exclusive).
    pub const fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }

    /// Whether the rectangle has no area.
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Raster parameters for a region within a page.
///
/// After the display core splits a page into tiles it calls
/// [`CpuRenderer::render_scene_region`] tile by tile: `origin_px` is the
/// position of this tile's top-left corner in "whole-page device pixel
/// coordinates" and `dpi` is the current render resolution (usually
/// 96 × zoom × device pixel ratio).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderRegion {
    /// Position of the region's top-left corner in whole-page device pixel coordinates.
    pub origin_px: (f64, f64),
    /// Render DPI.
    pub dpi: f64,
}

impl RenderRegion {
    /// Creates region parameters.
    pub fn new(origin_px: (f64, f64), dpi: f64) -> Self {
        Self { origin_px, dpi }
    }
}

impl RasterTarget {
    /// Creates a target from a device pixel size.
    ///
    /// Used for the display core's tile raster: the size is dictated by the tile
    /// size (usually 512×512), so DPI is not applied again.
    pub fn from_pixels(width: u32, height: u32, dpi: f64) -> Result<Self, RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::InvalidPage);
        }
        if width > u32::from(u16::MAX) || height > u32::from(u16::MAX) {
            return Err(RenderError::Backend(
                "target size exceeds the u16 range".to_owned(),
            ));
        }
        Ok(Self::new(width as u16, height as u16, dpi))
    }

    /// Creates a target with the given pixel size.
    pub fn new(width: u16, height: u16, dpi: f64) -> Self {
        Self {
            width,
            height,
            dpi,
            data: vec![0; usize::from(width) * usize::from(height) * 4],
        }
    }

    /// Creates a target from a millimeter size and a DPI.
    pub fn from_size(size: Size, dpi: f64) -> Result<Self, RenderError> {
        if !(dpi.is_finite() && dpi > 0.0)
            || !(size.width.is_finite() && size.width > 0.0)
            || !(size.height.is_finite() && size.height > 0.0)
        {
            return Err(RenderError::InvalidPage);
        }
        let width = (size.width * dpi / 25.4).ceil();
        let height = (size.height * dpi / 25.4).ceil();
        if width > f64::from(u16::MAX) || height > f64::from(u16::MAX) {
            return Err(RenderError::Backend(
                "target size exceeds the u16 range".to_owned(),
            ));
        }
        Ok(Self::new(width as u16, height as u16, dpi))
    }

    /// Width in pixels.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Resolution.
    pub fn dpi(&self) -> f64 {
        self.dpi
    }

    /// Non-premultiplied RGBA8 data.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    #[cfg(feature = "tiny-skia")]
    pub(crate) fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Takes out the pixel data.
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }

    /// Copies a sub-rectangle of this target into a new target.
    ///
    /// The rectangle is in this target's pixel coordinates and must be fully
    /// inside it.
    pub fn crop(&self, rect: PixelRect) -> Result<Self, RenderError> {
        if rect.is_empty()
            || rect.right() > u32::from(self.width)
            || rect.bottom() > u32::from(self.height)
        {
            return Err(RenderError::InvalidRegion);
        }
        let mut target = Self::from_pixels(rect.width, rect.height, self.dpi)?;
        let source_stride = usize::from(self.width) * 4;
        let dest_stride = usize::try_from(rect.width).unwrap_or(0) * 4;
        let row_bytes = dest_stride;
        for row in 0..usize::try_from(rect.height).unwrap_or(0) {
            let source_start = usize::try_from(rect.y).unwrap_or(0) + row;
            let source_index =
                source_start * source_stride + usize::try_from(rect.x).unwrap_or(0) * 4;
            let dest_index = row * dest_stride;
            target.data[dest_index..dest_index + row_bytes]
                .copy_from_slice(&self.data[source_index..source_index + row_bytes]);
        }
        Ok(target)
    }
}

/// CPU renderer built on `vello_cpu`.
pub struct CpuRenderer {
    options: RenderOptions,
    resources: Resources,
    fonts: HashMap<u64, FontData>,
    /// Decoded image pixmaps; optionally charged to a shared [`CacheBudget`].
    pixmaps: Arc<Mutex<SharedByteCache<(u64, u32), Pixmap>>>,
    report: Report,
    prepared: Option<(usize, u64, Arc<PreparedScene>)>,
    device_plans: crate::prepared::DevicePlanCache,
}

fn pixmap_bytes(pixmap: &Pixmap) -> u64 {
    u64::from(pixmap.width()) * u64::from(pixmap.height()) * 4
}

impl CpuRenderer {
    /// Creates a renderer.
    pub fn new(options: RenderOptions) -> Self {
        Self::new_with_budget(options, None)
    }

    /// Creates a renderer whose decoded-image cache shares `budget` with other
    /// backends.
    ///
    /// The local [`RenderOptions::max_image_cache_bytes`] limit still applies;
    /// the shared budget bounds the combined footprint of every participating
    /// cache.
    pub fn with_shared_budget(options: RenderOptions, budget: CacheBudget) -> Self {
        Self::new_with_budget(options, Some(budget))
    }

    fn new_with_budget(options: RenderOptions, budget: Option<CacheBudget>) -> Self {
        Self {
            options,
            resources: Resources::new(),
            fonts: HashMap::new(),
            pixmaps: SharedByteCache::new(options.max_image_cache_bytes, budget),
            report: Report::new(),
            prepared: None,
            device_plans: crate::prepared::DevicePlanCache::default(),
        }
    }

    /// Creates a renderer with default options.
    pub fn with_defaults() -> Self {
        Self::new(RenderOptions::default())
    }

    /// Current cache occupancy.
    pub fn cache_stats(&self) -> RenderCacheStats {
        let (images, image_bytes) = SharedByteCache::stats(&self.pixmaps);
        RenderCacheStats {
            fonts: self.fonts.len(),
            glyphs: 0,
            images,
            image_bytes,
            prepared_scene: self.prepared.is_some(),
        }
    }

    /// Clears the font, image and rendering resource caches.
    pub fn clear_caches(&mut self) {
        self.prepared = None;
        self.device_plans.clear();
        self.fonts.clear();
        SharedByteCache::clear(&self.pixmaps);
        self.resources = Resources::new();
    }

    /// Diagnostics produced while rendering; a single failed image only records a diagnostic and does not abort the page.
    pub fn report(&self) -> &Report {
        &self.report
    }

    /// Takes the diagnostics produced while rendering.
    pub fn take_report(&mut self) -> Report {
        std::mem::take(&mut self.report)
    }

    /// Records an image diagnostic.
    fn warn(&mut self, message: impl Into<String>) {
        self.report
            .diagnostics
            .push(Diagnostic::warning(DiagnosticCode::Image, message));
    }

    /// Renders a page into the target.
    pub fn render_page(
        &mut self,
        page: &Page,
        target: &mut RasterTarget,
    ) -> Result<(), RenderError> {
        self.render_scene(&page.content, target)
    }

    /// Renders a scene into the target.
    pub fn render_scene(
        &mut self,
        scene: &Scene,
        target: &mut RasterTarget,
    ) -> Result<(), RenderError> {
        let device = Matrix::scale(target.dpi / 25.4, target.dpi / 25.4);
        self.render_scene_with(scene, target, device)
    }

    /// Renders a union rectangle of a scene once, for later cropping.
    ///
    /// `rect` is expressed in whole-page device pixels; the scene is traversed
    /// once and rasterized into the returned target. Callers can then use
    /// [`RasterTarget::crop`] to cut individual tiles out of it.
    pub fn render_union(
        &mut self,
        scene: &Scene,
        dpi: f64,
        rect: PixelRect,
    ) -> Result<RasterTarget, RenderError> {
        if !(dpi.is_finite() && dpi > 0.0) || rect.is_empty() {
            return Err(RenderError::InvalidRegion);
        }
        let device_width = (scene.size.width * dpi / 25.4).ceil().max(1.0);
        let device_height = (scene.size.height * dpi / 25.4).ceil().max(1.0);
        if f64::from(rect.x) >= device_width || f64::from(rect.y) >= device_height {
            return Err(RenderError::InvalidRegion);
        }
        let width = rect
            .width
            .min((device_width - f64::from(rect.x)).max(0.0).ceil() as u32);
        let height = rect
            .height
            .min((device_height - f64::from(rect.y)).max(0.0).ceil() as u32);
        if width == 0 || height == 0 {
            return Err(RenderError::InvalidRegion);
        }
        let clipped = PixelRect::new(rect.x, rect.y, width, height);
        let mut target = RasterTarget::from_pixels(clipped.width, clipped.height, dpi)?;
        self.render_scene_region(
            scene,
            &mut target,
            RenderRegion::new((f64::from(clipped.x), f64::from(clipped.y)), dpi),
        )?;
        Ok(target)
    }

    /// Renders the given region of a scene into the target.
    ///
    /// The target only needs to cover the `region` area; `origin_px` is expressed
    /// in "whole-page device pixel coordinates" so the display core can refresh
    /// individual tiles.
    pub fn render_scene_region(
        &mut self,
        scene: &Scene,
        target: &mut RasterTarget,
        region: RenderRegion,
    ) -> Result<(), RenderError> {
        if !(region.dpi.is_finite() && region.dpi > 0.0)
            || !(region.origin_px.0.is_finite() && region.origin_px.1.is_finite())
        {
            return Err(RenderError::InvalidPage);
        }
        let k = region.dpi / 25.4;
        let device =
            Matrix::scale(k, k).then(Matrix::translate(-region.origin_px.0, -region.origin_px.1));
        self.render_scene_with(scene, target, device)
    }

    fn prepared_scene(&mut self, scene: &Scene) -> Arc<PreparedScene> {
        let key = (scene as *const Scene as usize, scene_fingerprint(scene));
        if let Some((cached_key, fingerprint, prepared)) = &self.prepared
            && *cached_key == key.0
            && *fingerprint == key.1
        {
            return Arc::clone(prepared);
        }
        let prepared = Arc::new(PreparedScene::from_scene(scene));
        self.prepared = Some((key.0, key.1, Arc::clone(&prepared)));
        prepared
    }

    fn render_scene_with(
        &mut self,
        scene: &Scene,
        target: &mut RasterTarget,
        device: Matrix,
    ) -> Result<(), RenderError> {
        let prepared = self.prepared_scene(scene);
        self.render_prepared_with(&prepared, target, device, None)
    }

    /// Renders a RenderPlan into a raster target.
    pub fn render_plan(
        &mut self,
        plan: &crate::prepared::RenderPlan,
        target: &mut RasterTarget,
    ) -> Result<(), RenderError> {
        self.render_prepared(plan, target)
    }

    /// Renders a region of a RenderPlan.
    pub fn render_plan_region(
        &mut self,
        plan: &crate::prepared::RenderPlan,
        target: &mut RasterTarget,
        region: RenderRegion,
    ) -> Result<(), RenderError> {
        self.render_prepared_region(plan, target, region)
    }

    /// Renders an already prepared scene into a raster target.
    pub fn render_prepared(
        &mut self,
        prepared: &PreparedScene,
        target: &mut RasterTarget,
    ) -> Result<(), RenderError> {
        let scale = target.dpi / 25.4;
        self.render_prepared_with(prepared, target, Matrix::scale(scale, scale), None)
    }

    /// Renders a region of an already prepared scene into the target.
    pub fn render_prepared_region(
        &mut self,
        prepared: &PreparedScene,
        target: &mut RasterTarget,
        region: RenderRegion,
    ) -> Result<(), RenderError> {
        if !(region.dpi.is_finite() && region.dpi > 0.0)
            || !(region.origin_px.0.is_finite() && region.origin_px.1.is_finite())
        {
            return Err(RenderError::InvalidPage);
        }
        let device_plan = self.device_plans.get_or_build(
            prepared,
            region.dpi,
            region.origin_px,
            u32::from(target.width()),
            u32::from(target.height()),
        );
        self.render_prepared_with(prepared, target, device_plan.device(), Some(&device_plan))
    }

    fn render_prepared_with(
        &mut self,
        prepared: &PreparedScene,
        target: &mut RasterTarget,
        device: Matrix,
        device_plan: Option<&crate::prepared::DevicePlan>,
    ) -> Result<(), RenderError> {
        let mut context = RenderContext::new(target.width, target.height);
        context.set_transform(Affine::IDENTITY);
        context.set_paint(color(self.options.background));
        context.fill_rect(&KurboRect::new(
            0.0,
            0.0,
            f64::from(target.width),
            f64::from(target.height),
        ));
        for (index, command) in prepared.commands.iter().enumerate() {
            if device_plan.is_some_and(|plan| !plan.is_visible(index)) {
                continue;
            }
            if let Some(bounds) = command.bounds()
                && !crate::prepared::bounds_visible(
                    bounds,
                    device,
                    u32::from(target.width),
                    u32::from(target.height),
                )
            {
                continue;
            }
            match command {
                PreparedCommand::PushLayer { clip, opacity } => {
                    context.set_transform(Affine::IDENTITY);
                    let clip_path = clip.as_ref().map(|clip| {
                        let mut path = to_bez_path(&clip.path);
                        path.apply_affine(affine(clip.transform.then(device)));
                        path
                    });
                    context.push_layer(
                        clip_path.as_ref(),
                        None,
                        (*opacity < 1.0).then_some(*opacity),
                        None,
                        None,
                    );
                }
                PreparedCommand::PopLayer => context.pop_layer(),
                PreparedCommand::FillPath {
                    path,
                    transform,
                    paint,
                    rule,
                    ..
                } => {
                    let element = PathElement {
                        path: Arc::clone(path),
                        transform: (*transform).then(device),
                        fill: Some(paint.clone()),
                        stroke: None,
                        rule: *rule,
                    };
                    self.draw_path(&mut context, &element, Matrix::IDENTITY)?;
                }
                PreparedCommand::StrokePath {
                    path,
                    transform,
                    stroke,
                    ..
                } => {
                    let element = PathElement {
                        path: Arc::clone(path),
                        transform: (*transform).then(device),
                        fill: None,
                        stroke: Some(stroke.clone()),
                        rule: FillRule::NonZero,
                    };
                    self.draw_path(&mut context, &element, Matrix::IDENTITY)?;
                }
                PreparedCommand::DrawImage {
                    image,
                    rect,
                    transform,
                    alpha,
                    ..
                } => {
                    let element = ImageElement {
                        image: Arc::clone(image),
                        rect: *rect,
                        transform: (*transform).then(device),
                        alpha: *alpha,
                    };
                    self.draw_image(&mut context, &element, Matrix::IDENTITY)?;
                }
                PreparedCommand::DrawText {
                    font,
                    font_size,
                    transform,
                    run,
                    fill,
                    stroke,
                    ..
                } => {
                    let element = TextElement {
                        font: Arc::clone(font),
                        font_size: *font_size,
                        transform: (*transform).then(device),
                        run: Arc::clone(run),
                        fill: fill.clone(),
                        stroke: stroke.clone(),
                    };
                    self.draw_text(&mut context, &element, Matrix::IDENTITY)?;
                }
            }
        }
        context.flush();
        let mut pixmap = Pixmap::new(target.width, target.height);
        context.render(&mut pixmap, &mut self.resources);
        for (pixel, source) in target
            .data
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(pixmap.data().iter().copied())
        {
            let alpha = source.a;
            if alpha == 0 {
                pixel.copy_from_slice(&[0, 0, 0, 0]);
            } else {
                let unpremultiply = |value: u8| {
                    u8::try_from((u32::from(value) * 255 + u32::from(alpha) / 2) / u32::from(alpha))
                        .unwrap_or(255)
                };
                pixel.copy_from_slice(&[
                    unpremultiply(source.r),
                    unpremultiply(source.g),
                    unpremultiply(source.b),
                    alpha,
                ]);
            }
        }
        Ok(())
    }

    fn draw_path(
        &mut self,
        context: &mut RenderContext,
        element: &PathElement,
        parent: Matrix,
    ) -> Result<(), RenderError> {
        let path = to_bez_path(&element.path);
        let transform = element.transform.then(parent);
        if let Some(paint) = &element.fill {
            if let Paint::RadialGradient(radial) = paint
                && let Some(ellipse) = ellipse_transform(radial)
                && let Some(inverse) = ellipse.invert()
            {
                let mut ellipse_path = path.clone();
                ellipse_path.apply_affine(affine(inverse));
                context.set_transform(affine(ellipse.then(transform)));
                context.set_fill_rule(rule(element.rule));
                context.set_paint(Brush::Gradient(gradient_radial_transformed(
                    radial, inverse,
                )));
                context.fill_path(&ellipse_path);
                context.set_transform(affine(transform));
            } else {
                context.set_transform(affine(transform));
                context.set_fill_rule(rule(element.rule));
                context.set_paint(brush(paint));
                context.fill_path(&path);
            }
        }
        if let Some(stroke) = &element.stroke {
            context.set_transform(affine(transform));
            context.set_paint(brush(&stroke.paint));
            context.set_stroke(kurbo_stroke(&stroke.style));
            context.stroke_path(&path);
        }
        Ok(())
    }

    fn draw_text(
        &mut self,
        context: &mut RenderContext,
        element: &TextElement,
        parent: Matrix,
    ) -> Result<(), RenderError> {
        if element.run.glyphs.is_empty() {
            return Ok(());
        }
        let transform = element.transform.then(parent);
        context.set_transform(affine(transform));
        let Some(font) = self.font_data(&element.font) else {
            return Ok(());
        };
        let glyphs = element.run.glyphs.iter().map(|glyph| Glyph {
            id: glyph.id,
            x: glyph.x as f32,
            y: glyph.y as f32,
        });
        // Font sizes are in user units (millimeters) and the text transform scales them up to
        // device pixels. Hinting/grid-fitting inside the glyph renderer would run at the
        // un-scaled size and then be upscaled by the transform, which distorts outlines (a
        // hinted SimSun "M" snapped a point below the baseline). Render unhinted outlines and
        // let the transform scale them.
        if let Some(fill) = &element.fill {
            context.set_paint(brush(fill));
            let _ = context
                .glyph_run(&mut self.resources, &font)
                .font_size(element.font_size as f32)
                .hint(false)
                .fill_glyphs(glyphs.clone());
        }
        if let Some(stroke) = &element.stroke {
            context.set_paint(brush(&stroke.paint));
            context.set_stroke(kurbo_stroke(&stroke.style));
            let _ = context
                .glyph_run(&mut self.resources, &font)
                .font_size(element.font_size as f32)
                .hint(false)
                .stroke_glyphs(glyphs);
        }
        Ok(())
    }

    fn draw_image(
        &mut self,
        context: &mut RenderContext,
        element: &ImageElement,
        parent: Matrix,
    ) -> Result<(), RenderError> {
        let max_dim = image_tier(element, element.transform.then(parent));
        let pixmap = match self.pixmap(element, max_dim) {
            Ok(pixmap) => pixmap,
            Err(error) => {
                self.warn(format!("image could not be drawn, skipping it: {error}"));
                return Ok(());
            }
        };
        let width = element
            .rect
            .width
            .is_finite()
            .then_some(element.rect.width)
            .filter(|width| *width > 0.0)
            .unwrap_or_else(|| f64::from(pixmap.width()));
        let height = element
            .rect
            .height
            .is_finite()
            .then_some(element.rect.height)
            .filter(|height| *height > 0.0)
            .unwrap_or_else(|| f64::from(pixmap.height()));
        let target = element.transform.then(parent);
        context.set_transform(affine(target));
        let image = ImageBrush {
            image: ImageSource::Pixmap(Arc::clone(&pixmap)),
            sampler: ImageSampler {
                alpha: element.alpha.clamp(0.0, 1.0),
                ..Default::default()
            },
        };
        context.set_paint(Brush::Image(image));
        context.fill_rect(&KurboRect::new(0.0, 0.0, width, height));
        Ok(())
    }

    fn font_data(&mut self, font: &FontAsset) -> Option<FontData> {
        if let Some(data) = self.fonts.get(&font.id().get()) {
            return Some(data.clone());
        }
        if skrifa::FontRef::from_index(font.data(), font.face_index()).is_err() {
            return None;
        }
        let blob = Blob::new(Arc::new(font.data().to_vec()));
        let data = FontData::new(blob, font.face_index());
        self.fonts.insert(font.id().get(), data.clone());
        Some(data)
    }

    fn pixmap(&mut self, element: &ImageElement, max_dim: u32) -> Result<Arc<Pixmap>, RenderError> {
        let key = (element.image.id().get(), max_dim);
        if let Some(pixmap) = SharedByteCache::get(&self.pixmaps, &key) {
            return Ok(pixmap);
        }
        let mut decoded = decode_image_tier(
            element.image.format,
            &element.image.encoded,
            self.options.max_image_pixels,
            max_dim,
        )?;
        if let Some(mask) = &element.image.mask {
            match decode_image(mask.format, &mask.encoded, self.options.max_image_pixels) {
                Ok(mask) => {
                    if let Err(error) = decoded.apply_mask(&mask) {
                        self.warn(format!(
                            "image mask could not be applied, drawing without it: {error}"
                        ));
                    }
                }
                Err(error) => {
                    self.warn(format!(
                        "image mask could not be decoded, drawing without it: {error}"
                    ));
                }
            }
        }
        let pixmap = Arc::new(create_pixmap(&decoded)?);
        let bytes = pixmap_bytes(&pixmap);
        SharedByteCache::insert(&self.pixmaps, key, Arc::clone(&pixmap), bytes);
        Ok(pixmap)
    }
}

/// Quantize the on-screen image size into a cache tier; 0 means full size.
///
/// The image local space is the rectangle the renderer fills; when the element leaves it empty
/// the image's pixel size is used (PDF images are drawn as a unit square scaled by the matrix),
/// so the tier still follows the on-screen size instead of falling back to full resolution.
fn image_tier(element: &ImageElement, transform: Matrix) -> u32 {
    let (width, height) = if element.rect.width.is_finite()
        && element.rect.height.is_finite()
        && element.rect.width > 0.0
        && element.rect.height > 0.0
    {
        (element.rect.width, element.rect.height)
    } else {
        (
            f64::from(element.image.pixel_width),
            f64::from(element.image.pixel_height),
        )
    };
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return 0;
    }
    let scale_x = (transform.a * transform.a + transform.b * transform.b).sqrt();
    let scale_y = (transform.c * transform.c + transform.d * transform.d).sqrt();
    let pixels = (width * scale_x).max(height * scale_y);
    if !(pixels.is_finite() && pixels >= 1.0) {
        return 0;
    }
    let mut tier = 64_u32;
    while f64::from(tier) < pixels && tier < 4096 {
        tier *= 2;
    }
    tier
}

fn create_pixmap(decoded: &crate::image::DecodedImage) -> Result<Pixmap, RenderError> {
    if decoded.width > u32::from(u16::MAX) || decoded.height > u32::from(u16::MAX) {
        return Err(RenderError::Image(
            "image size exceeds the u16 range".to_owned(),
        ));
    }
    Ok(Pixmap::from_parts(
        decoded.rgba.clone(),
        decoded.width as u16,
        decoded.height as u16,
        PixelMetadata::new(ImageAlphaType::Alpha, true),
    ))
}

fn affine(matrix: Matrix) -> Affine {
    Affine::new([matrix.a, matrix.b, matrix.c, matrix.d, matrix.e, matrix.f])
}

fn color(color: Rgba) -> AlphaColor<Srgb> {
    let [r, g, b, a] = color.to_rgba8();
    AlphaColor::from_rgba8(r, g, b, a)
}

fn rule(rule: FillRule) -> Fill {
    match rule {
        FillRule::NonZero => Fill::NonZero,
        FillRule::EvenOdd => Fill::EvenOdd,
    }
}

fn kurbo_stroke(style: &StrokeStyle) -> Stroke {
    let mut stroke = Stroke::new(style.width);
    stroke.miter_limit = style.miter_limit;
    stroke.start_cap = match style.cap {
        LineCap::Butt => vello_cpu::kurbo::Cap::Butt,
        LineCap::Round => vello_cpu::kurbo::Cap::Round,
        LineCap::Square => vello_cpu::kurbo::Cap::Square,
    };
    stroke.end_cap = stroke.start_cap;
    stroke.join = match style.join {
        LineJoin::Miter => vello_cpu::kurbo::Join::Miter,
        LineJoin::Round => vello_cpu::kurbo::Join::Round,
        LineJoin::Bevel => vello_cpu::kurbo::Join::Bevel,
    };
    if !style.dash.is_empty() {
        stroke.dash_pattern = style.dash.iter().copied().collect();
        stroke.dash_offset = style.dash_offset;
    }
    stroke
}

fn brush(paint: &Paint) -> PaintType {
    match paint {
        Paint::Color(value) => Brush::Solid(color(*value)),
        Paint::LinearGradient(gradient) => {
            let mut value = Gradient::new_linear(
                Point::new(gradient.start.x, gradient.start.y),
                Point::new(gradient.end.x, gradient.end.y),
            );
            value.extend = extend(gradient.extend);
            value.stops = stops(&gradient.stops);
            Brush::Gradient(value)
        }
        Paint::RadialGradient(radial) => Brush::Gradient(gradient_radial(radial)),
        _ => Brush::Solid(color(Rgba::BLACK)),
    }
}

fn gradient_radial(radial: &RadialGradient) -> Gradient {
    gradient_radial_transformed(radial, Matrix::IDENTITY)
}

fn gradient_radial_transformed(radial: &RadialGradient, transform: Matrix) -> Gradient {
    let start = transform.apply(radial.start_center);
    let end = transform.apply(radial.end_center);
    let mut value = Gradient::new_two_point_radial(
        Point::new(start.x, start.y),
        radial.start_radius as f32,
        Point::new(end.x, end.y),
        radial.end_radius as f32,
    );
    value.extend = extend(radial.extend);
    value.stops = stops(&radial.stops);
    value
}

/// Circle-to-ellipse transform: rotate by the major-axis angle and compress the minor axis by `sqrt(1 - e²)`.
fn ellipse_transform(radial: &RadialGradient) -> Option<Matrix> {
    let eccentricity = radial.eccentricity;
    if !(eccentricity.is_finite() && eccentricity > 1e-6) {
        return None;
    }
    let ratio = (1.0 - eccentricity.clamp(0.0, 0.999).powi(2))
        .max(1e-4)
        .sqrt();
    let (sin, cos) = radial.angle.to_radians().sin_cos();
    let rotation = Matrix::new(cos, sin, -sin, cos, 0.0, 0.0);
    Some(Matrix::scale(1.0, ratio).then(rotation))
}

fn stops(stops: &[GradientStop]) -> vello_cpu::peniko::ColorStops {
    let mut value = vello_cpu::peniko::ColorStops::new();
    for stop in stops {
        value.push(ColorStop {
            offset: stop.offset as f32,
            color: DynamicColor::from_alpha_color(color(stop.color)),
        });
    }
    value
}

fn extend(extend: GradientExtend) -> Extend {
    match extend {
        GradientExtend::Pad => Extend::Pad,
        GradientExtend::Repeat => Extend::Repeat,
        GradientExtend::Reflect => Extend::Reflect,
    }
}

fn to_bez_path(path: &Path) -> BezPath {
    let mut output = BezPath::new();
    let mut current = Point::ZERO;
    let mut subpath_start = Point::ZERO;
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(point) => {
                current = Point::new(point.x, point.y);
                subpath_start = current;
                output.move_to(current);
            }
            PathSegment::LineTo(point) => {
                current = Point::new(point.x, point.y);
                output.line_to(current);
            }
            PathSegment::QuadTo(control, to) => {
                output.quad_to(Point::new(control.x, control.y), Point::new(to.x, to.y));
                current = Point::new(to.x, to.y);
            }
            PathSegment::CubicTo(control1, control2, to) => {
                output.curve_to(
                    Point::new(control1.x, control1.y),
                    Point::new(control2.x, control2.y),
                    Point::new(to.x, to.y),
                );
                current = Point::new(to.x, to.y);
            }
            PathSegment::ArcTo {
                rx,
                ry,
                rotation,
                large_arc,
                sweep,
                to,
            } => {
                let end = Point::new(to.x, to.y);
                let arc = SvgArc {
                    from: current,
                    to: end,
                    radii: Vec2::new(*rx, *ry),
                    x_rotation: *rotation,
                    large_arc: *large_arc,
                    sweep: *sweep,
                };
                if let Some(arc) = KurboArc::from_svg_arc(&arc) {
                    arc.to_cubic_beziers(0.1, |p1, p2, p3| {
                        output.curve_to(p1, p2, p3);
                    });
                } else {
                    output.line_to(end);
                }
                current = end;
            }
            PathSegment::Close => {
                output.close_path();
                current = subpath_start;
            }
            _ => {}
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{CpuRenderer, PixelRect, RasterTarget, image_tier};
    use crate::options::RenderOptions;
    use ofdbox_folio_ir::{
        DiagnosticCode, Element, FillRule, Group, ImageAsset, ImageElement, ImageFormat, Matrix,
        Paint, Path, PathElement, Point, Rect, ResourceStore, Rgba, Scene, Size,
    };
    use std::sync::Arc;

    /// Generates a solid-color PNG.
    fn png_bytes(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("png header");
            writer
                .write_image_data(&rgba.repeat((width * height) as usize))
                .expect("png data");
        }
        bytes
    }

    fn scene_with_image(image: ImageAsset) -> Scene {
        scene_with_shared_image(Arc::new(image))
    }

    fn scene_with_shared_image(image: Arc<ImageAsset>) -> Scene {
        let width = f64::from(image.pixel_width.max(1));
        let height = f64::from(image.pixel_height.max(1));
        Scene {
            size: Size::new(10.0, 10.0),
            root: Group {
                children: vec![Element::Image(ImageElement {
                    image,
                    rect: Rect::default(),
                    transform: Matrix::scale(1.0 / width, 1.0 / height),
                    alpha: 1.0,
                })],
                ..Group::default()
            },
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn renders_mask_of_different_size() {
        // A 25×25 image with a 75×75 mask: after resampling it still renders normally and produces no diagnostic.
        let image = ImageAsset::new(
            ImageFormat::Png,
            Arc::from(png_bytes(4, 4, [200, 30, 30, 255])),
            4,
            4,
        );
        let mask = ImageAsset::new(
            ImageFormat::Png,
            Arc::from(png_bytes(12, 12, [0, 0, 0, 255])),
            12,
            12,
        );
        let scene = scene_with_image(image.with_mask(Arc::new(mask)));
        let mut target = RasterTarget::from_size(scene.size, 96.0).expect("target");
        let mut renderer = CpuRenderer::new(RenderOptions::default());
        renderer.render_scene(&scene, &mut target).expect("render");
        assert!(renderer.report().is_empty());
        // The mask is fully black, so the image is completely hidden and only the white page background remains.
        assert_eq!(&target.data()[..4], [255, 255, 255, 255]);
    }

    #[test]
    fn degrades_when_mask_is_undecodable() {
        let image = ImageAsset::new(
            ImageFormat::Png,
            Arc::from(png_bytes(4, 4, [200, 30, 30, 255])),
            4,
            4,
        );
        let mask = ImageAsset::new(ImageFormat::Png, Arc::from(&b"not-a-png"[..]), 4, 4);
        let scene = scene_with_image(image.with_mask(Arc::new(mask)));
        let mut target = RasterTarget::from_size(scene.size, 96.0).expect("target");
        let mut renderer = CpuRenderer::new(RenderOptions::default());
        renderer.render_scene(&scene, &mut target).expect("render");
        assert_eq!(renderer.report().diagnostics.len(), 1);
        assert_eq!(renderer.report().diagnostics[0].code, DiagnosticCode::Image);
        // A missing mask is only a degradation, the image itself is still drawn.
        assert_eq!(&target.data()[..4], [200, 30, 30, 255]);
    }

    #[test]
    fn skips_undecodable_image_without_failing_page() {
        let image = ImageAsset::new(ImageFormat::Png, Arc::from(&b"broken"[..]), 4, 4);
        let scene = scene_with_image(image);
        let mut target = RasterTarget::from_size(scene.size, 96.0).expect("target");
        let mut renderer = CpuRenderer::new(RenderOptions::default());
        renderer.render_scene(&scene, &mut target).expect("render");
        assert_eq!(renderer.report().diagnostics.len(), 1);
        // The whole page is still white; a single failed image does not raise an error.
        assert_eq!(&target.data()[..4], [255, 255, 255, 255]);
    }

    #[test]
    fn evicts_pixmaps_over_the_local_budget() {
        let cache = crate::cache_budget::SharedByteCache::new(2 * 2 * 4 * 2, None);
        crate::cache_budget::SharedByteCache::insert(
            &cache,
            (1, 64),
            Arc::new(vello_cpu::Pixmap::new(2, 2)),
            2 * 2 * 4,
        );
        crate::cache_budget::SharedByteCache::insert(
            &cache,
            (2, 64),
            Arc::new(vello_cpu::Pixmap::new(2, 2)),
            2 * 2 * 4,
        );
        assert!(crate::cache_budget::SharedByteCache::get(&cache, &(1, 64)).is_some());
        crate::cache_budget::SharedByteCache::insert(
            &cache,
            (3, 64),
            Arc::new(vello_cpu::Pixmap::new(2, 2)),
            2 * 2 * 4,
        );
        assert!(
            crate::cache_budget::SharedByteCache::get(&cache, &(2, 64)).is_none(),
            "the least recently used entry should be evicted"
        );
        assert!(crate::cache_budget::SharedByteCache::get(&cache, &(1, 64)).is_some());
        assert!(crate::cache_budget::SharedByteCache::get(&cache, &(3, 64)).is_some());
    }

    #[test]
    fn skips_cache_when_the_local_budget_is_zero() {
        let cache = crate::cache_budget::SharedByteCache::new(0, None);
        crate::cache_budget::SharedByteCache::insert(
            &cache,
            (1, 64),
            Arc::new(vello_cpu::Pixmap::new(2, 2)),
            2 * 2 * 4,
        );
        assert!(crate::cache_budget::SharedByteCache::get(&cache, &(1, 64)).is_none());
    }

    #[cfg(feature = "tiny-skia")]
    #[test]
    fn shared_budget_evicts_across_cpu_and_tiny_skia() {
        use crate::CacheBudget;
        use crate::TinySkiaRenderer;

        let mut store = ResourceStore::new();
        let first_id = store.add_image(ImageAsset::new(
            ImageFormat::Png,
            Arc::from(png_bytes(4, 4, [200, 30, 30, 255])),
            4,
            4,
        ));
        let second_id = store.add_image(ImageAsset::new(
            ImageFormat::Png,
            Arc::from(png_bytes(4, 4, [30, 200, 30, 255])),
            4,
            4,
        ));
        let third_id = store.add_image(ImageAsset::new(
            ImageFormat::Png,
            Arc::from(png_bytes(4, 4, [30, 30, 200, 255])),
            4,
            4,
        ));
        let first = Arc::clone(store.image(first_id).expect("first image"));
        let second = Arc::clone(store.image(second_id).expect("second image"));
        let third = Arc::clone(store.image(third_id).expect("third image"));

        // Probe the decoded byte size of one image through the CPU path.
        let mut target = RasterTarget::from_size(Size::new(10.0, 10.0), 96.0).expect("target");
        let mut probe = CpuRenderer::with_defaults();
        probe
            .render_scene(&scene_with_shared_image(Arc::clone(&first)), &mut target)
            .expect("probe render");
        let image_bytes = probe.cache_stats().image_bytes;
        assert!(image_bytes > 0);

        let budget = CacheBudget::new(image_bytes * 2);
        let options = RenderOptions {
            max_image_cache_bytes: 1 << 20,
            ..RenderOptions::default()
        };
        let mut cpu = CpuRenderer::with_shared_budget(options, budget.clone());
        let mut skia = TinySkiaRenderer::with_shared_budget(options, budget.clone());

        cpu.render_scene(&scene_with_shared_image(Arc::clone(&first)), &mut target)
            .expect("cpu render");
        assert_eq!(budget.used(), image_bytes);
        skia.render_scene(&scene_with_shared_image(Arc::clone(&second)), &mut target)
            .expect("skia render");
        assert_eq!(budget.used(), image_bytes * 2);
        assert_eq!(cpu.cache_stats().images, 1);
        assert_eq!(skia.cache_stats().images, 1);

        // A third image exceeds the combined limit and evicts the oldest charge,
        // which belongs to the CPU renderer.
        cpu.render_scene(&scene_with_shared_image(Arc::clone(&third)), &mut target)
            .expect("cpu third render");
        assert_eq!(budget.used(), image_bytes * 2);
        assert_eq!(cpu.cache_stats().images, 1);
        assert_eq!(skia.cache_stats().images, 1);

        // Rendering the first image again evicts the tiny-skia entry, proving
        // that eviction crosses backends in both directions.
        cpu.render_scene(&scene_with_shared_image(Arc::clone(&first)), &mut target)
            .expect("cpu first rerender");
        assert_eq!(budget.used(), image_bytes * 2);
        assert_eq!(skia.cache_stats().images, 0);
    }

    fn solid_rect_scene() -> Scene {
        let mut path = Path::new();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(10.0, 0.0));
        path.line_to(Point::new(10.0, 10.0));
        path.line_to(Point::new(0.0, 10.0));
        path.close();
        Scene {
            size: Size::new(10.0, 10.0),
            root: Group {
                children: vec![Element::Path(PathElement {
                    path: Arc::new(path),
                    transform: Matrix::IDENTITY,
                    fill: Some(Paint::Color(Rgba::from_rgb(255, 0, 0))),
                    stroke: None,
                    rule: FillRule::NonZero,
                })],
                ..Group::default()
            },
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn renders_region_at_origin_offset() {
        use super::RenderRegion;

        // A full-page 96 DPI render (10mm → 38px) matches the region (origin 8,8, 10×10) pixel for pixel.
        let scene = solid_rect_scene();
        let mut renderer = CpuRenderer::new(RenderOptions::default());
        let mut full = RasterTarget::from_size(scene.size, 96.0).expect("full target");
        renderer
            .render_scene(&scene, &mut full)
            .expect("full render");

        let mut tile = RasterTarget::from_pixels(10, 10, 96.0).expect("tile");
        renderer
            .render_scene_region(&scene, &mut tile, RenderRegion::new((8.0, 8.0), 96.0))
            .expect("region render");

        let full_width = usize::from(full.width());
        for y in 0..10usize {
            for x in 0..10usize {
                let fi = ((y + 8) * full_width + (x + 8)) * 4;
                let ti = (y * 10 + x) * 4;
                assert_eq!(
                    &full.data()[fi..fi + 4],
                    &tile.data()[ti..ti + 4],
                    "pixel ({x},{y}) mismatch"
                );
            }
        }
    }

    #[test]
    fn region_outside_page_is_background() {
        use super::RenderRegion;

        let scene = solid_rect_scene();
        let mut renderer = CpuRenderer::new(RenderOptions::default());
        // The region outside the page's right edge contains only the white background.
        let mut tile = RasterTarget::from_pixels(8, 8, 96.0).expect("tile");
        renderer
            .render_scene_region(&scene, &mut tile, RenderRegion::new((60.0, 60.0), 96.0))
            .expect("region render");
        assert_eq!(&tile.data()[..4], [255, 255, 255, 255]);
    }

    #[test]
    fn union_crop_matches_direct_region_render() {
        use super::RenderRegion;

        let mut path = Path::new();
        path.move_to(Point::new(2.0, 3.0));
        path.line_to(Point::new(18.0, 3.0));
        path.line_to(Point::new(18.0, 17.0));
        path.line_to(Point::new(2.0, 17.0));
        path.close();
        let scene = Scene {
            size: Size::new(20.0, 20.0),
            root: Group {
                children: vec![Element::Path(PathElement {
                    path: Arc::new(path),
                    transform: Matrix::IDENTITY,
                    fill: Some(Paint::Color(Rgba::from_rgb(20, 120, 220))),
                    stroke: None,
                    rule: FillRule::NonZero,
                })],
                ..Group::default()
            },
            diagnostics: Vec::new(),
        };
        let mut renderer = CpuRenderer::new(RenderOptions::default());
        let union = renderer
            .render_union(&scene, 96.0, PixelRect::new(5, 5, 50, 50))
            .expect("union render");

        let mut direct = RasterTarget::from_pixels(20, 15, 96.0).expect("direct");
        renderer
            .render_scene_region(&scene, &mut direct, RenderRegion::new((20.0, 25.0), 96.0))
            .expect("direct region");

        let cropped = union.crop(PixelRect::new(15, 20, 20, 15)).expect("crop");
        assert_eq!(cropped.data(), direct.data());
        assert!(matches!(
            union.crop(PixelRect::new(49, 5, 2, 2)),
            Err(crate::RenderError::InvalidRegion)
        ));
    }

    #[test]
    fn compacted_disjoint_paths_render_identically() {
        let mut first_path = Path::new();
        first_path.move_to(Point::new(0.0, 0.0));
        first_path.line_to(Point::new(4.0, 0.0));
        first_path.line_to(Point::new(4.0, 4.0));
        first_path.line_to(Point::new(0.0, 4.0));
        first_path.close();
        let mut second_path = Path::new();
        second_path.move_to(Point::new(10.0, 0.0));
        second_path.line_to(Point::new(14.0, 0.0));
        second_path.line_to(Point::new(14.0, 4.0));
        second_path.line_to(Point::new(10.0, 4.0));
        second_path.close();
        let scene = Scene {
            size: Size::new(20.0, 20.0),
            root: Group {
                children: vec![
                    Element::Path(PathElement {
                        path: Arc::new(first_path),
                        transform: Matrix::IDENTITY,
                        fill: Some(Paint::Color(Rgba::from_rgb(20, 120, 220))),
                        stroke: None,
                        rule: FillRule::NonZero,
                    }),
                    Element::Path(PathElement {
                        path: Arc::new(second_path),
                        transform: Matrix::IDENTITY,
                        fill: Some(Paint::Color(Rgba::from_rgb(20, 120, 220))),
                        stroke: None,
                        rule: FillRule::NonZero,
                    }),
                ],
                ..Group::default()
            },
            diagnostics: Vec::new(),
        };
        let mut renderer = CpuRenderer::new(RenderOptions::default());
        let mut before = RasterTarget::from_size(scene.size, 96.0).expect("before");
        renderer
            .render_scene(&scene, &mut before)
            .expect("render before");
        let (compacted, stats) = scene.compact();
        assert_eq!(stats.after, 1);
        let mut after = RasterTarget::from_size(scene.size, 96.0).expect("after");
        renderer
            .render_scene(&compacted, &mut after)
            .expect("render after");
        assert_eq!(before.data(), after.data());
    }

    #[test]
    fn compacted_nested_transforms_render_identically() {
        let mut path = Path::new();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(5.0, 0.0));
        path.line_to(Point::new(5.0, 5.0));
        path.line_to(Point::new(0.0, 5.0));
        path.close();
        let scene = Scene {
            size: Size::new(40.0, 40.0),
            root: Group {
                children: vec![Element::Group(Group {
                    transform: Matrix::translate(10.0, 10.0),
                    children: vec![Element::Group(Group {
                        transform: Matrix::scale(2.0, 2.0),
                        children: vec![Element::Path(PathElement {
                            path: Arc::new(path),
                            transform: Matrix::IDENTITY,
                            fill: Some(Paint::Color(Rgba::from_rgb(20, 120, 220))),
                            stroke: None,
                            rule: FillRule::NonZero,
                        })],
                        ..Group::default()
                    })],
                    ..Group::default()
                })],
                ..Group::default()
            },
            diagnostics: Vec::new(),
        };
        let mut renderer = CpuRenderer::new(RenderOptions::default());
        let mut before = RasterTarget::from_size(scene.size, 96.0).expect("before");
        renderer
            .render_scene(&scene, &mut before)
            .expect("render before");
        let (compacted, _stats) = scene.compact();
        let mut after = RasterTarget::from_size(scene.size, 96.0).expect("after");
        renderer
            .render_scene(&compacted, &mut after)
            .expect("render after");
        assert_eq!(before.data(), after.data());
    }

    #[test]
    fn renders_solid_path_pixels() {
        let mut path = Path::new();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(10.0, 0.0));
        path.line_to(Point::new(10.0, 10.0));
        path.line_to(Point::new(0.0, 10.0));
        path.close();
        let scene = Scene {
            size: Size::new(10.0, 10.0),
            root: Group {
                children: vec![Element::Path(PathElement {
                    path: Arc::new(path),
                    transform: Matrix::IDENTITY,
                    fill: Some(Paint::Color(Rgba::from_rgb(255, 0, 0))),
                    stroke: None,
                    rule: FillRule::NonZero,
                })],
                ..Group::default()
            },
            diagnostics: Vec::new(),
        };
        let mut target = RasterTarget::from_size(Size::new(10.0, 10.0), 96.0).expect("target");
        let mut renderer = CpuRenderer::new(RenderOptions::default());
        renderer.render_scene(&scene, &mut target).expect("render");
        assert_eq!(target.data()[..4], [255, 0, 0, 255]);
        assert_eq!(target.width(), 38);
        assert_eq!(target.height(), 38);
    }

    #[test]
    fn image_tier_follows_the_rendered_size_for_unit_square_images() {
        let store_image = Arc::new(ImageAsset::new(
            ImageFormat::Png,
            Arc::from(&[][..]),
            4000,
            5000,
        ));
        let element = ImageElement {
            image: store_image,
            rect: Rect::default(),
            transform: Matrix::scale(0.1, 0.1),
            alpha: 1.0,
        };
        // 5000 * 0.1 = 500 device pixels -> the next power-of-two tier.
        assert_eq!(image_tier(&element, Matrix::scale(0.1, 0.1)), 512);
    }
}
