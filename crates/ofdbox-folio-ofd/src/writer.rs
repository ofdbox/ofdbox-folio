//! Fixed IR 到 OFD 包的写出。
//!
//! 当前覆盖路径、文本、图片和基础资源；组裁剪、透明度和渐变的完整语义
//! 仍以 IR 为准，写出阶段暂做近似。输出使用固定时间戳和内容摘要 DocID，
//! 保证相同输入产生相同字节。

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::sync::Arc;

use ofdbox_folio_ir::{
    Destination, Element, FillRule, FixedDocument, FontAsset, Glyph, GradientExtend, Group,
    ImageAsset, ImageElement, ImageFormat, LineCap, LineJoin, Matrix, OutlineItem, Page, Paint,
    Path, PathElement, PathSegment, Point, Rect, Rgba, StrokeStyle, TextElement,
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
    /// 固定 ZIP 时间戳，保证可复现输出。
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

    let mut state = State {
        next_id: 1,
        fonts: HashMap::new(),
        font_entries: Vec::new(),
        images: HashMap::new(),
        image_entries: Vec::new(),
    };
    let mut pages = Vec::with_capacity(document.pages.len());
    for page in &document.pages {
        let page_id = state.alloc();
        let layer_id = state.alloc();
        let size = page_size(page);
        let mut writer = PageWriter {
            state: &mut state,
            content: String::new(),
            page_width: size.width,
            page_height: size.height,
            clips: Vec::new(),
            opacity: 1.0,
        };
        writer.group(&page.content.root, Matrix::IDENTITY);
        pages.push(PageXml {
            id: page_id,
            width: size.width,
            height: size.height,
            content: page_xml(layer_id, size, &writer.content),
        });
    }
    let max_unit_id = state.next_id.saturating_sub(1);
    let page_refs = pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            format!(
                "<ofd:Page ID=\"{}\" BaseLoc=\"Pages/Page_{index}/Content.xml\"/>",
                page.id
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let outline = outline_xml(&document.outline, &pages);
    let first = pages.first().expect("pages checked");
    let mut resources = String::new();
    if !state.font_entries.is_empty() {
        resources.push_str("<ofd:PublicRes>PublicRes.xml</ofd:PublicRes>");
    }
    if !state.image_entries.is_empty() {
        resources.push_str("<ofd:DocumentRes>DocumentRes.xml</ofd:DocumentRes>");
    }
    let document_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ofd:Document xmlns:ofd=\"http://www.ofdspec.org/2016\"><ofd:CommonData><ofd:MaxUnitID>{max_unit_id}</ofd:MaxUnitID><ofd:PageArea><ofd:PhysicalBox>0 0 {} {}</ofd:PhysicalBox></ofd:PageArea>{resources}</ofd:CommonData><ofd:Pages>{page_refs}</ofd:Pages>{outline}</ofd:Document>",
        number(first.width),
        number(first.height)
    );
    let doc_id = options
        .doc_id
        .clone()
        .unwrap_or_else(|| content_id(&pages, &options.creator));
    let ofd_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ofd:OFD xmlns:ofd=\"http://www.ofdspec.org/2016\" Version=\"1.1\" DocType=\"OFD\"><ofd:DocBody><ofd:DocInfo><ofd:DocID>{doc_id}</ofd:DocID><ofd:Creator>{}</ofd:Creator></ofd:DocInfo><ofd:DocRoot>Doc_0/Document.xml</ofd:DocRoot></ofd:DocBody></ofd:OFD>",
        xml_escape(&options.creator)
    );

    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut zip_options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    if options.deterministic {
        zip_options = zip_options.last_modified_time(zip::DateTime::default());
    }
    write_entry(&mut zip, "OFD.xml", ofd_xml.as_bytes(), zip_options)?;
    write_entry(
        &mut zip,
        "Doc_0/Document.xml",
        document_xml.as_bytes(),
        zip_options,
    )?;
    for (index, page) in pages.iter().enumerate() {
        write_entry(
            &mut zip,
            &format!("Doc_0/Pages/Page_{index}/Content.xml"),
            page.content.as_bytes(),
            zip_options,
        )?;
    }
    if !state.font_entries.is_empty() {
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ofd:Res xmlns:ofd=\"http://www.ofdspec.org/2016\" BaseLoc=\"Res\"><ofd:Fonts>",
        );
        for font in &state.font_entries {
            xml.push_str(&format!(
                "<ofd:Font ID=\"{}\" FontName=\"{}\"",
                font.id,
                xml_escape(&font.name)
            ));
            if let Some(family) = &font.family {
                xml.push_str(&format!(" FamilyName=\"{}\"", xml_escape(family)));
            }
            xml.push_str(" Charset=\"unicode\">");
            if !font.bytes.is_empty() {
                xml.push_str(&format!(
                    "<ofd:FontFile>{}</ofd:FontFile>",
                    xml_escape(&font.file)
                ));
            }
            xml.push_str("</ofd:Font>");
        }
        xml.push_str("</ofd:Fonts></ofd:Res>");
        write_entry(&mut zip, "Doc_0/PublicRes.xml", xml.as_bytes(), zip_options)?;
        for font in &state.font_entries {
            if !font.bytes.is_empty() {
                write_entry(
                    &mut zip,
                    &format!("Doc_0/Res/{}", font.file),
                    &font.bytes,
                    zip_options,
                )?;
            }
        }
    }
    if !state.image_entries.is_empty() {
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ofd:Res xmlns:ofd=\"http://www.ofdspec.org/2016\" BaseLoc=\"Res\"><ofd:MultiMedias>",
        );
        for image in &state.image_entries {
            xml.push_str(&format!(
                "<ofd:MultiMedia ID=\"{}\" Type=\"Image\" Format=\"{}\"><ofd:MediaFile>{}</ofd:MediaFile></ofd:MultiMedia>",
                image.id,
                image_format_name(image.format),
                xml_escape(&image.file)
            ));
        }
        xml.push_str("</ofd:MultiMedias></ofd:Res>");
        write_entry(
            &mut zip,
            "Doc_0/DocumentRes.xml",
            xml.as_bytes(),
            zip_options,
        )?;
        for image in &state.image_entries {
            write_entry(
                &mut zip,
                &format!("Doc_0/Res/{}", image.file),
                &image.bytes,
                zip_options,
            )?;
        }
    }
    let cursor = zip
        .finish()
        .map_err(|error| OfdError::Io(std::io::Error::other(error)))?;
    Ok(cursor.into_inner())
}

