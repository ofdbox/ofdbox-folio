//! End-to-end tests for single-column layout.
//!
//! Metrics come from the system's Arial; when the font is unavailable the tests
//! are skipped so they stay runnable across machines.

use std::sync::Arc;

use ofdbox_folio_ir::{
    Align, Block, Field, FieldKind, FlowDocument, FontError, FontProvider, FontRequest, ImageAsset,
    ImageBlock, ImageFormat, Inline, LineSpacing, Metadata, NamedStyle, PageSetup, ParaStyle,
    Paragraph, Size, TextRun, TextStyle, Twips,
};
use ofdbox_folio_layout::{LayoutOptions, layout, layout_with};

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

fn doc() -> FlowDocument {
    FlowDocument::new(Metadata::default())
}

#[test]
fn lays_out_headings_paragraphs_and_text_layer() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = doc();
    let mut title = Paragraph::plain(
        "Document Title",
        TextStyle {
            size: Twips::from_pt(22.0),
            ..TextStyle::default()
        },
    );
    title.heading_level = Some(1);
    flow.push(Block::Paragraph(title));
    flow.push(Block::Paragraph(paragraph(
        "The quick brown fox jumps over the lazy dog. \
         Pack my box with five dozen liquor jugs.",
    )));
    let mut section = Paragraph::plain(
        "Section One",
        TextStyle {
            size: Twips::from_pt(16.0),
            ..TextStyle::default()
        },
    );
    section.heading_level = Some(2);
    flow.push(Block::Paragraph(section));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert_eq!(
        fixed.pages.len(),
        1,
        "a short document should fit on one page"
    );
    let page = &fixed.pages[0];
    assert!((page.display_size().width - 210.0).abs() < 0.5);
    assert!((page.display_size().height - 297.0).abs() < 0.5);
    assert!(!page.text.items.is_empty(), "there should be text items");
    assert!(!page.text.lines.is_empty(), "there should be lines");
    assert!(
        page.text.plain_text().contains("quick brown"),
        "text layer content: {}",
        page.text.plain_text()
    );

    // Headings build the outline: a level-1 heading with one level-2 child.
    assert_eq!(fixed.outline.len(), 1, "outline: {:?}", fixed.outline);
    assert_eq!(fixed.outline[0].title, "Document Title");
    assert_eq!(fixed.outline[0].children.len(), 1);
    assert_eq!(fixed.outline[0].children[0].title, "Section One");

    // All text items are inside the page margins.
    for item in &page.text.items {
        assert!(
            item.bbox.x >= 20.0 && item.bbox.right() <= 190.0,
            "text out of bounds: {:?}",
            item.bbox
        );
    }
}

#[test]
fn paginates_long_text() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = doc();
    for index in 0..120 {
        flow.push(Block::Paragraph(paragraph(&format!(
            "Paragraph {index}: the quick brown fox jumps over the lazy dog again and again."
        ))));
    }
    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert!(fixed.pages.len() > 2, "pages: {}", fixed.pages.len());
    for page in &fixed.pages {
        assert!(!page.text.items.is_empty(), "every page should have text");
    }
    // Lines do not overlap.
    for page in &fixed.pages {
        for pair in page.text.lines.windows(2) {
            assert!(
                pair[1].baseline > pair[0].baseline,
                "baselines should increase"
            );
        }
    }
}

#[test]
fn renders_page_number_and_total_pages() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = doc();
    for index in 0..80 {
        flow.push(Block::Paragraph(paragraph(&format!(
            "Filler line {index}, lorem ipsum dolor sit amet consectetur."
        ))));
    }
    let mut footer = Paragraph::plain("Page ", body_style());
    footer
        .runs
        .push(Inline::Field(Field::new(FieldKind::PageNumber)));
    footer
        .runs
        .push(Inline::Text(TextRun::new(" of ", body_style())));
    footer
        .runs
        .push(Inline::Field(Field::new(FieldKind::TotalPages)));
    flow.push(Block::Paragraph(footer));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let total = fixed.pages.len();
    assert!(total > 1);
    let last = fixed.pages.last().expect("last page");
    let text = last.text.plain_text();
    assert!(
        text.contains(&format!("Page {total} of {total}")),
        "last page text: {text}"
    );
}

