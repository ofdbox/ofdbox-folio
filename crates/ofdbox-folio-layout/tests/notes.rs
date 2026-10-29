//! End-to-end tests for footnotes and endnotes.

use std::sync::Arc;

use ofdbox_folio_ir::{
    Block, FlowDocument, FontError, FontProvider, FontRequest, Inline, Metadata, Note, NoteKind,
    NoteRef, Paragraph, TextStyle, Twips,
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

fn note_style() -> TextStyle {
    TextStyle {
        size: Twips::from_pt(8.0),
        ..TextStyle::default()
    }
}

fn paragraph_with_note(text: &str, note: NoteRef) -> Paragraph {
    let mut paragraph = Paragraph::plain(text, body_style());
    paragraph.runs.push(Inline::Note(note));
    paragraph
}

/// The text and y of every line on a page.
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
            (text, line.bbox.y)
        })
        .collect()
}

fn page_text(page: &ofdbox_folio_ir::Page) -> String {
    lines(page)
        .into_iter()
        .map(|(text, _)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn footnote_lands_at_the_bottom_of_its_page() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    flow.notes.push(Note::new(
        NoteKind::Footnote,
        1,
        vec![Block::Paragraph(Paragraph::plain(
            "这是脚注正文",
            note_style(),
        ))],
    ));
    flow.push(Block::Paragraph(paragraph_with_note(
        "正文里带一个脚注",
        NoteRef::new(NoteKind::Footnote, 1),
    )));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert_eq!(fixed.pages.len(), 1);
    let page = &fixed.pages[0];
    let text = page_text(page);
    assert!(text.contains("正文里带一个脚注"), "body: {text}");
    assert!(text.contains("这是脚注正文"), "footnote: {text}");
    assert!(text.contains('1'), "there should be a number: {text}");

    // The footnote line sits lower than the body lines: near the bottom of the
    // text area.
    let note_y = lines(page)
        .iter()
        .find(|(text, _)| text.contains("这是脚注正文"))
        .map(|(_, y)| *y)
        .expect("footnote line");
    let body_y = lines(page)
        .iter()
        .find(|(text, _)| text.contains("正文里带一个脚注"))
        .map(|(_, y)| *y)
        .expect("body line");
    assert!(
        note_y > body_y + 100.0,
        "the footnote should be low on the page: {note_y}"
    );
    assert!(
        note_y > 240.0,
        "the footnote should sit near the bottom of the text area: {note_y}"
    );
}

#[test]
fn footnote_reserves_space_so_body_does_not_overlap() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    // A very tall footnote.
    let mut note_blocks = Vec::new();
    for index in 0..8 {
        note_blocks.push(Block::Paragraph(Paragraph::plain(
            format!("脚注第 {index} 行"),
            note_style(),
        )));
    }
    flow.notes
        .push(Note::new(NoteKind::Footnote, 1, note_blocks));
    flow.push(Block::Paragraph(paragraph_with_note(
        "第一段带脚注",
        NoteRef::new(NoteKind::Footnote, 1),
    )));
    for index in 0..40 {
        flow.push(Block::Paragraph(Paragraph::plain(
            format!("后续正文第 {index} 段"),
            body_style(),
        )));
    }

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let lines = lines(page);
    let note_top = lines
        .iter()
        .filter(|(text, _)| text.contains("脚注第"))
        .map(|(_, y)| *y)
        .fold(f64::MAX, f64::min);
    assert!(
        note_top < 300.0,
        "the note area should grow upward from the text area bottom: {note_top}"
    );
    // The body must not be pushed into the note area.
    for (text, y) in &lines {
        if text.contains("后续正文") {
            assert!(
                *y < note_top - 2.0,
                "body {text} falls into the note area: y={y} note_top={note_top}"
            );
        }
    }
}

