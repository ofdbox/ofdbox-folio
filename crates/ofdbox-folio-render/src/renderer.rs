//! CPU bitmap renderer built on `vello_cpu`.
//!
//! The renderer consumes only Fixed IR and is unaware of the source document
//! format. Page geometry is mapped to pixel coordinates via the device scale;
//! fonts and images are cached by resource ID.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use ofdbox_folio_ir::{
    Diagnostic, DiagnosticCode, Element, FillRule, FontAsset, GradientExtend, GradientStop, Group,
    ImageElement, LineCap, LineJoin, Matrix, Page, Paint, Path, PathElement, PathSegment,
    RadialGradient, Report, Rgba, Scene, Size, StrokeStyle, TextElement,
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

use crate::error::RenderError;
use crate::image::decode_image;
use crate::options::RenderOptions;

/// An RGBA8 pixel target.
#[derive(Clone, Debug)]
pub struct RasterTarget {
    width: u16,
    height: u16,
    dpi: f64,
    data: Vec<u8>,
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

    /// Takes out the pixel data.
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }
}

/// CPU renderer built on `vello_cpu`.
pub struct CpuRenderer {
    options: RenderOptions,
    resources: Resources,
    fonts: HashMap<u64, FontData>,
    pixmaps: PixmapCache,
    report: Report,
}

/// Decoded image cache: evicted against a byte budget; a budget of 0 disables caching entirely.
struct PixmapCache {
    entries: HashMap<u64, Arc<Pixmap>>,
    order: VecDeque<u64>,
    bytes: u64,
    budget: u64,
}

impl PixmapCache {
    fn new(budget: u64) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            budget,
        }
    }

    fn get(&self, id: u64) -> Option<Arc<Pixmap>> {
        self.entries.get(&id).cloned()
    }

    fn insert(&mut self, id: u64, pixmap: Arc<Pixmap>) {
        if self.entries.contains_key(&id) {
            return;
        }
        let size = pixmap_bytes(&pixmap);
        if self.budget == 0 || size > self.budget {
            return;
        }
        while self.bytes + size > self.budget {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(pixmap_bytes(&removed));
            }
        }
        self.order.push_back(id);
        self.entries.insert(id, pixmap);
        self.bytes += size;
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }
}

fn pixmap_bytes(pixmap: &Pixmap) -> u64 {
    u64::from(pixmap.width()) * u64::from(pixmap.height()) * 4
}

impl CpuRenderer {
    /// Creates a renderer.
    pub fn new(options: RenderOptions) -> Self {
        Self {
            options,
            resources: Resources::new(),
            fonts: HashMap::new(),
            pixmaps: PixmapCache::new(options.max_image_cache_bytes),
            report: Report::new(),
        }
    }

    /// Creates a renderer with default options.
    pub fn with_defaults() -> Self {
        Self::new(RenderOptions::default())
    }

