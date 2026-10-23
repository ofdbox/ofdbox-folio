//! PDF 读写集成测试。

use std::io::Read as _;
use std::sync::Arc;

use ofdbox_folio_ir::{
    Cluster, DocumentSource, Element, FillRule, FixedDocument, FontAsset, Glyph, GlyphRun,
    GradientExtend, GradientStop, Group, ImageAsset, ImageElement, ImageFormat, LinearGradient,
    Matrix, Metadata, Page, PageInfo, Paint, Path, PathElement, Point, Rect, ResourceStore, Rgba,
    Rotation, Scene, Size, TextElement,
};
use ofdbox_folio_pdf::{PdfBackend, PdfDocument, PdfOptions, PdfReadOptions, probe, write_fixed};

fn scene() -> Scene {
    Scene {
        size: Size::new(100.0, 100.0),
        root: Group::default(),
        diagnostics: Vec::new(),
    }
}

fn page(scene: Scene) -> Page {
    Page::new(
        PageInfo {
            media_box: Rect::new(0.0, 0.0, 100.0, 100.0),
            crop_box: Rect::new(0.0, 0.0, 100.0, 100.0),
            rotation: Rotation::None,
            label: None,
        },
        scene,
    )
}

fn fixed(scene: Scene) -> FixedDocument {
    let mut document = FixedDocument::default();
    document.pages.push(page(scene));
    document
}

fn rect_path(scene: &mut Scene) {
    let mut path = Path::new();
    path.move_to(Point::new(10.0, 10.0));
    path.line_to(Point::new(90.0, 10.0));
    path.line_to(Point::new(90.0, 90.0));
    path.close();
    scene.root.children.push(Element::Path(PathElement {
        path: Arc::new(path),
        transform: Matrix::IDENTITY,
        fill: Some(Paint::Color(Rgba::from_rgb(255, 0, 0))),
        stroke: None,
        rule: FillRule::NonZero,
    }));
}

fn pdf_text(pdf: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(pdf).into_owned();
    let mut rest = pdf;
    while let Some(start) = rest.windows(7).position(|window| window == b"stream\n") {
        let body = &rest[start + 7..];
        let Some(end) = body.windows(9).position(|window| window == b"endstream") else {
            break;
        };
        let mut decoded = String::new();
        if flate2::read::ZlibDecoder::new(&body[..end])
            .read_to_string(&mut decoded)
            .is_ok()
        {
            text.push_str(&decoded);
        }
        rest = &body[end + 9..];
    }
    text
}

#[test]
fn writes_empty_page() {
    let pdf = write_fixed(&fixed(scene()), &PdfOptions::default()).expect("pdf");
    let text = pdf_text(&pdf);
    assert!(text.starts_with("%PDF-1.7"));
    assert!(text.contains("/MediaBox [0 0 283.4646 283.4646]"));
    assert!(pdf.ends_with(b"%%EOF\n"));
}

#[test]
fn writes_mask_as_soft_mask() {
    // 2×2 图像配 4×4 全黑蒙版：尺寸不同也要重采样后写成 SMask。
    let image = masked_image(
        ImageAsset::new(
            ImageFormat::Png,
            Arc::from(solid_png(2, 2, [200, 30, 30, 255])),
            2,
            2,
        ),
        ImageAsset::new(
            ImageFormat::Png,
            Arc::from(solid_png(4, 4, [0, 0, 0, 255])),
            4,
            4,
        ),
    );
    let mut scene = scene();
    scene
        .root
        .children
        .push(Element::Image(image_element(image)));
    let pdf = write_fixed(&fixed(scene), &PdfOptions::default()).expect("pdf");
    let text = pdf_text(&pdf);
    assert!(text.contains("/SMask"), "缺少 SMask");
    assert!(text.contains("/DeviceGray"));
    let mut document = PdfDocument::open_bytes(pdf, PdfReadOptions::default()).expect("open");
    let page = document.load_page(0).expect("page");
    let image = first_image(&page).expect("image");
    // 蒙版全黑，读回后 alpha 全为 0。
    assert!(png_alphas(&image.encoded).iter().all(|alpha| *alpha == 0));
}

