//! PDF 页面前端：把 PDF 页面转换为 Fixed IR。
//!
//! 覆盖路径、文本、图片、裁剪、透明度和 Form XObject。字体优先使用嵌入
//! 字体，缺失时回退到调用方注入的 [`FontProvider`]，再退化为保留文本的
//! 占位字体。页码、文本层与页面尺寸在构建页面时一并生成。

use std::collections::HashMap;
use std::sync::Arc;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};

use ofdbox_folio_ir::{
    Clip, Cluster, Diagnostic, DiagnosticCode, Element, FillRule, FontAsset, FontProvider,
    FontRequest, Glyph, GlyphRun, GradientExtend, GradientStop, Group, ImageAsset, ImageElement,
    ImageFormat, LineCap, LineJoin, LinearGradient, Matrix, Page, PageInfo, Paint, Path,
    PathElement, PathSegment, Point, RadialGradient, Rect, ResourceStore, Rgba, Rotation, Scene,
    Size, StrokePaint, StrokeStyle, TextElement, TextItem, TextLayer, TextLine,
};

use crate::error::{PdfError, parse_error};

const PT_TO_MM: f64 = 25.4 / 72.0;

type PageBox = (f64, f64, f64, f64);

/// PDF 读取选项。
#[derive(Clone, Copy, Debug)]
pub struct PdfReadOptions {
    /// 单页内容流的最大字节数。
    pub max_page_bytes: usize,
    /// 单张图片允许的最大像素数。
    pub max_image_pixels: u64,
    /// Form XObject 的最大嵌套层数。
    pub max_recursion_depth: u32,
}

impl Default for PdfReadOptions {
    fn default() -> Self {
        let limits = ofdbox_folio_ir::Limits::default();
        Self {
            max_page_bytes: limits.max_page_bytes,
            max_image_pixels: limits.max_image_pixels,
            max_recursion_depth: limits.max_recursion_depth,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ClipState {
    path: Arc<Path>,
    rule: FillRule,
    transform: Matrix,
}

pub(crate) struct FontState {
    font: Arc<FontAsset>,
    identity: bool,
    to_unicode: HashMap<u32, u32>,
    /// 嵌入字体不可用、由外部字体替代。
    fallback: bool,
}

#[derive(Clone)]
struct State {
    ctm: Matrix,
    fill: Paint,
    stroke: Paint,
    fill_pattern: Option<Vec<u8>>,
    stroke_pattern: Option<Vec<u8>>,
    fill_space: Option<Vec<u8>>,
    stroke_space: Option<Vec<u8>>,
    style: StrokeStyle,
    fill_alpha: f32,
    stroke_alpha: f32,
    clips: Vec<ClipState>,
    font: Option<Arc<FontState>>,
    font_size: f64,
    char_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
    leading: f64,
    text_matrix: Matrix,
    line_matrix: Matrix,
    render_mode: u8,
}

impl Default for State {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            fill: Paint::Color(Rgba::BLACK),
            stroke: Paint::Color(Rgba::BLACK),
            fill_pattern: None,
            stroke_pattern: None,
            fill_space: None,
            stroke_space: None,
            style: StrokeStyle::default(),
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            clips: Vec::new(),
            font: None,
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            leading: 0.0,
            text_matrix: Matrix::IDENTITY,
            line_matrix: Matrix::IDENTITY,
            render_mode: 0,
        }
    }
}

/// 构建一页。
pub(crate) fn build_page(
    document: &Document,
    page_id: ObjectId,
    index: usize,
    options: &PdfReadOptions,
    store: &mut ResourceStore,
    font_cache: &mut HashMap<Vec<u8>, Option<Arc<FontState>>>,
    font_provider: &mut Option<Box<dyn FontProvider>>,
) -> Result<Page, PdfError> {
    let page = document
        .get_dictionary(page_id)
        .map_err(parse_error)?
        .clone();
    let (_, (x0, y0, x1, y1)) = page_box(document, page_id)?;
    let width = (x1 - x0) * PT_TO_MM;
    let height = (y1 - y0) * PT_TO_MM;
    let rotation = page_rotation(document, page_id)?;
    let display = rotation.apply_to_size(Size::new(width, height));
    let base = Matrix::scale(PT_TO_MM, -PT_TO_MM)
        .then(Matrix::translate(-x0 * PT_TO_MM, y0 * PT_TO_MM + height));
    let page_transform = base.then(rotation_matrix(rotation, width, height));
    let resources = page
        .get(b"Resources")
        .ok()
        .and_then(|object| resolve(document, object))
        .and_then(|object| object.as_dict().ok())
        .cloned()
        .unwrap_or_default();
    let content = document
        .get_page_content_with_limit(page_id, options.max_page_bytes)
        .map_err(parse_error)?;
    let content = Content::decode(&content).map_err(parse_error)?;
    font_cache.clear();
    let mut builder = Builder {
        document,
        options,
        store,
        font_cache,
        font_provider,
        diagnostics: Vec::new(),
    };
    let mut scene = Scene {
        size: display,
        root: Group::new(page_transform),
        diagnostics: Vec::new(),
    };
    let mut state = State::default();
    let mut stack = Vec::new();
    let mut path = Path::new();
    let mut current = Point::default();
    let mut pending_clip: Option<ClipState> = None;
    builder.execute(
        &content.operations,
        &resources,
        &mut scene,
        &mut state,
        &mut stack,
        &mut path,
        &mut current,
        &mut pending_clip,
        0,
    )?;
    scene.diagnostics = builder.diagnostics;
    let text = build_text_layer(&scene, index);
    let mut page = Page::new(
        PageInfo {
            media_box: Rect::new(0.0, 0.0, width, height),
            crop_box: Rect::new(0.0, 0.0, width, height),
            rotation,
            label: None,
        },
        scene,
    );
    page.text = text;
    Ok(page)
}

fn build_text_layer(scene: &Scene, index: usize) -> TextLayer {
    let mut items = Vec::new();
    let mut lines = Vec::new();
    collect_text(&scene.root, Matrix::IDENTITY, &mut items, &mut lines);
    merge_lines(&mut lines);
    TextLayer {
        page: index,
        items,
        lines,
    }
}

fn collect_text(
    group: &Group,
    parent: Matrix,
    items: &mut Vec<TextItem>,
    lines: &mut Vec<TextLine>,
) {
    let transform = group.transform.then(parent);
    for element in &group.children {
        match element {
            Element::Group(child) => collect_text(child, transform, items, lines),
            Element::Text(text) => {
                let item_transform = text.transform.then(transform);
                let bbox = text_bbox(&text.run, text.font_size, item_transform);
                let index = items.len() as u32;
                items.push(TextItem {
                    run: Arc::clone(&text.run),
                    transform: item_transform,
                    bbox,
                    font: Arc::clone(&text.font),
                    font_size: text.font_size,
                });
                lines.push(TextLine {
                    items: index..index + 1,
                    bbox,
                    baseline: bbox.bottom(),
                });
            }
            _ => {}
        }
    }
}

fn merge_lines(lines: &mut Vec<TextLine>) {
    let mut merged: Vec<TextLine> = Vec::with_capacity(lines.len());
    for line in lines.drain(..) {
        if let Some(previous) = merged.last_mut()
            && (previous.baseline - line.baseline).abs() <= line.bbox.height.max(0.1) * 0.5
        {
            previous.bbox = union(previous.bbox, line.bbox);
            previous.items.end = line.items.end;
            continue;
        }
        merged.push(line);
    }
    *lines = merged;
}

/// 页面尺寸与旋转信息，不解析内容。
pub(crate) fn page_info(document: &Document, page_id: ObjectId) -> Result<PageInfo, PdfError> {
    let (_, (x0, y0, x1, y1)) = page_box(document, page_id)?;
    Ok(PageInfo {
        media_box: Rect::new(0.0, 0.0, (x1 - x0) * PT_TO_MM, (y1 - y0) * PT_TO_MM),
        crop_box: Rect::new(0.0, 0.0, (x1 - x0) * PT_TO_MM, (y1 - y0) * PT_TO_MM),
        rotation: page_rotation(document, page_id)?,
        label: None,
    })
}

pub(crate) fn page_ids(document: &Document) -> Vec<ObjectId> {
    let mut pages = document.get_pages().into_iter().collect::<Vec<_>>();
    pages.sort_by_key(|(number, _)| *number);
    pages.into_iter().map(|(_, id)| id).collect()
}

fn page_box(document: &Document, page_id: ObjectId) -> Result<(ObjectId, PageBox), PdfError> {
    let page = document.get_dictionary(page_id).map_err(parse_error)?;
    for key in [b"CropBox".as_slice(), b"MediaBox".as_slice()] {
        if let Ok(object) = page.get(key)
            && let Some(resolved) = resolve(document, object)
            && let Ok(array) = resolved.as_array()
            && let [a, b, c, d] = array.as_slice()
            && let (Some(x0), Some(y0), Some(x1), Some(y1)) = (
                object_number(a),
                object_number(b),
                object_number(c),
                object_number(d),
            )
        {
            return Ok((page_id, (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))));
        }
    }
    Ok((page_id, (0.0, 0.0, 595.0, 842.0)))
}

