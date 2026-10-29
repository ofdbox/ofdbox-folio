//! Fixed IR 到 OFD 包的写出。
//!
//! 页面、图元、资源和大纲先映射为 `ofdbox-core` 的结构化模型对象，再由 core
//! 负责 XML 序列化和 ZIP 打包。输出 entry 布局保持稳定：
//! `OFD.xml`、`Doc_0/Document.xml`、`Doc_0/Pages/Page_{n}/Content.xml`、
//! `Doc_0/PublicRes.xml`（字体）、`Doc_0/DocumentRes.xml`（图片），字体和图片
//! 分别落在 `Doc_0/Res/` 下。core 固定 ZIP 时间戳并按名字排序条目，因此相同
//! 输入产生相同字节。
//!
//! 当前覆盖路径、文本、图片、基础资源和渐变；组裁剪、透明度按页面坐标近似写
//! 出，与旧实现保持一致。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ofdbox_core::model::definition::{
    Action, ActionEvent, ActionKind, Actions, ColorArray, DeltaArray, Dest, DestType, Goto,
    GotoKind, PageArea, StArray, StBox, StId, StLoc, StPos, StRefId,
};
use ofdbox_core::model::document::{OutlineElem, Outlines, PageEntry};
use ofdbox_core::model::page::{
    AxialShd, Cap, CgTransform, Clip, ClipArea, ClipContent, ClipPath, Clips, Color, ColorKind,
    GraphicObject, GraphicUnit, ImageContent, ImageObject, Join, Layer, LayerType, MapType,
    Page as OfdPage, PageContent, PathContent, PathObject, PathRule, RadialShd, ShdSegment,
    TextCode, TextContent, TextObject,
};
use ofdbox_core::model::res::{Charset, Font, Fonts, MultiMedia, MultiMediaType, MultiMedias, Res};
use ofdbox_core::model_xml;
use ofdbox_core::store::{CreateOptions, ObjectContext, OfdFile, Storage, Stored};

use ofdbox_folio_ir::{
    Destination, Element, FillRule, FixedDocument, FontAsset, Glyph, GradientExtend, Group,
    ImageAsset, ImageElement, ImageFormat, LineCap, LineJoin, Matrix, OutlineItem, Page, Paint,
    Path, PathElement, PathSegment, Point, Rect, Rgba, StrokeStyle, TextElement,
    extract_truetype_face, subset_truetype,
};

use crate::OfdError;

/// 大纲树最大嵌套深度，避免异常输入把写出拖入深递归。
const MAX_OUTLINE_DEPTH: usize = 16;

/// OFD 写出选项。
#[derive(Clone, Debug)]
pub struct OfdWriteOptions {
    /// 生成工具名称。
    pub creator: String,
    /// 固定文档 ID；不提供时由页面内容摘要生成。
    pub doc_id: Option<String>,
    /// 保留字段：core 打包时恒使用固定 ZIP 时间戳，该值不再影响输出。
    pub deterministic: bool,
}

impl Default for OfdWriteOptions {
    fn default() -> Self {
        Self {
            creator: "OFDBox Folio".to_owned(),
            doc_id: None,
            deterministic: true,
        }
    }
}