struct PageXml {
    id: u32,
    width: f64,
    height: f64,
    content: String,
}

struct State {
    next_id: u32,
    fonts: HashMap<u64, u32>,
    font_entries: Vec<FontEntry>,
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
    content: String,
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

    /// OFD 的裁剪区域位于对象空间，不随对象 CTM 缩放；写出时直接给页面坐标。
    fn clips_xml(&self) -> String {
        if self.clips.is_empty() {
            return String::new();
        }
        let mut xml = String::from("<ofd:Clips>");
        for clip in &self.clips {
            let path = transform_path(&clip.path, clip.world);
            let boundary = path
                .bounds()
                .unwrap_or_else(|| Rect::new(0.0, 0.0, self.page_width, self.page_height));
            xml.push_str(&format!(
                "<ofd:Clip><ofd:Area><ofd:Path Boundary=\"{}\"><ofd:AbbreviatedData>{}</ofd:AbbreviatedData></ofd:Path></ofd:Area></ofd:Clip>",
                box_text(boundary),
                path_data(&path)
            ));
        }
        xml.push_str("</ofd:Clips>");
        xml
    }

    fn path(&mut self, element: &PathElement, parent: Matrix) {
        if element.path.is_empty() {
            return;
        }
        let transform = element.transform.then(parent);
        let boundary = Rect::new(0.0, 0.0, self.page_width, self.page_height);
        let id = self.state.alloc();
        let mut attributes = format!(
            "ID=\"{id}\" Boundary=\"{}\" CTM=\"{}\" Fill=\"{}\" Stroke=\"{}\"",
            box_text(boundary),
            matrix_text(transform),
            element.fill.is_some(),
            element.stroke.is_some()
        );
        if element.rule == FillRule::EvenOdd {
            attributes.push_str(" Rule=\"EvenOdd\"");
        }
        let mut colors = self.clips_xml();
        if let Some(fill) = &element.fill {
            colors.push_str(&color_element("FillColor", fill, self.opacity));
        }
        if let Some(stroke) = &element.stroke {
            colors.push_str(&color_element("StrokeColor", &stroke.paint, self.opacity));
            attributes.push_str(&stroke_attributes(&stroke.style));
        }
        self.content.push_str(&format!(
            "<ofd:PathObject {attributes}>{colors}<ofd:AbbreviatedData>{}</ofd:AbbreviatedData></ofd:PathObject>",
            path_data(&element.path)
        ));
    }

