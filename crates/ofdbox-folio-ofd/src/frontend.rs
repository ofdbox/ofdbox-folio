//! OFD 页面前端：把 OFD 内容转换为 OFDBox Folio 的 Fixed IR。
//!
//! 读取基于 ofdbox 0.0.2 的强类型 `model` 与 `Stored<T>` 会话：
//! 页面内容、模板页与资源都通过 [`crate::loc`] 按需加载，路径解析兼容
//! 相对路径、包根路径与大小写差异。
//!
//! 真实文件里很多属性缺失时依赖 XSD 默认值（例如 `LineWidth` 缺省为
//! 0.353、`Visible` 缺省为 true）。强类型模型把「缺失」折叠成默认值，
//! 因此这里用「与默认值比较」还原旧 `*View` API 的 `Some`/`None` 语义。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use ofdbox_core::model::definition::{PageArea, StArray, StBox, StId, StLoc, StRefId};
use ofdbox_core::model::document::{Document, PageZOrder};
use ofdbox_core::model::page::{
    Cap, ClipContent, Color, ColorKind, CompositeObject, GraphicObject, GraphicUnit, ImageObject,
    Join, MapType, Page as PageModel, PageContent, PathObject, PathRule, Pattern, PatternReflect,
    PatternRelativeTo, ShdSegment, Template, TemplateZOrder, TextObject,
};
use ofdbox_core::model::res::{Charset, CompositeGraphicUnit, DrawParam, Res};
use ofdbox_folio_ir::{
    Clip, Cluster, Diagnostic, DiagnosticCode, Element, FillRule, FontAsset, FontProvider,
    FontRequest, Glyph, GlyphRun, GradientExtend, GradientStop, Group, ImageAsset, ImageElement,
    ImageFormat, LineCap, LineJoin, LinearGradient, Matrix, Page, PageInfo, Paint, Path,
    PathElement, PathSegment, Point, RadialGradient, Rect, Rgba, Rotation, Scene, Size,
    StrokePaint, StrokeStyle, TextElement, TextItem, TextLayer, TextLine, detect_image_format,
    image_dimensions,
};

use crate::OfdError;
use crate::package::Loaded;

/// OFD 中 `LineWidth` 的默认值（毫米）。
const DEFAULT_LINE_WIDTH: f64 = 0.353;
/// OFD 中 `MiterLimit` 的默认值。
const DEFAULT_MITER_LIMIT: f64 = 4.234;

/// OFD 前端选项。
#[derive(Clone, Debug)]
pub struct OfdReadOptions {
    /// 是否绘制注释。
    pub draw_annotations: bool,
    /// 遇到不支持特性时的行为。
    pub unsupported: UnsupportedPolicy,
    /// 单张图片允许的最大像素数。
    pub max_image_pixels: u64,
    /// 宽松解析模式。
    pub lenient: crate::LenientMode,
    /// 加密文档的口令。
    pub password: Option<String>,
    /// 加密文档的证书私钥（32 字节 SM2 标量）。
    pub certificate_private_key: Option<Vec<u8>>,
}

impl Default for OfdReadOptions {
    fn default() -> Self {
        Self {
            draw_annotations: true,
            unsupported: UnsupportedPolicy::Skip,
            max_image_pixels: 64 * 1024 * 1024,
            lenient: crate::LenientMode::OnFailure,
            password: None,
            certificate_private_key: None,
        }
    }
}

/// 不支持特性的处理策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedPolicy {
    /// 跳过并记录诊断。
    Skip,
    /// 返回错误。
    Error,
}

/// 构建一页 Fixed IR。
pub fn build_page(
    document: &Loaded<Document>,
    page: &Loaded<PageModel>,
    options: &OfdReadOptions,
    store: &mut ofdbox_folio_ir::ResourceStore,
    font_cache: &mut HashMap<String, Option<ResolvedFont>>,
    font_provider: &mut Option<Box<dyn FontProvider>>,
) -> Result<Page, OfdError> {
    let scene = build_scene(document, page, options, store, font_cache, font_provider)?;
    let text = build_text_layer(&scene);
    let size = scene.size;
    let info = PageInfo {
        media_box: Rect::new(0.0, 0.0, size.width, size.height),
        crop_box: Rect::new(0.0, 0.0, size.width, size.height),
        rotation: Rotation::None,
        label: None,
    };
    Ok(Page {
        info,
        content: scene,
        text,
        templates: Vec::new(),
        links: Vec::new(),
    })
}

fn build_scene(
    document: &Loaded<Document>,
    page: &Loaded<PageModel>,
    options: &OfdReadOptions,
    store: &mut ofdbox_folio_ir::ResourceStore,
    font_cache: &mut HashMap<String, Option<ResolvedFont>>,
    font_provider: &mut Option<Box<dyn FontProvider>>,
) -> Result<Scene, OfdError> {
    let common = &document.data.common_data;

    // 模板页：`CommonData/TemplatePage` 按数字 ID 建立索引；页内 `Template`
    // 用 `TemplateID` 引用，ZOrder 缺省时沿用模板页自身的声明。
    let mut templates = HashMap::new();
    for template in &common.template_pages {
        let id = template.id.to_string();
        let Ok(content) = document.context().load_page(&template.base_loc) else {
            continue;
        };
        templates.insert(
            id.clone(),
            TemplateEntry {
                id,
                z_order: template.z_order.clone().unwrap_or_default(),
                content: Rc::new(content),
            },
        );
    }

    let mut resources = ResourceSet::default();
    resources.collect_document(document);
    for template in templates.values() {
        resources.collect_page(&template.content);
    }
    resources.collect_page(page);

    let physical_box = page_area_box(page.data.area.as_ref())
        .or_else(|| {
            page.data.templates.iter().find_map(|reference| {
                let template = templates.get(&reference.template_id.to_string())?;
                page_area_box(template.content.data.area.as_ref())
            })
        })
        .or_else(|| declared_box(common.page_area.physical_box))
        .unwrap_or_else(|| StBox::new(0.0, 0.0, 210.0, 297.0));
    let physical = Rect::new(
        physical_box.x,
        physical_box.y,
        physical_box.width,
        physical_box.height,
    );
    let default_color_space = common.default_cs.map(|id| id.to_string());
    let page_id = page.id().map(str::to_owned);
    let mut builder = Builder {
        document,
        page_id: page_id.clone(),
        store,
        resources,
        templates,
        default_color_space,
        font_cache,
        font_provider,
        diagnostics: Vec::new(),
        depth: 0,
        composites: HashSet::new(),
        layer_draw_param: None,
    };
    if page.data.area.is_none() && common.page_area.physical_box == StBox::default() {
        builder.diagnostic(
            DiagnosticCode::Geometry,
            page_id.as_deref(),
            "页面未声明尺寸，使用 A4 默认值",
        );
    }
    let mut children = Vec::new();
    let mut visited_templates = HashSet::new();
    let references = page.data.templates.as_slice();
    builder.render_templates(
        references,
        TemplateOrder::Background,
        &mut children,
        &mut visited_templates,
    );
    builder.render_templates(
        references,
        TemplateOrder::Body,
        &mut children,
        &mut visited_templates,
    );
    if let Some(content) = &page.data.content {
        builder.render_layers(content, &mut children);
    }
    builder.render_templates(
        references,
        TemplateOrder::Foreground,
        &mut children,
        &mut visited_templates,
    );
    if options.draw_annotations {
        builder.render_annotations(&mut children);
    }
    if options.unsupported == UnsupportedPolicy::Error
        && let Some(diagnostic) = builder.diagnostics.first()
    {
        return Err(OfdError::Unsupported(diagnostic.message.clone()));
    }
    let mut root = Group::new(Matrix::translate(-physical.x, -physical.y));
    root.children = children;
    Ok(Scene {
        size: Size::new(physical.width, physical.height),
        root,
        diagnostics: builder.diagnostics,
    })
}

/// `StBox` 全零等价于「没有声明」（模型用默认值表达属性缺失）。
fn declared_box(value: StBox) -> Option<StBox> {
    (value != StBox::default()).then_some(value)
}

/// 页面区域里声明的物理框。
fn page_area_box(area: Option<&PageArea>) -> Option<StBox> {
    area.and_then(|area| declared_box(area.physical_box))
}

#[derive(Default)]
struct ResourceSet {
    fonts: HashMap<String, FontResource>,
    images: HashMap<String, MediaResource>,
    colors: HashMap<String, ColorSpaceResource>,
    draw_params: HashMap<String, DrawParamResource>,
    composites: HashMap<String, CompositeGraphicUnit>,
}

#[derive(Clone)]
struct FontResource {
    id: String,
    file: Option<StLoc>,
    name: Option<String>,
    family: Option<String>,
    charset: Option<String>,
    bold: bool,
    italic: bool,
    serif: bool,
    fixed_width: bool,
    /// 字体所属的 `Res`，用于读取 `FontFile`。
    res: Rc<Loaded<Res>>,
}

fn font_asset(data: Arc<[u8]>, resource: &FontResource) -> FontAsset {
    let mut asset = FontAsset::new(data, 0);
    if let Some(name) = &resource.name {
        asset = asset.with_font_name(name.clone());
    }
    if let Some(family) = &resource.family {
        asset = asset.with_family(family.clone());
    }
    asset
}

#[derive(Clone)]
pub(crate) struct ResolvedFont {
    asset: Arc<FontAsset>,
    fallback: bool,
}

#[derive(Clone)]
struct MediaResource {
    format: ImageFormat,
    file: StLoc,
    /// 媒体资源所属的 `Res`，用于读取 `MediaFile`。
    res: Rc<Loaded<Res>>,
}

struct ColorSpaceResource {
    kind: String,
    /// 每分量位数，标准允许 1/2/4/8/16，默认 8。
    bits: u32,
    palette: Vec<StArray>,
}

#[derive(Clone, Default)]
struct DrawParamResource {
    relative: Option<StRefId>,
    line_width: Option<f64>,
    join: Option<LineJoin>,
    cap: Option<LineCap>,
    dash_offset: Option<f64>,
    dash: Option<Vec<f64>>,
    miter_limit: Option<f64>,
    fill_color: Option<Color>,
    stroke_color: Option<Color>,
}

impl ResourceSet {
    fn collect_document(&mut self, document: &Loaded<Document>) {
        let common = &document.data.common_data;
        for location in common.public_res.iter().chain(common.document_res.iter()) {
            if let Ok(res) = document.context().load_res(location) {
                self.collect_res(res);
            }
        }
    }

    fn collect_page(&mut self, page: &Loaded<PageModel>) {
        for location in &page.data.page_res {
            if let Ok(res) = page.context().load_res(location) {
                self.collect_res(res);
            }
        }
    }

    fn collect_res(&mut self, res: Rc<Loaded<Res>>) {
        if let Some(fonts) = &res.data.fonts {
            for font in &fonts.fonts {
                self.fonts.insert(
                    font.id.to_string(),
                    FontResource {
                        id: font.id.to_string(),
                        file: font.font_file.clone(),
                        name: Some(font.font_name.trim().to_owned())
                            .filter(|name| !name.is_empty()),
                        family: font
                            .family_name
                            .as_ref()
                            .map(|family| family.trim().to_owned())
                            .filter(|family| !family.is_empty()),
                        // 默认字符集是 unicode，按「缺省」处理以保持旧行为。
                        charset: (font.charset != Charset::Unicode)
                            .then(|| font.charset.to_string()),
                        bold: font.bold,
                        italic: font.italic,
                        serif: font.serif,
                        fixed_width: font.fixed_width,
                        res: Rc::clone(&res),
                    },
                );
            }
        }
        if let Some(medias) = &res.data.multi_medias {
            for media in &medias.multi_medias {
                if media.media_file.as_str().trim().is_empty() {
                    continue;
                }
                let format = media
                    .format
                    .as_deref()
                    .map(image_format_from_media)
                    .unwrap_or_else(|| image_format_from_path(media.media_file.as_str()));
                self.images.insert(
                    media.id.to_string(),
                    MediaResource {
                        format,
                        file: media.media_file.clone(),
                        res: Rc::clone(&res),
                    },
                );
            }
        }
        if let Some(spaces) = &res.data.color_spaces {
            for color in &spaces.color_spaces {
                let bits = u32::try_from(color.bits_per_component)
                    .ok()
                    .filter(|value| (1..=16).contains(value))
                    .unwrap_or(8);
                self.colors.insert(
                    color.id.to_string(),
                    ColorSpaceResource {
                        kind: color.color_space_type.to_string(),
                        bits,
                        palette: color
                            .palette
                            .as_ref()
                            .map(|palette| palette.values.clone())
                            .unwrap_or_default(),
                    },
                );
            }
        }
        if let Some(params) = &res.data.draw_params {
            for param in &params.draw_params {
                self.draw_params
                    .insert(param.id.to_string(), draw_param(param));
            }
        }
        if let Some(units) = &res.data.composite_graphic_units {
            for unit in &units.units {
                self.composites.insert(unit.id.to_string(), unit.clone());
            }
        }
    }
}

