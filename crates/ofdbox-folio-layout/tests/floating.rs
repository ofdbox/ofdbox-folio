//! End-to-end tests for floating objects.

use std::sync::Arc;

use ofdbox_folio_ir::{
    Block, Element, FloatingObject, FlowDocument, FontError, FontProvider, FontRequest, ImageAsset,
    ImageBlock, ImageFormat, Metadata, Paragraph, Point, PositionFrom, TextStyle, Twips, WrapMode,
};
use ofdbox_folio_layout::layout;

/// Provider that always returns the same font bytes.
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
        .expect("floating image");
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
    // Relative to the top-left corner of the text area: (25.4, 25.4) + (60, 80).
    assert!((x - (25.4 + 60.0)).abs() < 0.5, "floating x={x}");
    assert!((y - (25.4 + 80.0)).abs() < 0.5, "floating y={y}");
    assert!((w - 20.0).abs() < 0.5, "floating width={w}");

    // The body cursor is not moved: the body is still at the top of the page.
    let text_y = page
        .text
        .lines
        .first()
        .map(|line| line.bbox.y)
        .expect("body line");
    assert!(text_y < 30.0, "body y={text_y}");
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
        .expect("body line");
    assert!(
        text_y >= image_y + image_h - 1.0,
        "the body should be laid out below the image: text_y={text_y} image_bottom={}",
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
    // A floating image at the top of the left half of the page with square
    // wrapping: lines vertically overlapping it must step aside line by line.
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
        .expect("body line");
    assert!(
        text_x >= image_x + image_w - 1.0,
        "the body should step aside to the right of the image: text_x={text_x} image_right={}",
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
    // Relative to the page top-left corner: page margins are not added again.
    assert!((x - 30.0).abs() < 0.5, "page-relative x={x}");
    assert!((y - 40.0).abs() < 0.5, "page-relative y={y}");
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

    // A long enough paragraph: the first few lines are inside the wrap band and
    // the later lines should return to the left margin.
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
    assert!(
        !in_band.is_empty(),
        "some lines should fall inside the wrap band"
    );
    assert!(
        in_band.iter().all(|x| *x >= image_right - 1.0),
        "lines inside the band should step aside to the right of the image: {in_band:?}"
    );
    assert!(
        !below_band.is_empty(),
        "some lines should fall below the wrap band"
    );
    assert!(
        below_band.iter().all(|x| (*x - 25.4).abs() < 1.0),
        "lines below the band should return to the left margin: {below_band:?}"
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
            .any(|diagnostic| diagnostic.message.contains("floating object nested in")),
        "diagnostics: {:?}",
        fixed.report.diagnostics
    );
    assert!(!fixed.pages.is_empty());
}

#[test]
fn page_break_clears_square_wrap_exclusions() {
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
    flow.push(Block::PageBreak);
    flow.push(Block::Paragraph(paragraph(
        "A long paragraph on the next page should use the full text width instead of \
         wrapping around a floating object from the previous page.",
    )));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert!(fixed.pages.len() >= 2, "expected a second page");
    let line = fixed.pages[1]
        .text
        .lines
        .first()
        .expect("line on the second page");
    assert!(
        line.bbox.width > 100.0,
        "square-wrap exclusion leaked to the next page: line bbox {:?}",
        line.bbox
    );
}

#[test]
fn paragraph_relative_anchor_starts_at_the_paragraph_position() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Paragraph(paragraph(
        "A paragraph occupying the top of the page.",
    )));
    let mut image = image_block(&mut flow);
    if let Block::Image(block) = &mut image {
        block.size = Some((Twips::from_mm(20.0), Twips::from_mm(20.0)));
    }
    let mut floating = FloatingObject::new(Point::new(0.0, 0.0), image);
    floating.position_from = (PositionFrom::Paragraph, PositionFrom::Paragraph);
    floating.width = Some(Twips::from_mm(20.0));
    flow.push(Block::Floating(Box::new(floating)));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let (_, image_y, _, _) = image_rect(page);
    let paragraph_bottom = page
        .text
        .lines
        .iter()
        .map(|line| line.bbox.y + line.bbox.height)
        .fold(0.0_f64, f64::max);
    assert!(
        image_y >= paragraph_bottom - 1.0,
        "paragraph-relative anchor should start at the anchor position: image_y={image_y} paragraph_bottom={paragraph_bottom}"
    );
}

#[test]
fn behind_text_floating_image_is_painted_before_text() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Paragraph(paragraph("Text in front of the image.")));
    let image = image_block(&mut flow);
    let mut floating = FloatingObject::new(Point::new(30.0, 30.0), image);
    floating.width = Some(Twips::from_mm(20.0));
    floating.behind_text = true;
    flow.push(Block::Floating(Box::new(floating)));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let children = &fixed.pages[0].content.root.children;
    let image_index = children
        .iter()
        .position(|element| matches!(element, Element::Image(_)))
        .expect("floating image");
    let text_index = children
        .iter()
        .position(|element| matches!(element, Element::Text(_)))
        .expect("body text");
    assert!(
        image_index < text_index,
        "behindDoc image should be painted before text: image={image_index} text={text_index}"
    );
}