    fn text(&mut self, element: &TextElement, parent: Matrix) {
        let run = &element.run;
        if run.text.is_empty() {
            return;
        }
        let font_id = self.font(&element.font);
        let transform = element.transform.then(parent);
        let boundary = Rect::new(0.0, 0.0, self.page_width, self.page_height);
        let id = self.state.alloc();
        let mut attributes = format!(
            "ID=\"{id}\" Boundary=\"{}\" CTM=\"{}\" Font=\"{font_id}\" Size=\"{}\" Fill=\"{}\" Stroke=\"{}\"",
            box_text(boundary),
            matrix_text(transform),
            number(element.font_size),
            element.fill.is_some(),
            element.stroke.is_some()
        );
        let mut body = self.clips_xml();
        if let Some(fill) = &element.fill {
            body.push_str(&color_element("FillColor", fill, self.opacity));
        }
        if let Some(stroke) = &element.stroke {
            body.push_str(&color_element("StrokeColor", &stroke.paint, self.opacity));
            attributes.push_str(&stroke_attributes(&stroke.style));
        }
        if !run.glyphs.is_empty() {
            let glyphs = run
                .glyphs
                .iter()
                .map(|glyph| glyph.id.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            body.push_str(&format!(
                "<ofd:CGTransform CodePosition=\"0\" CodeCount=\"{}\" GlyphCount=\"{}\"><ofd:Glyphs>{glyphs}</ofd:Glyphs></ofd:CGTransform>",
                run.char_count(),
                run.glyphs.len()
            ));
        }
        let first = run.glyphs.first().copied().unwrap_or(Glyph {
            id: 0,
            x: 0.0,
            y: 0.0,
            advance: 0.0,
        });
        // 直接用相邻字形的局部坐标差还原步进，避免字体度量或 HScale 引入偏差；
        // DeltaX/DeltaY 是“本字形之后”的偏移，最后一个字形补 0。
        let deltas_x = position_deltas(&run.glyphs, |glyph| glyph.x);
        let deltas_y = position_deltas(&run.glyphs, |glyph| glyph.y);
        body.push_str(&format!(
            "<ofd:TextCode X=\"{}\" Y=\"{}\"",
            number(first.x),
            number(first.y)
        ));
        if !deltas_x.is_empty() {
            body.push_str(&format!(" DeltaX=\"{}\"", joined_deltas(&deltas_x)));
        }
        if deltas_y.iter().any(|delta| delta.abs() > 1e-6) {
            body.push_str(&format!(" DeltaY=\"{}\"", joined_deltas(&deltas_y)));
        }
        body.push_str(&format!(">{}</ofd:TextCode>", xml_escape(&run.text)));
        self.content.push_str(&format!(
            "<ofd:TextObject {attributes}>{body}</ofd:TextObject>"
        ));
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
        let boundary = Rect::new(0.0, 0.0, self.page_width, self.page_height);
        let mut attributes = format!(
            " ID=\"{id}\" Boundary=\"{}\" CTM=\"{}\" ResourceID=\"{resource}\"",
            box_text(boundary),
            matrix_text(ctm)
        );
        if let Some(mask) = mask {
            attributes.push_str(&format!(" ImageMask=\"{mask}\""));
        }
        let alpha = (element.alpha.clamp(0.0, 1.0) * self.opacity).clamp(0.0, 1.0);
        if alpha < 1.0 {
            attributes.push_str(&format!(" Alpha=\"{}\"", (alpha * 255.0).round() as u8));
        }
        self.content.push_str(&format!(
            "<ofd:ImageObject{attributes}>{}</ofd:ImageObject>",
            self.clips_xml()
        ));
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

/// 生成 `<ofd:Outlines>` 片段；没有大纲时返回空串。
fn outline_xml(items: &[OutlineItem], pages: &[PageXml]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut xml = String::from("<ofd:Outlines>");
    push_outline(&mut xml, items, pages, 0);
    xml.push_str("</ofd:Outlines>");
    xml
}

fn push_outline(xml: &mut String, items: &[OutlineItem], pages: &[PageXml], depth: usize) {
    if depth >= MAX_OUTLINE_DEPTH {
        return;
    }
    let first_page = pages.first().map(|page| page.id).unwrap_or(1);
    for item in items {
        xml.push_str("<ofd:OutlineElem Title=\"");
        xml.push_str(&xml_escape(&item.title));
        xml.push_str("\">");
        let page_id = pages
            .get(item.destination.page)
            .map(|page| page.id)
            .unwrap_or(first_page);
        xml.push_str("<ofd:Actions><ofd:Action><ofd:Goto>");
        xml.push_str(&dest_xml(page_id, item.destination));
        xml.push_str("</ofd:Goto></ofd:Action></ofd:Actions>");
        push_outline(xml, &item.children, pages, depth + 1);
        xml.push_str("</ofd:OutlineElem>");
    }
}

/// 目标区域：整页用 `Fit`，带坐标用 `XYZ`。
fn dest_xml(page_id: u32, destination: Destination) -> String {
    let Point { x, y } = destination.point;
    if x.abs() < 1e-6 && y.abs() < 1e-6 {
        format!("<ofd:Dest Type=\"Fit\" PageID=\"{page_id}\"/>")
    } else {
        format!(
            "<ofd:Dest Type=\"XYZ\" PageID=\"{page_id}\" Left=\"{}\" Top=\"{}\"/>",
            number(x),
            number(y)
        )
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

fn page_xml(layer_id: u32, size: ofdbox_folio_ir::Size, content: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ofd:Page xmlns:ofd=\"http://www.ofdspec.org/2016\"><ofd:Area><ofd:PhysicalBox>0 0 {} {}</ofd:PhysicalBox></ofd:Area><ofd:Content><ofd:Layer ID=\"{layer_id}\">{content}</ofd:Layer></ofd:Content></ofd:Page>",
        number(size.width),
        number(size.height)
    )
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

fn color_element(name: &str, paint: &Paint, opacity: f32) -> String {
    match paint {
        Paint::Color(color) => solid_color(name, *color, opacity),
        Paint::LinearGradient(gradient) => {
            if gradient.stops.len() < 2 {
                return solid_color(
                    name,
                    gradient
                        .stops
                        .first()
                        .map_or(Rgba::BLACK, |stop| stop.color),
                    opacity,
                );
            }
            let mut attributes = format!(
                " StartPoint=\"{}\" EndPoint=\"{}\" {}",
                point_text(gradient.start),
                point_text(gradient.end),
                map_type(gradient.extend)
            );
            if gradient.extend != GradientExtend::Pad {
                attributes.push_str(&format!(
                    " MapUnit=\"{}\"",
                    number(distance(gradient.start, gradient.end))
                ));
            }
            format!(
                "<ofd:{name}><ofd:AxialShd{attributes}>{}</ofd:AxialShd></ofd:{name}>",
                segments_xml(&gradient.stops, opacity)
            )
        }
        Paint::RadialGradient(gradient) => {
            if gradient.stops.len() < 2 {
                return solid_color(
                    name,
                    gradient
                        .stops
                        .first()
                        .map_or(Rgba::BLACK, |stop| stop.color),
                    opacity,
                );
            }
            let mut attributes = format!(
                " StartPoint=\"{}\" EndPoint=\"{}\" StartRadius=\"{}\" EndRadius=\"{}\"",
                point_text(gradient.start_center),
                point_text(gradient.end_center),
                number(gradient.start_radius),
                number(gradient.end_radius)
            );
            if gradient.eccentricity.abs() > f64::EPSILON {
                attributes.push_str(&format!(
                    " Eccentricity=\"{}\"",
                    number(gradient.eccentricity)
                ));
            }
            if gradient.angle.abs() > f64::EPSILON {
                attributes.push_str(&format!(" Angle=\"{}\"", number(gradient.angle)));
            }
            attributes.push_str(&format!(" {}", map_type(gradient.extend)));
            if gradient.extend != GradientExtend::Pad {
                attributes.push_str(&format!(
                    " MapUnit=\"{}\"",
                    number(distance(gradient.start_center, gradient.end_center))
                ));
            }
            format!(
                "<ofd:{name}><ofd:RadialShd{attributes}>{}</ofd:RadialShd></ofd:{name}>",
                segments_xml(&gradient.stops, opacity)
            )
        }
        _ => solid_color(name, Rgba::BLACK, opacity),
    }
}

fn solid_color(name: &str, color: Rgba, opacity: f32) -> String {
    let alpha = (f32::from(color.a) * opacity).round().clamp(0.0, 255.0) as u8;
    let mut xml = format!("<ofd:{name} Value=\"{}\"", color_text(color));
    if alpha < 255 {
        xml.push_str(&format!(" Alpha=\"{alpha}\""));
    }
    xml.push_str("/>");
    xml
}

fn segments_xml(stops: &[ofdbox_folio_ir::GradientStop], opacity: f32) -> String {
    stops
        .iter()
        .map(|stop| {
            let alpha = (f32::from(stop.color.a) * opacity)
                .round()
                .clamp(0.0, 255.0) as u8;
            let mut color = format!("<ofd:Color Value=\"{}\"", color_text(stop.color));
            if alpha < 255 {
                color.push_str(&format!(" Alpha=\"{alpha}\""));
            }
            color.push_str("/>");
            format!(
                "<ofd:Segment Position=\"{}\">{color}</ofd:Segment>",
                number(stop.offset.clamp(0.0, 1.0))
            )
        })
        .collect()
}

fn map_type(extend: GradientExtend) -> String {
    // Direct 渐变下 Extend 控制两侧是否延伸；Pad 等价于两侧都延伸。
    let name = match extend {
        GradientExtend::Pad => "Direct\" Extend=\"3",
        GradientExtend::Repeat => "Repeat",
        GradientExtend::Reflect => "Reflect",
    };
    format!("MapType=\"{name}\"")
}

fn point_text(point: ofdbox_folio_ir::Point) -> String {
    format!("{} {}", number(point.x), number(point.y))
}

fn distance(a: ofdbox_folio_ir::Point, b: ofdbox_folio_ir::Point) -> f64 {
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

fn joined_deltas(deltas: &[f64]) -> String {
    deltas
        .iter()
        .map(|delta| number(*delta))
        .collect::<Vec<_>>()
        .join(" ")
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

fn color_text(color: Rgba) -> String {
    format!("{} {} {}", color.r, color.g, color.b)
}

fn stroke_attributes(style: &StrokeStyle) -> String {
    let mut attributes = format!(" LineWidth=\"{}\"", number(style.width));
    attributes.push_str(&format!(
        " Cap=\"{}\" Join=\"{}\" MiterLimit=\"{}\"",
        match style.cap {
            LineCap::Butt => "Butt",
            LineCap::Round => "Round",
            LineCap::Square => "Square",
        },
        match style.join {
            LineJoin::Miter => "Miter",
            LineJoin::Round => "Round",
            LineJoin::Bevel => "Bevel",
        },
        number(style.miter_limit)
    ));
    if !style.dash.is_empty() {
        attributes.push_str(&format!(
            " DashPattern=\"{}\" DashOffset=\"{}\"",
            style
                .dash
                .iter()
                .map(|value| number(*value))
                .collect::<Vec<_>>()
                .join(" "),
            number(style.dash_offset)
        ));
    }
    attributes
}

fn box_text(rect: Rect) -> String {
    format!(
        "{} {} {} {}",
        number(rect.x),
        number(rect.y),
        number(rect.width),
        number(rect.height)
    )
}

fn matrix_text(matrix: Matrix) -> String {
    format!(
        "{} {} {} {} {} {}",
        number(matrix.a),
        number(matrix.b),
        number(matrix.c),
        number(matrix.d),
        number(matrix.e),
        number(matrix.f)
    )
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

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn content_id(pages: &[PageXml], creator: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in creator
        .bytes()
        .chain(pages.iter().flat_map(|page| page.content.bytes()))
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

fn write_entry(
    zip: &mut zip::ZipWriter<Cursor<Vec<u8>>>,
    name: &str,
    data: &[u8],
    options: zip::write::SimpleFileOptions,
) -> Result<(), OfdError> {
    zip.start_file(name, options)
        .map_err(|error| OfdError::Io(std::io::Error::other(error)))?;
    zip.write_all(data)
        .map_err(|error| OfdError::Io(std::io::Error::other(error)))
}

#[cfg(test)]
mod tests {
    use super::{OfdWriteOptions, write_fixed};
    use crate::{OfdDocument, OfdReadOptions};
    use ofdbox_folio_ir::{
        Cluster, Destination, DocumentSource, Element, FillRule, FixedDocument, FontAsset, Glyph,
        GlyphRun, Group, Matrix, OutlineItem, Page, PageInfo, Paint, Path, PathElement, Point,
        Rect, Rgba, Rotation, Scene, Size, TextElement,
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

    #[test]
    fn writes_readable_and_deterministic_package() {
        let mut document = FixedDocument::default();
        document.pages.push(sample_page());
        let options = OfdWriteOptions::default();
        let first = write_fixed(&document, &options).expect("write");
        let second = write_fixed(&document, &options).expect("write again");
        assert_eq!(first, second);

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
    fn keeps_font_names_and_explicit_stroke_flags() {
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
        let mut page = sample_page();
        page.content.root.children.push(Element::Text(TextElement {
            font,
            font_size: 3.0,
            transform: Matrix::IDENTITY,
            run,
            fill: Some(Paint::Color(Rgba::BLACK)),
            stroke: None,
        }));
        let mut document = FixedDocument::default();
        document.pages.push(page);
        let bytes = write_fixed(&document, &OfdWriteOptions::default()).expect("write");

        let content = read_entry(&bytes, "Doc_0/Pages/Page_0/Content.xml");
        assert!(content.contains("Stroke=\"false\""));
        assert!(content.contains("DeltaX="));
        let resources = read_entry(&bytes, "Doc_0/PublicRes.xml");
        assert!(resources.contains("FontName=\"SimSun\""));
        assert!(resources.contains("FamilyName=\"SimSun\""));
        assert!(!resources.contains("FontFile"));
    }

    fn read_entry(bytes: &[u8], name: &str) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
        let mut file = archive.by_name(name).expect("entry");
        let mut text = String::new();
        file.read_to_string(&mut text).expect("read");
        text
    }
}