fn page_rotation(document: &Document, page_id: ObjectId) -> Result<Rotation, PdfError> {
    let page = document.get_dictionary(page_id).map_err(parse_error)?;
    let rotate = page
        .get(b"Rotate")
        .ok()
        .and_then(|object| resolve(document, object))
        .and_then(object_number)
        .unwrap_or(0.0);
    let rotate = ((rotate.round() as i64) % 360 + 360) % 360;
    Ok(match rotate {
        90 => Rotation::Cw90,
        180 => Rotation::Cw180,
        270 => Rotation::Cw270,
        _ => Rotation::None,
    })
}

fn rotation_matrix(rotation: Rotation, width: f64, height: f64) -> Matrix {
    match rotation {
        Rotation::None => Matrix::IDENTITY,
        Rotation::Cw90 => Matrix::new(0.0, 1.0, -1.0, 0.0, height, 0.0),
        Rotation::Cw180 => Matrix::new(-1.0, 0.0, 0.0, -1.0, width, height),
        Rotation::Cw270 => Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, width),
    }
}

struct Builder<'a> {
    document: &'a Document,
    options: &'a PdfReadOptions,
    store: &'a mut ResourceStore,
    font_cache: &'a mut HashMap<Vec<u8>, Option<Arc<FontState>>>,
    font_provider: &'a mut Option<Box<dyn FontProvider>>,
    diagnostics: Vec<Diagnostic>,
}