/// 把 FixedDocument 写成 OFD 包。
pub fn write_fixed(
    document: &FixedDocument,
    options: &OfdWriteOptions,
) -> Result<Vec<u8>, OfdError> {
    if document.pages.is_empty() {
        return Err(OfdError::InvalidStructure("没有可写出的页面".to_owned()));
    }

    // 旧行为：文档公共区域和每页区域都取页面尺寸。
    let first_size = page_size(&document.pages[0]);
    let page_area = page_area_value(first_size);

    let mut state = State {
        next_id: 1,
        fonts: HashMap::new(),
        font_entries: Vec::new(),
        font_glyphs: HashMap::new(),
        font_unmapped: HashSet::new(),
        images: HashMap::new(),
        image_entries: Vec::new(),
    };

    // 先构建全部页面模型：DocID 需要页面序列化结果，且核心建包要求先知道页区域。
    let mut built_pages = Vec::with_capacity(document.pages.len());
    for page in &document.pages {
        let page_id = state.alloc();
        let layer_id = state.alloc();
        let size = page_size(page);
        let mut writer = PageWriter {
            state: &mut state,
            objects: Vec::new(),
            page_width: size.width,
            page_height: size.height,
            clips: Vec::new(),
            opacity: 1.0,
        };
        writer.group(&page.content.root, Matrix::IDENTITY);
        let model = OfdPage {
            templates: Vec::new(),
            page_res: Vec::new(),
            area: Some(page_area_value(size)),
            content: Some(PageContent {
                layers: vec![Layer {
                    id: StId::new(layer_id),
                    layer_type: LayerType::Body,
                    draw_param: None,
                    objects: writer.objects,
                }],
            }),
            actions: None,
        };
        let bytes = model_xml::to_bytes(&model)
            .map_err(|error| OfdError::InvalidStructure(format!("序列化页面失败: {error}")))?;
        built_pages.push(BuiltPage {
            id: page_id,
            model,
            bytes,
        });
    }
    // 全局 ID 空间：页、图层、图元、字体和图片资源共用。
    let max_unit_id = state.next_id.saturating_sub(1);
    let page_ids = built_pages.iter().map(|page| page.id).collect::<Vec<_>>();
    let doc_id = options
        .doc_id
        .clone()
        .unwrap_or_else(|| content_id(&options.creator, &built_pages));

    let file = OfdFile::create_with(CreateOptions {
        storage: Storage::Memory,
        doc_id: Some(doc_id.clone()),
        page_area: Some(page_area.clone()),
    })?;

    // OFD.xml：版本固定 1.0（GB/T 33190-2016 / OFD.xsd），并写入元数据。
    let mut ofd = file.ofd()?;
    ofd.data.version = "1.0".to_owned();
    if let Some(body) = ofd.data.doc_bodies.first_mut() {
        body.doc_info.doc_id = doc_id;
        body.doc_info.creator = Some(options.creator.clone());
    }
    ofd.save()?;

    let mut doc = file.document(0)?;
    doc.data.common_data.max_unit_id = StId::new(max_unit_id);
    doc.data.common_data.page_area = page_area;

    // 手工登记页面：`add_page` 从 Page_1 起且页 ID 不可控，这里保持 Page_0 起。
    for (index, page) in built_pages.into_iter().enumerate() {
        let BuiltPage { id, model, .. } = page;
        let base_loc = StLoc::new(format!("Pages/Page_{index}/Content.xml"));
        let location = doc.context().resolve(&base_loc)?;
        let page_context = ObjectContext::child(doc.context(), location.clone())?;
        Stored::new(model, page_context, location).save()?;
        doc.data.pages.pages.push(PageEntry {
            id: id.to_string(),
            base_loc,
        });
    }

    // 字体：PublicRes.xml + Res/Font_{id}.{ext}。
    if !state.font_entries.is_empty() {
        let fonts = state
            .font_entries
            .iter()
            .map(|font| Font {
                id: StId::new(font.id),
                font_name: font.name.clone(),
                family_name: font.family.clone(),
                charset: Charset::Unicode,
                italic: false,
                bold: false,
                serif: false,
                fixed_width: false,
                font_file: (!font.bytes.is_empty()).then(|| StLoc::new(font.file.clone())),
            })
            .collect::<Vec<_>>();
        save_res(
            &doc,
            "PublicRes.xml",
            Res {
                base_loc: StLoc::new("Res"),
                fonts: Some(Fonts { fonts }),
                ..Default::default()
            },
        )?;
        doc.data.common_data.public_res = vec![StLoc::new("PublicRes.xml")];
        for font in &state.font_entries {
            if font.bytes.is_empty() {
                continue;
            }
            let subset = if state.font_unmapped.contains(&font.id) {
                None
            } else {
                state
                    .font_glyphs
                    .get(&font.id)
                    .and_then(|used| subset_truetype(&font.bytes, font.face_index, used))
            };
            // 集合字体即使不能子集，也要拆出单个 face 再内嵌：OFD 的 FontFile
            // 只接受单份字体，整包塞 .ttc 会让阅读器读不出字形。
            let face = if subset.is_none() {
                extract_truetype_face(&font.bytes, font.face_index)
            } else {
                None
            };
            let bytes = subset
                .as_deref()
                .or(face.as_deref())
                .unwrap_or(font.bytes.as_ref());
            let location = doc
                .context()
                .resolve(&StLoc::new(format!("Res/{}", font.file)))?;
            doc.context().write_entry(&location, bytes.to_vec())?;
        }
    }

    // 图片：DocumentRes.xml + Res/Image_{id}.{ext}。
    if !state.image_entries.is_empty() {
        let multi_medias = state
            .image_entries
            .iter()
            .map(|image| MultiMedia {
                id: StId::new(image.id),
                media_type: MultiMediaType::Image,
                format: Some(image_format_name(image.format).to_owned()),
                media_file: StLoc::new(image.file.clone()),
            })
            .collect::<Vec<_>>();
        save_res(
            &doc,
            "DocumentRes.xml",
            Res {
                base_loc: StLoc::new("Res"),
                multi_medias: Some(MultiMedias { multi_medias }),
                ..Default::default()
            },
        )?;
        doc.data.common_data.document_res = vec![StLoc::new("DocumentRes.xml")];
        for image in &state.image_entries {
            let location = doc
                .context()
                .resolve(&StLoc::new(format!("Res/{}", image.file)))?;
            doc.context().write_entry(&location, image.bytes.to_vec())?;
        }
    }

    doc.data.outlines = build_outlines(&document.outline, &page_ids);
    doc.save()?;

    Ok(file.to_bytes()?)
}

/// 已构建的页面：模型、序列化字节和页面 ID。
struct BuiltPage {
    id: u32,
    model: OfdPage,
    bytes: Vec<u8>,
}

struct State {
    next_id: u32,
    fonts: HashMap<u64, u32>,
    font_entries: Vec<FontEntry>,
    /// 每个字体用到的字形 ID，写出时据此子集化。
    font_glyphs: HashMap<u32, HashSet<u16>>,
    /// 只给了 `TextCode` 没给字形 ID 的字体：无法确定用字，必须整份嵌入。
    font_unmapped: HashSet<u32>,
    images: HashMap<u64, u32>,
    image_entries: Vec<ImageEntry>,
}

