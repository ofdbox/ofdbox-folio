//! End-to-end tests for the automatic table of contents.

use std::sync::Arc;

use ofdbox_folio_ir::TextStyle;
use ofdbox_folio_ir::{
    Block, FlowDocument, FontError, FontProvider, FontRequest, Metadata, Paragraph, TocBlock,
    TocStyle, Twips,
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

fn heading(text: &str, level: u8) -> Paragraph {
    let mut paragraph = Paragraph::plain(
        text,
        TextStyle {
            size: Twips::from_pt(16.0),
            ..TextStyle::default()
        },
    );
    paragraph.heading_level = Some(level);
    paragraph
}

/// The text of every line on a page.
fn lines(page: &ofdbox_folio_ir::Page) -> Vec<String> {
    page.text
        .lines
        .iter()
        .map(|line| {
            let start = line.items.start as usize;
            let end = line.items.end as usize;
            page.text.items[start..end]
                .iter()
                .map(|item| item.run.text.as_str())
                .collect()
        })
        .collect()
}

/// Build a document of "table of contents + 3 chapters of body text".
fn document(max_level: u8) -> FlowDocument {
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Toc(TocBlock {
        max_level,
        title: Some("目录".to_owned()),
        style: TocStyle::default(),
    }));
    // The table of contents occupies its own page and the body starts on the next
    // page, which makes the TOC content easy to assert.
    flow.push(Block::PageBreak);
    for chapter in 1..=3 {
        flow.push(Block::Paragraph(heading(&format!("第 {chapter} 章"), 1)));
        flow.push(Block::Paragraph(heading(&format!("第 {chapter} 节"), 2)));
        flow.push(Block::Paragraph(heading(&format!("小节 {chapter}"), 3)));
        for index in 0..50 {
            flow.push(Block::Paragraph(paragraph(&format!(
                "第 {chapter} 章的第 {index} 段正文，用来把内容推到后面几页。"
            ))));
        }
    }
    flow
}

#[test]
fn builds_toc_with_page_numbers_matching_the_body() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let flow = document(2);
    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert!(fixed.pages.len() >= 3, "pages: {}", fixed.pages.len());

    // TOC page: the title plus one "entry ⇥ page number" per heading (the tab
    // pushes the page number to the right tab stop).
    let toc_lines = lines(&fixed.pages[0]);
    assert!(
        toc_lines.iter().any(|line| line == "目录"),
        "TOC title is missing"
    );
    let entry = toc_lines
        .iter()
        .find(|line| line.starts_with("第 1 章"))
        .expect("chapter 1 entry");
    let listed_page = trailing_number(entry).expect("page number");

    // The page holding chapter 1's heading in the body (skipping the TOC page).
    let actual = fixed
        .pages
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, page)| lines(page).iter().any(|line| line == "第 1 章"))
        .map(|(index, _)| index + 1)
        .expect("chapter 1 in the body");
    assert_eq!(
        listed_page, actual,
        "the TOC page number should match the body"
    );
}

/// Take the trailing run of digits on a line.
fn trailing_number(line: &str) -> Option<usize> {
    let digits: String = line
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse().ok()
}

#[test]
fn toc_respects_max_level() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let fixed = layout(&document(2), &mut fonts).expect("layout");
    let toc_lines = lines(&fixed.pages[0]);
    assert!(
        toc_lines.iter().any(|line| line.starts_with("第 1 节")),
        "TOC: {toc_lines:?}"
    );
    assert!(
        !toc_lines.iter().any(|line| line.starts_with("小节 1")),
        "level-3 headings should not enter the TOC: {toc_lines:?}"
    );
}

#[test]
fn toc_is_stable_across_repeated_layouts() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let flow = document(2);
    let first = layout(&flow, &mut fonts).expect("layout");
    let second = layout(&flow, &mut fonts).expect("layout again");
    assert_eq!(
        first.pages.len(),
        second.pages.len(),
        "the page count should be stable"
    );
    assert_eq!(
        lines(&first.pages[0]),
        lines(&second.pages[0]),
        "the TOC should be stable"
    );
}
