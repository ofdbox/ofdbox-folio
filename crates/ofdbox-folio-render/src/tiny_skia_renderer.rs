//! Software renderer built on `tiny-skia`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use ofdbox_folio_ir::{
    Diagnostic, DiagnosticCode, FillRule, FontAsset, GradientExtend, ImageAsset, ImageElement,
    LineCap, LineJoin, Matrix, Page, Paint, Path, PathElement, PathSegment, Point, Rgba, Scene,
    StrokeStyle, TextElement,
};
use tiny_skia::{
    Color, FillRule as SkFillRule, IntSize, LinearGradient, Mask, Paint as SkPaint, PathBuilder,
    Pixmap, PixmapPaint, Point as SkPoint, RadialGradient, Shader, SpreadMode, Stroke, Transform,
};

use crate::error::RenderError;
use crate::image::{decode_image, decode_image_tier};
use crate::options::RenderOptions;
use crate::prepared::{PreparedClip, PreparedCommand, PreparedScene, scene_fingerprint};
use crate::renderer::{PixelRect, RasterTarget, RenderCacheStats, RenderRegion};

/// A software renderer that rasterizes Fixed IR with `tiny-skia`.
pub struct TinySkiaRenderer {
    options: RenderOptions,
    report: Report,
    glyphs: HashMap<(usize, u16), Arc<tiny_skia::Path>>,
    images: HashMap<(u64, u32), Arc<Pixmap>>,
    image_order: VecDeque<(u64, u32)>,
    image_bytes: usize,
    prepared: Option<(usize, u64, Arc<PreparedScene>)>,
}

#[derive(Default)]
struct Report {
    diagnostics: Vec<Diagnostic>,
}

impl TinySkiaRenderer {
    /// Creates a renderer with the given options.
    pub fn new(options: RenderOptions) -> Self {
        Self {
            options,
            report: Report::default(),
            glyphs: HashMap::new(),
            images: HashMap::new(),
            image_order: VecDeque::new(),
            image_bytes: 0,
            prepared: None,
        }
    }

    /// Creates a renderer with default options.
    pub fn with_defaults() -> Self {
        Self::new(RenderOptions::default())
    }

    /// Takes the diagnostics collected by the last render.
    pub fn take_report(&mut self) -> ofdbox_folio_ir::Report {
        let diagnostics = std::mem::take(&mut self.report.diagnostics);
        ofdbox_folio_ir::Report { diagnostics }
    }

    /// Current cache occupancy.
    pub fn cache_stats(&self) -> RenderCacheStats {
        RenderCacheStats {
            fonts: 0,
            glyphs: self.glyphs.len(),
            images: self.images.len(),
            image_bytes: self.image_bytes as u64,
            prepared_scene: self.prepared.is_some(),
        }
    }

    /// Drops cached glyph outlines and decoded images.
    pub fn clear_caches(&mut self) {
        self.prepared = None;
        self.glyphs.clear();
        self.images.clear();
        self.image_order.clear();
        self.image_bytes = 0;
    }

    /// Renders one Fixed IR page into a raster target.
    pub fn render_page(
        &mut self,
        page: &Page,
        target: &mut RasterTarget,
    ) -> Result<(), RenderError> {
        self.render_scene(&page.content, target)
    }

    /// Renders a scene into a raster target.
    pub fn render_scene(
        &mut self,
        scene: &Scene,
        target: &mut RasterTarget,
    ) -> Result<(), RenderError> {
        let scale = target.dpi() / 25.4;
        self.render_scene_with(scene, target, Matrix::scale(scale, scale))
    }

    /// Renders a union rectangle of a scene once, for later cropping.
    pub fn render_union(
        &mut self,
        scene: &Scene,
        dpi: f64,
        rect: PixelRect,
    ) -> Result<RasterTarget, RenderError> {
        if !(dpi.is_finite() && dpi > 0.0) || rect.is_empty() {
            return Err(RenderError::InvalidRegion);
        }
        let mut target = RasterTarget::from_pixels(rect.width, rect.height, dpi)?;
        self.render_scene_region(
            scene,
            &mut target,
            RenderRegion::new((f64::from(rect.x), f64::from(rect.y)), dpi),
        )?;
        Ok(target)
    }