#[test]
fn endnote_goes_to_the_end_of_the_document() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    flow.notes.push(Note::new(
        NoteKind::Endnote,
        7,
        vec![Block::Paragraph(Paragraph::plain(
            "这是尾注正文",
            note_style(),
        ))],
    ));
    flow.push(Block::Paragraph(paragraph_with_note(
        "带尾注的段落",
        NoteRef::new(NoteKind::Endnote, 7),
    )));
    flow.push(Block::Paragraph(Paragraph::plain("普通正文", body_style())));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert!(
        fixed.pages.len() >= 2,
        "the endnotes should start on a new page: {}",
        fixed.pages.len()
    );
    let last = fixed.pages.last().expect("last page");
    let text = page_text(last);
    assert!(text.contains("尾注"), "endnote page: {text}");
    assert!(text.contains("这是尾注正文"), "endnote page: {text}");
    assert!(
        !text.contains("普通正文"),
        "regular body text should not be on the endnote page: {text}"
    );
}

#[test]
fn note_chars_are_prepared_for_the_font_subset() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    // The footnote body uses characters that never appear in the body text: when
    // they are missing from the glyph table their glyph id is 0 (.notdef), the
    // OFD subset drops them and they render blank.
    flow.notes.push(Note::new(
        NoteKind::Footnote,
        1,
        vec![Block::Paragraph(Paragraph::plain("zqxjk", note_style()))],
    ));
    flow.push(Block::Paragraph(paragraph_with_note(
        "body",
        NoteRef::new(NoteKind::Footnote, 1),
    )));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    // The text layer has one item per character, so flatten it to (char, glyph id)
    // pairs and check them one by one.
    let glyphs: Vec<(char, u32)> = fixed.pages[0]
        .text
        .items
        .iter()
        .flat_map(|item| {
            item.run
                .text
                .chars()
                .zip(item.run.glyphs.iter().map(|glyph| glyph.id))
        })
        .collect();
    for ch in ['z', 'q', 'x', 'j', 'k'] {
        let id = glyphs
            .iter()
            .find(|(candidate, _)| *candidate == ch)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("footnote character {ch} is missing"));
        assert_ne!(
            id, 0,
            "footnote character {ch} is not in the glyph table (.notdef)"
        );
    }
}

#[test]
fn missing_note_body_is_reported() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Paragraph(paragraph_with_note(
        "引用一个不存在的注释",
        NoteRef::new(NoteKind::Footnote, 99),
    )));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert!(
        fixed
            .report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("has no body")),
        "diagnostics: {:?}",
        fixed.report.diagnostics
    );
}

#[test]
fn footnotes_are_numbered_in_reading_order() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    for (id, text) in [(3_u32, "注释甲"), (5, "注释乙")] {
        flow.notes.push(Note::new(
            NoteKind::Footnote,
            id,
            vec![Block::Paragraph(Paragraph::plain(text, note_style()))],
        ));
    }
    // Reference id=5 first and id=3 afterwards: numbering follows the order of
    // appearance, 1 then 2.
    flow.push(Block::Paragraph(paragraph_with_note(
        "第一次引用",
        NoteRef::new(NoteKind::Footnote, 5),
    )));
    flow.push(Block::Paragraph(paragraph_with_note(
        "第二次引用",
        NoteRef::new(NoteKind::Footnote, 3),
    )));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let text = page_text(&fixed.pages[0]);
    let yi = text
        .find("1) 注释乙")
        .expect("the first note should be numbered 1");
    let er = text
        .find("2) 注释甲")
        .expect("the second note should be numbered 2");
    assert!(yi < er, "numbering order: {text}");
}

#[test]
fn note_reference_mark_is_smaller_than_body_text() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    flow.notes.push(Note::new(
        NoteKind::Footnote,
        1,
        vec![Block::Paragraph(Paragraph::plain("脚注", note_style()))],
    ));
    flow.push(Block::Paragraph(paragraph_with_note(
        "正文",
        NoteRef::new(NoteKind::Footnote, 1),
    )));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let mark = page
        .text
        .items
        .iter()
        .find(|item| item.run.text == "1" && item.font_size < 3.5)
        .expect("small reference marker");
    assert!(mark.font_size < 3.2, "marker font size: {}", mark.font_size);
}