impl Builder<'_> {
    fn resolve<'a>(&'a self, object: &'a Object) -> Option<&'a Object> {
        self.document
            .dereference(object)
            .ok()
            .map(|(_, object)| object)
    }

    fn diagnostic(&mut self, code: DiagnosticCode, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic::warning(code, message));
    }

    #[allow(clippy::too_many_arguments)]
    fn execute(
        &mut self,
        operations: &[Operation],
        resources: &Dictionary,
        scene: &mut Scene,
        state: &mut State,
        stack: &mut Vec<State>,
        path: &mut Path,
        current: &mut Point,
        pending_clip: &mut Option<ClipState>,
        depth: u32,
    ) -> Result<(), PdfError> {
        if depth > self.options.max_recursion_depth.max(1) {
            self.diagnostic(DiagnosticCode::LimitExceeded, "PDF 内容嵌套过深");
            return Ok(());
        }
        for operation in operations {
            let op = operation.operator.as_bytes();
            let operands = &operation.operands;
            match op {
                b"q" => stack.push(state.clone()),
                b"Q" => {
                    if let Some(previous) = stack.pop() {
                        *state = previous;
                    }
                }
                b"cm" => {
                    if let Some(matrix) = matrix_operands(operands) {
                        state.ctm = matrix.then(state.ctm);
                    }
                }
                b"w" => {
                    if let Some(width) = operands.first().and_then(object_number) {
                        state.style.width = width;
                    }
                }
                b"J" => {
                    if let Some(cap) = operands.first().and_then(object_number) {
                        state.style.cap = match cap as i64 {
                            1 => LineCap::Round,
                            2 => LineCap::Square,
                            _ => LineCap::Butt,
                        };
                    }
                }
                b"j" => {
                    if let Some(join) = operands.first().and_then(object_number) {
                        state.style.join = match join as i64 {
                            1 => LineJoin::Round,
                            2 => LineJoin::Bevel,
                            _ => LineJoin::Miter,
                        };
                    }
                }
                b"M" => {
                    if let Some(limit) = operands.first().and_then(object_number) {
                        state.style.miter_limit = limit;
                    }
                }
                b"d" => {
                    if let Some(Object::Array(values)) = operands.first() {
                        state.style.dash = values.iter().filter_map(object_number).collect();
                        state.style.dash_offset =
                            operands.get(1).and_then(object_number).unwrap_or(0.0);
                    }
                }
                b"rg" | b"RG" => {
                    if let Some(color) = rgb_operands(operands) {
                        if op == b"rg" {
                            state.fill = Paint::Color(color);
                            state.fill_pattern = None;
                        } else {
                            state.stroke = Paint::Color(color);
                            state.stroke_pattern = None;
                        }
                    }
                }
                b"g" | b"G" => {
                    if let Some(value) = operands.first().and_then(object_number) {
                        let value = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                        let color = Rgba::new(value, value, value, 255);
                        if op == b"g" {
                            state.fill = Paint::Color(color);
                            state.fill_pattern = None;
                        } else {
                            state.stroke = Paint::Color(color);
                            state.stroke_pattern = None;
                        }
                    }
                }
                b"k" | b"K" => {
                    if let Some(color) = cmyk_operands(operands) {
                        if op == b"k" {
                            state.fill = Paint::Color(color);
                            state.fill_pattern = None;
                        } else {
                            state.stroke = Paint::Color(color);
                            state.stroke_pattern = None;
                        }
                    }
                }
                b"cs" | b"CS" => {
                    if let Some(name) = operands.first().and_then(object_name) {
                        if op == b"cs" {
                            state.fill_space = Some(name.to_vec());
                            state.fill_pattern = None;
                        } else {
                            state.stroke_space = Some(name.to_vec());
                            state.stroke_pattern = None;
                        }
                    }
                }
                b"sc" | b"scn" | b"SC" | b"SCN" => {
                    let fill = matches!(op, b"sc" | b"scn");
                    let space = if fill {
                        state.fill_space.as_deref()
                    } else {
                        state.stroke_space.as_deref()
                    };
                    let pattern = operands
                        .last()
                        .and_then(object_name)
                        .filter(|_| space == Some(b"Pattern"));
                    if let Some(name) = pattern {
                        if fill {
                            state.fill_pattern = Some(name.to_vec());
                        } else {
                            state.stroke_pattern = Some(name.to_vec());
                        }
                    } else if let Some(color) = components_operands(operands) {
                        if fill {
                            state.fill = Paint::Color(color);
                            state.fill_pattern = None;
                        } else {
                            state.stroke = Paint::Color(color);
                            state.stroke_pattern = None;
                        }
                    }
                }
                b"gs" => {
                    if let Some(name) = operands.first().and_then(object_name)
                        && let Some(dictionary) = resources
                            .get(b"ExtGState")
                            .ok()
                            .and_then(|object| self.resolve(object))
                            .and_then(|object| object.as_dict().ok())
                            .and_then(|dictionary| dictionary.get(name).ok())
                            .and_then(|object| self.resolve(object))
                            .and_then(|object| object.as_dict().ok())
                    {
                        if let Some(alpha) = dictionary.get(b"ca").ok().and_then(object_number) {
                            state.fill_alpha = alpha as f32;
                        }
                        if let Some(alpha) = dictionary.get(b"CA").ok().and_then(object_number) {
                            state.stroke_alpha = alpha as f32;
                        }
                    }
                }
                b"m" => {
                    if let Some(point) = point_operands(operands) {
                        path.move_to(point);
                        *current = point;
                    }
                }
                b"l" => {
                    if let Some(point) = point_operands(operands) {
                        path.line_to(point);
                        *current = point;
                    }
                }
                b"c" => {
                    if let Some((c1, c2, to)) = cubic_operands(operands) {
                        path.cubic_to(c1, c2, to);
                        *current = to;
                    }
                }
                b"v" => {
                    if let Some((c2, to)) = quadratic_operands(operands) {
                        path.cubic_to(*current, c2, to);
                        *current = to;
                    }
                }
                b"y" => {
                    if let Some((c1, to)) = quadratic_operands(operands) {
                        path.cubic_to(c1, to, to);
                        *current = to;
                    }
                }
                b"h" => path.close(),
                b"re" => {
                    if let Some((x, y, width, height)) = rect_operands(operands) {
                        path.move_to(Point::new(x, y));
                        path.line_to(Point::new(x + width, y));
                        path.line_to(Point::new(x + width, y + height));
                        path.line_to(Point::new(x, y + height));
                        path.close();
                        *current = Point::new(x, y);
                    }
                }
                b"W" | b"W*" => {
                    *pending_clip = Some(ClipState {
                        path: Arc::new(path.clone()),
                        rule: if op == b"W*" {
                            FillRule::EvenOdd
                        } else {
                            FillRule::NonZero
                        },
                        transform: state.ctm,
                    });
                }
                b"n" => {
                    if let Some(clip) = pending_clip.take() {
                        push_clip(state, clip);
                    }
                    path.segments_mut().clear();
                }
                b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" => {
                    let fill = matches!(op, b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*");
                    let stroke = matches!(op, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*");
                    let even_odd = matches!(op, b"f*" | b"B*" | b"b*");
                    if op == b"b" || op == b"b*" || op == b"s" {
                        path.close();
                    }
                    if let Some(clip) = pending_clip.take() {
                        push_clip(state, clip);
                    }
                    if !path.is_empty() {
                        let fill = fill.then(|| self.fill_paint(state, resources)).flatten();
                        let stroke = stroke
                            .then(|| self.stroke_paint(state, resources))
                            .flatten()
                            .map(|paint| StrokePaint {
                                paint,
                                style: state.style.clone(),
                            });
                        self.push_element(
                            scene,
                            state,
                            Element::Path(PathElement {
                                path: Arc::new(path.clone()),
                                transform: Matrix::IDENTITY,
                                fill,
                                stroke,
                                rule: if even_odd {
                                    FillRule::EvenOdd
                                } else {
                                    FillRule::NonZero
                                },
                            }),
                        );
                    }
                    path.segments_mut().clear();
                }
                b"BT" => {
                    state.text_matrix = Matrix::IDENTITY;
                    state.line_matrix = Matrix::IDENTITY;
                }
                b"ET" => {}
                b"Tf" => {
                    if let Some(size) = operands.get(1).and_then(object_number) {
                        state.font_size = size;
                    }
                    if let Some(name) = operands.first().and_then(object_name) {
                        state.font = self.load_font(name, resources);
                    }
                }
                b"Tm" => {
                    if let Some(matrix) = matrix_operands(operands) {
                        state.text_matrix = matrix;
                        state.line_matrix = matrix;
                    }
                }
                b"Td" => {
                    if let Some((x, y)) = pair_operands(operands) {
                        state.line_matrix = Matrix::translate(x, y).then(state.line_matrix);
                        state.text_matrix = state.line_matrix;
                    }
                }
                b"TD" => {
                    if let Some((x, y)) = pair_operands(operands) {
                        state.leading = -y;
                        state.line_matrix = Matrix::translate(x, y).then(state.line_matrix);
                        state.text_matrix = state.line_matrix;
                    }
                }
                b"T*" => {
                    state.line_matrix =
                        Matrix::translate(0.0, -state.leading).then(state.line_matrix);
                    state.text_matrix = state.line_matrix;
                }
                b"Tc" => {
                    if let Some(value) = operands.first().and_then(object_number) {
                        state.char_spacing = value;
                    }
                }
                b"Tw" => {
                    if let Some(value) = operands.first().and_then(object_number) {
                        state.word_spacing = value;
                    }
                }
                b"Tz" => {
                    if let Some(value) = operands.first().and_then(object_number) {
                        state.horizontal_scale = value / 100.0;
                    }
                }
                b"TL" => {
                    if let Some(value) = operands.first().and_then(object_number) {
                        state.leading = value;
                    }
                }
                b"Tr" => {
                    if let Some(value) = operands.first().and_then(object_number) {
                        state.render_mode = value.clamp(0.0, 7.0) as u8;
                    }
                }
                b"Tj" => {
                    if let Some(Object::String(bytes, _)) = operands.first() {
                        self.write_text(bytes, resources, scene, state);
                    }
                }
                b"'" => {
                    state.line_matrix =
                        Matrix::translate(0.0, -state.leading).then(state.line_matrix);
                    state.text_matrix = state.line_matrix;
                    if let Some(Object::String(bytes, _)) = operands.first() {
                        self.write_text(bytes, resources, scene, state);
                    }
                }
                b"\"" => {
                    if let Some(word) = operands.first().and_then(object_number) {
                        state.word_spacing = word;
                    }
                    if let Some(spacing) = operands.get(1).and_then(object_number) {
                        state.char_spacing = spacing;
                    }
                    state.line_matrix =
                        Matrix::translate(0.0, -state.leading).then(state.line_matrix);
                    state.text_matrix = state.line_matrix;
                    if let Some(Object::String(bytes, _)) = operands.get(2) {
                        self.write_text(bytes, resources, scene, state);
                    }
                }
                b"TJ" => {
                    if let Some(Object::Array(items)) = operands.first() {
                        for item in items {
                            match item {
                                Object::String(bytes, _) => {
                                    self.write_text(bytes, resources, scene, state);
                                }
                                other => {
                                    if let Some(adjust) = object_number(other) {
                                        state.text_matrix = Matrix::translate(
                                            -adjust / 1000.0
                                                * state.font_size
                                                * state.horizontal_scale,
                                            0.0,
                                        )
                                        .then(state.text_matrix);
                                    }
                                }
                            }
                        }
                    }
                }
                b"Do" => {
                    if let Some(name) = operands.first().and_then(object_name) {
                        self.draw_xobject(name, resources, scene, state, depth)?;
                    }
                }
                b"sh" => {
                    if let Some(name) = operands.first().and_then(object_name) {
                        match self.shading_paint(name, resources) {
                            Some(paint) => {
                                let path =
                                    Rect::new(-10_000.0, -10_000.0, 20_000.0, 20_000.0).to_path();
                                self.push_element(
                                    scene,
                                    state,
                                    Element::Path(PathElement {
                                        path: Arc::new(path),
                                        transform: Matrix::IDENTITY,
                                        fill: Some(paint),
                                        stroke: None,
                                        rule: FillRule::NonZero,
                                    }),
                                );
                            }
                            None => self.diagnostic(
                                DiagnosticCode::Paint,
                                format!(
                                    "PDF Shading 缺失或暂不支持: {}",
                                    String::from_utf8_lossy(name)
                                ),
                            ),
                        }
                    }
                }
                b"BI" => {
                    if let Some(Object::Stream(stream)) = operands.first() {
                        self.draw_image_stream(stream, scene, state)?;
                    }
                }
                b"ID" | b"EI" => {}
                _ => {}
            }
        }
        Ok(())
    }

    fn load_font(&mut self, name: &[u8], resources: &Dictionary) -> Option<Arc<FontState>> {
        if let Some(cached) = self.font_cache.get(name) {
            return cached.clone();
        }
        let loaded = self.load_font_inner(name, resources);
        self.font_cache.insert(name.to_vec(), loaded.clone());
        loaded
    }

    fn load_font_inner(&mut self, name: &[u8], resources: &Dictionary) -> Option<Arc<FontState>> {
        let (base_font, identity, embedded, to_unicode) = {
            let fonts = resources
                .get(b"Font")
                .ok()
                .and_then(|object| self.resolve(object))
                .and_then(|object| object.as_dict().ok())?;
            let dictionary = fonts
                .get(name)
                .ok()
                .and_then(|object| self.resolve(object))
                .and_then(|object| object.as_dict().ok())?;
            let base_font = dictionary
                .get(b"BaseFont")
                .ok()
                .and_then(|object| object.as_name().ok())
                .map(|name| String::from_utf8_lossy(name).into_owned());
            let subtype = dictionary
                .get(b"Subtype")
                .ok()
                .and_then(|object| object.as_name().ok())
                .unwrap_or_default();
            let identity = subtype == b"Type0";
            let font_dictionary = if identity {
                dictionary
                    .get(b"DescendantFonts")
                    .ok()
                    .and_then(|object| self.resolve(object))
                    .and_then(|object| object.as_array().ok())
                    .and_then(|array| array.first())
                    .and_then(|object| self.resolve(object))
                    .and_then(|object| object.as_dict().ok())
                    .unwrap_or(dictionary)
            } else {
                dictionary
            };
            let descriptor = font_dictionary
                .get(b"FontDescriptor")
                .ok()
                .and_then(|object| self.resolve(object))
                .and_then(|object| object.as_dict().ok());
            let embedded = descriptor.and_then(|descriptor| {
                [
                    b"FontFile2".as_slice(),
                    b"FontFile3".as_slice(),
                    b"FontFile".as_slice(),
                ]
                .into_iter()
                .find_map(|key| {
                    descriptor
                        .get(key)
                        .ok()
                        .and_then(|object| self.resolve(object))
                        .and_then(|object| object.as_stream().ok())
                })
                .map(|file| {
                    file.decompressed_content()
                        .unwrap_or_else(|_| file.content.clone())
                })
                .filter(|bytes| !bytes.is_empty())
            });
            let to_unicode = dictionary
                .get(b"ToUnicode")
                .ok()
                .and_then(|object| self.resolve(object))
                .and_then(|object| object.as_stream().ok())
                .and_then(|stream| stream.decompressed_content().ok())
                .map(|bytes| parse_to_unicode(&bytes))
                .unwrap_or_default();
            (base_font, identity, embedded, to_unicode)
        };
        let (data, fallback) = match embedded {
            Some(bytes) => (Arc::from(bytes), false),
            None => {
                let request = FontRequest {
                    font_id: None,
                    font_name: base_font.as_deref(),
                    family_name: base_font.as_deref(),
                    charset: None,
                    bold: false,
                    italic: false,
                    serif: false,
                    fixed_width: false,
                    font_file: None,
                };
                match self
                    .font_provider
                    .as_mut()
                    .and_then(|provider| provider.load(&request).ok().flatten())
                {
                    Some(bytes) => (bytes, true),
                    None => {
                        self.diagnostic(
                            DiagnosticCode::MissingFont,
                            format!("PDF 字体缺失: {}", base_font.as_deref().unwrap_or("未命名")),
                        );
                        (Arc::from(&b""[..]), true)
                    }
                }
            }
        };
        let mut asset = FontAsset::new(Arc::clone(&data), 0);
        if let Some(name) = &base_font {
            asset = asset.with_font_name(name.clone()).with_family(name.clone());
        }
        let id = self.store.add_font_asset(asset);
        let font = self.store.font(id).cloned()?;
        Some(Arc::new(FontState {
            font,
            identity,
            to_unicode,
            fallback,
        }))
    }

    fn write_text(
        &mut self,
        bytes: &[u8],
        resources: &Dictionary,
        scene: &mut Scene,
        state: &mut State,
    ) {
        let Some(font_state) = state.font.clone() else {
            return;
        };
        let face =
            ttf_parser::Face::parse(font_state.font.data(), font_state.font.face_index()).ok();
        let units = face
            .as_ref()
            .map(|face| f64::from(face.units_per_em()))
            .filter(|units| *units > 0.0);
        let mut glyphs = Vec::new();
        let mut text = String::new();
        let mut clusters = Vec::new();
        let mut x = 0.0;
        let size = state.font_size;
        if font_state.identity {
            for pair in bytes.as_chunks::<2>().0 {
                let code = u32::from(u16::from_be_bytes(*pair));
                let character = font_state
                    .to_unicode
                    .get(&code)
                    .and_then(|value| char::from_u32(*value))
                    .or_else(|| face.as_ref().and_then(|face| glyph_char(face, code as u16)))
                    .unwrap_or('\u{FFFD}');
                let glyph_id = if font_state.fallback {
                    face.as_ref()
                        .and_then(|face| face.glyph_index(character))
                        .map(|glyph| u32::from(glyph.0))
                        .unwrap_or(code)
                } else {
                    code
                };
                let advance =
                    glyph_advance(face.as_ref(), glyph_id, size, units) + state.char_spacing;
                let index = text.chars().count() as u32;
                text.push(character);
                clusters.push(Cluster {
                    text_start: index,
                    text_end: index + 1,
                    glyph_start: glyphs.len() as u32,
                    glyph_end: glyphs.len() as u32 + 1,
                });
                glyphs.push(Glyph {
                    id: glyph_id,
                    x,
                    y: 0.0,
                    advance,
                });
                x += advance;
            }
        } else {
            for &code in bytes {
                let codepoint = font_state
                    .to_unicode
                    .get(&u32::from(code))
                    .copied()
                    .unwrap_or(u32::from(code));
                let Some(character) = char::from_u32(codepoint) else {
                    continue;
                };
                let glyph_id = face
                    .as_ref()
                    .and_then(|face| face.glyph_index(character))
                    .map(|glyph| u32::from(glyph.0))
                    .unwrap_or(u32::from(code));
                let advance = glyph_advance(face.as_ref(), glyph_id, size, units)
                    + state.char_spacing
                    + if character == ' ' {
                        state.word_spacing
                    } else {
                        0.0
                    };
                let index = text.chars().count() as u32;
                text.push(character);
                clusters.push(Cluster {
                    text_start: index,
                    text_end: index + 1,
                    glyph_start: glyphs.len() as u32,
                    glyph_end: glyphs.len() as u32 + 1,
                });
                glyphs.push(Glyph {
                    id: glyph_id,
                    x,
                    y: 0.0,
                    advance,
                });
                x += advance;
            }
        }
        if glyphs.is_empty() {
            return;
        }
        // PDF 文本默认只填充；只有 Tr 指定时才描边。模式 3/7 不可见，
        // 但仍保留文本层，供搜索与选择使用。
        let fill = matches!(state.render_mode, 0 | 2 | 4 | 6)
            .then(|| self.fill_paint(state, resources))
            .flatten();
        let stroke = matches!(state.render_mode, 1 | 2 | 5 | 6)
            .then(|| self.stroke_paint(state, resources))
            .flatten()
            .map(|paint| StrokePaint {
                paint,
                style: state.style.clone(),
            });
        let run = Arc::new(GlyphRun::new(text, glyphs, clusters));
        let element = TextElement {
            font: Arc::clone(&font_state.font),
            font_size: size,
            transform: Matrix::scale(state.horizontal_scale, 1.0)
                .then(Matrix::scale(1.0, -1.0))
                .then(state.text_matrix),
            run,
            fill,
            stroke,
        };
        self.push_element(scene, state, Element::Text(element));
        state.text_matrix = Matrix::translate(x, 0.0).then(state.text_matrix);
    }

    fn push_element(&mut self, scene: &mut Scene, state: &State, element: Element) {
        let mut node = Element::Group(Group {
            transform: state.ctm,
            children: vec![element],
            ..Group::default()
        });
        for clip in state.clips.iter().rev() {
            let path = transform_path(&clip.path, clip.transform);
            node = Element::Group(Group {
                transform: Matrix::IDENTITY,
                clip: Some(Arc::new(Clip {
                    path: Arc::new(path),
                    rule: clip.rule,
                })),
                opacity: 1.0,
                children: vec![node],
            });
        }
        scene.root.children.push(node);
    }

    fn draw_xobject(
        &mut self,
        name: &[u8],
        resources: &Dictionary,
        scene: &mut Scene,
        state: &State,
        depth: u32,
    ) -> Result<(), PdfError> {
        let Some(xobjects) = resources
            .get(b"XObject")
            .ok()
            .and_then(|object| self.resolve(object))
            .and_then(|object| object.as_dict().ok())
        else {
            return Ok(());
        };
        let Some(object) = xobjects
            .get(name)
            .ok()
            .and_then(|object| self.resolve(object))
        else {
            return Ok(());
        };
        let Ok(stream) = object.as_stream().cloned() else {
            return Ok(());
        };
        let subtype = stream
            .dict
            .get(b"Subtype")
            .ok()
            .and_then(|object| object.as_name().ok())
            .unwrap_or_default();
        if subtype == b"Form" {
            if depth >= self.options.max_recursion_depth {
                self.diagnostic(DiagnosticCode::LimitExceeded, "PDF Form XObject 嵌套过深");
                return Ok(());
            }
            let form_dictionary = stream.dict.clone();
            let data = stream
                .decompressed_content()
                .unwrap_or_else(|_| stream.content.clone());
            let Ok(content) = Content::decode(&data) else {
                return Ok(());
            };
            let form_resources = form_dictionary
                .get(b"Resources")
                .ok()
                .and_then(|object| self.resolve(object))
                .and_then(|object| object.as_dict().ok())
                .cloned()
                .unwrap_or_else(|| resources.clone());
            let matrix = form_dictionary
                .get(b"Matrix")
                .ok()
                .and_then(|object| self.resolve(object))
                .and_then(|object| object.as_array().ok())
                .and_then(|values| matrix_from_values(values))
                .unwrap_or(Matrix::IDENTITY);
            let mut form_state = state.clone();
            form_state.ctm = matrix.then(state.ctm);
            let mut form_path = Path::new();
            let mut form_current = Point::default();
            let mut form_clip = None;
            let mut form_stack = Vec::new();
            self.execute(
                &content.operations,
                &form_resources,
                scene,
                &mut form_state,
                &mut form_stack,
                &mut form_path,
                &mut form_current,
                &mut form_clip,
                depth + 1,
            )?;
            return Ok(());
        }
        if subtype != b"Image" {
            return Ok(());
        }
        self.draw_image_stream(&stream, scene, state)
    }

    /// 绘制图片流；XObject 与内联图片共用。
    fn draw_image_stream(
        &mut self,
        stream: &lopdf::Stream,
        scene: &mut Scene,
        state: &State,
    ) -> Result<(), PdfError> {
        let Some(width) = stream_number(&stream.dict, b"Width", b"W").filter(|width| *width > 0.0)
        else {
            return Ok(());
        };
        let Some(height) =
            stream_number(&stream.dict, b"Height", b"H").filter(|height| *height > 0.0)
        else {
            return Ok(());
        };
        if width * height > self.options.max_image_pixels as f64 {
            self.diagnostic(DiagnosticCode::LimitExceeded, "PDF 图片超过像素限制");
            return Ok(());
        }
        let filter = stream_value(&stream.dict, b"Filter", b"F").and_then(|object| match object {
            Object::Name(name) => Some(name.as_slice()),
            Object::Array(values) => values.first().and_then(|value| value.as_name().ok()),
            _ => None,
        });
        let (format, bytes) = if filter == Some(b"DCTDecode") {
            (ImageFormat::Jpeg, Arc::<[u8]>::from(stream.content.clone()))
        } else if filter == Some(b"JPXDecode") {
            self.diagnostic(DiagnosticCode::Unsupported, "PDF JPEG2000 图片暂不支持");
            return Ok(());
        } else if filter == Some(b"JBIG2Decode") {
            (
                ImageFormat::Jbig2,
                Arc::<[u8]>::from(stream.content.clone()),
            )
        } else if filter == Some(b"CCITTFaxDecode") {
            match self.ccitt_image(stream, width as u32, height as u32) {
                Some(bytes) => (ImageFormat::Png, Arc::from(bytes)),
                None => {
                    self.diagnostic(DiagnosticCode::Image, "PDF CCITT 图片解码失败");
                    return Ok(());
                }
            }
        } else {
            match self.raw_image_rgba(stream, width as u32, height as u32) {
                Some(bytes) => (ImageFormat::Png, Arc::from(bytes)),
                None => {
                    self.diagnostic(
                        DiagnosticCode::Image,
                        format!(
                            "PDF 图片编码暂不支持: {:?}",
                            filter.map(String::from_utf8_lossy)
                        ),
                    );
                    return Ok(());
                }
            }
        };
        let image = ImageAsset::new(format, bytes, width as u32, height as u32);
        let id = self.store.add_image(image);
        let Some(image) = self.store.image(id).cloned() else {
            return Ok(());
        };
        let alpha = state.fill_alpha;
        self.push_element(
            scene,
            state,
            Element::Image(ImageElement {
                image,
                rect: Rect::default(),
                transform: Matrix::scale(1.0 / width, 1.0 / height)
                    .then(Matrix::scale(1.0, -1.0))
                    .then(Matrix::translate(0.0, 1.0)),
                alpha,
            }),
        );
        Ok(())
    }

    fn fill_paint(&self, state: &State, resources: &Dictionary) -> Option<Paint> {
        match &state.fill_pattern {
            Some(name) => self.pattern_paint(name, resources),
            None => paint_with_alpha(&state.fill, state.fill_alpha),
        }
    }

    fn stroke_paint(&self, state: &State, resources: &Dictionary) -> Option<Paint> {
        match &state.stroke_pattern {
            Some(name) => self.pattern_paint(name, resources),
            None => paint_with_alpha(&state.stroke, state.stroke_alpha),
        }
    }

    /// 着色图案（PatternType 2）。
    fn pattern_paint(&self, name: &[u8], resources: &Dictionary) -> Option<Paint> {
        let patterns = resources
            .get(b"Pattern")
            .ok()
            .and_then(|object| self.resolve(object))
            .and_then(|object| object.as_dict().ok())?;
        let object = patterns.get(name).ok()?;
        let dictionary = self
            .resolve(object)
            .and_then(|object| object.as_dict().ok())?;
        let pattern_type = dictionary
            .get(b"PatternType")
            .ok()
            .and_then(|object| self.resolve_number(object))? as i64;
        if pattern_type != 2 {
            return None;
        }
        let shading = dictionary.get(b"Shading").ok()?;
        let shading = self
            .resolve(shading)
            .and_then(|object| object.as_dict().ok())?;
        let paint = self.shading_from_dict(shading)?;
        let Some(matrix) = dictionary
            .get(b"Matrix")
            .ok()
            .and_then(|object| self.resolve(object))
            .and_then(|object| object.as_array().ok())
            .and_then(|values| matrix_from_values(values))
        else {
            return Some(paint);
        };
        Some(transform_paint(paint, matrix))
    }

    /// 把 PDF Shading 资源转换为 Fixed IR 渐变。
    fn shading_paint(&self, name: &[u8], resources: &Dictionary) -> Option<Paint> {
        let shadings = resources
            .get(b"Shading")
            .ok()
            .and_then(|object| self.resolve(object))
            .and_then(|object| object.as_dict().ok())?;
        let object = shadings.get(name).ok()?;
        let dictionary = self
            .resolve(object)
            .and_then(|object| object.as_dict().ok())?;
        self.shading_from_dict(dictionary)
    }

    fn shading_from_dict(&self, dictionary: &Dictionary) -> Option<Paint> {
        let shading_type = dictionary
            .get(b"ShadingType")
            .ok()
            .and_then(|object| self.resolve_number(object))? as i64;
        let coords = self.number_array(dictionary, b"Coords")?;
        let function = dictionary.get(b"Function").ok()?;
        let stops = self.shading_stops(function)?;
        match shading_type {
            2 if coords.len() >= 4 => Some(Paint::LinearGradient(LinearGradient {
                start: Point::new(coords[0], coords[1]),
                end: Point::new(coords[2], coords[3]),
                stops,
                extend: GradientExtend::Pad,
            })),
            3 if coords.len() >= 6 => Some(Paint::RadialGradient(RadialGradient {
                start_center: Point::new(coords[0], coords[1]),
                start_radius: coords[2].max(0.0),
                end_center: Point::new(coords[3], coords[4]),
                end_radius: coords[5].max(0.0),
                stops,
                extend: GradientExtend::Pad,
                eccentricity: 0.0,
                angle: 0.0,
            })),
            _ => None,
        }
    }

    fn shading_stops(&self, function: &Object) -> Option<Vec<GradientStop>> {
        const SAMPLES: usize = 32;
        let mut stops = Vec::with_capacity(SAMPLES + 1);
        for index in 0..=SAMPLES {
            let t = index as f64 / SAMPLES as f64;
            let values = self.eval_function(function, t)?;
            stops.push(GradientStop {
                offset: t,
                color: rgba_from_components(&values)?,
            });
        }
        Some(stops)
    }

    fn eval_function(&self, object: &Object, t: f64) -> Option<Vec<f64>> {
        let object = self.resolve(object)?;
        match object {
            Object::Stream(stream) => self.eval_sampled_function(stream, t),
            Object::Dictionary(dictionary) => self.eval_function_dict(dictionary, t),
            _ => None,
        }
    }

    fn eval_function_dict(&self, dictionary: &Dictionary, t: f64) -> Option<Vec<f64>> {
        let function_type = dictionary
            .get(b"FunctionType")
            .ok()
            .and_then(|object| self.resolve_number(object))? as i64;
        let domain = self
            .number_array(dictionary, b"Domain")
            .unwrap_or_else(|| vec![0.0, 1.0]);
        if domain.len() < 2 || domain[1] <= domain[0] {
            return None;
        }
        let input = t.clamp(domain[0], domain[1]);
        match function_type {
            2 => {
                let c0 = self.number_array(dictionary, b"C0")?;
                let c1 = self.number_array(dictionary, b"C1")?;
                if c0.len() != c1.len() {
                    return None;
                }
                let exponent = dictionary
                    .get(b"N")
                    .ok()
                    .and_then(|object| self.resolve_number(object))
                    .unwrap_or(1.0);
                let position = (input - domain[0]) / (domain[1] - domain[0]);
                let position = if (exponent - 1.0).abs() < f64::EPSILON {
                    position
                } else {
                    position.max(0.0).powf(exponent)
                };
                Some(
                    c0.iter()
                        .zip(&c1)
                        .map(|(start, end)| start + (end - start) * position)
                        .collect(),
                )
            }
            3 => {
                let bounds = self.number_array(dictionary, b"Bounds").unwrap_or_default();
                let encode = self.number_array(dictionary, b"Encode").unwrap_or_default();
                let functions = dictionary
                    .get(b"Functions")
                    .ok()
                    .and_then(|object| self.resolve(object))
                    .and_then(|object| object.as_array().ok())?;
                if functions.is_empty() {
                    return None;
                }
                let mut index = 0;
                while index < bounds.len() && input > bounds[index] {
                    index += 1;
                }
                let low = if index == 0 {
                    domain[0]
                } else {
                    bounds[index - 1]
                };
                let high = bounds.get(index).copied().unwrap_or(domain[1]);
                let local = if high > low {
                    (input - low) / (high - low)
                } else {
                    0.0
                };
                let (encode_start, encode_end) = if encode.len() >= 2 * index + 2 {
                    (encode[2 * index], encode[2 * index + 1])
                } else {
                    (0.0, 1.0)
                };
                let sub_t = encode_start + (encode_end - encode_start) * local;
                self.eval_function(functions.get(index)?, sub_t)
            }
            _ => None,
        }
    }

    /// 采样函数（FunctionType 0）：一维输入、单通道输出。
    fn eval_sampled_function(&self, stream: &lopdf::Stream, t: f64) -> Option<Vec<f64>> {
        let function_type = stream
            .dict
            .get(b"FunctionType")
            .ok()
            .and_then(|object| self.resolve_number(object))? as i64;
        if function_type != 0 {
            return None;
        }
        let size = self.number_array(&stream.dict, b"Size")?;
        if size.is_empty() || size.iter().any(|value| *value < 1.0) {
            return None;
        }
        let count = size[0] as usize;
        let domain = self
            .number_array(&stream.dict, b"Domain")
            .unwrap_or_else(|| vec![0.0, 1.0]);
        if domain.len() < 2 || domain[1] <= domain[0] {
            return None;
        }
        let encode = self
            .number_array(&stream.dict, b"Encode")
            .unwrap_or_else(|| vec![0.0, (count - 1) as f64]);
        let decode = self
            .number_array(&stream.dict, b"Decode")
            .unwrap_or_else(|| vec![0.0, 1.0]);
        let bits = stream
            .dict
            .get(b"BitsPerSample")
            .ok()
            .and_then(|object| self.resolve_number(object))
            .unwrap_or(8.0) as usize;
        let input = t.clamp(domain[0], domain[1]);
        let position = if domain[1] > domain[0] {
            (input - domain[0]) / (domain[1] - domain[0])
        } else {
            0.0
        };
        let sample = encode[0] + (encode[1] - encode[0]) * position;
        let index = if size.len() == 1 {
            (sample.round().max(0.0) as usize).min(count - 1)
        } else {
            // 多维采样函数取全零输入，得到常量输出。
            0
        };
        let data = stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone());
        let raw = match bits {
            8 => f64::from(*data.get(index)?) / 255.0,
            16 => {
                let bytes = data.get(index * 2..index * 2 + 2)?;
                f64::from(u16::from_be_bytes([bytes[0], bytes[1]])) / 65535.0
            }
            _ => return None,
        };
        let value = decode[0] + (decode.get(1).copied().unwrap_or(1.0) - decode[0]) * raw;
        Some(vec![value])
    }

    fn resolve_number(&self, object: &Object) -> Option<f64> {
        let object = self.resolve(object).unwrap_or(object);
        object_number(object)
    }

    fn number_array(&self, dictionary: &Dictionary, key: &[u8]) -> Option<Vec<f64>> {
        dictionary
            .get(key)
            .ok()
            .and_then(|object| self.resolve(object))
            .and_then(|object| object.as_array().ok())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| self.resolve_number(value))
                    .collect()
            })
    }

    /// 解码 CCITT G3/G4 传真编码，并转成 RGBA PNG。
    fn ccitt_image(&self, stream: &lopdf::Stream, width: u32, height: u32) -> Option<Vec<u8>> {
        let params = stream_value(&stream.dict, b"DecodeParms", b"DP")
            .and_then(|object| self.resolve(object))
            .and_then(|object| match object {
                Object::Dictionary(dictionary) => Some(dictionary.clone()),
                Object::Array(values) => values
                    .first()
                    .and_then(|value| self.resolve(value))
                    .and_then(|value| value.as_dict().ok())
                    .cloned(),
                _ => None,
            });
        let number = |key: &[u8]| {
            params
                .as_ref()
                .and_then(|dictionary| dictionary.get(key).ok())
                .and_then(object_number)
        };
        let boolean = |key: &[u8], default: bool| {
            params
                .as_ref()
                .and_then(|dictionary| dictionary.get(key).ok())
                .and_then(|object| object.as_bool().ok())
                .unwrap_or(default)
        };
        let settings = CcittParams {
            k: number(b"K").unwrap_or(0.0) as i64,
            black_is_1: boolean(b"BlackIs1", false),
            end_of_line: boolean(b"EndOfLine", false),
            encoded_byte_align: boolean(b"EncodedByteAlign", false),
        };
        let rgba = decode_ccitt_rgba(&stream.content, width, height, settings)?;
        encode_rgba_png(width, height, &rgba)
    }

    /// 把 8 位、无预测器的原始像素编码成 RGBA PNG。
    fn raw_image_rgba(&self, stream: &lopdf::Stream, width: u32, height: u32) -> Option<Vec<u8>> {
        let bits = stream_number(&stream.dict, b"BitsPerComponent", b"BPC").unwrap_or(8.0);
        if bits != 8.0 {
            return None;
        }
        let colorspace = stream_value(&stream.dict, b"ColorSpace", b"CS")
            .and_then(|object| self.color_space(object))?;
        let data = stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone());
        if data.len() < width as usize * height as usize * colorspace {
            return None;
        }
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for pixel in data.chunks_exact(colorspace) {
            let (r, g, b) = match colorspace {
                1 => (pixel[0], pixel[0], pixel[0]),
                3 => (pixel[0], pixel[1], pixel[2]),
                4 => {
                    let k = f32::from(pixel[3]) / 255.0;
                    let c = f32::from(pixel[0]) / 255.0;
                    let m = f32::from(pixel[1]) / 255.0;
                    let y = f32::from(pixel[2]) / 255.0;
                    (
                        ((1.0 - c) * (1.0 - k) * 255.0) as u8,
                        ((1.0 - m) * (1.0 - k) * 255.0) as u8,
                        ((1.0 - y) * (1.0 - k) * 255.0) as u8,
                    )
                }
                _ => return None,
            };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
        if let Some(mask) = stream.dict.get(b"SMask").ok()
            && let Some(mask) = self
                .resolve(mask)
                .and_then(|object| object.as_stream().ok())
            && let Ok(mask_data) = mask.decompressed_content()
            && mask_data.len() >= rgba.len() / 4
        {
            for (pixel, alpha) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(mask_data) {
                pixel[3] = alpha;
            }
        }
        encode_rgba_png(width, height, &rgba)
    }

    fn color_space(&self, object: &Object) -> Option<usize> {
        let object = self.resolve(object).unwrap_or(object);
        match object {
            Object::Name(name) => match name.as_slice() {
                b"DeviceGray" | b"G" => Some(1),
                b"DeviceRGB" | b"RGB" => Some(3),
                b"DeviceCMYK" | b"CMYK" => Some(4),
                _ => None,
            },
            Object::Array(array) => {
                let name = array.first()?.as_name().ok()?;
                match name {
                    b"ICCBased" => {
                        let stream = self.resolve(array.get(1)?)?.as_stream().ok()?;
                        stream
                            .dict
                            .get(b"N")
                            .ok()
                            .and_then(object_number)
                            .map(|value| value as usize)
                    }
                    _ => self.color_space(array.first()?),
                }
            }
            _ => None,
        }
    }
}