    /// Renders the given region of a scene into the target.
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
        self.render_prepared_with(&prepared, target, device)
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
        let scale = target.dpi() / 25.4;
        self.render_prepared_with(prepared, target, Matrix::scale(scale, scale))
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
        let k = region.dpi / 25.4;
        let device =
            Matrix::scale(k, k).then(Matrix::translate(-region.origin_px.0, -region.origin_px.1));
        self.render_prepared_with(prepared, target, device)
    }

    fn render_prepared_with(
        &mut self,
        prepared: &PreparedScene,
        target: &mut RasterTarget,
        device: Matrix,
    ) -> Result<(), RenderError> {
        let width = u32::from(target.width());
        let height = u32::from(target.height());
        let mut frames = vec![Pixmap::new(width, height).ok_or(RenderError::InvalidPage)?];
        fill_background(
            frames.last_mut().expect("base frame"),
            self.options.background,
        );
        let mut layers: Vec<(Option<PreparedClip>, f32)> = Vec::new();
        for command in &prepared.commands {
            match command {
                PreparedCommand::PushLayer { clip, opacity } => {
                    frames.push(Pixmap::new(width, height).ok_or(RenderError::InvalidPage)?);
                    layers.push((clip.clone(), *opacity));
                }
                PreparedCommand::PopLayer => {
                    let Some(layer) = frames.pop() else {
                        continue;
                    };
                    let Some((clip, opacity)) = layers.pop() else {
                        continue;
                    };
                    let Some(parent) = frames.last_mut() else {
                        continue;
                    };
                    let mask = clip.as_ref().map(|clip| {
                        let mut mask = Mask::new(width, height)
                            .expect("mask dimensions are checked by the target");
                        if let Some(path) = to_skia_path(&clip.path) {
                            mask.fill_path(
                                &path,
                                match clip.rule {
                                    FillRule::NonZero => SkFillRule::Winding,
                                    FillRule::EvenOdd => SkFillRule::EvenOdd,
                                },
                                true,
                                to_skia_transform(clip.transform.then(device)),
                            );
                        }
                        mask
                    });
                    parent.draw_pixmap(
                        0,
                        0,
                        layer.as_ref(),
                        &PixmapPaint {
                            opacity,
                            ..PixmapPaint::default()
                        },
                        Transform::identity(),
                        mask.as_ref(),
                    );
                }
                PreparedCommand::FillPath {
                    path,
                    transform,
                    paint,
                    rule,
                } => {
                    let element = PathElement {
                        path: Arc::clone(path),
                        transform: (*transform).then(device),
                        fill: Some(paint.clone()),
                        stroke: None,
                        rule: *rule,
                    };
                    self.draw_path(
                        frames.last_mut().expect("current frame"),
                        &element,
                        Matrix::IDENTITY,
                        None,
                    );
                }
                PreparedCommand::StrokePath {
                    path,
                    transform,
                    stroke,
                } => {
                    let element = PathElement {
                        path: Arc::clone(path),
                        transform: (*transform).then(device),
                        fill: None,
                        stroke: Some(stroke.clone()),
                        rule: FillRule::NonZero,
                    };
                    self.draw_path(
                        frames.last_mut().expect("current frame"),
                        &element,
                        Matrix::IDENTITY,
                        None,
                    );
                }
                PreparedCommand::DrawImage {
                    image,
                    rect,
                    transform,
                    alpha,
                } => {
                    let element = ImageElement {
                        image: Arc::clone(image),
                        rect: *rect,
                        transform: (*transform).then(device),
                        alpha: *alpha,
                    };
                    self.draw_image(
                        frames.last_mut().expect("current frame"),
                        &element,
                        Matrix::IDENTITY,
                        None,
                    );
                }
                PreparedCommand::DrawText {
                    font,
                    font_size,
                    transform,
                    run,
                    fill,
                    stroke,
                } => {
                    let element = TextElement {
                        font: Arc::clone(font),
                        font_size: *font_size,
                        transform: (*transform).then(device),
                        run: Arc::clone(run),
                        fill: fill.clone(),
                        stroke: stroke.clone(),
                    };
                    self.draw_text(
                        frames.last_mut().expect("current frame"),
                        &element,
                        Matrix::IDENTITY,
                        None,
                    );
                }
            }
        }
        let data = frames[0].data();
        let out = target.data_mut();
        for (target_pixel, source) in out
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(data.as_chunks::<4>().0)
        {
            let alpha = source[3];
            if alpha == 0 {
                target_pixel.copy_from_slice(&[0, 0, 0, 0]);
            } else if alpha == 255 {
                target_pixel.copy_from_slice(source);
            } else {
                let unpremultiply = |value: u8| {
                    u8::try_from((u32::from(value) * 255 + u32::from(alpha) / 2) / u32::from(alpha))
                        .unwrap_or(255)
                };
                target_pixel.copy_from_slice(&[
                    unpremultiply(source[0]),
                    unpremultiply(source[1]),
                    unpremultiply(source[2]),
                    alpha,
                ]);
            }
        }
        Ok(())
    }

    fn draw_path(
        &mut self,
        pixmap: &mut Pixmap,
        element: &PathElement,
        parent: Matrix,
        mask: Option<&Mask>,
    ) {
        let Some(path) = to_skia_path(&element.path) else {
            return;
        };
        let transform = element.transform.then(parent);
        let device = to_skia_transform(transform);
        if let Some(fill) = &element.fill {
            let mut paint = self.skia_paint(fill);
            paint.anti_alias = true;
            pixmap.fill_path(
                &path,
                &paint,
                match element.rule {
                    FillRule::NonZero => SkFillRule::Winding,
                    FillRule::EvenOdd => SkFillRule::EvenOdd,
                },
                device,
                mask,
            );
        }
        if let Some(stroke) = &element.stroke {
            let mut paint = self.skia_paint(&stroke.paint);
            paint.anti_alias = true;
            let width = stroke.style.width as f32;
            if width > 0.0 {
                let stroke = to_skia_stroke(&stroke.style, width);
                pixmap.stroke_path(&path, &paint, &stroke, device, mask);
            }
        }
    }

    fn draw_text(
        &mut self,
        pixmap: &mut Pixmap,
        element: &TextElement,
        parent: Matrix,
        mask: Option<&Mask>,
    ) {
        let Some(fill) = &element.fill else {
            return;
        };
        let Ok(face) = ttf_parser::Face::parse(element.font.data(), element.font.face_index())
        else {
            return;
        };
        let units = f64::from(face.units_per_em());
        if units == 0.0 {
            return;
        }
        let glyph_scale = element.font_size / units;
        let text_transform = element.transform.then(parent);
        #[cfg(debug_assertions)]
        if element.run.text == "F" {
            eprintln!(
                "BODY F size={} transform={:?} glyphs={:?}",
                element.font_size, text_transform, element.run.glyphs
            );
        }
        let mut draws = Vec::new();
        for glyph in &element.run.glyphs {
            let Ok(glyph_id) = u16::try_from(glyph.id) else {
                continue;
            };
            let Some(glyph_path) = self.glyph_path(&element.font, &face, glyph_id) else {
                continue;
            };
            let transform = Matrix::scale(glyph_scale, -glyph_scale)
                .then(Matrix::translate(glyph.x, glyph.y))
                .then(text_transform);
            draws.push((glyph_path, transform));
        }
        let mut paint = self.skia_paint(fill);
        paint.anti_alias = true;
        for (glyph_path, transform) in draws {
            pixmap.fill_path(
                &glyph_path,
                &paint,
                SkFillRule::Winding,
                to_skia_transform(transform),
                mask,
            );
        }
    }

    fn draw_image(
        &mut self,
        pixmap: &mut Pixmap,
        element: &ImageElement,
        parent: Matrix,
        mask: Option<&Mask>,
    ) {
        let tier = Self::image_tier(element, element.transform.then(parent));
        let Some(image) = self.image(&element.image, tier) else {
            return;
        };
        let width = if element.rect.width.is_finite() && element.rect.width > 0.0 {
            element.rect.width
        } else {
            f64::from(image.width())
        };
        let height = if element.rect.height.is_finite() && element.rect.height > 0.0 {
            element.rect.height
        } else {
            f64::from(image.height())
        };
        let transform = Matrix::scale(
            width / f64::from(image.width()),
            height / f64::from(image.height()),
        )
        .then(Matrix::translate(element.rect.x, element.rect.y))
        .then(element.transform)
        .then(parent);
        pixmap.draw_pixmap(
            0,
            0,
            image.as_ref().as_ref(),
            &PixmapPaint {
                opacity: element.alpha.clamp(0.0, 1.0),
                ..PixmapPaint::default()
            },
            to_skia_transform(transform),
            mask,
        );
    }

    fn glyph_path(
        &mut self,
        font: &Arc<FontAsset>,
        face: &ttf_parser::Face<'_>,
        glyph_id: u16,
    ) -> Option<Arc<tiny_skia::Path>> {
        let key = (Arc::as_ptr(font) as usize, glyph_id);
        if let Some(path) = self.glyphs.get(&key) {
            return Some(Arc::clone(path));
        }
        let mut builder = GlyphBuilder::default();
        face.outline_glyph(ttf_parser::GlyphId(glyph_id), &mut builder)?;
        let path = Arc::new(builder.builder.finish()?);
        self.glyphs.insert(key, Arc::clone(&path));
        Some(path)
    }

    fn image(&mut self, asset: &Arc<ImageAsset>, max_dim: u32) -> Option<Arc<Pixmap>> {
        let key = (asset.id().get(), max_dim);
        if let Some(image) = self.images.get(&key) {
            return Some(Arc::clone(image));
        }
        let mut decoded = decode_image_tier(
            asset.format,
            &asset.encoded,
            self.options.max_image_pixels,
            max_dim,
        )
        .map_err(|error| {
            self.report.diagnostics.push(Diagnostic::warning(
                DiagnosticCode::Image,
                error.to_string(),
            ));
        })
        .ok()?;
        if let Some(mask) = &asset.mask
            && let Ok(mask) =
                decode_image(mask.format, &mask.encoded, self.options.max_image_pixels)
        {
            let _ = decoded.apply_mask(&mask);
        }
        let size = IntSize::from_wh(decoded.width, decoded.height)?;
        let mut data = decoded.rgba;
        for pixel in data.as_chunks_mut::<4>().0 {
            let alpha = u32::from(pixel[3]);
            for channel in &mut pixel[..3] {
                *channel = u8::try_from((u32::from(*channel) * alpha + 127) / 255).unwrap_or(255);
            }
        }
        let pixmap = Pixmap::from_vec(data, size)?;
        let bytes = pixmap.data().len();
        let image = Arc::new(pixmap);
        if self.options.max_image_cache_bytes > 0 {
            self.images.insert(key, Arc::clone(&image));
            self.image_order.push_back(key);
            self.image_bytes += bytes;
            while self.image_bytes > self.options.max_image_cache_bytes as usize {
                let Some(oldest) = self.image_order.pop_front() else {
                    break;
                };
                if let Some(old) = self.images.remove(&oldest) {
                    self.image_bytes = self.image_bytes.saturating_sub(old.data().len());
                }
            }
        }
        Some(image)
    }

    fn image_tier(element: &ImageElement, transform: Matrix) -> u32 {
        if !(element.rect.width.is_finite()
            && element.rect.height.is_finite()
            && element.rect.width > 0.0
            && element.rect.height > 0.0)
        {
            return 0;
        }
        let scale = (transform.a * transform.d - transform.b * transform.c)
            .abs()
            .sqrt();
        if !(scale.is_finite() && scale > 0.0) {
            return 0;
        }
        let pixels = element.rect.width.max(element.rect.height) * scale;
        if !(pixels.is_finite() && pixels >= 1.0) {
            return 0;
        }
        let mut tier = 64_u32;
        while f64::from(tier) < pixels && tier < 4096 {
            tier *= 2;
        }
        tier
    }

    fn skia_paint(&self, paint: &Paint) -> SkPaint<'_> {
        let shader = match paint {
            Paint::Color(color) => Shader::SolidColor(to_skia_color(*color)),
            Paint::LinearGradient(gradient) => {
                let stops = to_skia_stops(&gradient.stops);
                LinearGradient::new(
                    SkPoint::from_xy(gradient.start.x as f32, gradient.start.y as f32),
                    SkPoint::from_xy(gradient.end.x as f32, gradient.end.y as f32),
                    stops,
                    to_spread(gradient.extend),
                    Transform::identity(),
                )
                .unwrap_or(Shader::SolidColor(Color::BLACK))
            }
            Paint::RadialGradient(gradient) => {
                let stops = to_skia_stops(&gradient.stops);
                RadialGradient::new(
                    SkPoint::from_xy(
                        gradient.start_center.x as f32,
                        gradient.start_center.y as f32,
                    ),
                    SkPoint::from_xy(gradient.end_center.x as f32, gradient.end_center.y as f32),
                    gradient.start_radius.max(0.0001) as f32,
                    stops,
                    to_spread(gradient.extend),
                    Transform::identity(),
                )
                .unwrap_or(Shader::SolidColor(Color::BLACK))
            }
            _ => Shader::SolidColor(Color::BLACK),
        };
        SkPaint {
            shader,
            anti_alias: true,
            ..SkPaint::default()
        }
    }
}