fn apply_draw_param(style: &mut StrokeStyle, param: &DrawParamResource) {
    if let Some(width) = param.line_width {
        style.width = width;
    }
    if let Some(join) = param.join {
        style.join = join;
    }
    if let Some(cap) = param.cap {
        style.cap = cap;
    }
    if let Some(offset) = param.dash_offset {
        style.dash_offset = offset;
    }
    if let Some(dash) = &param.dash {
        style.dash = dash.clone();
    }
    if let Some(miter) = param.miter_limit {
        style.miter_limit = miter;
    }
}

/// `DrawParam` 的强类型属性始终带 XSD 默认值；与默认值相等时视为「未声明」，
/// 这样 `Relative` 继承链的行为与旧 `*View` 的属性读取一致。
fn draw_param(param: &DrawParam) -> DrawParamResource {
    DrawParamResource {
        relative: param.relative,
        line_width: (param.line_width != DEFAULT_LINE_WIDTH).then_some(param.line_width),
        join: (param.join != "Miter")
            .then(|| parse_join(&param.join))
            .flatten(),
        cap: (param.cap != "Butt")
            .then(|| parse_cap(&param.cap))
            .flatten(),
        dash_offset: (param.dash_offset != 0.0).then_some(param.dash_offset),
        dash: param.dash_pattern.as_ref().map(|pattern| pattern.0.clone()),
        miter_limit: (param.miter_limit != DEFAULT_MITER_LIMIT).then_some(param.miter_limit),
        fill_color: param.fill_color.clone(),
        stroke_color: param.stroke_color.clone(),
    }
}

struct Builder<'a> {
    document: &'a Loaded<Document>,
    page_id: Option<String>,
    store: &'a mut ofdbox_folio_ir::ResourceStore,
    resources: ResourceSet,
    templates: HashMap<String, TemplateEntry>,
    default_color_space: Option<String>,
    font_cache: &'a mut HashMap<String, Option<ResolvedFont>>,
    font_provider: &'a mut Option<Box<dyn FontProvider>>,
    diagnostics: Vec<Diagnostic>,
    depth: usize,
    composites: HashSet<String>,
    layer_draw_param: Option<String>,
}

fn object_common(object: &GraphicObject) -> Option<&GraphicUnit> {
    match object {
        GraphicObject::Text(object) => Some(&object.content.graphic),
        GraphicObject::Path(object) => Some(&object.content.graphic),
        GraphicObject::Image(object) => Some(&object.content.graphic),
        GraphicObject::Composite(object) => Some(&object.content.graphic),
        GraphicObject::PageBlock(_) => None,
    }
}

/// 图元诊断里的对象标识；ID 缺省（0）时不带 location。
fn id_label(id: StId) -> Option<String> {
    (id.0 != 0).then(|| id.to_string())
}

/// 图元对象的 ID。
fn object_id(object: &GraphicObject) -> StId {
    match object {
        GraphicObject::Text(object) => object.id,
        GraphicObject::Path(object) => object.id,
        GraphicObject::Image(object) => object.id,
        GraphicObject::Composite(object) => object.id,
        GraphicObject::PageBlock(block) => block.id,
    }
}

fn matrix_from_array(values: &StArray) -> Option<Matrix> {
    let values = &values.0;
    if values.len() != 6 {
        return None;
    }
    Some(Matrix::new(
        values[0], values[1], values[2], values[3], values[4], values[5],
    ))
}

fn point_from_pos(position: ofdbox_core::model::definition::StPos) -> Point {
    Point::new(position.x, position.y)
}