fn push_clip(state: &mut State, clip: ClipState) {
    state.clips.push(clip);
}

/// 把图案矩阵应用到渐变坐标；非均匀缩放按平均比例近似半径。
fn transform_paint(paint: Paint, matrix: Matrix) -> Paint {
    match paint {
        Paint::LinearGradient(mut gradient) => {
            gradient.start = matrix.transform_point(gradient.start);
            gradient.end = matrix.transform_point(gradient.end);
            Paint::LinearGradient(gradient)
        }
        Paint::RadialGradient(mut gradient) => {
            let scale = (matrix.a.hypot(matrix.b) + matrix.c.hypot(matrix.d)) * 0.5;
            gradient.start_center = matrix.transform_point(gradient.start_center);
            gradient.end_center = matrix.transform_point(gradient.end_center);
            if scale.is_finite() && scale > 0.0 {
                gradient.start_radius *= scale;
                gradient.end_radius *= scale;
            }
            Paint::RadialGradient(gradient)
        }
        other => other,
    }
}

fn rgba_from_components(values: &[f64]) -> Option<Rgba> {
    let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    match values.len() {
        1 => {
            let value = channel(values[0]);
            Some(Rgba::new(value, value, value, 255))
        }
        3 => Some(Rgba::new(
            channel(values[0]),
            channel(values[1]),
            channel(values[2]),
            255,
        )),
        _ if values.len() >= 4 => {
            let c = values[0].clamp(0.0, 1.0);
            let m = values[1].clamp(0.0, 1.0);
            let y = values[2].clamp(0.0, 1.0);
            let k = values[3].clamp(0.0, 1.0);
            Some(Rgba::new(
                channel((1.0 - c) * (1.0 - k)),
                channel((1.0 - m) * (1.0 - k)),
                channel((1.0 - y) * (1.0 - k)),
                255,
            ))
        }
        _ => None,
    }
}