#[test]
fn masked_jpeg_is_not_passed_through() {
    let jpeg = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/stamp.jpg"
    ))
    .expect("jpeg fixture");

    let image = masked_image(
        ImageAsset::new(ImageFormat::Jpeg, Arc::from(jpeg.clone()), 8, 8),
        ImageAsset::new(
            ImageFormat::Png,
            Arc::from(solid_png(8, 8, [0, 0, 0, 255])),
            8,
            8,
        ),
    );
    let mut content = scene();
    content
        .root
        .children
        .push(Element::Image(image_element(image)));
    let text = pdf_text(&write_fixed(&fixed(content), &PdfOptions::default()).expect("pdf"));
    assert!(text.contains("/SMask"), "带蒙版的 JPEG 必须写 SMask");
    assert!(!text.contains("/DCTDecode"), "带蒙版的 JPEG 不能直通");

    let image = plain_image(ImageAsset::new(ImageFormat::Jpeg, Arc::from(jpeg), 8, 8));
    let mut content = scene();
    content
        .root
        .children
        .push(Element::Image(image_element(image)));
    let text = pdf_text(&write_fixed(&fixed(content), &PdfOptions::default()).expect("pdf"));
    assert!(text.contains("/DCTDecode"), "无蒙版 JPEG 仍应直通");
    assert!(!text.contains("/SMask"));
}

fn solid_png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
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

fn masked_image(image: ImageAsset, mask: ImageAsset) -> Arc<ImageAsset> {
    let mut store = ResourceStore::new();
    let mask_id = store.add_image(mask);
    let mask = store.image(mask_id).cloned().expect("mask");
    let image_id = store.add_image(image.with_mask(mask));
    store.image(image_id).cloned().expect("image")
}

fn plain_image(image: ImageAsset) -> Arc<ImageAsset> {
    let mut store = ResourceStore::new();
    let id = store.add_image(image);
    store.image(id).cloned().expect("image")
}

fn image_element(image: Arc<ImageAsset>) -> ImageElement {
    ImageElement {
        image,
        rect: Rect::new(0.0, 0.0, 10.0, 10.0),
        transform: Matrix::IDENTITY,
        alpha: 1.0,
    }
}

fn first_image(page: &Page) -> Option<Arc<ImageAsset>> {
    fn walk(elements: &[Element]) -> Option<Arc<ImageAsset>> {
        for element in elements {
            match element {
                Element::Image(image) => return Some(Arc::clone(&image.image)),
                Element::Group(group) => {
                    if let Some(image) = walk(&group.children) {
                        return Some(image);
                    }
                }
                _ => {}
            }
        }
        None
    }
    walk(&page.content.root.children)
}

fn png_alphas(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().expect("png info");
    let mut buffer = vec![0; reader.output_buffer_size().expect("png size")];
    let info = reader.next_frame(&mut buffer).expect("png frame");
    assert_eq!(info.color_type, png::ColorType::Rgba, "读回的图片应为 RGBA");
    buffer[..info.buffer_size()]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| pixel[3])
        .collect()
}

#[test]
fn writes_path_commands() {
    let mut scene = scene();
    rect_path(&mut scene);
    let pdf = write_fixed(&fixed(scene), &PdfOptions::default()).expect("pdf");
    let text = pdf_text(&pdf);
    assert!(text.contains("10 10 m"));
    assert!(text.contains("90 90 l"));
    assert!(text.contains("1 0 0 rg"));
    assert!(text.contains("f\n"));
}

#[test]
fn writes_image_xobject() {
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, 2, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer
            .write_image_data(&[
                0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255,
            ])
            .expect("png data");
        writer.finish().expect("png finish");
    }
    let mut scene = scene();
    scene.root.children.push(Element::Image(ImageElement {
        image: Arc::new(ImageAsset::new(
            ImageFormat::Png,
            Arc::from(png_bytes),
            2,
            2,
        )),
        rect: Rect::new(0.0, 0.0, 2.0, 2.0),
        transform: Matrix::IDENTITY,
        alpha: 1.0,
    }));
    let pdf = write_fixed(&fixed(scene), &PdfOptions::default()).expect("pdf");
    let text = pdf_text(&pdf);
    assert!(text.contains("/XObject"));
    assert!(text.contains("/Im1"));
    assert!(text.contains("/Im1 Do"));
}