impl Builder<'_> {
    fn render_object(&mut self, object: &GraphicObject, output: &mut Vec<Element>) {
        if let GraphicObject::PageBlock(block) = object {
            let label = (block.id.0 != 0).then(|| block.id.to_string());
            if self.depth > 8 {
                self.diagnostic(
                    DiagnosticCode::UnsupportedObject,
                    label.as_deref(),
                    "对象嵌套过深",
                );
                return;
            }
            self.depth += 1;
            for child in &block.block.objects {
                self.render_object(child, output);
            }
            self.depth -= 1;
            return;
        }
        let Some(common) = object_common(object) else {
            return;
        };
        if !common.visible {
            return;
        }
        let label = id_label(object_id(object));
        let id = label.as_deref();
        if self.depth > 8 {
            self.diagnostic(DiagnosticCode::UnsupportedObject, id, "对象嵌套过深");
            return;
        }
        let boxed = common.boundary;
        if boxed.width <= 0.0 || boxed.height <= 0.0 {
            let message = if boxed == StBox::default() {
                "缺少有效 Boundary"
            } else {
                "Boundary 尺寸无效"
            };
            self.diagnostic(DiagnosticCode::Geometry, id, message);
            return;
        }
        let boundary = Rect::new(boxed.x, boxed.y, boxed.width, boxed.height);
        let ctm = match &common.ctm {
            Some(values) => match matrix_from_array(values) {
                Some(matrix) => matrix,
                None => {
                    self.diagnostic(DiagnosticCode::Geometry, id, "CTM 无效");
                    return;
                }
            },
            None => Matrix::IDENTITY,
        };

        let mut elements = Vec::new();
        match object {
            GraphicObject::Path(path) => {
                if let Some(element) = self.build_path(path, &boundary) {
                    elements.push(element);
                }
            }
            GraphicObject::Text(text) => {
                elements.extend(self.build_text(text));
            }
            GraphicObject::Image(image) => {
                if let Some(element) = self.build_image(image) {
                    elements.push(element);
                }
            }
            GraphicObject::Composite(composite) => {
                elements.extend(self.build_composite(composite, &boundary));
            }
            GraphicObject::PageBlock(_) => {}
        }
        if elements.is_empty() {
            return;
        }

        // 裁剪区位于对象空间（Boundary 局部坐标），不能被对象 CTM 缩放；
        // 先做对象空间裁剪，再对内容应用 CTM。
        let mut content = Group::new(ctm);
        content.children = elements;
        let mut clipped = vec![Element::Group(content)];
        for path in self.object_clips(object) {
            clipped = vec![Element::Group(clip_group(path, Matrix::IDENTITY, clipped))];
        }
        // Boundary 左上角把对象空间映射到父空间。
        let mut object_group = Group::new(Matrix::translate(boundary.x, boundary.y));
        object_group.opacity = (common.alpha.clamp(0, 255) as f32) / 255.0;
        object_group.children = clipped;

        // Boundary 是父空间中的外接矩形，超出部分裁剪。
        let mut wrapper = Group::new(Matrix::IDENTITY);
        wrapper.clip = Some(Arc::new(Clip {
            path: Arc::new(boundary.to_path()),
            rule: FillRule::NonZero,
        }));
        wrapper.children.push(Element::Group(object_group));
        output.push(Element::Group(wrapper));
        if let GraphicObject::Image(image) = object
            && let Some(border) = self.build_image_border(image, &boundary)
        {
            output.push(border);
        }
    }

    fn render_layers(&mut self, content: &PageContent, output: &mut Vec<Element>) {
        for layer in &content.layers {
            let previous = self.layer_draw_param.clone();
            self.layer_draw_param = layer.draw_param.map(|id| id.to_string());
            for object in &layer.objects {
                self.render_object(object, output);
            }
            self.layer_draw_param = previous;
        }
    }

    fn build_image_border(&mut self, object: &ImageObject, boundary: &Rect) -> Option<Element> {
        let border = object.content.border.as_ref()?;
        // `Border.LineWidth` 缺省为 0.353；显式给 0 表示不画边框。
        let line_width = border.line_width;
        if line_width <= 0.0 {
            return None;
        }
        let label = id_label(object.id);
        let paint = border
            .color
            .as_ref()
            .and_then(|color| self.paint(color, label.as_deref()))
            .unwrap_or(Paint::Color(Rgba::BLACK));
        let horizontal = border.horizontal_corner_radius;
        let vertical = border.vertical_corner_radius;
        let path = rounded_rect_path(*boundary, horizontal, vertical);
        let style = StrokeStyle {
            width: line_width,
            dash_offset: border.dash_offset,
            dash: border
                .dash_pattern
                .as_ref()
                .map(|pattern| pattern.0.clone())
                .unwrap_or_default(),
            ..StrokeStyle::default()
        };
        Some(Element::Path(PathElement {
            path: Arc::new(path),
            fill: None,
            stroke: Some(StrokePaint { paint, style }),
            rule: FillRule::NonZero,
            transform: Matrix::IDENTITY,
        }))
    }

    fn object_clips(&mut self, object: &GraphicObject) -> Vec<Arc<Path>> {
        let Some(common) = object_common(object) else {
            return Vec::new();
        };
        let Some(clips) = &common.clips else {
            return Vec::new();
        };
        let label = id_label(object_id(object));
        let id = label.as_deref();
        let mut paths = Vec::new();
        for clip in &clips.clips {
            // 同一 Clip 下的多个 Area 是并集，合并为一条路径；多个 Clip 之间再取交集。
            let mut union = Path::new();
            let mut has_area = false;
            for area in &clip.areas {
                let ctm = area
                    .ctm
                    .as_ref()
                    .and_then(matrix_from_array)
                    .unwrap_or(Matrix::IDENTITY);
                match &area.content {
                    ClipContent::Path(content) => {
                        if !content.content.abbreviated_data.is_empty()
                            && let Some(path) = parse_path_data(&content.content.abbreviated_data)
                        {
                            append_transformed_path(&mut union, &path, ctm);
                            has_area = true;
                        }
                    }
                    ClipContent::Text(content) => {
                        // 标准允许文字轮廓作为裁剪区；当前按外接矩形近似。
                        let boxed = content.content.graphic.boundary;
                        if boxed != StBox::default() {
                            let boundary = Rect::new(boxed.x, boxed.y, boxed.width, boxed.height);
                            append_transformed_path(&mut union, &boundary.to_path(), ctm);
                            has_area = true;
                            self.diagnostic(
                                DiagnosticCode::UnsupportedObject,
                                id,
                                "文本裁剪按外接矩形近似",
                            );
                        }
                    }
                    ClipContent::Empty => {}
                }
            }
            if has_area {
                paths.push(Arc::new(union));
            }
        }
        paths
    }

    fn build_path(&mut self, object: &PathObject, boundary: &Rect) -> Option<Element> {
        let data = object.content.abbreviated_data.as_str();
        if data.trim().is_empty() {
            return None;
        }
        let label = id_label(object.id);
        let id = label.as_deref();
        let path = match parse_path_data(data) {
            Some(path) => path,
            None => {
                self.diagnostic(DiagnosticCode::Geometry, id, "路径数据无效");
                return None;
            }
        };
        let fill = object.content.fill;
        let stroke = object.content.stroke;
        let fill_color = if fill {
            self.fill_color(&object.content.graphic, object.content.fill_color.as_ref())
        } else {
            None
        };
        let pattern = match fill_color.as_ref().map(|color| &color.kind) {
            Some(ColorKind::Pattern(pattern)) => Some(pattern),
            _ => None,
        };
        let mesh = match fill_color.as_ref().map(|color| &color.kind) {
            Some(ColorKind::GouraudShd(_) | ColorKind::LaGouraudShd(_)) => fill_color.as_ref(),
            _ => None,
        };
        let fill_paint = if pattern.is_none() && mesh.is_none() {
            fill_color.as_ref().and_then(|color| self.paint(color, id))
        } else {
            None
        };
        let stroke_paint = if stroke {
            let paint = self
                .stroke_color(
                    &object.content.graphic,
                    object.content.stroke_color.as_ref(),
                )
                .and_then(|color| self.paint(&color, id))
                .or(Some(Paint::Color(Rgba::BLACK)));
            paint.map(|paint| StrokePaint {
                paint,
                style: self.stroke_style(&object.content.graphic),
            })
        } else {
            None
        };
        let rule = path_rule(&object.content.rule);
        if pattern.is_some() || mesh.is_some() {
            let fill_element = if let Some(pattern) = pattern {
                let alpha = fill_color.as_ref().map_or(255, |color| color.alpha);
                self.build_pattern_fill(boundary, &path, pattern, alpha, rule, id)
            } else if let Some(color) = mesh {
                self.build_mesh_fill(&path, color, rule, id)
            } else {
                None
            };
            let mut children = Vec::new();
            if let Some(element) = fill_element {
                children.push(element);
            }
            if let Some(stroke) = stroke_paint {
                children.push(Element::Path(PathElement {
                    path: Arc::new(path),
                    fill: None,
                    stroke: Some(stroke),
                    rule,
                    transform: Matrix::IDENTITY,
                }));
            }
            if children.is_empty() {
                return None;
            }
            let mut group = Group::new(Matrix::IDENTITY);
            group.children = children;
            return Some(Element::Group(group));
        }
        let gradient_clip = if fill_paint.is_some() {
            fill_color.as_ref().and_then(gradient_fill_clip)
        } else {
            None
        };
        if let Some(clip) = gradient_clip {
            let fill = Element::Path(PathElement {
                path: Arc::new(path.clone()),
                fill: fill_paint,
                stroke: None,
                rule,
                transform: Matrix::IDENTITY,
            });
            let mut children = vec![Element::Group(clip_group(
                Arc::new(clip),
                Matrix::IDENTITY,
                vec![fill],
            ))];
            if let Some(stroke) = stroke_paint {
                children.push(Element::Path(PathElement {
                    path: Arc::new(path),
                    fill: None,
                    stroke: Some(stroke),
                    rule,
                    transform: Matrix::IDENTITY,
                }));
            }
            let mut group = Group::new(Matrix::IDENTITY);
            group.children = children;
            return Some(Element::Group(group));
        }
        Some(Element::Path(PathElement {
            path: Arc::new(path),
            fill: fill_paint,
            stroke: stroke_paint,
            rule,
            transform: Matrix::IDENTITY,
        }))
    }

    /// 把底纹展开为按 XStep/YStep 排列的单元，再按填充路径裁剪。
    fn build_pattern_fill(
        &mut self,
        boundary: &Rect,
        path: &Path,
        pattern: &Pattern,
        alpha: i32,
        rule: FillRule,
        object_id: Option<&str>,
    ) -> Option<Element> {
        let width = pattern.width;
        let height = pattern.height;
        if !(width > 0.0 && height > 0.0) {
            self.diagnostic(DiagnosticCode::Paint, object_id, "底纹尺寸无效");
            return None;
        }
        let x_step = pattern.x_step.unwrap_or(width).max(width);
        let y_step = pattern.y_step.unwrap_or(height).max(height);
        let relative_to_page = matches!(&pattern.relative_to, PatternRelativeTo::Page)
            || matches!(&pattern.relative_to, PatternRelativeTo::Other(value) if value.eq_ignore_ascii_case("Page"));
        let origin_x = if relative_to_page { -boundary.x } else { 0.0 };
        let origin_y = if relative_to_page { -boundary.y } else { 0.0 };
        let start_col = ((0.0 - origin_x) / x_step).floor() as i64;
        let end_col = ((boundary.width - origin_x) / x_step).ceil() as i64;
        let start_row = ((0.0 - origin_y) / y_step).floor() as i64;
        let end_row = ((boundary.height - origin_y) / y_step).ceil() as i64;
        let columns = end_col - start_col + 1;
        let rows = end_row - start_row + 1;
        if columns <= 0 || rows <= 0 {
            return None;
        }
        let count = i128::from(columns) * i128::from(rows);
        if count > 4096 {
            self.diagnostic(DiagnosticCode::Paint, object_id, "底纹单元过多");
            return None;
        }

        let mut tile = Vec::new();
        self.depth += 1;
        for child in &pattern.cell_content.objects {
            self.render_object(child, &mut tile);
        }
        self.depth -= 1;
        if tile.is_empty() {
            return None;
        }

        let mirror_columns = matches!(
            &pattern.reflect_method,
            PatternReflect::Column | PatternReflect::RowAndColumn
        ) || matches!(&pattern.reflect_method, PatternReflect::Other(value)
                if value.eq_ignore_ascii_case("Column") || value.eq_ignore_ascii_case("RowAndColumn"));
        let mirror_rows = matches!(
            &pattern.reflect_method,
            PatternReflect::Row | PatternReflect::RowAndColumn
        ) || matches!(&pattern.reflect_method, PatternReflect::Other(value)
                if value.eq_ignore_ascii_case("Row") || value.eq_ignore_ascii_case("RowAndColumn"));
        // 相邻单元在公共边各自抗锯齿会留下白缝，放大单元并向外扩一点制造重叠。
        const PATTERN_OVERLAP: f64 = 0.25;
        let factor_x = (width + PATTERN_OVERLAP * 2.0) / width;
        let factor_y = (height + PATTERN_OVERLAP * 2.0) / height;
        let tile_clip = Arc::new(Rect::new(0.0, 0.0, width, height).to_path());
        let mut cells = Vec::with_capacity(count as usize);
        for row in start_row..=end_row {
            for col in start_col..=end_col {
                let scale_x = if mirror_columns && col.rem_euclid(2) == 1 {
                    -1.0
                } else {
                    1.0
                };
                let scale_y = if mirror_rows && row.rem_euclid(2) == 1 {
                    -1.0
                } else {
                    1.0
                };
                let x = origin_x + col as f64 * x_step;
                let y = origin_y + row as f64 * y_step;
                let translate_x = if scale_x < 0.0 {
                    x + width + PATTERN_OVERLAP
                } else {
                    x - PATTERN_OVERLAP
                };
                let translate_y = if scale_y < 0.0 {
                    y + height + PATTERN_OVERLAP
                } else {
                    y - PATTERN_OVERLAP
                };
                let transform = Matrix::scale(scale_x * factor_x, scale_y * factor_y)
                    .then(Matrix::translate(translate_x, translate_y));
                let mut cell = Group::new(transform);
                cell.clip = Some(Arc::new(Clip {
                    path: Arc::clone(&tile_clip),
                    rule: FillRule::NonZero,
                }));
                cell.children = tile.clone();
                cells.push(Element::Group(cell));
            }
        }

        let mut arrangement = Group::new(
            pattern
                .ctm
                .as_ref()
                .and_then(matrix_from_array)
                .unwrap_or(Matrix::IDENTITY),
        );
        arrangement.children = cells;
        let mut fill = Group::new(Matrix::IDENTITY);
        fill.clip = Some(Arc::new(Clip {
            path: Arc::new(path.clone()),
            rule,
        }));
        fill.opacity = (alpha.clamp(0, 255) as f32) / 255.0;
        fill.children.push(Element::Group(arrangement));
        Some(Element::Group(fill))
    }

    /// 把高洛德/网格高洛德渐变细分并展开为纯色三角形。
    ///
    /// vello_cpu/peniko 没有网格渐变画笔，这里按控制点做重心细分，
    /// 用每个子三角形的平均色近似渐变；网格高洛德按 `VerticesPerRow` 切分四边形。
    fn build_mesh_fill(
        &mut self,
        path: &Path,
        color: &Color,
        rule: FillRule,
        object_id: Option<&str>,
    ) -> Option<Element> {
        let alpha = (color.alpha.clamp(0, 255) as f32) / 255.0;
        let (points, vertices_per_row, back_color, extend) = match &color.kind {
            ColorKind::GouraudShd(shd) => {
                let points = shd
                    .points
                    .iter()
                    .filter_map(|point| {
                        self.mesh_point(Point::new(point.x, point.y), &point.color, alpha)
                    })
                    .collect::<Vec<_>>();
                let back_color =
                    shd.back_color
                        .as_deref()
                        .and_then(|back| match self.solid_paint(back) {
                            Some(Paint::Color(value)) => Some(scale_alpha(value, alpha)),
                            _ => None,
                        });
                (points, None, back_color, shd.extend.unwrap_or(0) != 0)
            }
            ColorKind::LaGouraudShd(shd) => {
                let points = shd
                    .points
                    .iter()
                    .filter_map(|point| {
                        self.mesh_point(Point::new(point.x?, point.y?), &point.color, alpha)
                    })
                    .collect::<Vec<_>>();
                let back_color =
                    shd.back_color
                        .as_deref()
                        .and_then(|back| match self.solid_paint(back) {
                            Some(Paint::Color(value)) => Some(scale_alpha(value, alpha)),
                            _ => None,
                        });
                let per_row = (shd.vertices_per_row > 0)
                    .then(|| usize::try_from(shd.vertices_per_row).ok())
                    .flatten();
                (points, per_row, back_color, shd.extend.unwrap_or(0) != 0)
            }
            _ => return None,
        };

        let mut triangles = Vec::new();
        match vertices_per_row {
            Some(per_row) if per_row >= 2 && points.len() >= per_row * 2 => {
                let rows = points.len() / per_row;
                for row in 0..rows - 1 {
                    for column in 0..per_row - 1 {
                        let index = row * per_row + column;
                        let a = points[index];
                        let b = points[index + 1];
                        let c = points[index + per_row];
                        let d = points[index + per_row + 1];
                        triangles.push([a, b, d]);
                        triangles.push([a, d, c]);
                    }
                }
            }
            _ => {
                if points.len() < 3 {
                    self.diagnostic(DiagnosticCode::Paint, object_id, "网格渐变控制点不足");
                    return None;
                }
                for index in 1..points.len() - 1 {
                    triangles.push([points[0], points[index], points[index + 1]]);
                }
            }
        }
        if triangles.is_empty() {
            return None;
        }
        const MAX_TRIANGLES: usize = 8192;
        if triangles.len() > MAX_TRIANGLES {
            self.diagnostic(DiagnosticCode::Paint, object_id, "网格渐变单元过多");
            triangles.truncate(MAX_TRIANGLES);
        }
        let mut depth = 3u8;
        while depth > 0
            && triangles.len().saturating_mul(4usize.pow(u32::from(depth))) > MAX_TRIANGLES
        {
            depth -= 1;
        }
        let mut output = Vec::new();
        for triangle in &triangles {
            subdivide_mesh(*triangle, depth, &mut output);
        }

        let mut fill = Group::new(Matrix::IDENTITY);
        fill.clip = Some(Arc::new(Clip {
            path: Arc::new(path.clone()),
            rule,
        }));
        if extend && let Some(back) = back_color {
            fill.children.push(Element::Path(PathElement {
                path: Arc::new(path.clone()),
                fill: Some(Paint::Color(back)),
                stroke: None,
                rule,
                transform: Matrix::IDENTITY,
            }));
        }
        // 先铺一层原始三角形的平均色，细分块之间的抗锯齿缝隙不会再露出背景色。
        for triangle in &triangles {
            fill.children.push(Element::Path(PathElement {
                path: Arc::new(triangle_path(triangle)),
                fill: Some(Paint::Color(average_color(triangle))),
                stroke: None,
                rule: FillRule::NonZero,
                transform: Matrix::IDENTITY,
            }));
        }
        for triangle in output {
            fill.children.push(Element::Path(PathElement {
                path: Arc::new(triangle_path(&triangle)),
                fill: Some(Paint::Color(average_color(&triangle))),
                stroke: None,
                rule: FillRule::NonZero,
                transform: Matrix::IDENTITY,
            }));
        }
        Some(Element::Group(fill))
    }

    fn mesh_point(&self, position: Point, color: &Color, alpha: f32) -> Option<MeshPoint> {
        let value = match self.solid_paint(color)? {
            Paint::Color(value) => value,
            _ => return None,
        };
        Some(MeshPoint {
            position,
            color: scale_alpha(value, alpha),
        })
    }

    fn build_text(&mut self, object: &TextObject) -> Vec<Element> {
        let label = id_label(object.id);
        let id = label.as_deref();
        if object.content.font.0 == 0 {
            self.diagnostic(DiagnosticCode::Font, id, "文字缺少 Font");
            return Vec::new();
        }
        let font_id = object.content.font.to_string();
        let size = object.content.size;
        if size == 0.0 {
            self.diagnostic(DiagnosticCode::Geometry, id, "文字缺少有效 Size");
            return Vec::new();
        }
        if size < 0.0 {
            self.diagnostic(DiagnosticCode::Geometry, id, "字号必须为正");
            return Vec::new();
        }
        let mut read_direction = object.content.read_direction;
        if !matches!(read_direction, 0 | 90 | 180 | 270) {
            self.diagnostic(
                DiagnosticCode::UnsupportedObject,
                id,
                format!("不支持的 ReadDirection: {read_direction}"),
            );
            read_direction = 0;
        }
        let mut char_direction = object.content.char_direction;
        if !matches!(char_direction, 0 | 90 | 180 | 270) {
            self.diagnostic(
                DiagnosticCode::UnsupportedObject,
                id,
                format!("不支持的 CharDirection: {char_direction}"),
            );
            char_direction = 0;
        }
        let runs = collect_code_runs(object);
        let all_chars = runs
            .iter()
            .flat_map(|run| run.chars.iter().copied())
            .collect::<Vec<_>>();
        if all_chars.is_empty() {
            return Vec::new();
        }
        let (font, used_fallback) = match self.font(&font_id) {
            Some(resolved) => (resolved.asset, resolved.fallback),
            None => {
                self.diagnostic(DiagnosticCode::Font, id, "字体资源缺失，文本层使用估算布局");
                let asset = match self.resources.fonts.get(&font_id) {
                    Some(resource) => font_asset(Arc::from(&b""[..]), resource),
                    None => FontAsset::new(Arc::from(&b""[..]), 0),
                };
                let asset_id = self.store.add_font_asset(asset);
                match self.store.font(asset_id).cloned() {
                    Some(font) => (font, true),
                    None => return Vec::new(),
                }
            }
        };
        let face = ttf_parser::Face::parse(font.data(), font.face_index()).ok();
        let has_face = face.is_some();
        let h_scale = object.content.h_scale;
        let weight = if (100..=900).contains(&object.content.weight) {
            object.content.weight
        } else {
            400
        };
        let italic = object.content.italic;
        let fill = object.content.fill;
        let stroke = object.content.stroke;
        let fill_paint = if fill {
            self.fill_color(&object.content.graphic, object.content.fill_color.as_ref())
                .map(|color| self.paint(&color, id))
                .unwrap_or(Some(Paint::Color(Rgba::BLACK)))
        } else {
            None
        };
        let stroke_paint = if stroke {
            self.stroke_color(
                &object.content.graphic,
                object.content.stroke_color.as_ref(),
            )
            .and_then(|color| self.paint(&color, id))
            .map(|paint| StrokePaint {
                paint,
                style: self.stroke_style(&object.content.graphic),
            })
        } else {
            None
        };
        // Weight >= 600 且没有显式描边时，用细描边模拟粗体。
        let synthetic_bold = if weight >= 600 && stroke_paint.is_none() {
            fill_paint.as_ref().map(|paint| {
                let style = StrokeStyle {
                    width: (size * 0.03).max(0.01),
                    cap: LineCap::Round,
                    join: LineJoin::Round,
                    ..StrokeStyle::default()
                };
                StrokePaint {
                    paint: paint.clone(),
                    style,
                }
            })
        } else {
            None
        };

        let mut slots = glyph_slots(object, all_chars.len());
        if used_fallback {
            // 回退字体无法复用原字体的 glyph index，按 Unicode 重新取字形。
            slots.fill(None);
        }

        let mut elements = Vec::new();
        let mut offset = 0usize;
        let rotate_glyphs = char_direction != 0;
        let scale_glyphs = (h_scale - 1.0).abs() > f64::EPSILON;
        let transform_glyphs = rotate_glyphs || scale_glyphs || italic;
        let shear = if italic {
            Matrix::new(1.0, 0.0, -0.21, 1.0, 0.0, 0.0)
        } else {
            Matrix::IDENTITY
        };
        for run in runs {
            let mut x = run.x;
            let mut y = run.y;
            let mut glyphs = Vec::with_capacity(run.chars.len());
            let mut clusters = Vec::with_capacity(run.chars.len());
            // CGTransform 之后 DeltaX/DeltaY 按“字形”给偏移，而不是按字符；
            // 多对一/一对多/多对多的簇只消耗一个偏移。
            let mut delta_index = 0usize;
            for (index, character) in run.chars.iter().enumerate() {
                let slot = offset + index;
                let slot_glyphs = match slots.get(slot) {
                    Some(Some(glyphs)) => glyphs.clone(),
                    Some(None) | None => face
                        .as_ref()
                        .and_then(|face| face.glyph_index(*character))
                        .map(|id| vec![u32::from(id.0)])
                        .unwrap_or_else(|| vec![0]),
                };
                let cluster_glyph_start = glyphs.len() as u32;
                // DeltaX 是排版者给出的实际步进，必须保留到 IR；否则写出时
                // 用字体度量重算会累积偏差，行尾被裁剪。
                let delta_x = run.delta_x.get(delta_index).copied();
                let delta_y = run.delta_y.get(delta_index).copied();
                for glyph_id in slot_glyphs.iter().copied() {
                    let advance = if has_face {
                        glyph_advance(face.as_ref(), glyph_id, size)
                    } else {
                        estimate_advance(*character, size)
                    };
                    let effective_advance = delta_x.unwrap_or(advance * h_scale.max(0.0));
                    if transform_glyphs {
                        let transform = Matrix::scale(h_scale.max(0.0), 1.0)
                            .then(shear)
                            .then(Matrix::rotate(f64::from(char_direction).to_radians()))
                            .then(Matrix::translate(x, y));
                        elements.push(Element::Text(TextElement {
                            font: Arc::clone(&font),
                            font_size: size,
                            transform,
                            run: Arc::new(GlyphRun::new(
                                character.to_string(),
                                vec![Glyph {
                                    id: glyph_id,
                                    x: 0.0,
                                    y: 0.0,
                                    advance,
                                }],
                                vec![Cluster {
                                    text_start: 0,
                                    text_end: 1,
                                    glyph_start: 0,
                                    glyph_end: 1,
                                }],
                            )),
                            fill: fill_paint.clone(),
                            stroke: stroke_paint.clone().or_else(|| synthetic_bold.clone()),
                        }));
                    } else {
                        glyphs.push(Glyph {
                            id: glyph_id,
                            x,
                            y,
                            advance: effective_advance,
                        });
                    }
                    if let Some(delta) = delta_x {
                        x += delta;
                    } else {
                        match read_direction {
                            180 => x -= advance * h_scale.max(0.0),
                            90 | 270 => {}
                            _ => x += advance * h_scale.max(0.0),
                        }
                    }
                    if let Some(delta) = delta_y {
                        y += delta;
                    } else {
                        match read_direction {
                            90 => y += advance,
                            270 => y -= advance,
                            _ => {}
                        }
                    }
                    delta_index += 1;
                }
                clusters.push(Cluster {
                    text_start: index as u32,
                    text_end: index as u32 + 1,
                    glyph_start: cluster_glyph_start,
                    glyph_end: glyphs.len() as u32,
                });
            }
            offset += run.chars.len();
            if !glyphs.is_empty() {
                elements.push(Element::Text(TextElement {
                    font: Arc::clone(&font),
                    font_size: size,
                    transform: Matrix::IDENTITY,
                    run: Arc::new(GlyphRun::new(
                        run.chars.iter().collect::<String>(),
                        glyphs,
                        clusters,
                    )),
                    fill: fill_paint.clone(),
                    stroke: stroke_paint.clone().or_else(|| synthetic_bold.clone()),
                }));
            }
        }
        elements
    }

    fn build_image(&mut self, object: &ImageObject) -> Option<Element> {
        let label = id_label(object.id);
        let id = label.as_deref();
        if object.content.resource_id.0 == 0 {
            return None;
        }
        let resource_id = object.content.resource_id.to_string();
        let mut candidates = Vec::new();
        if let Some(substitution) = object
            .content
            .substitution
            .and_then(|id| self.resources.images.get(&id.to_string()).cloned())
        {
            candidates.push(substitution);
        }
        if let Some(primary) = self.resources.images.get(&resource_id).cloned() {
            candidates.push(primary);
        }
        if candidates.is_empty() {
            return None;
        }
        let mask = object
            .content
            .image_mask
            .and_then(|id| self.resources.images.get(&id.to_string()).cloned())
            .and_then(|mask| {
                let bytes = mask
                    .res
                    .context()
                    .read_res_file(&mask.res, &mask.file)
                    .ok()?;
                let format = detect_image_format(&bytes).unwrap_or(mask.format);
                Some((format, Arc::<[u8]>::from(bytes)))
            });

        let mut loaded = None;
        let mut last_error = None;
        for media in candidates {
            let bytes = match media.res.context().read_res_file(&media.res, &media.file) {
                Ok(bytes) => Arc::<[u8]>::from(bytes),
                Err(error) => {
                    last_error = Some(format!("读取图片失败: {error}"));
                    continue;
                }
            };
            let format = detect_image_format(&bytes).unwrap_or(media.format);
            let Some((width, height)) = image_dimensions(&bytes) else {
                last_error = Some("无法识别图片尺寸".to_owned());
                continue;
            };
            let mut asset = ImageAsset::new(format, bytes, width, height);
            if let Some((mask_format, mask_bytes)) = &mask {
                let mask_id = self.store.add_image(ImageAsset::new(
                    *mask_format,
                    Arc::clone(mask_bytes),
                    0,
                    0,
                ));
                if let Some(mask) = self.store.image(mask_id).cloned() {
                    asset = asset.with_mask(mask);
                }
            }
            let id = self.store.add_image(asset);
            if let Some(image) = self.store.image(id).cloned() {
                loaded = Some((image, width, height));
                break;
            }
        }
        let Some((image, width, height)) = loaded else {
            self.diagnostic(
                DiagnosticCode::Image,
                id,
                last_error.unwrap_or_else(|| "图片资源缺失".to_owned()),
            );
            return None;
        };
        let scale_x = 1.0 / f64::from(width.max(1));
        let scale_y = 1.0 / f64::from(height.max(1));
        let alpha = (object.content.graphic.alpha.clamp(0, 255) as f32) / 255.0;
        Some(Element::Image(ImageElement {
            image,
            rect: Rect::default(),
            transform: Matrix::scale(scale_x, scale_y),
            alpha,
        }))
    }

    fn build_composite(&mut self, object: &CompositeObject, boundary: &Rect) -> Vec<Element> {
        let label = id_label(object.id);
        let id = label.as_deref();
        if object.content.resource_id.0 == 0 {
            self.diagnostic(DiagnosticCode::Geometry, id, "复合对象缺少 ResourceID");
            return Vec::new();
        }
        let resource_id = object.content.resource_id.to_string();
        let Some(unit) = self.resources.composites.get(&resource_id).cloned() else {
            self.diagnostic(DiagnosticCode::UnsupportedObject, id, "复合对象资源缺失");
            return Vec::new();
        };
        if !self.composites.insert(resource_id.clone()) {
            self.diagnostic(
                DiagnosticCode::UnsupportedObject,
                id,
                "复合对象存在循环引用",
            );
            return Vec::new();
        }
        let unit_width = unit.width;
        let unit_height = unit.height;
        let mut elements = Vec::new();
        if unit_width > 0.0 && unit_height > 0.0 {
            let mut nested = Vec::new();
            self.depth += 1;
            for child in &unit.content.objects {
                self.render_object(child, &mut nested);
            }
            self.depth -= 1;
            let mut group = Group::new(Matrix::scale(
                boundary.width / unit_width,
                boundary.height / unit_height,
            ));
            group.children = nested;
            elements.push(Element::Group(group));
        }
        self.composites.remove(&resource_id);
        elements
    }

    fn render_annotations(&mut self, output: &mut Vec<Element>) {
        let Some(loc) = self.document.data.annotations.as_ref() else {
            return;
        };
        let Ok(index) = self.document.context().load_annotations(loc) else {
            let page_id = self.page_id.clone();
            self.diagnostic(
                DiagnosticCode::UnsupportedObject,
                page_id.as_deref(),
                "注释索引读取失败".to_owned(),
            );
            return;
        };
        let Some(page_id) = self.page_id.clone() else {
            return;
        };
        for entry in &index.data.pages {
            if entry.page_id != page_id {
                continue;
            }
            let Ok(page_annot) = index.context().load_page_annot(&entry.file_loc) else {
                continue;
            };
            for annot in &page_annot.annots {
                if !annot.visible || !annot.print {
                    continue;
                }
                for object in &annot.appearance.page_block.objects {
                    self.render_object(object, output);
                }
            }
        }
    }

    fn render_templates(
        &mut self,
        references: &[Template],
        order: TemplateOrder,
        output: &mut Vec<Element>,
        visited: &mut HashSet<String>,
    ) {
        for reference in references {
            if reference.template_id.0 == 0 {
                continue;
            }
            let id = reference.template_id.to_string();
            let Some(template) = self.templates.get(&id).cloned() else {
                self.diagnostic(
                    DiagnosticCode::UnsupportedObject,
                    Some(&id),
                    format!("模板页缺失: {id}"),
                );
                continue;
            };
            // 页内 `Template` 的 ZOrder 缺省（Background）时沿用模板页声明。
            let reference_order = match &reference.z_order {
                TemplateZOrder::Background => template_page_order(&template.z_order),
                other => template_ref_order(other),
            };
            if reference_order == order {
                self.render_template(&template, output, visited);
            }
        }
    }

    fn render_template(
        &mut self,
        template: &TemplateEntry,
        output: &mut Vec<Element>,
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(template.id.clone()) {
            self.diagnostic(
                DiagnosticCode::UnsupportedObject,
                Some(&template.id),
                "模板页存在循环引用",
            );
            return;
        }
        let content = &template.content;
        let references = content.data.templates.as_slice();
        self.render_templates(references, TemplateOrder::Background, output, visited);
        self.render_templates(references, TemplateOrder::Body, output, visited);
        if let Some(page_content) = &content.data.content {
            self.render_layers(page_content, output);
        }
        self.render_templates(references, TemplateOrder::Foreground, output, visited);
        visited.remove(&template.id);
    }

    fn font(&mut self, id: &str) -> Option<ResolvedFont> {
        let resource = self.resources.fonts.get(id).cloned()?;
        let key = format!("{}/{id}", resource.res.location().as_str());
        if let Some(Some(cached)) = self.font_cache.get(&key) {
            return Some(cached.clone());
        }
        let embedded = resource.file.as_ref().and_then(|file| {
            resource
                .res
                .context()
                .read_res_file(&resource.res, file)
                .ok()
        });
        let (data, fallback) = if let Some(bytes) = embedded.filter(|bytes| !bytes.is_empty()) {
            (Arc::<[u8]>::from(bytes), false)
        } else {
            let provider = self.font_provider.as_mut()?;
            let request = FontRequest {
                font_id: Some(resource.id.as_str()),
                font_name: resource.name.as_deref(),
                family_name: resource.family.as_deref(),
                charset: resource.charset.as_deref(),
                bold: resource.bold,
                italic: resource.italic,
                serif: resource.serif,
                fixed_width: resource.fixed_width,
                font_file: resource.file.as_ref().map(|file| file.as_str()),
            };
            (provider.load(&request).ok()??, true)
        };
        let asset_id = self.store.add_font_asset(font_asset(data, &resource));
        let asset = self
            .store
            .font(asset_id)
            .cloned()
            .map(|asset| ResolvedFont { asset, fallback });
        self.font_cache.insert(key, asset.clone());
        asset
    }

    fn resolved_draw_param(&self, id: &str) -> Option<DrawParamResource> {
        let mut chain = Vec::new();
        let mut current = Some(id.to_owned());
        let mut seen = HashSet::new();
        while let Some(id) = current {
            if !seen.insert(id.clone()) {
                break;
            }
            let Some(resource) = self.resources.draw_params.get(&id) else {
                break;
            };
            current = resource.relative.map(|relative| relative.to_string());
            chain.push(resource.clone());
        }
        if chain.is_empty() {
            return None;
        }
        let mut merged = DrawParamResource::default();
        for resource in chain.iter().rev() {
            if resource.line_width.is_some() {
                merged.line_width = resource.line_width;
            }
            if resource.join.is_some() {
                merged.join = resource.join;
            }
            if resource.cap.is_some() {
                merged.cap = resource.cap;
            }
            if resource.dash_offset.is_some() {
                merged.dash_offset = resource.dash_offset;
            }
            if resource.dash.is_some() {
                merged.dash = resource.dash.clone();
            }
            if resource.miter_limit.is_some() {
                merged.miter_limit = resource.miter_limit;
            }
            if resource.fill_color.is_some() {
                merged.fill_color = resource.fill_color.clone();
            }
            if resource.stroke_color.is_some() {
                merged.stroke_color = resource.stroke_color.clone();
            }
        }
        Some(merged)
    }

    fn object_draw_param(&self, common: &GraphicUnit) -> Option<DrawParamResource> {
        common
            .draw_param
            .and_then(|id| self.resolved_draw_param(&id.to_string()))
    }

    fn layer_draw_param_resource(&self) -> Option<DrawParamResource> {
        self.layer_draw_param
            .as_deref()
            .and_then(|id| self.resolved_draw_param(id))
    }

    fn fill_color(&self, common: &GraphicUnit, color: Option<&Color>) -> Option<Color> {
        color
            .cloned()
            .or_else(|| {
                self.object_draw_param(common)
                    .and_then(|param| param.fill_color)
            })
            .or_else(|| {
                self.layer_draw_param_resource()
                    .and_then(|param| param.fill_color)
            })
    }

    fn stroke_color(&self, common: &GraphicUnit, color: Option<&Color>) -> Option<Color> {
        color
            .cloned()
            .or_else(|| {
                self.object_draw_param(common)
                    .and_then(|param| param.stroke_color)
            })
            .or_else(|| {
                self.layer_draw_param_resource()
                    .and_then(|param| param.stroke_color)
            })
    }

    fn stroke_style(&mut self, common: &GraphicUnit) -> StrokeStyle {
        let mut style = StrokeStyle::default();
        if let Some(param) = self.layer_draw_param_resource() {
            apply_draw_param(&mut style, &param);
        }
        if let Some(param) = self.object_draw_param(common) {
            apply_draw_param(&mut style, &param);
        }
        // 与 XSD 默认值相等视为「未声明」，保持旧行为（默认宽度 1.0）。
        if common.line_width != DEFAULT_LINE_WIDTH {
            style.width = common.line_width;
        }
        if common.miter_limit != DEFAULT_MITER_LIMIT {
            style.miter_limit = common.miter_limit;
        }
        if common.dash_offset != 0.0 {
            style.dash_offset = common.dash_offset;
        }
        if let Some(pattern) = &common.dash_pattern {
            style.dash = pattern.0.clone();
        }
        if let Some(cap) = cap_from(&common.cap) {
            style.cap = cap;
        }
        if let Some(join) = join_from(&common.join) {
            style.join = join;
        }
        style.width = style.width.max(0.0);
        style
    }

    fn paint(&mut self, color: &Color, object_id: Option<&str>) -> Option<Paint> {
        match &color.kind {
            ColorKind::None => self.solid_paint(color),
            ColorKind::AxialShd(shd) => {
                let start = point_from_pos(shd.start_point);
                let end = point_from_pos(shd.end_point);
                let (start, end) = gradient_axis(start, end, &shd.map_type, shd.map_unit);
                Some(Paint::LinearGradient(LinearGradient {
                    start,
                    end,
                    stops: self.gradient_stops(&shd.segments, color.alpha),
                    extend: gradient_extend(&shd.map_type),
                }))
            }
            ColorKind::RadialShd(shd) => {
                let start_center = point_from_pos(shd.start_point);
                let end_center = point_from_pos(shd.end_point);
                let start_radius = shd.start_radius;
                let end_radius = shd.end_radius;
                let (start_center, end_center) =
                    gradient_axis(start_center, end_center, &shd.map_type, shd.map_unit);
                Some(Paint::RadialGradient(RadialGradient {
                    start_center,
                    start_radius,
                    end_center,
                    end_radius,
                    stops: self.gradient_stops(&shd.segments, color.alpha),
                    extend: gradient_extend(&shd.map_type),
                    eccentricity: shd.eccentricity,
                    angle: shd.angle,
                }))
            }
            other => {
                self.diagnostic(
                    DiagnosticCode::Paint,
                    object_id,
                    format!("暂不支持的颜色形态: {}", color_kind_name(other)),
                );
                None
            }
        }
    }

    fn solid_paint(&self, color: &Color) -> Option<Paint> {
        let space = color
            .color_space
            .and_then(|id| self.resources.colors.get(&id.to_string()))
            .or_else(|| {
                self.default_color_space
                    .as_deref()
                    .and_then(|id| self.resources.colors.get(id))
            });
        let bits = space.map_or(8, |space| space.bits);
        let default_value = match space
            .map(|space| space.kind.to_ascii_uppercase())
            .as_deref()
        {
            Some("GRAY") => "0",
            Some("CMYK") => "0 0 0 0",
            _ => "0 0 0",
        };
        let value = if let Some(index) = color.index {
            if index < 0 {
                return None;
            }
            space?.palette.get(index as usize)?.to_string()
        } else {
            color
                .value
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| default_value.to_owned())
        };
        let components = parse_color_components(&value, bits);
        let kind = space.map_or("", |space| space.kind.as_str());
        let alpha = (color.alpha.clamp(0, 255) as f32) / 255.0;
        Some(Paint::Color(components_to_rgba(&components, kind, alpha)?))
    }

    fn gradient_stops(&mut self, segments: &[ShdSegment], alpha: i32) -> Vec<GradientStop> {
        let alpha = (alpha.clamp(0, 255) as f32) / 255.0;
        let collected = segments
            .iter()
            .filter_map(|segment| {
                let mut stop = match self.solid_paint(&segment.color)? {
                    Paint::Color(color) => GradientStop {
                        offset: segment.position.unwrap_or(f64::NAN),
                        color,
                    },
                    _ => return None,
                };
                stop.color = scale_alpha(stop.color, alpha);
                Some(stop)
            })
            .collect::<Vec<_>>();
        distribute_stops(collected)
    }

    fn diagnostic(&mut self, code: DiagnosticCode, id: Option<&str>, message: impl Into<String>) {
        let mut diagnostic = Diagnostic::warning(code, message);
        if let Some(id) = id {
            diagnostic.location = Some(ofdbox_folio_ir::Location {
                page: None,
                block: None,
                item: None,
                object: Some(id.to_owned()),
            });
        }
        self.diagnostics.push(diagnostic);
    }
}