fn paint_with_alpha(paint: &Paint, alpha: f32) -> Option<Paint> {
    match paint {
        Paint::Color(color) => {
            let mut color = *color;
            color.a = (f32::from(color.a) * alpha.clamp(0.0, 1.0)).round() as u8;
            Some(Paint::Color(color))
        }
        other => Some(other.clone()),
    }
}

#[derive(Clone, Copy, Default)]
struct CcittParams {
    k: i64,
    black_is_1: bool,
    end_of_line: bool,
    encoded_byte_align: bool,
}

/// 解码 CCITT G3/G4 位流为 RGBA8；1 位表示黑色，0 位表示白色。
fn decode_ccitt_rgba(data: &[u8], width: u32, height: u32, params: CcittParams) -> Option<Vec<u8>> {
    let encoding = if params.k < 0 {
        hayro_ccitt::EncodingMode::Group4
    } else if params.k == 0 {
        hayro_ccitt::EncodingMode::Group3_1D
    } else {
        hayro_ccitt::EncodingMode::Group3_2D { k: params.k as u32 }
    };
    let settings = hayro_ccitt::DecodeSettings {
        columns: width,
        rows: height,
        end_of_block: true,
        end_of_line: params.end_of_line,
        rows_are_byte_aligned: params.encoded_byte_align,
        encoding,
        invert_black: params.black_is_1,
    };
    let mut context = hayro_ccitt::DecoderContext::new(settings);
    let mut bitmap = BitmapDecoder::default();
    hayro_ccitt::decode(data, &mut bitmap, &mut context).ok()?;
    let expected = width as usize * height as usize;
    if bitmap.pixels.len() < expected {
        return None;
    }
    let mut rgba = Vec::with_capacity(expected * 4);
    for white in bitmap.pixels.into_iter().take(expected) {
        let value = if white { 255 } else { 0 };
        rgba.extend_from_slice(&[value, value, value, 255]);
    }
    Some(rgba)
}