fn fill_background(pixmap: &mut Pixmap, color: Rgba) {
    let paint = SkPaint {
        shader: Shader::SolidColor(to_skia_color(color)),
        ..SkPaint::default()
    };
    if let Some(rect) =
        tiny_skia::Rect::from_xywh(0.0, 0.0, pixmap.width() as f32, pixmap.height() as f32)
    {
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
}

fn to_skia_color(color: Rgba) -> Color {
    Color::from_rgba8(color.r, color.g, color.b, color.a)
}

fn to_skia_stops(stops: &[ofdbox_folio_ir::GradientStop]) -> Vec<tiny_skia::GradientStop> {
    stops
        .iter()
        .map(|stop| {
            tiny_skia::GradientStop::new(
                stop.offset.clamp(0.0, 1.0) as f32,
                to_skia_color(stop.color),
            )
        })
        .collect()
}

fn to_spread(extend: GradientExtend) -> SpreadMode {
    match extend {
        GradientExtend::Pad => SpreadMode::Pad,
        GradientExtend::Repeat => SpreadMode::Repeat,
        GradientExtend::Reflect => SpreadMode::Reflect,
    }
}

fn to_skia_transform(matrix: Matrix) -> Transform {
    Transform::from_row(
        matrix.a as f32,
        matrix.b as f32,
        matrix.c as f32,
        matrix.d as f32,
        matrix.e as f32,
        matrix.f as f32,
    )
}

fn to_skia_stroke(style: &StrokeStyle, width: f32) -> Stroke {
    let mut stroke = Stroke {
        width,
        line_cap: match style.cap {
            LineCap::Butt => tiny_skia::LineCap::Butt,
            LineCap::Round => tiny_skia::LineCap::Round,
            LineCap::Square => tiny_skia::LineCap::Square,
        },
        line_join: match style.join {
            LineJoin::Miter => tiny_skia::LineJoin::Miter,
            LineJoin::Round => tiny_skia::LineJoin::Round,
            LineJoin::Bevel => tiny_skia::LineJoin::Bevel,
        },
        miter_limit: style.miter_limit as f32,
        dash: None,
    };
    if !style.dash.is_empty() {
        stroke.dash = tiny_skia::StrokeDash::new(
            style.dash.iter().map(|value| *value as f32).collect(),
            style.dash_offset as f32,
        );
    }
    stroke
}

fn to_skia_path(path: &Path) -> Option<tiny_skia::Path> {
    let mut builder = PathBuilder::new();
    let mut current = Point::default();
    let mut start = Point::default();
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(point) => {
                builder.move_to(point.x as f32, point.y as f32);
                current = *point;
                start = *point;
            }
            PathSegment::LineTo(point) => {
                builder.line_to(point.x as f32, point.y as f32);
                current = *point;
            }
            PathSegment::QuadTo(control, to) => {
                builder.quad_to(control.x as f32, control.y as f32, to.x as f32, to.y as f32);
                current = *to;
            }
            PathSegment::CubicTo(control1, control2, to) => {
                builder.cubic_to(
                    control1.x as f32,
                    control1.y as f32,
                    control2.x as f32,
                    control2.y as f32,
                    to.x as f32,
                    to.y as f32,
                );
                current = *to;
            }
            PathSegment::ArcTo { to, .. } => {
                builder.line_to(to.x as f32, to.y as f32);
                current = *to;
            }
            PathSegment::Close => {
                builder.close();
                current = start;
            }
            _ => {}
        }
    }
    let _ = current;
    builder.finish()
}

