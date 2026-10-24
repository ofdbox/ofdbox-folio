//! 多栏分节的端到端测试。

use std::sync::Arc;

use ofdbox_folio_ir::{
    Block, FlowDocument, FontError, FontProvider, FontRequest, Metadata, PageSetup, Paragraph,
    TextStyle, Twips,
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

/// 每行文字与起始 x。
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
    assert!(fixed.pages.len() >= 2, "页数: {}", fixed.pages.len());

    let first_page = lines(&fixed.pages[0]);
    let left_edge = 25.4;
    // 左栏顶部的第一段与右栏顶部的某一段应同时出现在第一页。
    let xs: Vec<f64> = first_page.iter().map(|(_, x)| *x).collect();
    let left = xs.iter().filter(|x| (**x - left_edge).abs() < 1.0).count();
    let right = xs.iter().filter(|x| **x > left_edge + 80.0).count();
    assert!(left > 0, "左栏没有内容: {xs:?}");
    assert!(right > 0, "右栏没有内容: {xs:?}");

    // 左栏顶部的段落序号应小于右栏顶部的段落序号。
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
        .expect("左栏首个段落");
    let right_first = entries
        .iter()
        .filter(|(_, x)| *x > left_edge + 80.0)
        .map(|(index, _)| *index)
        .min()
        .expect("右栏首个段落");
    assert!(
        right_first > left_first,
        "右栏应从更靠后的段落开始: {right_first} vs {left_first}"
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
    // 两栏的行数应该接近（内容总量相同时）。用文本行的 y 范围来验证：
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
        "两栏应从同一高度开始: {left_top} vs {right_top}"
    );
    assert!(
        (left_bottom - right_bottom).abs() < 20.0,
        "两栏应在大致相同的底部结束: {left_bottom} vs {right_bottom}"
    );
}

#[test]
fn single_column_keeps_full_width_lines() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let fixed = layout(&document(1), &mut fonts).expect("layout");
    let first_page = lines(&fixed.pages[0]);
    // 单栏时所有行都从左边距开始。
    assert!(
        first_page.iter().all(|(_, x)| (*x - 25.4).abs() < 1.0),
        "单栏行起点: {:?}",
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
        "两栏应更省页: {} vs {}",
        double.pages.len(),
        single.pages.len()
    );
}