#[test]
fn writes_linear_shading() {
    let mut scene = scene();
    let mut path = Path::new();
    path.move_to(Point::new(10.0, 10.0));
    path.line_to(Point::new(90.0, 10.0));
    path.line_to(Point::new(90.0, 90.0));
    path.close();
    scene.root.children.push(Element::Path(PathElement {
        path: Arc::new(path),
        transform: Matrix::IDENTITY,
        fill: Some(Paint::LinearGradient(LinearGradient {
            start: Point::new(10.0, 50.0),
            end: Point::new(90.0, 50.0),
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
    }));
    let pdf = write_fixed(&fixed(scene), &PdfOptions::default()).expect("pdf");
    let text = pdf_text(&pdf);
    assert!(text.contains("/ShadingType 2"));
    assert!(text.contains("/FunctionType 2"));
    assert!(text.contains(" sh\n"));
}

#[test]
fn writes_group_opacity() {
    let mut scene = scene();
    let mut inner = Group::new(Matrix::IDENTITY);
    inner.opacity = 0.5;
    let mut path = Path::new();
    path.move_to(Point::new(10.0, 10.0));
    path.line_to(Point::new(90.0, 90.0));
    inner.children.push(Element::Path(PathElement {
        path: Arc::new(path),
        transform: Matrix::IDENTITY,
        fill: Some(Paint::Color(Rgba::from_rgb(255, 0, 0))),
        stroke: None,
        rule: FillRule::NonZero,
    }));
    scene.root.children.push(Element::Group(inner));
    let pdf = write_fixed(&fixed(scene), &PdfOptions::default()).expect("pdf");
    let text = pdf_text(&pdf);
    assert!(text.contains("/ExtGState"));
    assert!(text.contains(" gs\n"));
}

#[test]
fn writes_metadata_and_outlines() {
    let mut document = fixed(scene());
    document.pages.push(document.pages[0].clone());
    document.metadata = Metadata {
        title: Some("Test".to_owned()),
        author: Some("OFDBox".to_owned()),
        ..Metadata::default()
    };
    document.outline.push(ofdbox_folio_ir::OutlineItem {
        title: "First".to_owned(),
        destination: ofdbox_folio_ir::Destination {
            page: 0,
            point: Point::new(0.0, 0.0),
        },
        children: Vec::new(),
    });
    document.outline.push(ofdbox_folio_ir::OutlineItem {
        title: "Second".to_owned(),
        destination: ofdbox_folio_ir::Destination {
            page: 1,
            point: Point::new(0.0, 0.0),
        },
        children: Vec::new(),
    });
    let pdf = write_fixed(&document, &PdfOptions::default()).expect("pdf");
    let text = pdf_text(&pdf);
    assert!(text.contains("/Info"));
    assert!(text.contains("/Title (Test)"));
    assert!(text.contains("/Outlines"));
    assert!(text.contains("/PageMode /UseOutlines"));
    assert!(text.contains("/Title (First)"));
    assert_eq!(text.matches("/Dest [").count(), 2);

    let reader = PdfDocument::open_bytes(pdf, PdfReadOptions::default()).expect("read");
    let outline = &reader.info().outline;
    assert_eq!(outline.len(), 2);
    assert_eq!(outline[0].title, "First");
    assert_eq!(outline[0].destination.page, 0);
    assert_eq!(outline[1].title, "Second");
    assert_eq!(outline[1].destination.page, 1);
}

#[test]
fn keeps_nested_outline_with_coordinates() {
    let mut document = fixed(scene());
    document.pages.push(page(scene()));
    document.pages.push(page(scene()));
    document.outline.push(ofdbox_folio_ir::OutlineItem {
        title: "第一章".to_owned(),
        destination: ofdbox_folio_ir::Destination {
            page: 0,
            point: Point::new(0.0, 0.0),
        },
        children: vec![ofdbox_folio_ir::OutlineItem {
            title: "1.1 节".to_owned(),
            destination: ofdbox_folio_ir::Destination {
                page: 2,
                point: Point::new(12.5, 34.0),
            },
            children: Vec::new(),
        }],
    });
    let pdf = write_fixed(&document, &PdfOptions::default()).expect("pdf");
    let reader = PdfDocument::open_bytes(pdf, PdfReadOptions::default()).expect("read");
    let outline = &reader.info().outline;
    assert_eq!(outline.len(), 1, "大纲: {outline:?}");
    assert_eq!(outline[0].title, "第一章");
    assert_eq!(outline[0].children.len(), 1);
    let child = &outline[0].children[0];
    assert_eq!(child.title, "1.1 节");
    assert_eq!(child.destination.page, 2);
    // PDF 坐标以点为单位，往返后按 0.01 毫米精度比较。
    assert!(
        (child.destination.point.x - 12.5).abs() < 0.01,
        "x = {}",
        child.destination.point.x
    );
    assert!(
        (child.destination.point.y - 34.0).abs() < 0.01,
        "y = {}",
        child.destination.point.y
    );
}

#[test]
fn backend_writes_single_page() {
    let pdf = PdfBackend::new().render(&scene()).expect("pdf");
    assert!(pdf.starts_with(b"%PDF-1.7"));
}

#[test]
fn generated_page_reads_back() {
    let mut scene = scene();
    rect_path(&mut scene);
    let pdf = write_fixed(&fixed(scene), &PdfOptions::default()).expect("pdf");
    assert!(probe(&pdf));
    let mut document = PdfDocument::open_bytes(pdf, PdfReadOptions::default()).expect("open");
    assert_eq!(document.info().page_count, 1);
    let info = document.page_info(0).expect("info");
    assert!((info.crop_box.width - 100.0).abs() < 0.01);
    assert!((info.crop_box.height - 100.0).abs() < 0.01);
    assert_eq!(info.rotation, Rotation::None);
    let page = document.load_page(0).expect("page");
    assert!(!page.content.root.children.is_empty());
}

#[cfg(windows)]
fn text_page() -> Option<Page> {
    let bytes = std::fs::read("C:/Windows/Fonts/arial.ttf").ok()?;
    let face = ttf_parser::Face::parse(&bytes, 0).ok()?;
    let glyph = face.glyph_index('A')?;
    let font = Arc::new(FontAsset::new(Arc::from(bytes), 0).with_family("Arial"));
    let run = Arc::new(GlyphRun::new(
        "A".to_owned(),
        vec![Glyph {
            id: u32::from(glyph.0),
            x: 0.0,
            y: 0.0,
            advance: 10.0,
        }],
        vec![Cluster {
            text_start: 0,
            text_end: 1,
            glyph_start: 0,
            glyph_end: 1,
        }],
    ));
    let mut scene = scene();
    scene.root.children.push(Element::Text(TextElement {
        font,
        font_size: 10.0,
        transform: Matrix::IDENTITY,
        run,
        fill: Some(Paint::Color(Rgba::BLACK)),
        stroke: None,
    }));
    Some(page(scene))
}

#[cfg(windows)]
#[test]
fn embeds_and_reads_text() {
    let Some(page) = text_page() else {
        return;
    };
    let mut document = FixedDocument::default();
    document.pages.push(page);
    let pdf = write_fixed(&document, &PdfOptions::default()).expect("pdf");
    let text = pdf_text(&pdf);
    assert!(text.contains("/Type0"));
    assert!(text.contains(" Tj\n"));
    assert!(text.contains("/ToUnicode"));

    let mut reader = PdfDocument::open_bytes(pdf, PdfReadOptions::default()).expect("read");
    let page = reader.load_page(0).expect("page");
    assert!(page.text.plain_text().contains('A'));
}

#[test]
fn reads_gradient_shading() {
    let mut scene = scene();
    let mut path = Path::new();
    path.move_to(Point::new(10.0, 10.0));
    path.line_to(Point::new(90.0, 10.0));
    path.line_to(Point::new(90.0, 90.0));
    path.close();
    scene.root.children.push(Element::Path(PathElement {
        path: Arc::new(path),
        transform: Matrix::IDENTITY,
        fill: Some(Paint::LinearGradient(LinearGradient {
            start: Point::new(10.0, 50.0),
            end: Point::new(90.0, 50.0),
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
    }));
    let pdf = write_fixed(&fixed(scene), &PdfOptions::default()).expect("pdf");
    let mut document = PdfDocument::open_bytes(pdf, PdfReadOptions::default()).expect("open");
    let page = document.load_page(0).expect("page");

    fn find_gradient(group: &Group) -> Option<Paint> {
        for child in &group.children {
            match child {
                Element::Group(group) => {
                    if let Some(paint) = find_gradient(group) {
                        return Some(paint);
                    }
                }
                Element::Path(path) => {
                    if let Some(paint @ Paint::LinearGradient(_)) = &path.fill {
                        return Some(paint.clone());
                    }
                }
                _ => {}
            }
        }
        None
    }

    let Some(Paint::LinearGradient(gradient)) = find_gradient(&page.content.root) else {
        panic!("gradient shading should be read back");
    };
    assert_eq!(gradient.start, Point::new(10.0, 50.0));
    assert_eq!(gradient.end, Point::new(90.0, 50.0));
    assert!(gradient.stops.len() >= 2);
    assert!(gradient.stops[0].color.r > 240);
    assert!(gradient.stops[0].color.b < 20);
    let last = gradient.stops.last().expect("last stop");
    assert!(last.color.b > 240);
    assert!(last.color.r < 20);
}

#[test]
fn reads_shading_pattern_fill() {
    use lopdf::{Document as SourceDocument, Object, Stream, dictionary};

    let mut source = SourceDocument::with_version("1.7");
    let pages_id = source.new_object_id();
    let function_id = source.add_object(dictionary! {
        "FunctionType" => Object::Integer(2),
        "Domain" => Object::Array(vec![Object::Integer(0), Object::Integer(1)]),
        "C0" => Object::Array(vec![Object::Integer(1), Object::Integer(0), Object::Integer(0)]),
        "C1" => Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(1)]),
        "N" => Object::Integer(1),
    });
    let shading_id = source.add_object(dictionary! {
        "ShadingType" => Object::Integer(2),
        "ColorSpace" => Object::Name(b"DeviceRGB".to_vec()),
        "Coords" => Object::Array(vec![
            Object::Integer(10),
            Object::Integer(10),
            Object::Integer(90),
            Object::Integer(10),
        ]),
        "Extend" => Object::Array(vec![Object::Boolean(true), Object::Boolean(true)]),
        "Function" => Object::Reference(function_id),
    });
    let pattern_id = source.add_object(dictionary! {
        "PatternType" => Object::Integer(2),
        "Shading" => Object::Reference(shading_id),
    });
    let resources = dictionary! {
        "Pattern" => dictionary! {
            "P1" => Object::Reference(pattern_id),
        },
    };
    let content_id = source.add_object(Stream::new(
        dictionary! {},
        b"q /Pattern cs /P1 scn 10 10 80 80 re f Q".to_vec(),
    ));
    let page_id = source.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(100),
        ]),
        "Resources" => Object::Dictionary(resources),
        "Contents" => Object::Reference(content_id),
    });
    source.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            "Count" => Object::Integer(1),
        }),
    );
    let catalog_id = source.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    source.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    source.save_to(&mut bytes).expect("save");

    let mut document = PdfDocument::open_bytes(bytes, PdfReadOptions::default()).expect("open");
    let page = document.load_page(0).expect("page");

    fn find_gradient(group: &Group) -> Option<Paint> {
        for child in &group.children {
            match child {
                Element::Group(group) => {
                    if let Some(paint) = find_gradient(group) {
                        return Some(paint);
                    }
                }
                Element::Path(path) => {
                    if let Some(paint @ Paint::LinearGradient(_)) = &path.fill {
                        return Some(paint.clone());
                    }
                }
                _ => {}
            }
        }
        None
    }

    let Some(Paint::LinearGradient(gradient)) = find_gradient(&page.content.root) else {
        panic!("shading pattern should be read as a gradient fill");
    };
    assert_eq!(gradient.start, Point::new(10.0, 10.0));
    assert_eq!(gradient.end, Point::new(90.0, 10.0));
    assert!(gradient.stops[0].color.r > 240);
    assert!(gradient.stops.last().expect("last").color.b > 240);
}