    /// Clears the font, image and rendering resource caches.
    pub fn clear_caches(&mut self) {
        self.fonts.clear();
        self.pixmaps.clear();
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

    fn render_scene_with(
        &mut self,
        scene: &Scene,
        target: &mut RasterTarget,
        device: Matrix,
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
        self.draw_group(&mut context, &scene.root, device)?;
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

    fn draw_group(
        &mut self,
        context: &mut RenderContext,
        group: &Group,
        parent: Matrix,
    ) -> Result<(), RenderError> {
        let transform = group.transform.then(parent);
        context.set_transform(affine(transform));
        let layered = group.clip.is_some() || group.opacity < 1.0;
        if layered {
            let clip = group.clip.as_ref().map(|clip| to_bez_path(&clip.path));
            context.push_layer(
                clip.as_ref(),
                None,
                (group.opacity < 1.0).then_some(group.opacity),
                None,
                None,
            );
        }
        for element in &group.children {
            match element {
                Element::Group(child) => self.draw_group(context, child, transform)?,
                Element::Path(path) => self.draw_path(context, path, transform)?,
                Element::Text(text) => self.draw_text(context, text, transform)?,
                Element::Image(image) => self.draw_image(context, image, transform)?,
                _ => {}
            }
        }
        if layered {
            context.pop_layer();
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
        if let Some(paint) = &element.fill {
            if let Paint::RadialGradient(radial) = paint
                && let Some(ellipse) = ellipse_transform(radial)
                && let Some(inverse) = ellipse.invert()
            {
                let mut ellipse_path = path.clone();
                ellipse_path.apply_affine(affine(inverse));
                context.set_transform(affine(ellipse.then(parent)));
                context.set_fill_rule(rule(element.rule));
                context.set_paint(Brush::Gradient(gradient_radial_transformed(
                    radial, inverse,
                )));
                context.fill_path(&ellipse_path);
                context.set_transform(affine(parent));
            } else {
                context.set_fill_rule(rule(element.rule));
                context.set_paint(brush(paint));
                context.fill_path(&path);
            }
        }
        if let Some(stroke) = &element.stroke {
            context.set_transform(affine(parent));
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
        if let Some(fill) = &element.fill {
            context.set_paint(brush(fill));
            let _ = context
                .glyph_run(&mut self.resources, &font)
                .font_size(element.font_size as f32)
                .fill_glyphs(glyphs.clone());
        }
        if let Some(stroke) = &element.stroke {
            context.set_paint(brush(&stroke.paint));
            context.set_stroke(kurbo_stroke(&stroke.style));
            let _ = context
                .glyph_run(&mut self.resources, &font)
                .font_size(element.font_size as f32)
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
        let pixmap = match self.pixmap(element) {
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

    fn pixmap(&mut self, element: &ImageElement) -> Result<Arc<Pixmap>, RenderError> {
        if let Some(pixmap) = self.pixmaps.get(element.image.id().get()) {
            return Ok(pixmap);
        }
        let mut decoded = decode_image(
            element.image.format,
            &element.image.encoded,
            self.options.max_image_pixels,
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
        self.pixmaps
            .insert(element.image.id().get(), Arc::clone(&pixmap));
        Ok(pixmap)
    }
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
    use super::{CpuRenderer, PixmapCache, RasterTarget};
    use crate::options::RenderOptions;
    use ofdbox_folio_ir::{
        DiagnosticCode, Element, FillRule, Group, ImageAsset, ImageElement, ImageFormat, Matrix,
        Paint, Path, PathElement, Point, Rect, Rgba, Scene, Size,
    };
    use std::sync::Arc;
    use vello_cpu::Pixmap;

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
        let width = f64::from(image.pixel_width.max(1));
        let height = f64::from(image.pixel_height.max(1));
        Scene {
            size: Size::new(10.0, 10.0),
            root: Group {
                children: vec![Element::Image(ImageElement {
                    image: Arc::new(image),
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
    fn evicts_pixmaps_over_budget() {
        // Two 2×2 RGBA images exactly fill the budget, so a third evicts the earliest inserted entry.
        let mut cache = PixmapCache::new(2 * 2 * 4 * 2);
        cache.insert(1, Arc::new(Pixmap::new(2, 2)));
        cache.insert(2, Arc::new(Pixmap::new(2, 2)));
        assert!(cache.get(1).is_some());
        cache.insert(3, Arc::new(Pixmap::new(2, 2)));
        assert!(
            cache.get(1).is_none(),
            "the earliest cache entry should be evicted"
        );
        assert!(cache.get(2).is_some());
        assert!(cache.get(3).is_some());
        assert!(cache.bytes <= cache.budget);
    }

    #[test]
    fn skips_cache_when_budget_is_zero() {
        let mut cache = PixmapCache::new(0);
        cache.insert(1, Arc::new(Pixmap::new(2, 2)));
        assert!(cache.get(1).is_none());
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
}