fn clip_group(path: Arc<Path>, transform: Matrix, children: Vec<Element>) -> Group {
    let mut group = Group::new(transform);
    group.clip = Some(Arc::new(Clip {
        path,
        rule: FillRule::NonZero,
    }));
    group.children = children;
    group
}

/// 颜色形态的短名，用于诊断文本。
fn color_kind_name(kind: &ColorKind) -> &'static str {
    match kind {
        ColorKind::Pattern(_) => "Pattern",
        ColorKind::AxialShd(_) => "AxialShd",
        ColorKind::RadialShd(_) => "RadialShd",
        ColorKind::GouraudShd(_) => "GouraudShd",
        ColorKind::LaGouraudShd(_) => "LaGouraudShd",
        ColorKind::None => "None",
    }
}

struct CodeRun {
    chars: Vec<char>,
    x: f64,
    y: f64,
    delta_x: Vec<f64>,
    delta_y: Vec<f64>,
}

fn collect_code_runs(object: &TextObject) -> Vec<CodeRun> {
    let mut runs = Vec::new();
    let mut last_x = 0.0;
    let mut last_y = 0.0;
    for code in &object.content.text_codes {
        let x = code.x.unwrap_or(last_x);
        let y = code.y.unwrap_or(last_y);
        last_x = x;
        last_y = y;
        runs.push(CodeRun {
            chars: decode_text(&code.text),
            x,
            y,
            delta_x: code
                .delta_x
                .as_ref()
                .map(|values| values.0.clone())
                .unwrap_or_default(),
            delta_y: code
                .delta_y
                .as_ref()
                .map(|values| values.0.clone())
                .unwrap_or_default(),
        });
    }
    runs
}