#[test]
fn reads_inline_image() {
    use lopdf::{Document as SourceDocument, Object, Stream, dictionary};

    let mut source = SourceDocument::with_version("1.7");
    let pages_id = source.new_object_id();
    let resources = dictionary! {};
    let content_id = source.add_object(Stream::new(
        dictionary! {},
        b"q 10 0 0 10 20 20 cm BI /W 1 /H 1 /CS /RGB /BPC 8 ID \xff\x00\x00\nEI Q".to_vec(),
    ));
    let page_id = source.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(100),
        ]),
        "Resources" => Object::Dictionary(resources),
        "Contents" => Object::Reference(content_id),
    });
    source.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            "Count" => Object::Integer(1),
        }),
    );
    let catalog_id = source.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    source.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    source.save_to(&mut bytes).expect("save");

    let mut document = PdfDocument::open_bytes(bytes, PdfReadOptions::default()).expect("open");
    let page = document.load_page(0).expect("page");

    fn find_image(group: &Group) -> Option<&ImageElement> {
        for child in &group.children {
            match child {
                Element::Group(group) => {
                    if let Some(image) = find_image(group) {
                        return Some(image);
                    }
                }
                Element::Image(image) => return Some(image),
                _ => {}
            }
        }
        None
    }

    let image = find_image(&page.content.root).expect("inline image should be read");
    assert_eq!(image.image.pixel_width, 1);
    assert_eq!(image.image.pixel_height, 1);
    assert_eq!(image.image.format, ImageFormat::Png);

    let decoder = png::Decoder::new(std::io::Cursor::new(image.image.encoded.to_vec()));
    let mut reader = decoder.read_info().expect("png info");
    let mut buffer = vec![0; reader.output_buffer_size().expect("buffer size")];
    let info = reader.next_frame(&mut buffer).expect("png frame");
    assert_eq!(info.width, 1);
    assert_eq!(info.height, 1);
    assert_eq!(&buffer[..4], &[255, 0, 0, 255]);
}