impl State {
    fn alloc(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

struct FontEntry {
    id: u32,
    name: String,
    family: Option<String>,
    file: String,
    bytes: Arc<[u8]>,
    face_index: u32,
}

struct ImageEntry {
    id: u32,
    format: ImageFormat,
    file: String,
    bytes: Arc<[u8]>,
}

struct ClipSpec {
    path: Arc<Path>,
    world: Matrix,
}

struct PageWriter<'a> {
    state: &'a mut State,
    objects: Vec<GraphicObject>,
    page_width: f64,
    page_height: f64,
    clips: Vec<ClipSpec>,
    opacity: f32,
}

impl PageWriter<'_> {
    fn group(&mut self, group: &Group, parent: Matrix) {
        let transform = group.transform.then(parent);
        let clipped = group.clip.is_some();
        if let Some(clip) = &group.clip {
            self.clips.push(ClipSpec {
                path: Arc::clone(&clip.path),
                world: transform,
            });
        }
        let previous_opacity = self.opacity;
        self.opacity *= group.opacity.clamp(0.0, 1.0);
        for element in &group.children {
            match element {
                Element::Group(child) => self.group(child, transform),
                Element::Path(path) => self.path(path, transform),
                Element::Text(text) => self.text(text, transform),
                Element::Image(image) => self.image(image, transform),
                _ => {}
            }
        }
        self.opacity = previous_opacity;
        if clipped {
            self.clips.pop();
        }
    }

    /// 图元公共属性：页面边界、CTM 和当前裁剪。
    fn common(&self, transform: Matrix) -> GraphicUnit {
        GraphicUnit {
            boundary: StBox::new(0.0, 0.0, self.page_width, self.page_height),
            ctm: Some(matrix_array(transform)),
            clips: self.clips_value(),
            ..Default::default()
        }
    }

    /// OFD 的裁剪区域位于对象空间，不随对象 CTM 缩放；写出时直接给页面坐标。
    fn clips_value(&self) -> Option<Clips> {
        if self.clips.is_empty() {
            return None;
        }
        let clips = self
            .clips
            .iter()
            .map(|clip| {
                let path = transform_path(&clip.path, clip.world);
                let boundary = path
                    .bounds()
                    .unwrap_or_else(|| Rect::new(0.0, 0.0, self.page_width, self.page_height));
                Clip {
                    areas: vec![ClipArea {
                        draw_param: None,
                        ctm: None,
                        content: ClipContent::Path(ClipPath {
                            content: PathContent {
                                graphic: GraphicUnit {
                                    boundary: st_box(boundary),
                                    ..Default::default()
                                },
                                abbreviated_data: path_data(&path),
                                ..Default::default()
                            },
                        }),
                    }],
                }
            })
            .collect();
        Some(Clips { clips })
    }

    fn path(&mut self, element: &PathElement, parent: Matrix) {
        if element.path.is_empty() {
            return;
        }
        let transform = element.transform.then(parent);
        let id = self.state.alloc();
        let mut common = self.common(transform);
        let mut stroke_color = None;
        let mut fill_color = None;
        if let Some(fill) = &element.fill {
            fill_color = Some(color_value(fill, self.opacity));
        }
        if let Some(stroke) = &element.stroke {
            stroke_color = Some(color_value(&stroke.paint, self.opacity));
            apply_stroke(&mut common, &stroke.style);
        }
        self.objects.push(GraphicObject::Path(PathObject {
            id: StId::new(id),
            content: PathContent {
                graphic: common,
                stroke_color,
                fill_color,
                abbreviated_data: path_data(&element.path),
                stroke: element.stroke.is_some(),
                fill: element.fill.is_some(),
                rule: if element.rule == FillRule::EvenOdd {
                    PathRule::EvenOdd
                } else {
                    PathRule::NonZero
                },
            },
        }));
    }