/// 每个字符位置的显式字形。
///
/// - `None`：没有 CGTransform，按字符 CMAP 取字形；
/// - `Some(vec![])`：被簇覆盖但不产生字形（多对一的后续字符）；
/// - `Some(glyphs)`：一个或多个字形，整个簇锚定在该编码位置。
fn glyph_slots(object: &TextObject, count: usize) -> Vec<Option<Vec<u32>>> {
    let mut slots = vec![None; count];
    for transform in &object.content.cg_transforms {
        let glyphs = transform
            .glyphs
            .as_deref()
            .map(parse_u32s)
            .unwrap_or_default();
        if glyphs.is_empty() {
            continue;
        }
        let position = transform.code_position.max(0) as usize;
        if position >= count {
            continue;
        }
        let code_count = transform.code_count.max(1) as usize;
        let glyph_count = transform.glyph_count.max(1) as usize;
        if code_count == glyph_count && glyphs.len() == code_count {
            for (index, glyph) in glyphs.into_iter().enumerate() {
                if let Some(slot) = slots.get_mut(position + index) {
                    *slot = Some(vec![glyph]);
                }
            }
        } else {
            if let Some(slot) = slots.get_mut(position) {
                *slot = Some(glyphs);
            }
            for index in 1..code_count {
                if let Some(slot) = slots.get_mut(position + index) {
                    *slot = Some(Vec::new());
                }
            }
        }
    }
    slots
}