#[test]
fn reads_attachments() {
    use lopdf::{Document as SourceDocument, Object, Stream, dictionary};

    let mut source = SourceDocument::with_version("1.7");
    let pages_id = source.new_object_id();
    let stream_id = source.add_object(Stream::new(
        dictionary! {
            "Type" => "EmbeddedFile",
            "Subtype" => Object::Name(b"text#2Fplain".to_vec()),
        },
        b"hello".to_vec(),
    ));
    let filespec_id = source.add_object(dictionary! {
        "Type" => "Filespec",
        "F" => Object::String(b"readme.txt".to_vec(), lopdf::StringFormat::Literal),
        "UF" => Object::String(b"readme.txt".to_vec(), lopdf::StringFormat::Literal),
        "EF" => dictionary! { "F" => Object::Reference(stream_id) },
    });
    let names = dictionary! {
        "EmbeddedFiles" => dictionary! {
            "Names" => Object::Array(vec![
                Object::String(b"readme.txt".to_vec(), lopdf::StringFormat::Literal),
                Object::Reference(filespec_id),
            ]),
        },
    };
    let content_id = source.add_object(Stream::new(dictionary! {}, b"".to_vec()));
    let page_id = source.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(100),
        ]),
        "Contents" => Object::Reference(content_id),
    });
    source.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            "Count" => Object::Integer(1),
        }),
    );
    let catalog_id = source.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
        "Names" => Object::Dictionary(names),
    });
    source.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    source.save_to(&mut bytes).expect("save");

    let mut reader = PdfDocument::open_bytes(bytes, PdfReadOptions::default()).expect("open");
    let attachments = reader.info().attachments.clone();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].name, "readme.txt");
    assert_eq!(attachments[0].format.as_deref(), Some("text/plain"));
    assert_eq!(attachments[0].size, 5);
    let data = reader
        .load_attachment(attachments[0].id)
        .expect("attachment");
    assert_eq!(&data[..], b"hello");
}