    fn text(&mut self, element: &TextElement, parent: Matrix) {
        let run = &element.run;
        if run.text.is_empty() {
            return;
        }
        let font_id = self.font(&element.font);
        // 记录实际用到的字形，写出时按需子集化；只有 TextCode、没有字形 ID 的
        // 字体无法判断用字，标记为整份嵌入。
        if run.glyphs.is_empty() {
            self.state.font_unmapped.insert(font_id);
        } else {
            let used = self.state.font_glyphs.entry(font_id).or_default();
            // TrueType 字形 ID 是 16 位，超出的 ID 不可能出现在字体里。
            used.extend(
                run.glyphs
                    .iter()
                    .filter_map(|glyph| u16::try_from(glyph.id).ok()),
            );
        }
        let transform = element.transform.then(parent);
        let id = self.state.alloc();
        let mut common = self.common(transform);
        let mut stroke_color = None;
        let mut fill_color = None;
        if let Some(fill) = &element.fill {
            fill_color = Some(color_value(fill, self.opacity));
        }
        if let Some(stroke) = &element.stroke {
            stroke_color = Some(color_value(&stroke.paint, self.opacity));
            apply_stroke(&mut common, &stroke.style);
        }
        let mut cg_transforms = Vec::new();
        if !run.glyphs.is_empty() {
            let glyphs = run
                .glyphs
                .iter()
                .map(|glyph| glyph.id.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            cg_transforms.push(CgTransform {
                code_position: 0,
                code_count: i32::try_from(run.char_count()).unwrap_or(i32::MAX),
                glyph_count: i32::try_from(run.glyphs.len()).unwrap_or(i32::MAX),
                glyphs: Some(glyphs),
            });
        }
        let first = run.glyphs.first().copied().unwrap_or_default();
        // 直接用相邻字形的局部坐标差还原步进，避免字体度量或 HScale 引入偏差；
        // DeltaX/DeltaY 是“本字形之后”的偏移，最后一个字形补 0。
        let deltas_x = position_deltas(&run.glyphs, |glyph| glyph.x);
        let deltas_y = position_deltas(&run.glyphs, |glyph| glyph.y);
        let text_code = TextCode {
            x: Some(first.x),
            y: Some(first.y),
            delta_x: Some(DeltaArray::new(deltas_x)),
            delta_y: deltas_y
                .iter()
                .any(|delta| delta.abs() > 1e-6)
                .then(|| DeltaArray::new(deltas_y)),
            text: run.text.clone(),
        };
        self.objects.push(GraphicObject::Text(TextObject {
            id: StId::new(id),
            content: TextContent {
                graphic: common,
                font: StRefId::new(font_id),
                size: element.font_size,
                stroke: element.stroke.is_some(),
                fill: element.fill.is_some(),
                fill_color,
                stroke_color,
                cg_transforms,
                text_codes: vec![text_code],
                ..Default::default()
            },
        }));
    }

    fn image(&mut self, element: &ImageElement, parent: Matrix) {
        let id = self.state.alloc();
        let (resource, mask) = self.image_resource(&element.image);
        let width = if element.rect.width > 0.0 {
            element.rect.width
        } else {
            f64::from(element.image.pixel_width.max(1))
        };
        let height = if element.rect.height > 0.0 {
            element.rect.height
        } else {
            f64::from(element.image.pixel_height.max(1))
        };
        let translate = Matrix::translate(element.rect.x, element.rect.y);
        let ctm = Matrix::scale(width, height)
            .then(translate)
            .then(element.transform)
            .then(parent);
        let alpha = (element.alpha.clamp(0.0, 1.0) * self.opacity).clamp(0.0, 1.0);
        let mut common = self.common(ctm);
        common.alpha = (alpha * 255.0).round().clamp(0.0, 255.0) as i32;
        self.objects.push(GraphicObject::Image(ImageObject {
            id: StId::new(id),
            content: ImageContent {
                graphic: common,
                resource_id: StRefId::new(resource),
                substitution: None,
                image_mask: mask.map(StRefId::new),
                border: None,
            },
        }));
    }

    fn font(&mut self, font: &Arc<FontAsset>) -> u32 {
        let key = font.id().get();
        if let Some(id) = self.state.fonts.get(&key) {
            return *id;
        }
        let id = self.state.alloc();
        let name = font
            .font_name
            .clone()
            .or_else(|| font.family.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("Font{id}"));
        let family = font
            .family
            .clone()
            .or_else(|| font.font_name.clone())
            .filter(|name| !name.is_empty());
        let extension = font_extension(&font.data);
        let file = format!("Font_{id}.{extension}");
        self.state.fonts.insert(key, id);
        self.state.font_entries.push(FontEntry {
            id,
            name,
            family,
            file,
            bytes: Arc::clone(&font.data),
            face_index: font.face_index,
        });
        id
    }

    fn image_resource(&mut self, image: &Arc<ImageAsset>) -> (u32, Option<u32>) {
        let id = self.image_id(image);
        let mask = image.mask.as_ref().map(|mask| self.image_id(mask));
        (id, mask)
    }

    fn image_id(&mut self, image: &Arc<ImageAsset>) -> u32 {
        let key = image.id().get();
        if let Some(id) = self.state.images.get(&key) {
            return *id;
        }
        let id = self.state.alloc();
        let extension = image_extension(image.format);
        let file = format!("Image_{id}.{extension}");
        self.state.images.insert(key, id);
        self.state.image_entries.push(ImageEntry {
            id,
            format: image.format,
            file,
            bytes: Arc::clone(&image.encoded),
        });
        id
    }
}

/// 通过文档上下文写出 `PublicRes.xml` / `DocumentRes.xml`。
fn save_res(
    doc: &Stored<ofdbox_core::model::document::Document>,
    name: &str,
    res: Res,
) -> Result<(), OfdError> {
    let location = doc.context().resolve(&StLoc::new(name))?;
    let context = ObjectContext::child(doc.context(), location.clone())?;
    Stored::new(res, context, location).save()?;
    Ok(())
}

/// 生成 `Outlines`；没有大纲时返回 `None`，序列化后不产生节点。
fn build_outlines(items: &[OutlineItem], page_ids: &[u32]) -> Option<Outlines> {
    if items.is_empty() {
        return None;
    }
    let first_page = page_ids.first().copied().unwrap_or(1);
    Some(Outlines {
        outline_elems: push_outline(items, page_ids, first_page, 0),
    })
}

fn push_outline(
    items: &[OutlineItem],
    page_ids: &[u32],
    first_page: u32,
    depth: usize,
) -> Vec<OutlineElem> {
    if depth >= MAX_OUTLINE_DEPTH {
        return Vec::new();
    }
    items
        .iter()
        .map(|item| {
            let page_id = page_ids
                .get(item.destination.page)
                .copied()
                .unwrap_or(first_page);
            OutlineElem {
                title: item.title.clone(),
                count: None,
                expanded: true,
                actions: Some(Actions {
                    actions: vec![Action {
                        event: ActionEvent::Do,
                        region: None,
                        kind: ActionKind::Goto(Goto {
                            kind: GotoKind::Dest(dest_value(page_id, item.destination)),
                        }),
                    }],
                }),
                children: push_outline(&item.children, page_ids, first_page, depth + 1),
            }
        })
        .collect()
}

/// 目标区域：整页用 `Fit`，带坐标用 `XYZ`。
fn dest_value(page_id: u32, destination: Destination) -> Dest {
    let Point { x, y } = destination.point;
    if x.abs() < 1e-6 && y.abs() < 1e-6 {
        Dest {
            dest_type: DestType::Fit,
            page_id: StRefId::new(page_id),
            ..Default::default()
        }
    } else {
        Dest {
            dest_type: DestType::Xyz,
            page_id: StRefId::new(page_id),
            left: Some(x),
            top: Some(y),
            ..Default::default()
        }
    }
}

fn page_size(page: &Page) -> ofdbox_folio_ir::Size {
    let size = page.content.size;
    if size.width > 0.0 && size.height > 0.0 {
        size
    } else {
        ofdbox_folio_ir::Size::new(page.info.crop_box.width, page.info.crop_box.height)
    }
}

fn page_area_value(size: ofdbox_folio_ir::Size) -> PageArea {
    PageArea {
        physical_box: StBox::new(0.0, 0.0, size.width, size.height),
        ..Default::default()
    }
}

fn st_box(rect: Rect) -> StBox {
    StBox::new(rect.x, rect.y, rect.width, rect.height)
}

fn matrix_array(matrix: Matrix) -> StArray {
    StArray::new(vec![
        matrix.a, matrix.b, matrix.c, matrix.d, matrix.e, matrix.f,
    ])
}

fn st_pos(point: Point) -> StPos {
    StPos {
        x: point.x,
        y: point.y,
    }
}

fn apply_stroke(common: &mut GraphicUnit, style: &StrokeStyle) {
    common.line_width = style.width;
    common.cap = match style.cap {
        LineCap::Butt => Cap::Butt,
        LineCap::Round => Cap::Round,
        LineCap::Square => Cap::Square,
    };
    common.join = match style.join {
        LineJoin::Miter => Join::Miter,
        LineJoin::Round => Join::Round,
        LineJoin::Bevel => Join::Bevel,
    };
    common.miter_limit = style.miter_limit;
    if !style.dash.is_empty() {
        common.dash_pattern = Some(StArray::new(style.dash.clone()));
        common.dash_offset = style.dash_offset;
    }
}

fn path_data(path: &Path) -> String {
    let mut output = String::new();
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(point) => {
                output.push_str(&format!("M {} {} ", number(point.x), number(point.y)));
            }
            PathSegment::LineTo(point) => {
                output.push_str(&format!("L {} {} ", number(point.x), number(point.y)));
            }
            PathSegment::QuadTo(control, to) => {
                output.push_str(&format!(
                    "Q {} {} {} {} ",
                    number(control.x),
                    number(control.y),
                    number(to.x),
                    number(to.y)
                ));
            }
            PathSegment::CubicTo(control1, control2, to) => {
                output.push_str(&format!(
                    "B {} {} {} {} {} {} ",
                    number(control1.x),
                    number(control1.y),
                    number(control2.x),
                    number(control2.y),
                    number(to.x),
                    number(to.y)
                ));
            }
            PathSegment::ArcTo {
                rx,
                ry,
                rotation,
                large_arc,
                sweep,
                to,
            } => {
                output.push_str(&format!(
                    "A {} {} {} {} {} {} {} ",
                    number(*rx),
                    number(*ry),
                    number(rotation.to_degrees()),
                    u8::from(*large_arc),
                    u8::from(*sweep),
                    number(to.x),
                    number(to.y)
                ));
            }
            PathSegment::Close => output.push_str("C "),
            _ => {}
        }
    }
    output.trim_end().to_owned()
}