#[derive(Default)]
struct GlyphBuilder {
    builder: PathBuilder,
}

impl ttf_parser::OutlineBuilder for GlyphBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.builder.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.builder.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.builder.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ofdbox_folio_ir::{Element, Group};
    use ofdbox_folio_ir::{PathElement, Size};

    fn rect_element(x: f64, y: f64, width: f64, height: f64) -> Element {
        let mut path = Path::new();
        path.move_to(Point::new(x, y));
        path.line_to(Point::new(x + width, y));
        path.line_to(Point::new(x + width, y + height));
        path.line_to(Point::new(x, y + height));
        path.close();
        Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: Some(Paint::Color(Rgba::from_rgb(255, 0, 0))),
            stroke: None,
            rule: FillRule::NonZero,
        })
    }

    fn scene(children: Vec<Element>) -> Scene {
        Scene {
            size: Size::new(20.0, 20.0),
            root: Group {
                children,
                ..Group::default()
            },
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn renders_solid_rectangle() {
        let scene = scene(vec![rect_element(5.0, 5.0, 10.0, 10.0)]);
        let mut target = RasterTarget::from_size(scene.size, 96.0).expect("target");
        let mut renderer = TinySkiaRenderer::with_defaults();
        renderer.render_scene(&scene, &mut target).expect("render");
        let x = (10.0_f64 / 25.4 * 96.0) as usize;
        let y = (10.0_f64 / 25.4 * 96.0) as usize;
        let index = (y * usize::from(target.width()) + x) * 4;
        assert!(target.data()[index] > 200);
        assert!(target.data()[index + 1] < 40);
    }

    #[test]
    fn applies_region_origin() {
        let scene = scene(vec![rect_element(5.0, 5.0, 10.0, 10.0)]);
        let mut target = RasterTarget::from_pixels(32, 32, 96.0).expect("target");
        let mut renderer = TinySkiaRenderer::with_defaults();
        renderer
            .render_scene_region(&scene, &mut target, RenderRegion::new((10.0, 10.0), 96.0))
            .expect("render");
        let red = target
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0] > 200 && pixel[1] < 40)
            .count();
        assert!(red > 20, "expected red pixels in the tile, got {red}");
    }

    #[test]
    fn applies_group_clip() {
        let mut clip = Path::new();
        clip.move_to(Point::new(0.0, 0.0));
        clip.line_to(Point::new(5.0, 0.0));
        clip.line_to(Point::new(5.0, 20.0));
        clip.line_to(Point::new(0.0, 20.0));
        clip.close();
        let group = Group {
            clip: Some(Arc::new(ofdbox_folio_ir::Clip {
                path: Arc::new(clip),
                rule: FillRule::NonZero,
            })),
            children: vec![rect_element(0.0, 0.0, 20.0, 20.0)],
            ..Group::default()
        };
        let scene = scene(vec![Element::Group(group)]);
        let mut target = RasterTarget::from_size(scene.size, 96.0).expect("target");
        let mut renderer = TinySkiaRenderer::with_defaults();
        renderer.render_scene(&scene, &mut target).expect("render");
        let clip_x = (5.0_f64 / 25.4 * 96.0).ceil() as usize + 2;
        for y in 0..usize::from(target.height()) {
            for x in clip_x..usize::from(target.width()) {
                let index = (y * usize::from(target.width()) + x) * 4;
                assert!(
                    target.data()[index] > 200,
                    "pixel ({x}, {y}) escaped the clip"
                );
            }
        }
    }

    #[test]
    fn renders_text_outline() {
        let Ok(font_data) = std::fs::read("C:/Windows/Fonts/arial.ttf") else {
            return;
        };
        let font = Arc::new(FontAsset::new(Arc::from(font_data.into_boxed_slice()), 0));
        let face = ttf_parser::Face::parse(font.data(), 0).expect("face");
        let glyph_id = face.glyph_index('A').expect("glyph").0;
        let run = Arc::new(ofdbox_folio_ir::GlyphRun::new(
            "A",
            vec![ofdbox_folio_ir::Glyph {
                id: u32::from(glyph_id),
                x: 0.0,
                y: 0.0,
                advance: 6.0,
            }],
            Vec::new(),
        ));
        let element = Element::Text(TextElement {
            font,
            font_size: 10.0,
            transform: Matrix::translate(5.0, 15.0),
            run,
            fill: Some(Paint::Color(Rgba::from_rgb(0, 0, 0))),
            stroke: None,
        });
        let scene = scene(vec![element]);
        let mut target = RasterTarget::from_size(scene.size, 96.0).expect("target");
        let mut renderer = TinySkiaRenderer::with_defaults();
        renderer.render_scene(&scene, &mut target).expect("render");
        let dark = target
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0] < 128)
            .count();
        assert!(dark > 5, "expected glyph pixels, got {dark}");
    }
    #[test]
    fn renders_image_quad() {
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("png header");
            writer
                .write_image_data(&[
                    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
                ])
                .expect("png data");
        }
        let image = Arc::new(ImageAsset::new(
            ofdbox_folio_ir::ImageFormat::Png,
            Arc::from(encoded.into_boxed_slice()),
            2,
            2,
        ));
        let element = Element::Image(ImageElement {
            image,
            rect: ofdbox_folio_ir::Rect::new(0.0, 0.0, 10.0, 10.0),
            transform: Matrix::translate(5.0, 5.0),
            alpha: 1.0,
        });
        let scene = scene(vec![element]);
        let mut target = RasterTarget::from_size(scene.size, 96.0).expect("target");
        let mut renderer = TinySkiaRenderer::with_defaults();
        renderer.render_scene(&scene, &mut target).expect("render");
        let x = (10.0_f64 / 25.4 * 96.0) as usize;
        let y = (10.0_f64 / 25.4 * 96.0) as usize;
        let index = (y * usize::from(target.width()) + x) * 4;
        assert!(target.data()[index] > 200);
        assert!(target.data()[index + 1] < 40);
    }

    #[test]
    fn renders_linear_gradient_path() {
        let mut path = Path::new();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(20.0, 0.0));
        path.line_to(Point::new(20.0, 20.0));
        path.line_to(Point::new(0.0, 20.0));
        path.close();
        let element = Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: Some(Paint::LinearGradient(ofdbox_folio_ir::LinearGradient {
                start: Point::new(0.0, 0.0),
                end: Point::new(20.0, 0.0),
                stops: vec![
                    ofdbox_folio_ir::GradientStop {
                        offset: 0.0,
                        color: Rgba::from_rgb(255, 0, 0),
                    },
                    ofdbox_folio_ir::GradientStop {
                        offset: 1.0,
                        color: Rgba::from_rgb(0, 0, 255),
                    },
                ],
                extend: Default::default(),
            })),
            stroke: None,
            rule: FillRule::NonZero,
        });
        let scene = scene(vec![element]);
        let mut target = RasterTarget::from_size(scene.size, 96.0).expect("target");
        let mut renderer = TinySkiaRenderer::with_defaults();
        renderer.render_scene(&scene, &mut target).expect("render");
        let red = (
            (2.0_f64 / 25.4 * 96.0) as usize,
            (10.0_f64 / 25.4 * 96.0) as usize,
        );
        let blue = (
            (18.0_f64 / 25.4 * 96.0) as usize,
            (10.0_f64 / 25.4 * 96.0) as usize,
        );
        let red_index = (red.1 * usize::from(target.width()) + red.0) * 4;
        let blue_index = (blue.1 * usize::from(target.width()) + blue.0) * 4;
        assert!(target.data()[red_index] > 180, "red gradient side missing");
        assert!(
            target.data()[blue_index + 2] > 180,
            "blue gradient side missing"
        );
    }
    #[test]
    fn renders_scaled_down_text() {
        let Ok(font_data) = std::fs::read("C:/Windows/Fonts/arial.ttf") else {
            return;
        };
        let font = Arc::new(FontAsset::new(Arc::from(font_data.into_boxed_slice()), 0));
        let face = ttf_parser::Face::parse(font.data(), 0).expect("face");
        let glyph_id = face.glyph_index('A').expect("glyph").0;
        let run = Arc::new(ofdbox_folio_ir::GlyphRun::new(
            "A",
            vec![ofdbox_folio_ir::Glyph {
                id: u32::from(glyph_id),
                x: 0.0,
                y: 0.0,
                advance: 1500.0,
            }],
            Vec::new(),
        ));
        let element = Element::Text(TextElement {
            font,
            font_size: 200.0,
            transform: Matrix::scale(0.06, 0.06).then(Matrix::translate(300.0, 150.0)),
            run,
            fill: Some(Paint::Color(Rgba::from_rgb(0, 0, 0))),
            stroke: None,
        });
        let scene = Scene {
            size: Size::new(358.0, 507.0),
            root: Group {
                children: vec![element],
                ..Group::default()
            },
            diagnostics: Vec::new(),
        };
        let mut target = RasterTarget::from_size(scene.size, 72.0).expect("target");
        let mut renderer = TinySkiaRenderer::with_defaults();
        renderer.render_scene(&scene, &mut target).expect("render");
        let dark = target
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0] < 128)
            .count();
        assert!(dark > 5, "expected scaled glyph pixels, got {dark}");
    }
}