#[derive(Default)]
struct BitmapDecoder {
    pixels: Vec<bool>,
}

impl hayro_ccitt::Decoder for BitmapDecoder {
    fn push_pixel(&mut self, white: bool) {
        self.pixels.push(white);
    }

    fn push_pixel_chunk(&mut self, white: bool, chunk_count: u32) {
        self.pixels
            .extend(std::iter::repeat_n(white, chunk_count as usize * 8));
    }

    fn next_line(&mut self) {}
}

fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut png_bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut png_bytes, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(png_bytes)
}

fn glyph_char(face: &ttf_parser::Face<'_>, glyph: u16) -> Option<char> {
    let cmap = face.tables().cmap?;
    for subtable in cmap.subtables {
        if !subtable.is_unicode() {
            continue;
        }
        let mut result = None;
        subtable.codepoints(|codepoint| {
            if result.is_none()
                && subtable
                    .glyph_index(codepoint)
                    .is_some_and(|id| id.0 == glyph)
            {
                result = char::from_u32(codepoint);
            }
        });
        if result.is_some() {
            return result;
        }
    }
    None
}

fn glyph_advance(
    face: Option<&ttf_parser::Face<'_>>,
    glyph: u32,
    size: f64,
    units: Option<f64>,
) -> f64 {
    match (face, units) {
        (Some(face), Some(units)) => face
            .glyph_hor_advance(ttf_parser::GlyphId(glyph.min(u32::from(u16::MAX)) as u16))
            .map(|advance| f64::from(advance) / units * size)
            .unwrap_or(size * 0.5),
        _ => size * 0.5,
    }
}