fn glyph_advance(face: Option<&ttf_parser::Face<'_>>, glyph: u32, size: f64) -> f64 {
    let Some(face) = face else {
        return size;
    };
    let units = face.units_per_em();
    if units == 0 {
        return size;
    }
    let advance = glyph
        .try_into()
        .ok()
        .and_then(|id| face.glyph_hor_advance(ttf_parser::GlyphId(id)))
        .map(f64::from)
        .unwrap_or(f64::from(units));
    advance / f64::from(units) * size
}

fn estimate_advance(character: char, size: f64) -> f64 {
    if character.is_whitespace() {
        size * 0.25
    } else if is_cjk(character) {
        size
    } else {
        size * 0.5
    }
}

fn is_cjk(character: char) -> bool {
    matches!(
        u32::from(character),
        0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF
    )
}

fn decode_text(text: &str) -> Vec<char> {
    let mut output = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        // 只有完整的 `\XXXX` 才消费后续字符，否则保留反斜杠本身。
        let mut lookahead = chars.clone();
        let mut hex = String::with_capacity(4);
        for _ in 0..4 {
            match lookahead.next() {
                Some(next) if next.is_ascii_hexdigit() => hex.push(next),
                _ => {
                    hex.clear();
                    break;
                }
            }
        }
        if hex.len() == 4
            && let Ok(value) = u32::from_str_radix(&hex, 16)
            && let Some(decoded) = char::from_u32(value)
        {
            for _ in 0..4 {
                chars.next();
            }
            output.push(decoded);
        } else {
            output.push('\\');
        }
    }
    output
}

fn rounded_rect_path(rect: Rect, horizontal: f64, vertical: f64) -> Path {
    let rx = horizontal.max(0.0).min(rect.width / 2.0);
    let ry = vertical.max(0.0).min(rect.height / 2.0);
    if rx <= 0.0 || ry <= 0.0 {
        return rect.to_path();
    }
    let mut path = Path::new();
    path.move_to(Point::new(rect.x + rx, rect.y));
    path.line_to(Point::new(rect.x + rect.width - rx, rect.y));
    path.arc_to(
        rx,
        ry,
        0.0,
        false,
        true,
        Point::new(rect.x + rect.width, rect.y + ry),
    );
    path.line_to(Point::new(rect.x + rect.width, rect.y + rect.height - ry));
    path.arc_to(
        rx,
        ry,
        0.0,
        false,
        true,
        Point::new(rect.x + rect.width - rx, rect.y + rect.height),
    );
    path.line_to(Point::new(rect.x + rx, rect.y + rect.height));
    path.arc_to(
        rx,
        ry,
        0.0,
        false,
        true,
        Point::new(rect.x, rect.y + rect.height - ry),
    );
    path.line_to(Point::new(rect.x, rect.y + ry));
    path.arc_to(rx, ry, 0.0, false, true, Point::new(rect.x + rx, rect.y));
    path.close();
    path
}

fn append_transformed_path(target: &mut Path, source: &Path, matrix: Matrix) {
    let scale = ((matrix.a * matrix.a + matrix.b * matrix.b).sqrt()
        + (matrix.c * matrix.c + matrix.d * matrix.d).sqrt())
        / 2.0;
    let extra_rotation = matrix.b.atan2(matrix.a);
    for segment in source.segments() {
        match *segment {
            PathSegment::MoveTo(point) => target.move_to(matrix.apply(point)),
            PathSegment::LineTo(point) => target.line_to(matrix.apply(point)),
            PathSegment::QuadTo(control, to) => {
                target.quadratic_to(matrix.apply(control), matrix.apply(to));
            }
            PathSegment::CubicTo(control1, control2, to) => {
                target.cubic_to(
                    matrix.apply(control1),
                    matrix.apply(control2),
                    matrix.apply(to),
                );
            }
            PathSegment::ArcTo {
                rx,
                ry,
                rotation,
                large_arc,
                sweep,
                to,
            } => {
                target.arc_to(
                    rx * scale,
                    ry * scale,
                    rotation + extra_rotation,
                    large_arc,
                    sweep,
                    matrix.apply(to),
                );
            }
            PathSegment::Close => target.close(),
            _ => {}
        }
    }
}

fn parse_path_data(data: &str) -> Option<Path> {
    let tokens = tokenize_path(data)?;
    let mut path = Path::new();
    let mut index = 0usize;
    let mut command = None;
    while index < tokens.len() {
        if let PathToken::Command(value) = tokens[index] {
            command = Some(value);
            index += 1;
            if command == Some('C') {
                path.close();
                continue;
            }
        }
        let command = command?;
        match command {
            // S 定义子路径起点，M 移动当前点：两者都开始新的子路径。
            'S' | 'M' => {
                let x = number(tokens.get(index)?)?;
                let y = number(tokens.get(index + 1)?)?;
                index += 2;
                path.move_to(Point::new(x, y));
            }
            'L' => {
                let x = number(tokens.get(index)?)?;
                let y = number(tokens.get(index + 1)?)?;
                index += 2;
                path.line_to(Point::new(x, y));
            }
            'Q' => {
                let x1 = number(tokens.get(index)?)?;
                let y1 = number(tokens.get(index + 1)?)?;
                let x = number(tokens.get(index + 2)?)?;
                let y = number(tokens.get(index + 3)?)?;
                index += 4;
                path.quadratic_to(Point::new(x1, y1), Point::new(x, y));
            }
            'B' => {
                let x1 = number(tokens.get(index)?)?;
                let y1 = number(tokens.get(index + 1)?)?;
                let x2 = number(tokens.get(index + 2)?)?;
                let y2 = number(tokens.get(index + 3)?)?;
                let x = number(tokens.get(index + 4)?)?;
                let y = number(tokens.get(index + 5)?)?;
                index += 6;
                path.cubic_to(Point::new(x1, y1), Point::new(x2, y2), Point::new(x, y));
            }
            'A' => {
                let rx = number(tokens.get(index)?)?;
                let ry = number(tokens.get(index + 1)?)?;
                let angle = number(tokens.get(index + 2)?)?;
                let large = number(tokens.get(index + 3)?)? != 0.0;
                let sweep = number(tokens.get(index + 4)?)? != 0.0;
                let x = number(tokens.get(index + 5)?)?;
                let y = number(tokens.get(index + 6)?)?;
                index += 7;
                let to = Point::new(x, y);
                path.arc_to(rx.abs(), ry.abs(), angle.to_radians(), large, sweep, to);
            }
            _ => return None,
        }
    }
    Some(path)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PathToken {
    Command(char),
    Number(f64),
}

/// 紧缩路径数据的词法切分。
///
/// 标准只说明 `AbbreviatedData` 是“紧缩的操作符和操作数”，并未规定分隔符；
/// 真实文件里既有 `M 0 0 L 1 0` 也有 `M 0 0L 1 0`。这里按字母/数字边界扫描，
/// 同时接受空格、逗号、正负号和小数点作为隐式分隔符。
fn tokenize_path(data: &str) -> Option<Vec<PathToken>> {
    let bytes = data.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() || byte == b',' {
            index += 1;
            continue;
        }
        if byte.is_ascii_alphabetic() {
            tokens.push(PathToken::Command((byte as char).to_ascii_uppercase()));
            index += 1;
            continue;
        }
        let start = index;
        if byte == b'+' || byte == b'-' {
            index += 1;
        }
        let mut digits = false;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
            digits = true;
        }
        if index < bytes.len() && bytes[index] == b'.' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
                digits = true;
            }
        }
        if !digits {
            return None;
        }
        if index < bytes.len() && (bytes[index] == b'e' || bytes[index] == b'E') {
            let exponent = index;
            index += 1;
            if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
                index += 1;
            }
            let mut exponent_digits = false;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
                exponent_digits = true;
            }
            if !exponent_digits {
                index = exponent;
            }
        }
        tokens.push(PathToken::Number(data[start..index].parse::<f64>().ok()?));
    }
    Some(tokens)
}

fn number(token: &PathToken) -> Option<f64> {
    match token {
        PathToken::Number(value) => Some(*value),
        PathToken::Command(_) => None,
    }
}

fn parse_u32s(text: &str) -> Vec<u32> {
    text.split_whitespace()
        .filter_map(|value| value.parse().ok())
        .collect()
}

fn path_rule(rule: &PathRule) -> FillRule {
    match rule {
        PathRule::EvenOdd => FillRule::EvenOdd,
        PathRule::NonZero => FillRule::NonZero,
        PathRule::Other(value)
            if value.eq_ignore_ascii_case("Even-Odd") || value.eq_ignore_ascii_case("EvenOdd") =>
        {
            FillRule::EvenOdd
        }
        PathRule::Other(_) => FillRule::NonZero,
    }
}

fn parse_cap(text: &str) -> Option<LineCap> {
    match text {
        "Butt" | "butt" => Some(LineCap::Butt),
        "Round" | "round" => Some(LineCap::Round),
        "Square" | "square" => Some(LineCap::Square),
        _ => None,
    }
}

fn parse_join(text: &str) -> Option<LineJoin> {
    match text {
        "Miter" | "miter" => Some(LineJoin::Miter),
        "Round" | "round" => Some(LineJoin::Round),
        "Bevel" | "bevel" => Some(LineJoin::Bevel),
        _ => None,
    }
}

fn cap_from(cap: &Cap) -> Option<LineCap> {
    match cap {
        Cap::Butt => None,
        Cap::Round => Some(LineCap::Round),
        Cap::Square => Some(LineCap::Square),
        Cap::Other(value) => parse_cap(value),
    }
}

fn join_from(join: &Join) -> Option<LineJoin> {
    match join {
        Join::Miter => None,
        Join::Round => Some(LineJoin::Round),
        Join::Bevel => Some(LineJoin::Bevel),
        Join::Other(value) => parse_join(value),
    }
}

