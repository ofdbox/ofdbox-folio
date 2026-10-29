//! End-to-end tests for multi-column sections.

use std::sync::Arc;

use ofdbox_folio_ir::{
    Block, FlowDocument, FontError, FontProvider, FontRequest, Metadata, PageSetup, Paragraph,
    TextStyle, Twips,
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

/// The text and starting x of every line.
fn lines(page: &ofdbox_folio_ir::Page) -> Vec<(String, f64)> {
    page.text
        .lines
        .iter()
        .map(|line| {
            let start = line.items.start as usize;
            let end = line.items.end as usize;
            let text: String = page.text.items[start..end]
                .iter()
                .map(|item| item.run.text.as_str())
                .collect();
            (text, line.bbox.x)
        })
        .collect()
}

fn document(columns: u16) -> FlowDocument {
    let mut flow = FlowDocument::new(Metadata::default());
    let mut setup = PageSetup::a4();
    setup.columns = columns;
    setup.column_gap = Twips::from_mm(8.0);
    flow.sections[0].setup = setup;
    for index in 0..160 {
        flow.push(Block::Paragraph(paragraph(&format!(
            "第 {index} 段：两栏排版应该先灌满左栏，再流到右栏，最后才换页。"
        ))));
    }
    flow
}

#[test]
fn two_columns_fill_left_column_before_right() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let fixed = layout(&document(2), &mut fonts).expect("layout");
    assert!(fixed.pages.len() >= 2, "pages: {}", fixed.pages.len());

    let first_page = lines(&fixed.pages[0]);
    let left_edge = 25.4;
    // The first paragraph at the top of the left column and some paragraph at the
    // top of the right column should both appear on the first page.
    let xs: Vec<f64> = first_page.iter().map(|(_, x)| *x).collect();
    let left = xs.iter().filter(|x| (**x - left_edge).abs() < 1.0).count();
    let right = xs.iter().filter(|x| **x > left_edge + 80.0).count();
    assert!(left > 0, "the left column is empty: {xs:?}");
    assert!(right > 0, "the right column is empty: {xs:?}");

    // The paragraph index at the top of the left column should be smaller than the
    // one at the top of the right column.
    let index_of = |page: &ofdbox_folio_ir::Page| -> Vec<(usize, f64)> {
        lines(page)
            .into_iter()
            .filter_map(|(text, x)| {
                let digits: String = text
                    .chars()
                    .skip_while(|ch| !ch.is_ascii_digit())
                    .take_while(char::is_ascii_digit)
                    .collect();
                digits.parse().ok().map(|value| (value, x))
            })
            .collect()
    };
    let entries = index_of(&fixed.pages[0]);
    let left_first = entries
        .iter()
        .filter(|(_, x)| (*x - left_edge).abs() < 1.0)
        .map(|(index, _)| *index)
        .min()
        .expect("first paragraph of the left column");
    let right_first = entries
        .iter()
        .filter(|(_, x)| *x > left_edge + 80.0)
        .map(|(index, _)| *index)
        .min()
        .expect("first paragraph of the right column");
    assert!(
        right_first > left_first,
        "the right column should start at a later paragraph: {right_first} vs {left_first}"
    );
}

#[test]
fn columns_share_the_same_tops_and_bottoms() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let fixed = layout(&document(2), &mut fonts).expect("layout");
    let first_page = lines(&fixed.pages[0]);
    let rows = |min_x: f64, max_x: f64| -> Vec<f64> {
        first_page
            .iter()
            .filter(|(_, x)| *x >= min_x && *x < max_x)
            .map(|(_, _)| 0.0)
            .collect()
    };
    let _ = rows(0.0, 100.0);
    // The two columns should have a similar number of lines (for the same total
    // content). Verify with the y range of the text lines:
    let left: Vec<f64> = fixed.pages[0]
        .text
        .lines
        .iter()
        .filter(|line| line.bbox.x < 100.0)
        .map(|line| line.bbox.y)
        .collect();
    let right: Vec<f64> = fixed.pages[0]
        .text
        .lines
        .iter()
        .filter(|line| line.bbox.x >= 100.0)
        .map(|line| line.bbox.y)
        .collect();
    let left_top = left.iter().cloned().fold(f64::MAX, f64::min);
    let right_top = right.iter().cloned().fold(f64::MAX, f64::min);
    let left_bottom = left.iter().cloned().fold(f64::MIN, f64::max);
    let right_bottom = right.iter().cloned().fold(f64::MIN, f64::max);
    assert!(
        (left_top - right_top).abs() < 0.5,
        "both columns should start at the same height: {left_top} vs {right_top}"
    );
    assert!(
        (left_bottom - right_bottom).abs() < 20.0,
        "both columns should end at roughly the same bottom: {left_bottom} vs {right_bottom}"
    );
}

#[test]
fn single_column_keeps_full_width_lines() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let fixed = layout(&document(1), &mut fonts).expect("layout");
    let first_page = lines(&fixed.pages[0]);
    // In a single column every line starts at the left margin.
    assert!(
        first_page.iter().all(|(_, x)| (*x - 25.4).abs() < 1.0),
        "single-column line starts: {:?}",
        first_page.iter().map(|(_, x)| *x).collect::<Vec<_>>()
    );
}

#[test]
fn page_count_drops_with_columns() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let single = layout(&document(1), &mut fonts).expect("single");
    let double = layout(&document(2), &mut fonts).expect("double");
    assert!(
        double.pages.len() < single.pages.len(),
        "two columns should use fewer pages: {} vs {}",
        double.pages.len(),
        single.pages.len()
    );
}