#[test]
fn opens_encrypted_pdf_with_password() {
    use ofdbox_folio_pdf::PdfError;

    let bytes = include_bytes!("fixtures/encrypted_user.pdf").to_vec();
    let error = PdfDocument::open_bytes(bytes.clone(), PdfReadOptions::default())
        .err()
        .expect("password should be required");
    assert!(matches!(error, PdfError::PasswordRequired));

    let options = PdfReadOptions {
        password: Some("wrong".to_owned()),
        ..PdfReadOptions::default()
    };
    let error = PdfDocument::open_bytes(bytes.clone(), options)
        .err()
        .expect("wrong password should fail");
    assert!(matches!(error, PdfError::PasswordIncorrect));

    let options = PdfReadOptions {
        password: Some("user".to_owned()),
        ..PdfReadOptions::default()
    };
    let mut document = PdfDocument::open_bytes(bytes, options).expect("open");
    assert_eq!(document.info().page_count, 1);
    let page = document.load_page(0).expect("page");
    assert!(!page.content.root.children.is_empty());
}

#[test]
fn opens_permission_only_encrypted_pdf() {
    let bytes = include_bytes!("fixtures/encrypted_owner.pdf").to_vec();
    let document = PdfDocument::open_bytes(bytes, PdfReadOptions::default()).expect("open");
    assert_eq!(document.info().page_count, 1);
}