/// 解析颜色各通道值并归一化到 `[0, 1]`。
///
/// 标准允许十进制或 `#` 开头的十六进制，取值范围 `[0, 2^BitsPerComponent - 1]`；
/// 真实文件里也能见到 `0.0 0.0 0.0` 这类已归一化的写法，按小数处理。
fn parse_color_components(value: &str, bits: u32) -> Vec<f64> {
    let bits = bits.clamp(1, 16);
    let max = f64::from((1u32 << bits) - 1);
    value
        .split(|character: char| character.is_whitespace() || character == ',')
        .filter(|token| !token.is_empty())
        .filter_map(|token| parse_color_component(token, max))
        .collect()
}

fn parse_color_component(token: &str, max: f64) -> Option<f64> {
    let token = token.trim();
    if let Some(hex) = token.strip_prefix('#') {
        let value = i64::from_str_radix(hex, 16).ok()?;
        return Some((value as f64 / max).clamp(0.0, 1.0));
    }
    let value = token.parse::<f64>().ok()?;
    if token.contains('.') || token.contains('e') || token.contains('E') {
        Some(value.clamp(0.0, 1.0))
    } else {
        Some((value / max).clamp(0.0, 1.0))
    }
}

fn components_to_rgba(components: &[f64], kind: &str, alpha: f32) -> Option<Rgba> {
    let normalized = |value: f64| value.clamp(0.0, 1.0);
    let (r, g, b) = match components.len() {
        1 => {
            let value = normalized(components[0]);
            (value, value, value)
        }
        4 => {
            let c = normalized(components[0]);
            let m = normalized(components[1]);
            let y = normalized(components[2]);
            let k = normalized(components[3]);
            (
                (1.0 - c) * (1.0 - k),
                (1.0 - m) * (1.0 - k),
                (1.0 - y) * (1.0 - k),
            )
        }
        3 if kind.eq_ignore_ascii_case("CMYK") => {
            let c = normalized(components[0]);
            let m = normalized(components[1]);
            let y = normalized(components[2]);
            (1.0 - c, 1.0 - m, 1.0 - y)
        }
        3 => (
            normalized(components[0]),
            normalized(components[1]),
            normalized(components[2]),
        ),
        _ => return None,
    };
    Some(Rgba::new(
        to_byte(r),
        to_byte(g),
        to_byte(b),
        to_byte(f64::from(alpha.clamp(0.0, 1.0))),
    ))
}

fn to_byte(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn scale_alpha(color: Rgba, alpha: f32) -> Rgba {
    let alpha = alpha.clamp(0.0, 1.0);
    Rgba {
        a: (f32::from(color.a) * alpha).round() as u8,
        ..color
    }
}

/// `MapType` 决定渐变区间内如何重复；`Extend` 的 0/1/2/3 是端点延伸，
/// 后端只有 Pad/Repeat/Reflect 三种模式，这里对 Direct 统一用 Pad 近似。
fn gradient_extend(map_type: &MapType) -> GradientExtend {
    let repeating = match map_type {
        MapType::Repeat => Some(GradientExtend::Repeat),
        MapType::Reflect => Some(GradientExtend::Reflect),
        MapType::Direct => None,
        MapType::Other(value) if value.eq_ignore_ascii_case("Repeat") => {
            Some(GradientExtend::Repeat)
        }
        MapType::Other(value) if value.eq_ignore_ascii_case("Reflect") => {
            Some(GradientExtend::Reflect)
        }
        MapType::Other(_) => None,
    };
    repeating.unwrap_or(GradientExtend::Pad)
}

/// 非 Direct 时，一个渐变区间的长度由 `MapUnit` 决定（默认轴线长度），
/// 因此把终点缩放到距起点 `MapUnit` 的位置，再交给后端的 Repeat/Reflect。
fn gradient_axis(
    start: Point,
    end: Point,
    map_type: &MapType,
    map_unit: Option<f64>,
) -> (Point, Point) {
    if is_direct(map_type) {
        return (start, end);
    }
    let length = distance(start, end);
    let unit = map_unit
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(length);
    if length <= f64::EPSILON || !unit.is_finite() || unit <= 0.0 {
        return (start, end);
    }
    let dx = (end.x - start.x) / length;
    let dy = (end.y - start.y) / length;
    (start, Point::new(start.x + dx * unit, start.y + dy * unit))
}

/// Direct 渐变在 `Extend` 不允许延伸的方向上不应继续绘制。
/// 返回一个裁剪路径，用于把填充限制在渐变实际覆盖的区域内。
fn gradient_fill_clip(color: &Color) -> Option<Path> {
    match &color.kind {
        ColorKind::AxialShd(shd) => {
            if !is_direct(&shd.map_type) {
                return None;
            }
            let extend = shd.extend;
            if extend == 3 {
                return None;
            }
            let start = point_from_pos(shd.start_point);
            let end = point_from_pos(shd.end_point);
            axial_clip_path(start, end, extend)
        }
        ColorKind::RadialShd(shd) => {
            if !is_direct(&shd.map_type) {
                return None;
            }
            let extend = shd.extend;
            if extend == 3 {
                return None;
            }
            let start = point_from_pos(shd.start_point);
            let end = point_from_pos(shd.end_point);
            let start_radius = shd.start_radius;
            let end_radius = shd.end_radius;
            let eccentricity = shd.eccentricity.clamp(0.0, 0.999);
            let ratio = (1.0 - eccentricity * eccentricity).max(1e-4).sqrt();
            let angle = shd.angle;
            radial_clip_path(start, start_radius, end, end_radius, ratio, angle, extend)
        }
        _ => None,
    }
}

fn is_direct(map_type: &MapType) -> bool {
    match map_type {
        MapType::Direct => true,
        MapType::Other(value) => value.eq_ignore_ascii_case("Direct"),
        _ => false,
    }
}

fn axial_clip_path(start: Point, end: Point, extend: i32) -> Option<Path> {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length <= f64::EPSILON {
        return None;
    }
    let unit_x = dx / length;
    let unit_y = dy / length;
    let normal_x = -unit_y;
    let normal_y = unit_x;
    const BIG: f64 = 1.0e5;
    let along = |origin: Point, distance: f64| {
        Point::new(origin.x + unit_x * distance, origin.y + unit_y * distance)
    };
    let offset = |origin: Point, distance: f64| {
        Point::new(
            origin.x + normal_x * distance,
            origin.y + normal_y * distance,
        )
    };
    let (from, to) = match extend {
        0 => (start, end),
        1 => (along(start, -BIG), end),
        2 => (start, along(end, BIG)),
        _ => return None,
    };
    let mut path = Path::new();
    path.move_to(offset(from, BIG));
    path.line_to(offset(to, BIG));
    path.line_to(offset(to, -BIG));
    path.line_to(offset(from, -BIG));
    path.close();
    Some(path)
}

#[allow(clippy::too_many_arguments)]
fn radial_clip_path(
    start: Point,
    start_radius: f64,
    end: Point,
    end_radius: f64,
    ratio: f64,
    angle: f64,
    extend: i32,
) -> Option<Path> {
    let end_rx = end_radius.max(0.0);
    let end_ry = (end_radius * ratio).max(0.0);
    let start_rx = start_radius.max(0.0);
    let start_ry = (start_radius * ratio).max(0.0);
    match extend {
        0 => {
            if end_rx <= 0.0 || end_ry <= 0.0 {
                return Some(Path::new());
            }
            let mut path = ellipse_path(end, end_rx, end_ry, angle, false);
            if start_rx > 0.0 && start_ry > 0.0 {
                append_ellipse(&mut path, start, start_rx, start_ry, angle, true);
            }
            Some(path)
        }
        1 => {
            if end_rx <= 0.0 || end_ry <= 0.0 {
                return Some(Path::new());
            }
            Some(ellipse_path(end, end_rx, end_ry, angle, false))
        }
        2 => {
            if start_rx <= 0.0 || start_ry <= 0.0 {
                // 起始半径为 0 时，“起始椭圆之外”就是整个平面。
                return None;
            }
            const BIG: f64 = 1.0e5;
            let mut path = Path::new();
            path.move_to(Point::new(start.x - BIG, start.y - BIG));
            path.line_to(Point::new(start.x + BIG, start.y - BIG));
            path.line_to(Point::new(start.x + BIG, start.y + BIG));
            path.line_to(Point::new(start.x - BIG, start.y + BIG));
            path.close();
            append_ellipse(&mut path, start, start_rx, start_ry, angle, true);
            Some(path)
        }
        _ => None,
    }
}

fn ellipse_path(center: Point, rx: f64, ry: f64, angle: f64, reverse: bool) -> Path {
    let mut path = Path::new();
    append_ellipse(&mut path, center, rx, ry, angle, reverse);
    path
}

fn append_ellipse(path: &mut Path, center: Point, rx: f64, ry: f64, angle: f64, reverse: bool) {
    const STEPS: usize = 64;
    let (sin_a, cos_a) = angle.to_radians().sin_cos();
    for step in 0..=STEPS {
        let mut t = std::f64::consts::TAU * step as f64 / STEPS as f64;
        if reverse {
            t = -t;
        }
        let (sin_t, cos_t) = t.sin_cos();
        let x = rx * cos_t;
        let y = ry * sin_t;
        let point = Point::new(
            center.x + cos_a * x - sin_a * y,
            center.y + sin_a * x + cos_a * y,
        );
        if step == 0 {
            path.move_to(point);
        } else {
            path.line_to(point);
        }
    }
    path.close();
}

fn distribute_stops(mut stops: Vec<GradientStop>) -> Vec<GradientStop> {
    if stops.is_empty() {
        return stops;
    }
    let last = stops.len() - 1;
    if !stops.iter().any(|stop| stop.offset.is_finite()) {
        let divisor = last.max(1) as f64;
        for (index, stop) in stops.iter_mut().enumerate() {
            stop.offset = index as f64 / divisor;
        }
        return stops;
    }
    if !stops[0].offset.is_finite() {
        stops[0].offset = 0.0;
    }
    if !stops[last].offset.is_finite() {
        stops[last].offset = 1.0;
    }
    let mut index = 0;
    while index < stops.len() {
        if stops[index].offset.is_finite() {
            index += 1;
            continue;
        }
        let start = index - 1;
        let mut end = index;
        while end < stops.len() && !stops[end].offset.is_finite() {
            end += 1;
        }
        let span = end - start;
        let start_offset = stops[start].offset;
        let end_offset = stops[end].offset;
        for step in 1..span {
            stops[start + step].offset =
                start_offset + (end_offset - start_offset) * step as f64 / span as f64;
        }
        index = end;
    }
    stops.sort_by(|a, b| {
        a.offset
            .partial_cmp(&b.offset)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    stops
}

fn distance(a: Point, b: Point) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

#[derive(Clone, Copy, Debug)]
struct MeshPoint {
    position: Point,
    color: Rgba,
}

fn mesh_midpoint(a: MeshPoint, b: MeshPoint) -> MeshPoint {
    let mix = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
    MeshPoint {
        position: Point::new(
            (a.position.x + b.position.x) * 0.5,
            (a.position.y + b.position.y) * 0.5,
        ),
        color: Rgba::new(
            mix(a.color.r, b.color.r),
            mix(a.color.g, b.color.g),
            mix(a.color.b, b.color.b),
            mix(a.color.a, b.color.a),
        ),
    }
}

fn subdivide_mesh(triangle: [MeshPoint; 3], depth: u8, output: &mut Vec<[MeshPoint; 3]>) {
    if depth == 0 {
        output.push(triangle);
        return;
    }
    let [a, b, c] = triangle;
    let ab = mesh_midpoint(a, b);
    let bc = mesh_midpoint(b, c);
    let ca = mesh_midpoint(c, a);
    subdivide_mesh([a, ab, ca], depth - 1, output);
    subdivide_mesh([ab, b, bc], depth - 1, output);
    subdivide_mesh([ca, bc, c], depth - 1, output);
    subdivide_mesh([ab, bc, ca], depth - 1, output);
}

/// 相邻纯色三角形各自抗锯齿会在公共边留下白缝，渲染时把顶点向外扩一点制造重叠。
const MESH_OVERLAP: f64 = 0.25;

fn triangle_path(triangle: &[MeshPoint; 3]) -> Path {
    let center_x = (triangle[0].position.x + triangle[1].position.x + triangle[2].position.x) / 3.0;
    let center_y = (triangle[0].position.y + triangle[1].position.y + triangle[2].position.y) / 3.0;
    let expand = |position: Point| {
        let dx = position.x - center_x;
        let dy = position.y - center_y;
        let length = (dx * dx + dy * dy).sqrt();
        if length <= f64::EPSILON {
            position
        } else {
            Point::new(
                position.x + dx / length * MESH_OVERLAP,
                position.y + dy / length * MESH_OVERLAP,
            )
        }
    };
    let mut path = Path::new();
    path.move_to(expand(triangle[0].position));
    path.line_to(expand(triangle[1].position));
    path.line_to(expand(triangle[2].position));
    path.close();
    path
}

fn average_color(triangle: &[MeshPoint; 3]) -> Rgba {
    let [a, b, c] = triangle;
    let mix = |a: u8, b: u8, c: u8| ((u16::from(a) + u16::from(b) + u16::from(c)) / 3) as u8;
    Rgba::new(
        mix(a.color.r, b.color.r, c.color.r),
        mix(a.color.g, b.color.g, c.color.g),
        mix(a.color.b, b.color.b, c.color.b),
        mix(a.color.a, b.color.a, c.color.a),
    )
}

#[derive(Clone)]
struct TemplateEntry {
    id: String,
    z_order: PageZOrder,
    content: Rc<Loaded<PageModel>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TemplateOrder {
    Background,
    Body,
    Foreground,
}

fn template_page_order(value: &PageZOrder) -> TemplateOrder {
    match value {
        PageZOrder::Foreground => TemplateOrder::Foreground,
        PageZOrder::Background => TemplateOrder::Background,
        PageZOrder::Other(value) => template_order_text(value),
    }
}

fn template_ref_order(value: &TemplateZOrder) -> TemplateOrder {
    match value {
        TemplateZOrder::Foreground => TemplateOrder::Foreground,
        TemplateZOrder::Background => TemplateOrder::Background,
        TemplateZOrder::Other(value) => template_order_text(value),
    }
}

fn template_order_text(value: &str) -> TemplateOrder {
    if value.eq_ignore_ascii_case("Foreground") {
        TemplateOrder::Foreground
    } else if value.eq_ignore_ascii_case("Body") {
        TemplateOrder::Body
    } else {
        TemplateOrder::Background
    }
}

fn build_text_layer(scene: &Scene) -> TextLayer {
    let mut items = Vec::new();
    let mut lines = Vec::new();
    collect_text(&scene.root, Matrix::IDENTITY, &mut items, &mut lines);
    TextLayer {
        page: 0,
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
                let bbox = text_bbox(text, item_transform);
                let index = items.len() as u32;
                let baseline = text
                    .run
                    .glyphs
                    .first()
                    .map(|glyph| item_transform.apply(Point::new(0.0, glyph.y)).y)
                    .unwrap_or(bbox.y);
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
                    baseline,
                });
            }
            _ => {}
        }
    }
}

