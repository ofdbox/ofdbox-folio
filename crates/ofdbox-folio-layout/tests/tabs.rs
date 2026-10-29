//! End-to-end tests for tab stops and leaders.

use std::sync::Arc;

use ofdbox_folio_ir::{
    Block, Element, FlowDocument, FontError, FontProvider, FontRequest, Inline, LineSpacing,
    Metadata, ParaStyle, Paragraph, TabAlign, TabLeader, TabStop, TextRun, TextStyle, Twips,
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

/// A paragraph that separates two pieces of text with a tab stop.
fn tabbed(left: &str, right: &str, stop: TabStop) -> Paragraph {
    let mut paragraph = Paragraph::new(ParaStyle {
        tabs: vec![stop],
        ..ParaStyle::default()
    });
    paragraph
        .runs
        .push(Inline::Text(TextRun::new(left, body_style())));
    paragraph
        .runs
        .push(Inline::Text(TextRun::new("\t", body_style())));
    paragraph
        .runs
        .push(Inline::Text(TextRun::new(right, body_style())));
    paragraph
}

/// The characters and positions in the page's text layer.
fn chars(page: &ofdbox_folio_ir::Page) -> Vec<(String, f64)> {
    page.text
        .items
        .iter()
        .map(|item| (item.run.text.clone(), item.bbox.x))
        .collect()
}

/// Right edge of a character (its first occurrence).
fn right_edge(page: &ofdbox_folio_ir::Page, needle: &str) -> f64 {
    page.text
        .items
        .iter()
        .find(|item| item.run.text == needle)
        .map(|item| item.bbox.right())
        .unwrap_or_else(|| panic!("did not find {needle}"))
}

const MARGIN: f64 = 25.4;
const CONTENT: f64 = 210.0 - 2.0 * 25.4;

#[test]
fn right_tab_puts_trailing_text_at_the_stop() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Paragraph(tabbed(
        "Chapter One",
        "12",
        TabStop::new(Twips::from_mm(CONTENT), TabAlign::Right, TabLeader::None),
    )));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    // The right edge of page number "12" should sit on the right tab stop (the
    // right edge of the text area).
    let right = right_edge(page, "2");
    assert!(
        (right - (MARGIN + CONTENT)).abs() < 0.6,
        "page number right edge {right}, expected {}",
        MARGIN + CONTENT
    );
    // The tab itself produces no glyph.
    assert!(!chars(page).iter().any(|(text, _)| text.contains('\t')));
}

#[test]
fn left_tab_moves_to_the_stop() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Paragraph(tabbed(
        "A",
        "B",
        TabStop::new(Twips::from_mm(80.0), TabAlign::Left, TabLeader::None),
    )));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let b = page
        .text
        .items
        .iter()
        .find(|item| item.run.text == "B")
        .map(|item| item.bbox.x)
        .expect("B");
    assert!(
        (b - (MARGIN + 80.0)).abs() < 0.6,
        "left tab stop lands at {b}, expected {}",
        MARGIN + 80.0
    );
}

#[test]
fn centre_and_decimal_tabs_align_as_expected() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Paragraph(tabbed(
        "价格",
        "12.50",
        TabStop::new(Twips::from_mm(60.0), TabAlign::Decimal, TabLeader::None),
    )));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    // The decimal point "." should land on the tab stop.
    let dot = page
        .text
        .items
        .iter()
        .find(|item| item.run.text == ".")
        .map(|item| item.bbox.x)
        .expect("decimal point");
    assert!(
        (dot - (MARGIN + 60.0)).abs() < 0.8,
        "decimal point at {dot}, expected {}",
        MARGIN + 60.0
    );

    // Center tab stop: the centre of the text lands on the tab stop.
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Paragraph(tabbed(
        "X",
        "MID",
        TabStop::new(Twips::from_mm(90.0), TabAlign::Center, TabLeader::None),
    )));
    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let middle_right = right_edge(page, "D");
    let middle_left = page
        .text
        .items
        .iter()
        .find(|item| item.run.text == "M")
        .map(|item| item.bbox.x)
        .expect("M");
    let centre = (middle_left + middle_right) / 2.0;
    assert!(
        (centre - (MARGIN + 90.0)).abs() < 0.8,
        "centre of the text on the centre tab stop is {centre}, expected {}",
        MARGIN + 90.0
    );
}