/// IR 画笔映射为 OFD 颜色；纯色和轴向/径向渐变都落到 `Color`。
fn color_value(paint: &Paint, opacity: f32) -> Color {
    match paint {
        Paint::Color(color) => solid_color(*color, opacity),
        Paint::LinearGradient(gradient) => {
            if gradient.stops.len() < 2 {
                return solid_color(
                    gradient
                        .stops
                        .first()
                        .map_or(Rgba::BLACK, |stop| stop.color),
                    opacity,
                );
            }
            let (map_type, map_unit, extend) =
                gradient_map(gradient.extend, distance(gradient.start, gradient.end));
            Color {
                kind: ColorKind::AxialShd(AxialShd {
                    segments: shd_segments(&gradient.stops, opacity),
                    map_type,
                    map_unit,
                    extend,
                    start_point: st_pos(gradient.start),
                    end_point: st_pos(gradient.end),
                }),
                ..Default::default()
            }
        }
        Paint::RadialGradient(gradient) => {
            if gradient.stops.len() < 2 {
                return solid_color(
                    gradient
                        .stops
                        .first()
                        .map_or(Rgba::BLACK, |stop| stop.color),
                    opacity,
                );
            }
            let (map_type, map_unit, extend) = gradient_map(
                gradient.extend,
                distance(gradient.start_center, gradient.end_center),
            );
            Color {
                kind: ColorKind::RadialShd(RadialShd {
                    segments: shd_segments(&gradient.stops, opacity),
                    map_type,
                    map_unit,
                    eccentricity: gradient.eccentricity,
                    angle: gradient.angle,
                    start_point: st_pos(gradient.start_center),
                    start_radius: gradient.start_radius,
                    end_point: st_pos(gradient.end_center),
                    end_radius: gradient.end_radius,
                    extend,
                }),
                ..Default::default()
            }
        }
        _ => solid_color(Rgba::BLACK, opacity),
    }
}

fn solid_color(color: Rgba, opacity: f32) -> Color {
    Color {
        // core 的 ColorArray 用 u32 分量，兼容灰度/CMYK 等超过 255 的通道值。
        value: Some(ColorArray::new(vec![
            u32::from(color.r),
            u32::from(color.g),
            u32::from(color.b),
        ])),
        alpha: color_alpha(color.a, opacity),
        ..Default::default()
    }
}

fn color_alpha(alpha: u8, opacity: f32) -> i32 {
    (f32::from(alpha) * opacity).round().clamp(0.0, 255.0) as i32
}