fn text_bbox(element: &TextElement, transform: Matrix) -> Rect {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for glyph in &element.run.glyphs {
        min_x = min_x.min(glyph.x);
        max_x = max_x.max(glyph.x + glyph.advance);
        min_y = min_y.min(glyph.y - element.font_size);
        max_y = max_y.max(glyph.y + element.font_size * 0.2);
    }
    if !min_x.is_finite() {
        return Rect::default();
    }
    transform.transform_rect(Rect::new(min_x, min_y, max_x - min_x, max_y - min_y))
}

fn image_format_from_media(format: &str) -> ImageFormat {
    match format.trim().to_ascii_uppercase().as_str() {
        "PNG" => ImageFormat::Png,
        "JPG" | "JPEG" => ImageFormat::Jpeg,
        "BMP" => ImageFormat::Bmp,
        "JBIG2" | "GBIG2" | "JB2" => ImageFormat::Jbig2,
        "TIF" | "TIFF" => ImageFormat::Tiff,
        _ => ImageFormat::Unknown,
    }
}

fn image_format_from_path(path: &str) -> ImageFormat {
    let extension = path
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or("");
    image_format_from_media(extension)
}

#[cfg(test)]
mod tests {
    use super::{
        components_to_rgba, distribute_stops, gradient_axis, gradient_extend, matrix_from_array,
        parse_color_components, parse_path_data,
    };
    use ofdbox_core::model::definition::{DeltaArray, StArray};
    use ofdbox_core::model::page::MapType;
    use ofdbox_folio_ir::{GradientExtend, GradientStop, Matrix, PathSegment, Point, Rgba};
    use std::str::FromStr;

    #[test]
    fn parses_spaced_path() {
        let path = parse_path_data("M 0 0 L 10 0 L 10 10 C").expect("path");
        assert_eq!(path.segments().len(), 4);
        assert_eq!(
            path.segments()[0],
            PathSegment::MoveTo(Point::new(0.0, 0.0))
        );
        assert_eq!(
            path.segments()[1],
            PathSegment::LineTo(Point::new(10.0, 0.0))
        );
        assert_eq!(path.segments()[3], PathSegment::Close);
    }

    #[test]
    fn parses_glued_commands() {
        let path =
            parse_path_data("M 0 0L 215.9 0L 215.9 1152.172L 0 1152.172L 0 0 C").expect("path");
        assert_eq!(path.segments().len(), 6);
        assert_eq!(
            path.segments()[0],
            PathSegment::MoveTo(Point::new(0.0, 0.0))
        );
        assert_eq!(
            path.segments()[1],
            PathSegment::LineTo(Point::new(215.9, 0.0))
        );
        assert_eq!(
            path.segments()[2],
            PathSegment::LineTo(Point::new(215.9, 1152.172))
        );
        assert_eq!(path.segments()[5], PathSegment::Close);
    }

    #[test]
    fn parses_numbers_without_separators() {
        // 负号和小数点作为数字之间的隐式分隔符。
        let path = parse_path_data("M1-2L3.5.5").expect("path");
        assert_eq!(
            path.segments(),
            vec![
                PathSegment::MoveTo(Point::new(1.0, -2.0)),
                PathSegment::LineTo(Point::new(3.5, 0.5)),
            ]
        );
    }

    #[test]
    fn parses_exponent_and_comma() {
        let path = parse_path_data("M0,0L1e1,-1.5e-1").expect("path");
        assert_eq!(
            path.segments(),
            vec![
                PathSegment::MoveTo(Point::new(0.0, 0.0)),
                PathSegment::LineTo(Point::new(10.0, -0.15)),
            ]
        );
    }

    #[test]
    fn starts_new_subpath_on_every_move() {
        let path = parse_path_data("M 0 0 L 1 0 M 2 2 L 3 3").expect("path");
        assert_eq!(
            path.segments(),
            vec![
                PathSegment::MoveTo(Point::new(0.0, 0.0)),
                PathSegment::LineTo(Point::new(1.0, 0.0)),
                PathSegment::MoveTo(Point::new(2.0, 2.0)),
                PathSegment::LineTo(Point::new(3.0, 3.0)),
            ]
        );
    }

    #[test]
    fn maps_gradient_extend_and_map_unit() {
        assert_eq!(gradient_extend(&MapType::Repeat), GradientExtend::Repeat);
        assert_eq!(gradient_extend(&MapType::Reflect), GradientExtend::Reflect);
        assert_eq!(gradient_extend(&MapType::Direct), GradientExtend::Pad);
        assert_eq!(
            gradient_extend(&MapType::Other("Direct".to_owned())),
            GradientExtend::Pad
        );

        let (_, end) = gradient_axis(
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            &MapType::Repeat,
            Some(25.0),
        );
        assert_eq!(end, Point::new(25.0, 0.0));

        // Direct 不使用 MapUnit。
        let (_, end) = gradient_axis(
            Point::new(0.0, 0.0),
            Point::new(0.0, 100.0),
            &MapType::Direct,
            Some(25.0),
        );
        assert_eq!(end, Point::new(0.0, 100.0));
    }

    /// `DeltaX`/`DeltaY` 的 `g <次数> <值>` 压缩写法由 `DeltaArray` 解析。
    #[test]
    fn expands_grouped_deltas() {
        assert_eq!(
            DeltaArray::from_str("1 2 3 g 2 4 5").expect("array").0,
            vec![1.0, 2.0, 3.0, 4.0, 4.0, 5.0]
        );
        assert_eq!(
            DeltaArray::from_str("G 3 1.5").expect("array").0,
            vec![1.5, 1.5, 1.5]
        );
        assert_eq!(
            DeltaArray::from_str("g 0 9").expect("array").0,
            Vec::<f64>::new()
        );
    }

    #[test]
    fn converts_ctm_arrays() {
        assert_eq!(
            matrix_from_array(&StArray::new(vec![1.0, 0.0, 0.0, 1.0, 10.0, 20.0])),
            Some(Matrix::new(1.0, 0.0, 0.0, 1.0, 10.0, 20.0))
        );
        assert_eq!(matrix_from_array(&StArray::new(vec![1.0])), None);
        assert_eq!(matrix_from_array(&StArray::default()), None);
    }

    #[test]
    fn distributes_gradient_positions() {
        let stop = |offset: f64| GradientStop {
            offset,
            color: Rgba::BLACK,
        };
        let stops = distribute_stops(vec![
            stop(f64::NAN),
            stop(f64::NAN),
            stop(0.5),
            stop(f64::NAN),
            stop(f64::NAN),
        ]);
        assert_eq!(
            stops.iter().map(|stop| stop.offset).collect::<Vec<_>>(),
            vec![0.0, 0.25, 0.5, 0.75, 1.0]
        );

        let stops = distribute_stops(vec![stop(f64::NAN), stop(f64::NAN), stop(f64::NAN)]);
        assert_eq!(
            stops.iter().map(|stop| stop.offset).collect::<Vec<_>>(),
            vec![0.0, 0.5, 1.0]
        );
    }

    #[test]
    fn decodes_text_escapes() {
        use super::decode_text;

        assert_eq!(decode_text("A\\0042C"), vec!['A', 'B', 'C']);
        assert_eq!(decode_text("\\0020"), vec![' ']);
        assert_eq!(decode_text("\\00ZZ"), vec!['\\', '0', '0', 'Z', 'Z']);
        assert_eq!(decode_text("\\zzzz"), vec!['\\', 'z', 'z', 'z', 'z']);
    }

    #[test]
    fn parses_hex_colors_with_bpc() {
        assert_eq!(
            parse_color_components("#ff #0 #80", 8),
            vec![1.0, 0.0, 128.0 / 255.0]
        );
        assert_eq!(
            parse_color_components("#ffff #0000 #8000", 16),
            vec![1.0, 0.0, 32768.0 / 65535.0]
        );
        assert_eq!(parse_color_components("#1 #0", 1), vec![1.0, 0.0]);
    }

    #[test]
    fn parses_integer_and_fractional_color_values() {
        assert_eq!(
            parse_color_components("255 0 128", 8),
            vec![1.0, 0.0, 128.0 / 255.0]
        );
        assert_eq!(
            parse_color_components("0.5 0.25 0.0", 8),
            vec![0.5, 0.25, 0.0]
        );
        assert_eq!(parse_color_components("65535 0 0", 16), vec![1.0, 0.0, 0.0]);
    }

    #[test]
    fn converts_rgb_and_cmyk() {
        assert_eq!(
            components_to_rgba(&[1.0, 0.0, 0.0], "RGB", 1.0),
            Some(Rgba::new(255, 0, 0, 255))
        );
        assert_eq!(
            components_to_rgba(&[0.0, 0.0, 0.0, 0.0], "CMYK", 1.0),
            Some(Rgba::new(255, 255, 255, 255))
        );
    }

    #[test]
    fn rejects_incomplete_or_unknown_commands() {
        assert!(parse_path_data("M 0").is_none());
        assert!(parse_path_data("X 0 0").is_none());
        assert!(parse_path_data("M 0 0 L 1").is_none());
    }
}
