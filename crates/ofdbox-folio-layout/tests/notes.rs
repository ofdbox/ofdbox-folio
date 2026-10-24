//! 脚注与尾注的端到端测试。

use std::sync::Arc;

use ofdbox_folio_ir::{
    Block, FlowDocument, FontError, FontProvider, FontRequest, Inline, Metadata, Note, NoteKind,
    NoteRef, Paragraph, TextStyle, Twips,
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

/// 页面里每一行的文字与 y。
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
    assert!(text.contains("正文里带一个脚注"), "正文: {text}");
    assert!(text.contains("这是脚注正文"), "脚注: {text}");
    assert!(text.contains('1'), "应有序号: {text}");

    // 脚注行比正文行更靠下：接近版心底边。
    let note_y = lines(page)
        .iter()
        .find(|(text, _)| text.contains("这是脚注正文"))
        .map(|(_, y)| *y)
        .expect("脚注行");
    let body_y = lines(page)
        .iter()
        .find(|(text, _)| text.contains("正文里带一个脚注"))
        .map(|(_, y)| *y)
        .expect("正文行");
    assert!(note_y > body_y + 100.0, "脚注应在页面下方: {note_y}");
    assert!(note_y > 240.0, "脚注应贴近版心底: {note_y}");
}

#[test]
fn footnote_reserves_space_so_body_does_not_overlap() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    // 一条很高的脚注。
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
    assert!(note_top < 300.0, "脚注区应从版心底往上: {note_top}");
    // 正文不应压进注释区。
    for (text, y) in &lines {
        if text.contains("后续正文") {
            assert!(
                *y < note_top - 2.0,
                "正文 {text} 落进注释区: y={y} note_top={note_top}"
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
        "尾注应另起一页: {}",
        fixed.pages.len()
    );
    let last = fixed.pages.last().expect("最后一页");
    let text = page_text(last);
    assert!(text.contains("尾注"), "尾注页: {text}");
    assert!(text.contains("这是尾注正文"), "尾注页: {text}");
    assert!(!text.contains("普通正文"), "普通正文不应在尾注页: {text}");
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
            .any(|diagnostic| diagnostic.message.contains("缺少正文")),
        "诊断: {:?}",
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
    // 先引用 id=5，再引用 id=3：编号应按出现顺序 1、2。
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
    let yi = text.find("1) 注释乙").expect("第一条例注应编号 1");
    let er = text.find("2) 注释甲").expect("第二条例注应编号 2");
    assert!(yi < er, "编号顺序: {text}");
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
        .expect("小号引用标记");
    assert!(mark.font_size < 3.2, "标记字号: {}", mark.font_size);
}
