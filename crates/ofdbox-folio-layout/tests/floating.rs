//! 浮动对象的端到端测试。

use std::sync::Arc;

use ofdbox_folio_ir::{
    Block, FloatingObject, FlowDocument, FontError, FontProvider, FontRequest, ImageAsset,
    ImageBlock, ImageFormat, Metadata, Paragraph, Point, PositionFrom, TextStyle, Twips, WrapMode,
};
use ofdbox_folio_layout::layout;

/// 固定返回同一份字体字节的提供者。
struct SystemFonts {
    bytes: Arc<[u8]>,
}

impl FontProvider for SystemFonts {
    fn load(&mut self, _request: &FontRequest<'_>) -> Result<Option<Arc<[u8]>>, FontError> {
        Ok(Some(Arc::clone(&self.bytes)))
    }
}

fn fonts() -> Option<SystemFonts> {
    let path = if cfg!(windows) {
        "C:/Windows/Fonts/arial.ttf"
    } else {
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
    };
    let bytes = std::fs::read(path).ok()?;
    Some(SystemFonts {
        bytes: Arc::from(bytes),
    })
}

fn body_style() -> TextStyle {
    TextStyle {
        size: Twips::from_pt(11.0),
        ..TextStyle::default()
    }
}

fn paragraph(text: &str) -> Paragraph {
    Paragraph::plain(text, body_style())
}

fn image_block(flow: &mut FlowDocument) -> Block {
    let id = flow.resources.add_image(ImageAsset::new(
        ImageFormat::Png,
        Arc::from(&b"png"[..]),
        40,
        40,
    ));
    Block::Image(ImageBlock::new(id))
}

fn image_rect(page: &ofdbox_folio_ir::Page) -> (f64, f64, f64, f64) {
    let image = page
        .content
        .root
        .children
        .iter()
        .find_map(|element| match element {
            ofdbox_folio_ir::Element::Image(image) => Some(image),
            _ => None,
        })
        .expect("浮动图片");
    (
        image.transform.e,
        image.transform.f,
        image.transform.a * f64::from(image.image.pixel_width),
        image.transform.d * f64::from(image.image.pixel_height),
    )
}

#[test]
fn floating_object_is_positioned_absolutely() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let mut image = image_block(&mut flow);
    if let Block::Image(block) = &mut image {
        block.size = Some((Twips::from_mm(20.0), Twips::from_mm(20.0)));
    }
    let mut floating = FloatingObject::new(Point::new(60.0, 80.0), image);
    floating.width = Some(Twips::from_mm(20.0));
    flow.push(Block::Floating(Box::new(floating)));
    flow.push(Block::Paragraph(paragraph("正文从页顶开始")));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let (x, y, w, _h) = image_rect(page);
    // 相对版心左上角：(25.4, 25.4) + (60, 80)。
    assert!((x - (25.4 + 60.0)).abs() < 0.5, "浮动 x={x}");
    assert!((y - (25.4 + 80.0)).abs() < 0.5, "浮动 y={y}");
    assert!((w - 20.0).abs() < 0.5, "浮动宽度={w}");

    // 不移交正文游标：正文仍在页首。
    let text_y = page
        .text
        .lines
        .first()
        .map(|line| line.bbox.y)
        .expect("正文行");
    assert!(text_y < 30.0, "正文 y={text_y}");
}

#[test]
fn top_bottom_wrap_pushes_text_below() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let image = image_block(&mut flow);
    let mut floating = FloatingObject::new(Point::new(20.0, 60.0), image);
    floating.width = Some(Twips::from_mm(20.0));
    floating.wrap = WrapMode::TopBottom;
    flow.push(Block::Floating(Box::new(floating)));
    flow.push(Block::Paragraph(paragraph("这一段应在图片下方")));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let (_, image_y, _, image_h) = image_rect(page);
    let text_y = page
        .text
        .lines
        .first()
        .map(|line| line.bbox.y)
        .expect("正文行");
    assert!(
        text_y >= image_y + image_h - 1.0,
        "正文应排在图片下方: text_y={text_y} image_bottom={}",
        image_y + image_h
    );
}