#[test]
fn honours_justification() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = doc();
    let style = ParaStyle {
        align: Align::Justify,
        ..ParaStyle::default()
    };
    flow.push(Block::Paragraph(Paragraph {
        style,
        runs: vec![Inline::Text(TextRun::new(
            "Justified text should stretch its spaces so that every line but the last one \
             reaches the right margin exactly, and the last line stays left aligned.",
            body_style(),
        ))],
        heading_level: None,
        anchor: None,
    }));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    assert!(
        page.text.lines.len() >= 2,
        "line count: {}",
        page.text.lines.len()
    );
    let content_right = 210.0 - 25.4;
    let first = &page.text.lines[0];
    assert!(
        (first.bbox.right() - content_right).abs() < 1.0,
        "first line right edge {:.2}, expected {:.2}",
        first.bbox.right(),
        content_right
    );
}

#[test]
fn lays_out_block_image_with_intrinsic_size() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = doc();
    // 32×16 pixels at 96 DPI → 8.47×4.23 millimeters.
    let id = flow.resources.add_image(ImageAsset::new(
        ImageFormat::Png,
        Arc::from(&b"png"[..]),
        32,
        16,
    ));
    flow.push(Block::Image(ImageBlock::new(id)));

    let fixed = layout_with(&flow, &mut fonts, LayoutOptions::default()).expect("layout");
    let page = &fixed.pages[0];
    let image = page
        .content
        .root
        .children
        .iter()
        .find_map(|element| match element {
            ofdbox_folio_ir::Element::Image(image) => Some(image),
            _ => None,
        })
        .expect("image element");
    let size = Size::new(
        image.transform.a * f64::from(image.image.pixel_width),
        image.transform.d * f64::from(image.image.pixel_height),
    );
    assert!((size.width - 8.47).abs() < 0.1, "width: {}", size.width);
    assert!((size.height - 4.23).abs() < 0.1, "height: {}", size.height);
}

#[test]
fn page_setup_margins_are_respected() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = doc();
    let mut setup = PageSetup::a4();
    setup.margins.left = Twips::from_mm(30.0);
    setup.margins.top = Twips::from_mm(40.0);
    flow.sections[0].setup = setup;
    flow.push(Block::Paragraph(paragraph("Hello margins.")));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let item = page.text.items.first().expect("text item");
    assert!(
        (item.bbox.x - 30.0).abs() < 0.5,
        "left margin: {}",
        item.bbox.x
    );
    assert!(item.bbox.y >= 39.0, "top margin: {}", item.bbox.y);
}

#[test]
fn inherits_named_paragraph_style_through_based_on() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = doc();
    flow.styles.paragraph = vec![
        NamedStyle {
            id: "Body".to_owned(),
            name: "Body".to_owned(),
            based_on: None,
            props: ParaStyle {
                first_line_indent: Twips::from_pt(24.0),
                line_spacing: LineSpacing::Multiple(1.5),
                ..ParaStyle::default()
            },
        },
        NamedStyle {
            id: "BodyIndent".to_owned(),
            name: "BodyIndent".to_owned(),
            based_on: Some("Body".to_owned()),
            props: ParaStyle {
                left_indent: Twips::from_mm(10.0),
                ..ParaStyle::default()
            },
        },
    ];
    flow.push(Block::Paragraph(Paragraph {
        style: ParaStyle {
            style_id: Some("BodyIndent".to_owned()),
            ..ParaStyle::default()
        },
        runs: vec![Inline::Text(TextRun::new(
            "Indented body text.",
            body_style(),
        ))],
        heading_level: None,
        anchor: None,
    }));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let item = fixed.pages[0].text.items.first().expect("text item");
    // Left margin 25.4 + left indent 10 + inherited first-line indent of 24pt.
    let expected = 25.4 + 10.0 + 24.0 * 25.4 / 72.0;
    assert!(
        (item.bbox.x - expected).abs() < 0.5,
        "first line starts at {:.2}, expected {:.2}",
        item.bbox.x,
        expected
    );
}