fn text_bbox(run: &GlyphRun, size: f64, transform: Matrix) -> Rect {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for glyph in &run.glyphs {
        min_x = min_x.min(glyph.x);
        max_x = max_x.max(glyph.x + glyph.advance);
        min_y = min_y.min(glyph.y - size);
        max_y = max_y.max(glyph.y + size * 0.2);
    }
    if !min_x.is_finite() {
        return Rect::default();
    }
    transform.transform_rect(Rect::new(min_x, min_y, max_x - min_x, max_y - min_y))
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect::new(x, y, right - x, bottom - y)
}

fn transform_path(path: &Path, matrix: Matrix) -> Path {
    let mut output = Path::new();
    let scale_x = matrix.a.hypot(matrix.b);
    let scale_y = matrix.c.hypot(matrix.d);
    let rotation = matrix.b.atan2(matrix.a);
    let determinant = matrix.a * matrix.d - matrix.b * matrix.c;
    for segment in path.segments() {
        match *segment {
            PathSegment::MoveTo(point) => output.move_to(matrix.transform_point(point)),
            PathSegment::LineTo(point) => output.line_to(matrix.transform_point(point)),
            PathSegment::QuadTo(control, to) => {
                output.quadratic_to(matrix.transform_point(control), matrix.transform_point(to));
            }
            PathSegment::CubicTo(control_a, control_b, to) => output.cubic_to(
                matrix.transform_point(control_a),
                matrix.transform_point(control_b),
                matrix.transform_point(to),
            ),
            PathSegment::ArcTo {
                rx,
                ry,
                rotation: arc_rotation,
                large_arc,
                sweep,
                to,
            } => output.arc_to(
                (rx * scale_x).abs(),
                (ry * scale_y).abs(),
                arc_rotation + rotation,
                large_arc,
                if determinant < 0.0 { !sweep } else { sweep },
                matrix.transform_point(to),
            ),
            PathSegment::Close => output.close(),
            _ => {}
        }
    }
    output
}