#[test]
fn square_wrap_indents_paragraphs_beside_the_object() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let image = image_block(&mut flow);
    // 左半页顶部的浮动图，四周型绕排：正文与它垂直重叠时要逐行让开。
    let mut floating = FloatingObject::new(Point::new(10.0, 0.0), image);
    floating.width = Some(Twips::from_mm(25.0));
    floating.wrap = WrapMode::Square;
    flow.push(Block::Floating(Box::new(floating)));
    flow.push(Block::Paragraph(paragraph(
        "这段正文应该让开左边的图片，从图片右侧开始排，而不是压在图片上。",
    )));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let (image_x, _, image_w, _) = image_rect(page);
    let text_x = page
        .text
        .lines
        .first()
        .map(|line| line.bbox.x)
        .expect("正文行");
    assert!(
        text_x >= image_x + image_w - 1.0,
        "正文应让到图片右侧: text_x={text_x} image_right={}",
        image_x + image_w
    );
}

#[test]
fn page_relative_position_ignores_margins() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let mut image = image_block(&mut flow);
    if let Block::Image(block) = &mut image {
        block.size = Some((Twips::from_mm(20.0), Twips::from_mm(20.0)));
    }
    let mut floating = FloatingObject::new(Point::new(30.0, 40.0), image);
    floating.width = Some(Twips::from_mm(20.0));
    floating.position_from = (PositionFrom::Page, PositionFrom::Page);
    flow.push(Block::Floating(Box::new(floating)));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let (x, y, _, _) = image_rect(&fixed.pages[0]);
    // 相对页面左上角：不再叠加页边距。
    assert!((x - 30.0).abs() < 0.5, "page 基准 x={x}");
    assert!((y - 40.0).abs() < 0.5, "page 基准 y={y}");
}

#[test]
fn square_wrap_releases_lines_below_the_object() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let mut image = image_block(&mut flow);
    if let Block::Image(block) = &mut image {
        block.size = Some((Twips::from_mm(25.0), Twips::from_mm(20.0)));
    }
    let mut floating = FloatingObject::new(Point::new(10.0, 0.0), image);
    floating.width = Some(Twips::from_mm(25.0));
    floating.wrap = WrapMode::Square;
    flow.push(Block::Floating(Box::new(floating)));

    // 足够长的一段：前几行在绕排带里，后面的行应该回到左边距。
    let text = (0..20)
        .map(|index| format!("第{index}句用来占满多行以便观察逐行绕排。"))
        .collect::<Vec<_>>()
        .join("");
    flow.push(Block::Paragraph(paragraph(&text)));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let (_, _, image_w, image_h) = image_rect(page);
    let image_right = 25.4 + 10.0 + image_w;
    let band_bottom = 25.4 + image_h;

    let xs: Vec<(f64, f64)> = page
        .text
        .lines
        .iter()
        .map(|line| (line.bbox.x, line.bbox.y))
        .collect();
    let in_band: Vec<f64> = xs
        .iter()
        .filter(|(_, y)| *y < band_bottom - 1.0)
        .map(|(x, _)| *x)
        .collect();
    let below_band: Vec<f64> = xs
        .iter()
        .filter(|(_, y)| *y > band_bottom + 1.0)
        .map(|(x, _)| *x)
        .collect();
    assert!(!in_band.is_empty(), "应有行落在绕排带里");
    assert!(
        in_band.iter().all(|x| *x >= image_right - 1.0),
        "带内的行应让到图片右侧: {in_band:?}"
    );
    assert!(!below_band.is_empty(), "应有行落在绕排带下方");
    assert!(
        below_band.iter().all(|x| (*x - 25.4).abs() < 1.0),
        "带下方的行应回到左边距: {below_band:?}"
    );
}

#[test]
fn floating_object_in_table_cell_falls_back_to_inline() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let image = image_block(&mut flow);
    let floating = FloatingObject::new(Point::new(10.0, 10.0), image);
    let mut table = ofdbox_folio_ir::Table::new(vec![ofdbox_folio_ir::Column {
        width: ofdbox_folio_ir::ColumnWidth::Twips(Twips::from_mm(60.0)),
    }]);
    table.rows = vec![ofdbox_folio_ir::Row::new(vec![ofdbox_folio_ir::Cell::new(
        vec![Block::Floating(Box::new(floating))],
    )])];
    flow.push(Block::Table(table));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert!(
        fixed
            .report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("浮动对象嵌在")),
        "诊断: {:?}",
        fixed.report.diagnostics
    );
    assert!(!fixed.pages.is_empty());
}