fn shd_segments(stops: &[ofdbox_folio_ir::GradientStop], opacity: f32) -> Vec<ShdSegment> {
    stops
        .iter()
        .map(|stop| ShdSegment {
            position: Some(stop.offset.clamp(0.0, 1.0)),
            color: solid_color(stop.color, opacity),
        })
        .collect()
}

/// Direct 渐变下 Extend 控制两侧是否延伸；Pad 等价于两侧都延伸。
fn gradient_map(extend: GradientExtend, map_unit: f64) -> (MapType, Option<f64>, i32) {
    let map_type = match extend {
        GradientExtend::Pad => MapType::Direct,
        GradientExtend::Repeat => MapType::Repeat,
        GradientExtend::Reflect => MapType::Reflect,
    };
    if extend == GradientExtend::Pad {
        (map_type, None, 3)
    } else {
        (map_type, Some(map_unit), 0)
    }
}

fn distance(a: Point, b: Point) -> f64 {
    (b.x - a.x).hypot(b.y - a.y)
}

fn position_deltas(glyphs: &[Glyph], position: impl Fn(&Glyph) -> f64) -> Vec<f64> {
    let mut deltas = Vec::with_capacity(glyphs.len());
    for pair in glyphs.windows(2) {
        deltas.push(position(&pair[1]) - position(&pair[0]));
    }
    deltas.push(0.0);
    deltas
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

fn number(value: f64) -> String {
    let mut text = format!("{value:.4}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" { "0".to_owned() } else { text }
}

/// 内容摘要 DocID：FNV-1a over 生成工具名与序列化后的页面 XML。
fn content_id(creator: &str, pages: &[BuiltPage]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in creator
        .bytes()
        .chain(pages.iter().flat_map(|page| page.bytes.iter().copied()))
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016X}")
}

fn font_extension(data: &[u8]) -> &'static str {
    if data.starts_with(b"OTTO") {
        "otf"
    } else if data.starts_with(b"ttcf") {
        "ttc"
    } else {
        "ttf"
    }
}

fn image_extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tif",
        ImageFormat::Jbig2 => "jb2",
        ImageFormat::Ccitt => "ccitt",
        _ => "bin",
    }
}