fn parse_to_unicode(data: &[u8]) -> HashMap<u32, u32> {
    let text = String::from_utf8_lossy(data);
    let mut map = HashMap::new();
    let mut source: Option<u32> = None;
    for token in text.split_whitespace() {
        if let Some(hex) = token
            .strip_prefix('<')
            .and_then(|value| value.strip_suffix('>'))
            && let Ok(value) = u32::from_str_radix(hex, 16)
        {
            if let Some(source) = source.take() {
                map.insert(source, value);
            } else {
                source = Some(value);
            }
        }
    }
    map
}

fn matrix_operands(operands: &[Object]) -> Option<Matrix> {
    matrix_from_values(operands)
}

fn matrix_from_values(values: &[Object]) -> Option<Matrix> {
    let values = values.iter().filter_map(object_number).collect::<Vec<_>>();
    (values.len() >= 6).then(|| {
        Matrix::new(
            values[0], values[1], values[2], values[3], values[4], values[5],
        )
    })
}

fn point_operands(operands: &[Object]) -> Option<Point> {
    pair_operands(operands).map(|(x, y)| Point::new(x, y))
}

fn pair_operands(operands: &[Object]) -> Option<(f64, f64)> {
    Some((
        operands.first().and_then(object_number)?,
        operands.get(1).and_then(object_number)?,
    ))
}

fn cubic_operands(operands: &[Object]) -> Option<(Point, Point, Point)> {
    let values = operands
        .iter()
        .filter_map(object_number)
        .collect::<Vec<_>>();
    (values.len() >= 6).then(|| {
        (
            Point::new(values[0], values[1]),
            Point::new(values[2], values[3]),
            Point::new(values[4], values[5]),
        )
    })
}

fn quadratic_operands(operands: &[Object]) -> Option<(Point, Point)> {
    let values = operands
        .iter()
        .filter_map(object_number)
        .collect::<Vec<_>>();
    (values.len() >= 4).then(|| {
        (
            Point::new(values[0], values[1]),
            Point::new(values[2], values[3]),
        )
    })
}

fn rect_operands(operands: &[Object]) -> Option<(f64, f64, f64, f64)> {
    let values = operands
        .iter()
        .filter_map(object_number)
        .collect::<Vec<_>>();
    (values.len() >= 4).then(|| (values[0], values[1], values[2], values[3]))
}

fn components_operands(operands: &[Object]) -> Option<Rgba> {
    let values = operands
        .iter()
        .filter_map(object_number)
        .collect::<Vec<_>>();
    match values.as_slice() {
        [gray] => {
            let value = (gray.clamp(0.0, 1.0) * 255.0).round() as u8;
            Some(Rgba::new(value, value, value, 255))
        }
        [r, g, b] => Some(Rgba::new(
            (r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (b.clamp(0.0, 1.0) * 255.0).round() as u8,
            255,
        )),
        [c, m, y, k] => {
            let k = k.clamp(0.0, 1.0);
            let channel = |value: f64| ((1.0 - value.clamp(0.0, 1.0)) * (1.0 - k) * 255.0) as u8;
            Some(Rgba::new(channel(*c), channel(*m), channel(*y), 255))
        }
        _ => None,
    }
}

fn rgb_operands(operands: &[Object]) -> Option<Rgba> {
    let values = operands
        .iter()
        .filter_map(object_number)
        .collect::<Vec<_>>();
    (values.len() >= 3).then(|| {
        Rgba::new(
            (values[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (values[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (values[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            255,
        )
    })
}

fn cmyk_operands(operands: &[Object]) -> Option<Rgba> {
    let values = operands
        .iter()
        .filter_map(object_number)
        .collect::<Vec<_>>();
    (values.len() >= 4).then(|| {
        let k = values[3];
        let channel = |value: f64| ((1.0 - value) * (1.0 - k) * 255.0).clamp(0.0, 255.0) as u8;
        Rgba::new(
            channel(values[0]),
            channel(values[1]),
            channel(values[2]),
            255,
        )
    })
}

fn stream_value<'a>(
    dictionary: &'a Dictionary,
    name: &[u8],
    abbreviated: &[u8],
) -> Option<&'a Object> {
    dictionary
        .get(name)
        .ok()
        .or_else(|| dictionary.get(abbreviated).ok())
}

fn stream_number(dictionary: &Dictionary, name: &[u8], abbreviated: &[u8]) -> Option<f64> {
    stream_value(dictionary, name, abbreviated).and_then(object_number)
}

fn object_number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}

fn object_name(object: &Object) -> Option<&[u8]> {
    object.as_name().ok()
}

#[allow(dead_code)]
fn resolve<'a>(document: &'a Document, object: &'a Object) -> Option<&'a Object> {
    document.dereference(object).ok().map(|(_, object)| object)
}

#[cfg(test)]
mod tests {
    use super::{CcittParams, decode_ccitt_rgba};

    #[test]
    fn decodes_group4_ccitt() {
        // Pillow 生成的 16x8 Group 4 位流：白底、x=4..10 / y=2..5 有黑块。
        let data = [0x26, 0xa0, 0xbf, 0x2f, 0xff, 0xff, 0x1e, 0x00, 0x20, 0x02];
        let rgba = decode_ccitt_rgba(
            &data,
            16,
            8,
            CcittParams {
                k: -1,
                // TIFF 以 Photometric=1 (0=黑) 存储，需按相反极性解释。
                black_is_1: true,
                ..CcittParams::default()
            },
        )
        .expect("decode");
        let pixel = |x: usize, y: usize| &rgba[(y * 16 + x) * 4..][..4];
        assert_eq!(pixel(0, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(5, 3), [0, 0, 0, 255]);
        assert_eq!(pixel(15, 7), [255, 255, 255, 255]);
    }
}
