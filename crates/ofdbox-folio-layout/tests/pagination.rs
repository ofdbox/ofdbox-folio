//! Pagination control: keep_with_next, keep_lines_together, widows/orphans.

use std::sync::Arc;

use ofdbox_folio_ir::{
    Block, FlowDocument, FontError, FontProvider, FontRequest, Metadata, PageSetup, Paragraph,
    TextStyle, Twips,
};
use ofdbox_folio_layout::layout;

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

fn style() -> TextStyle {
    TextStyle {
        size: Twips::from_pt(11.0),
        ..TextStyle::default()
    }
}

fn paragraph(text: &str) -> Paragraph {
    Paragraph::plain(text, style())
}

/// The line text of each page.
fn page_lines(page: &ofdbox_folio_ir::Page) -> Vec<String> {
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

/// Which page a given marker appears on.
fn page_of(fixed: &ofdbox_folio_ir::FixedDocument, needle: &str) -> Option<usize> {
    fixed
        .pages
        .iter()
        .position(|page| page_lines(page).iter().any(|line| line.contains(needle)))
}

/// Fill the page to near the bottom with a long enough run of text, then return
/// the document.
fn fill_page(flow: &mut FlowDocument, count: usize, prefix: &str) {
    for index in 0..count {
        flow.push(Block::Paragraph(paragraph(&format!(
            "{prefix}{index}：这一段用来把页面填满，方便观察分页行为。"
        ))));
    }
}

#[test]
fn keep_with_next_keeps_heading_with_its_paragraph() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    // Sweep the fill amount: whenever a heading lands exactly at the page bottom,
    // it would be split off without keep_with_next.
    for fill in 30..=52 {
        let mut flow = FlowDocument::new(Metadata::default());
        fill_page(&mut flow, fill, "填充");
        let mut heading = paragraph("标题不能孤零零留在页尾");
        heading.style.keep_with_next = true;
        flow.push(Block::Paragraph(heading));
        flow.push(Block::Paragraph(paragraph(
            "标题后面的正文，必须和标题在同一页。",
        )));

        let fixed = layout(&flow, &mut fonts).expect("layout");
        let heading_page = page_of(&fixed, "标题不能孤零零").expect("heading");
        let body_page = page_of(&fixed, "标题后面的正文").expect("body");
        assert_eq!(
            heading_page, body_page,
            "fill {fill}: the heading and the following body should be on the same page"
        );
    }
}

#[test]
fn keep_lines_together_moves_the_whole_paragraph() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    fill_page(&mut flow, 26, "填充");

    let mut together = paragraph(
        "这一段设置了 keep_lines_together，整段必须落在同一页，\
         所以它会被整体挪到下一页，而不是在页尾被拆开。",
    );
    together.style.keep_lines_together = true;
    flow.push(Block::Paragraph(together));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let pages: Vec<usize> = fixed
        .pages
        .iter()
        .enumerate()
        .filter(|(_, page)| {
            page_lines(page)
                .iter()
                .any(|line| line.contains("keep_lines_together"))
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        pages.len(),
        1,
        "the whole paragraph should appear on one page only: {pages:?}"
    );
}

#[test]
fn split_paragraph_keeps_at_least_two_lines_per_page() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    // The fill amount varies with the font, so sweep it here and check the line
    // count per page as soon as a page break occurs.
    let long = (0..30)
        .map(|index| format!("拆分标记第{index}行文字，用来确认孤行寡行。"))
        .collect::<Vec<_>>()
        .join("");
    let mut checked = false;
    for fill in 38..=52 {
        let mut flow = FlowDocument::new(Metadata::default());
        fill_page(&mut flow, fill, "填充");
        flow.push(Block::Paragraph(paragraph(&long)));
        let fixed = layout(&flow, &mut fonts).expect("layout");
        let per_page: Vec<usize> = fixed
            .pages
            .iter()
            .map(|page| {
                page_lines(page)
                    .iter()
                    .filter(|line| line.contains("拆分标记"))
                    .count()
            })
            .filter(|count| *count > 0)
            .collect();
        if per_page.len() >= 2 {
            checked = true;
            for count in &per_page {
                assert!(
                    *count >= 2,
                    "at least two lines per page (fill {fill}), got: {per_page:?}"
                );
            }
        }
    }
    assert!(
        checked,
        "at least one fill amount should make the paragraph span pages"
    );
}

#[test]
fn keep_with_next_chain_moves_together() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    for fill in 30..=52 {
        let mut flow = FlowDocument::new(Metadata::default());
        fill_page(&mut flow, fill, "填充");

        // Both heading levels have keep_with_next: either they both stay on this
        // page or they both move to the next one.
        let mut first = paragraph("一级标题链");
        first.style.keep_with_next = true;
        let mut second = paragraph("二级标题链");
        second.style.keep_with_next = true;
        flow.push(Block::Paragraph(first));
        flow.push(Block::Paragraph(second));
        flow.push(Block::Paragraph(paragraph("链尾正文")));

        let fixed = layout(&flow, &mut fonts).expect("layout");
        let first_page = page_of(&fixed, "一级标题链").expect("level 1");
        let second_page = page_of(&fixed, "二级标题链").expect("level 2");
        let body_page = page_of(&fixed, "链尾正文").expect("body");
        assert_eq!(
            first_page, second_page,
            "fill {fill}: both heading levels should be on the same page"
        );
        assert_eq!(
            second_page, body_page,
            "fill {fill}: the level-2 heading should be on the same page as the body"
        );
    }
}

#[test]
fn measuring_for_keep_with_next_does_not_duplicate_headings() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    fill_page(&mut flow, 30, "填充");
    let mut heading = paragraph("唯一标题");
    heading.style.keep_with_next = true;
    heading.heading_level = Some(1);
    flow.push(Block::Paragraph(heading));
    flow.push(Block::Paragraph(paragraph("标题后的正文")));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert_eq!(
        fixed.outline.len(),
        1,
        "the outline should not be registered twice: {:?}",
        fixed.outline
    );
}

#[test]
fn single_page_document_is_unaffected() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    flow.sections[0].setup = PageSetup::a4();
    flow.push(Block::Paragraph(paragraph("只有一段")));
    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert_eq!(fixed.pages.len(), 1);
    assert_eq!(page_lines(&fixed.pages[0]).len(), 1);
}