fn image_format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Tiff => "TIFF",
        ImageFormat::Jbig2 => "JBIG2",
        ImageFormat::Ccitt => "CCITT",
        _ => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::{OfdWriteOptions, write_fixed};
    use crate::{OfdDocument, OfdReadOptions};
    use ofdbox_core::model::definition::{StBox, StId};
    use ofdbox_core::model::page::{ColorKind, GraphicObject, MapType, Page as OfdPage};
    use ofdbox_core::model::res::Res;
    use ofdbox_core::model_xml;
    use ofdbox_folio_ir::{
        Cluster, Destination, DocumentSource, Element, FillRule, FixedDocument, FontAsset, Glyph,
        GlyphRun, GradientExtend, GradientStop, Group, ImageAsset, ImageElement, ImageFormat,
        LinearGradient, Matrix, OutlineItem, Page, PageInfo, Paint, Path, PathElement, Point, Rect,
        Rgba, Rotation, Scene, Size, TextElement,
    };
    use std::io::{Cursor, Read};
    use std::sync::Arc;

    fn sample_page() -> Page {
        let mut path = Path::new();
        path.move_to(Point::new(10.0, 10.0));
        path.line_to(Point::new(90.0, 10.0));
        path.line_to(Point::new(90.0, 90.0));
        path.line_to(Point::new(10.0, 90.0));
        path.close();
        Page::new(
            PageInfo {
                media_box: Rect::new(0.0, 0.0, 100.0, 100.0),
                crop_box: Rect::new(0.0, 0.0, 100.0, 100.0),
                rotation: Rotation::None,
                label: None,
            },
            Scene {
                size: Size::new(100.0, 100.0),
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
            },
        )
    }

    fn text_page(font: Arc<FontAsset>, run: Arc<GlyphRun>) -> Page {
        let mut page = sample_page();
        page.content.root.children.clear();
        page.content.root.children.push(Element::Text(TextElement {
            font,
            font_size: 3.0,
            transform: Matrix::IDENTITY,
            run,
            fill: Some(Paint::Color(Rgba::BLACK)),
            stroke: None,
        }));
        page
    }

    #[test]
    fn writes_readable_and_deterministic_package() {
        let mut document = FixedDocument::default();
        document.pages.push(sample_page());
        let options = OfdWriteOptions::default();
        let first = write_fixed(&document, &options).expect("write");
        let second = write_fixed(&document, &options).expect("write again");
        assert_eq!(first, second);

        // entry 布局与页索引保持旧行为：Page_0 起，资源固定文件名。
        let names = entry_names(&first);
        for name in [
            "OFD.xml",
            "Doc_0/Document.xml",
            "Doc_0/Pages/Page_0/Content.xml",
        ] {
            assert!(
                names.iter().any(|entry| entry == name),
                "缺少 {name}: {names:?}"
            );
        }

        let document_xml = read_entry(&first, "Doc_0/Document.xml");
        let document: ofdbox_core::model::document::Document =
            model_xml::from_str(&document_xml).expect("parse document");
        // 页 ID=1、图层 ID=2、路径 ID=3。
        assert_eq!(document.common_data.max_unit_id, StId::new(3));
        assert_eq!(
            document.common_data.page_area.physical_box,
            StBox::new(0.0, 0.0, 100.0, 100.0)
        );
        assert_eq!(document.pages.pages.len(), 1);
        assert_eq!(
            document.pages.pages[0].base_loc.as_str(),
            "Pages/Page_0/Content.xml"
        );
        assert!(document.common_data.public_res.is_empty());
        assert!(document.common_data.document_res.is_empty());

        let mut reader =
            OfdDocument::open_bytes(first, OfdReadOptions::default()).expect("open generated");
        assert_eq!(reader.page_count(), 1);
        let page = reader.load_page(0).expect("load page");
        assert_eq!(page.content.size, Size::new(100.0, 100.0));
        assert!(reader.info().outline.is_empty());
    }

    #[test]
    fn round_trips_outline() {
        let mut document = FixedDocument::default();
        document.pages.push(sample_page());
        document.pages.push(sample_page());
        document.outline.push(OutlineItem {
            title: "扉页".to_owned(),
            destination: Destination {
                page: 0,
                point: Point::new(0.0, 0.0),
            },
            children: Vec::new(),
        });
        document.outline.push(OutlineItem {
            title: "正文 & 附录".to_owned(),
            destination: Destination {
                page: 1,
                point: Point::new(12.5, 34.0),
            },
            children: vec![OutlineItem {
                title: "第一节".to_owned(),
                destination: Destination {
                    page: 1,
                    point: Point::new(0.0, 0.0),
                },
                children: Vec::new(),
            }],
        });

        let bytes = write_fixed(&document, &OfdWriteOptions::default()).expect("write");
        let reader = OfdDocument::open_bytes(bytes, OfdReadOptions::default()).expect("open");
        let outline = &reader.info().outline;
        assert_eq!(outline.len(), 2);
        assert_eq!(outline[0].title, "扉页");
        assert_eq!(outline[0].destination.page, 0);
        assert_eq!(outline[1].title, "正文 & 附录");
        assert_eq!(outline[1].destination.page, 1);
        assert!((outline[1].destination.point.x - 12.5).abs() < 1e-6);
        assert!((outline[1].destination.point.y - 34.0).abs() < 1e-6);
        assert_eq!(outline[1].children.len(), 1);
        assert_eq!(outline[1].children[0].title, "第一节");
    }

    #[test]
    fn keeps_font_names_and_default_text_flags() {
        let font = Arc::new(
            FontAsset::new(Arc::from(&b""[..]), 0)
                .with_font_name("SimSun")
                .with_family("SimSun"),
        );
        let run = Arc::new(GlyphRun::new(
            "A".to_owned(),
            vec![Glyph {
                id: 1,
                x: 0.0,
                y: 0.0,
                advance: 2.0,
            }],
            vec![Cluster {
                text_start: 0,
                text_end: 1,
                glyph_start: 0,
                glyph_end: 1,
            }],
        ));
        let mut document = FixedDocument::default();
        document.pages.push(text_page(font, run));
        let bytes = write_fixed(&document, &OfdWriteOptions::default()).expect("write");

        let content = read_entry(&bytes, "Doc_0/Pages/Page_0/Content.xml");
        // TextObject 的 Fill/Stroke 等于默认值，序列化时省略；读回后语义不变。
        assert!(!content.contains("Stroke="), "{content}");
        assert!(!content.contains("Fill="), "{content}");
        assert!(content.contains("DeltaX="), "{content}");

        let page: OfdPage = model_xml::from_str(&content).expect("parse page");
        let layer = page
            .content
            .as_ref()
            .expect("content")
            .layers
            .first()
            .expect("layer");
        let text = layer
            .objects
            .iter()
            .find_map(|object| match object {
                GraphicObject::Text(text) => Some(text),
                _ => None,
            })
            .expect("text object");
        assert!(text.content.fill);
        assert!(!text.content.stroke);
        assert_eq!(text.content.size, 3.0);
        assert!(
            text.content.font.0 >= 1,
            "字体资源 ID 必须非默认: {}",
            text.content.font.0
        );
        assert_eq!(text.content.text_codes[0].text, "A");
        assert!(text.content.text_codes[0].delta_x.is_some());
        assert_eq!(text.content.cg_transforms[0].glyphs.as_deref(), Some("1"));

        let resources = read_entry(&bytes, "Doc_0/PublicRes.xml");
        assert!(resources.contains("FontName=\"SimSun\""));
        assert!(resources.contains("FamilyName=\"SimSun\""));
        assert!(!resources.contains("FontFile"));
        let res: Res = model_xml::from_str(&resources).expect("parse res");
        let font = res
            .fonts
            .as_ref()
            .and_then(|fonts| fonts.fonts.first())
            .expect("font");
        assert_eq!(font.font_name, "SimSun");
        assert_eq!(font.family_name.as_deref(), Some("SimSun"));
        assert!(font.font_file.is_none());
    }

    #[test]
    fn writes_gradients_images_and_clips_semantically() {
        let mut clip_path = Path::new();
        clip_path.move_to(Point::new(0.0, 0.0));
        clip_path.line_to(Point::new(50.0, 0.0));
        clip_path.line_to(Point::new(50.0, 50.0));
        clip_path.close();
        let mut shape = Path::new();
        shape.move_to(Point::new(5.0, 5.0));
        shape.line_to(Point::new(95.0, 5.0));
        shape.line_to(Point::new(95.0, 95.0));
        shape.close();
        let image = Arc::new(ImageAsset::new(
            ImageFormat::Png,
            Arc::from(&[0x89, b'P', b'N', b'G'][..]),
            2,
            2,
        ));
        let mut page = sample_page();
        page.content.root = Group {
            clip: Some(Arc::new(ofdbox_folio_ir::Clip {
                path: Arc::new(clip_path),
                rule: FillRule::NonZero,
            })),
            children: vec![
                Element::Path(PathElement {
                    path: Arc::new(shape),
                    transform: Matrix::IDENTITY,
                    fill: Some(Paint::LinearGradient(LinearGradient {
                        start: Point::new(0.0, 0.0),
                        end: Point::new(100.0, 0.0),
                        stops: vec![
                            GradientStop {
                                offset: 0.0,
                                color: Rgba::from_rgb(255, 0, 0),
                            },
                            GradientStop {
                                offset: 1.0,
                                color: Rgba::from_rgb(0, 0, 255),
                            },
                        ],
                        extend: GradientExtend::Pad,
                    })),
                    stroke: None,
                    rule: FillRule::NonZero,
                }),
                Element::Image(ImageElement {
                    image,
                    rect: Rect::new(10.0, 10.0, 20.0, 20.0),
                    transform: Matrix::IDENTITY,
                    alpha: 0.5,
                }),
            ],
            ..Group::default()
        };
        let mut document = FixedDocument::default();
        document.pages.push(page);
        let bytes = write_fixed(&document, &OfdWriteOptions::default()).expect("write");

        let content = read_entry(&bytes, "Doc_0/Pages/Page_0/Content.xml");
        assert!(content.contains("AxialShd"), "{content}");
        assert!(content.contains("Extend=\"3\""), "{content}");
        assert!(content.contains("Clips"), "{content}");
        assert!(content.contains("ImageObject"), "{content}");
        assert!(content.contains("Alpha=\"128\""), "{content}");

        let page: OfdPage = model_xml::from_str(&content).expect("parse page");
        let objects = &page.content.as_ref().expect("content").layers[0].objects;
        let GraphicObject::Path(path) = &objects[0] else {
            panic!("第一个图元应为路径");
        };
        assert!(path.content.graphic.clips.is_some(), "裁剪区域丢失");
        let Some(ColorKind::AxialShd(shd)) =
            path.content.fill_color.as_ref().map(|color| &color.kind)
        else {
            panic!("轴向渐变丢失: {:?}", path.content.fill_color);
        };
        assert_eq!(shd.segments.len(), 2);
        assert_eq!(shd.map_type, MapType::Direct);
        assert_eq!(shd.extend, 3);
        let GraphicObject::Image(image) = &objects[1] else {
            panic!("第二个图元应为图片");
        };
        assert_eq!(image.content.graphic.alpha, 128);
        assert!(image.content.resource_id.0 >= 1, "图片资源 ID 必须非默认");

        let resources = read_entry(&bytes, "Doc_0/DocumentRes.xml");
        assert!(resources.contains("Format=\"PNG\""), "{resources}");
        assert!(resources.contains("MediaFile"), "{resources}");
        let res: Res = model_xml::from_str(&resources).expect("parse res");
        let media = res.multi_medias.as_ref().expect("multimedias");
        assert_eq!(media.multi_medias.len(), 1);
        assert!(
            media.multi_medias[0]
                .media_file
                .as_str()
                .starts_with("Image_")
        );
        assert!(
            entry_names(&bytes)
                .iter()
                .any(|name| name.starts_with("Doc_0/Res/Image_")),
            "缺少图片 entry"
        );
    }

    #[test]
    fn embeds_only_used_glyphs_of_subsettable_font() {
        let path = if cfg!(windows) {
            "C:/Windows/Fonts/arial.ttf"
        } else {
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
        };
        let Ok(raw) = std::fs::read(path) else {
            return;
        };
        let font = Arc::new(FontAsset::new(Arc::from(&raw[..]), 0).with_font_name("Arial"));
        let run = Arc::new(GlyphRun::new(
            "AB".to_owned(),
            vec![
                Glyph {
                    id: 36,
                    x: 0.0,
                    y: 0.0,
                    advance: 2.0,
                },
                Glyph {
                    id: 37,
                    x: 2.0,
                    y: 0.0,
                    advance: 2.0,
                },
            ],
            vec![Cluster {
                text_start: 0,
                text_end: 2,
                glyph_start: 0,
                glyph_end: 2,
            }],
        ));
        let mut document = FixedDocument::default();
        document.pages.push(text_page(font, run));
        let bytes = write_fixed(&document, &OfdWriteOptions::default()).expect("write");

        let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).expect("zip");
        let index = (0..archive.len())
            .find(|index| {
                archive
                    .by_index(*index)
                    .ok()
                    .and_then(|entry| entry.name().ok().map(|name| name.into_owned()))
                    .is_some_and(|name| name.starts_with("Doc_0/Res/Font_"))
            })
            .expect("嵌入字体");
        let mut subset = Vec::new();
        archive
            .by_index(index)
            .expect("font entry")
            .read_to_end(&mut subset)
            .expect("read");
        assert!(
            subset.len() * 2 < raw.len(),
            "子集 {} 字节，原字体 {} 字节",
            subset.len(),
            raw.len()
        );
        // 字形顺序不变：字形表里仍保留原字形数量，引用原始 GID 的文本才有效。
        let glyphs = u16::from_be_bytes([subset[4], subset[5]]);
        assert!(glyphs > 0);
    }

    fn entry_names(bytes: &[u8]) -> Vec<String> {
        let archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
        archive
            .file_names()
            .map(|name| name.expect("entry name").into_owned())
            .collect()
    }

    fn read_entry(bytes: &[u8], name: &str) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
        let mut file = archive.by_name(name).expect("entry");
        let mut text = String::new();
        file.read_to_string(&mut text).expect("read");
        text
    }
}
