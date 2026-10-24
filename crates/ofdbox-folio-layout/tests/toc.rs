//! 自动目录的端到端测试。

use std::sync::Arc;

use ofdbox_folio_ir::TextStyle;
use ofdbox_folio_ir::{
    Block, FlowDocument, FontError, FontProvider, FontRequest, Metadata, Paragraph, TocBlock,
    TocStyle, Twips,
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

/// 每页每一行的文字。
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

/// 造一份「目录 + 3 章正文」的文档。
fn document(max_level: u8) -> FlowDocument {
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Toc(TocBlock {
        max_level,
        title: Some("目录".to_owned()),
        style: TocStyle::default(),
    }));
    // 目录独占一页，正文从下一页开始，便于断言目录内容。
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
    assert!(fixed.pages.len() >= 3, "页数: {}", fixed.pages.len());

    // 目录页：标题 + 每条「左标题 / 右页码」。
    let toc_lines = lines(&fixed.pages[0]);
    assert!(toc_lines.iter().any(|line| line == "目录"), "目录标题缺失");
    let entry = toc_lines
        .iter()
        .position(|line| line == "第 1 章")
        .expect("第一章条目");
    let listed_page: usize = toc_lines
        .get(entry + 1)
        .and_then(|line| line.trim().parse().ok())
        .expect("页码");

    // 正文里的第一章标题所在页（跳过目录页）。
    let actual = fixed
        .pages
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, page)| lines(page).iter().any(|line| line == "第 1 章"))
        .map(|(index, _)| index + 1)
        .expect("正文里的第一章");
    assert_eq!(listed_page, actual, "目录页码应与正文一致");
}

#[test]
fn toc_respects_max_level() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let fixed = layout(&document(2), &mut fonts).expect("layout");
    let first_page = lines(&fixed.pages[0]);
    let toc_lines = &first_page[..];
    assert!(
        toc_lines.iter().any(|line| line == "第 1 节"),
        "目录: {toc_lines:?}"
    );
    assert!(
        !toc_lines.iter().any(|line| line == "小节 1"),
        "三级标题不应进目录: {toc_lines:?}"
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
    assert_eq!(first.pages.len(), second.pages.len(), "页数应稳定");
    assert_eq!(
        lines(&first.pages[0]),
        lines(&second.pages[0]),
        "目录应稳定"
    );
}