#[test]
fn tab_stop_beyond_the_margin_is_clamped() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    // The tab stop is declared outside the page margins: the content must be
    // clipped back to the body right edge and a diagnostic recorded.
    flow.push(Block::Paragraph(tabbed(
        "超长制表位",
        "99",
        TabStop::new(Twips::from_mm(400.0), TabAlign::Right, TabLeader::None),
    )));
    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let right = 25.4 + (210.0 - 25.4 * 2.0);
    let edge = right_edge(page, "9");
    assert!(
        edge <= right + 0.2,
        "content right edge {edge:.2} should not cross the body right edge {right:.2}"
    );
    assert!(
        fixed
            .report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("tab stop")),
        "a tab stop diagnostic is expected: {:?}",
        fixed.report.diagnostics
    );
}

#[test]
fn tab_content_wider_than_the_gap_does_not_overlap() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    // A very close tab stop with long trailing text: right alignment would push
    // the start before x, so it must degrade to placing the text directly after.
    flow.push(Block::Paragraph(tabbed(
        "abc",
        "0123456789",
        TabStop::new(Twips::from_mm(10.0), TabAlign::Right, TabLeader::None),
    )));
    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let items = chars(page);
    let mut previous_x = f64::MIN;
    for (text, x) in &items {
        assert!(
            *x + 0.01 >= previous_x,
            "character {text:?} overlaps the preceding text: x={x:.2} previous_x={previous_x:.2}"
        );
        previous_x = *x;
    }
    assert!(
        fixed
            .report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("tab stop")),
        "a tab stop diagnostic is expected: {:?}",
        fixed.report.diagnostics
    );
}

#[test]
fn dot_leader_fills_the_gap() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Paragraph(tabbed(
        "Chapter",
        "7",
        TabStop::new(Twips::from_mm(CONTENT), TabAlign::Right, TabLeader::Dot),
    )));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let leaders: Vec<_> = page
        .content
        .root
        .children
        .iter()
        .filter_map(|element| match element {
            Element::Path(path) if path.stroke.is_some() => Some(path),
            _ => None,
        })
        .collect();
    assert_eq!(leaders.len(), 1, "there should be one leader");
    let stroke = leaders[0].stroke.as_ref().expect("stroke");
    assert!(
        !stroke.style.dash.is_empty(),
        "a dotted line should have a dash pattern"
    );
    // The leader is drawn from after the first text to the left of the page number
    // (the page number itself occupies the right side of the tab stop).
    let bounds = leaders[0]
        .path
        .bounds()
        .expect("the leader should have a bounding box");
    let number_x = page
        .text
        .items
        .iter()
        .find(|item| item.run.text == "7")
        .map(|item| item.bbox.x)
        .expect("page number");
    assert!(
        (bounds.right() - number_x).abs() < 0.6,
        "leader right end {}, page number start {number_x}",
        bounds.right()
    );
    assert!(bounds.x > MARGIN, "leader left end {}", bounds.x);
}

#[test]
fn tab_without_stops_advances_to_default_grid() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let mut paragraph = Paragraph::new(ParaStyle {
        line_spacing: LineSpacing::Single,
        ..ParaStyle::default()
    });
    paragraph
        .runs
        .push(Inline::Text(TextRun::new("A", body_style())));
    paragraph
        .runs
        .push(Inline::Text(TextRun::new("\t", body_style())));
    paragraph
        .runs
        .push(Inline::Text(TextRun::new("B", body_style())));
    flow.push(Block::Paragraph(paragraph));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let a = page
        .text
        .items
        .iter()
        .find(|item| item.run.text == "A")
        .map(|item| item.bbox.x)
        .expect("A");
    let b = page
        .text
        .items
        .iter()
        .find(|item| item.run.text == "B")
        .map(|item| item.bbox.x)
        .expect("B");
    // Integer multiples of the default 12.5mm step.
    let step = 12.5;
    let grid = ((b - MARGIN) / step).round() * step + MARGIN;
    assert!(
        (b - grid).abs() < 0.6,
        "B lands at {b}, nearest grid point {grid}"
    );
    assert!(b > a, "the tab should push B past A");
}
