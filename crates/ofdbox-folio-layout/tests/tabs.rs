//! 制表位与前导符的端到端测试。

use std::sync::Arc;

use ofdbox_folio_ir::{
    Block, Element, FlowDocument, FontError, FontProvider, FontRequest, Inline, LineSpacing,
    Metadata, ParaStyle, Paragraph, TabAlign, TabLeader, TabStop, TextRun, TextStyle, Twips,
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

/// 段落用制表位把两段文字分开。
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

/// 页面文本层里的字符与位置。
fn chars(page: &ofdbox_folio_ir::Page) -> Vec<(String, f64)> {
    page.text
        .items
        .iter()
        .map(|item| (item.run.text.clone(), item.bbox.x))
        .collect()
}

/// 某个字符（首次出现）的右边界。
fn right_edge(page: &ofdbox_folio_ir::Page, needle: &str) -> f64 {
    page.text
        .items
        .iter()
        .find(|item| item.run.text == needle)
        .map(|item| item.bbox.right())
        .unwrap_or_else(|| panic!("没有找到 {needle}"))
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
    // 页码“12”的右边界应贴在右制表位上（版心右边界）。
    let right = right_edge(page, "2");
    assert!(
        (right - (MARGIN + CONTENT)).abs() < 0.6,
        "页码右边界 {right}，期望 {}",
        MARGIN + CONTENT
    );
    // 制表符本身不产生字形。
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
        "左制表位落点 {b}，期望 {}",
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
    // 小数点“.”应落在制表位上。
    let dot = page
        .text
        .items
        .iter()
        .find(|item| item.run.text == ".")
        .map(|item| item.bbox.x)
        .expect("小数点");
    assert!(
        (dot - (MARGIN + 60.0)).abs() < 0.8,
        "小数点位置 {dot}，期望 {}",
        MARGIN + 60.0
    );

    // 居中制表位：文字中心落在制表位上。
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
        "居中制表位的文字中心 {centre}，期望 {}",
        MARGIN + 90.0
    );
}

#[test]
fn tab_stop_beyond_the_margin_is_clamped() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    // 制表位声明在页边距之外：内容必须被裁回正文右边缘，并记一条诊断。
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
        "内容右边界 {edge:.2} 应不越过正文右边缘 {right:.2}"
    );
    assert!(
        fixed
            .report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("制表位")),
        "应有制表位诊断: {:?}",
        fixed.report.diagnostics
    );
}

#[test]
fn tab_content_wider_than_the_gap_does_not_overlap() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    // 制表位很近、后面文字很长：右对齐会把起点推到 x 之前，必须退化成紧接排布。
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
            "字符 {text:?} 与前文重叠: x={x:.2} previous_x={previous_x:.2}"
        );
        previous_x = *x;
    }
    assert!(
        fixed
            .report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("制表位")),
        "应有制表位诊断: {:?}",
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
    assert_eq!(leaders.len(), 1, "应有一条前导符");
    let stroke = leaders[0].stroke.as_ref().expect("描边");
    assert!(!stroke.style.dash.is_empty(), "点线应有虚线节奏");
    // 前导符从第一个字之后画到页码左侧（页码本身占住制表位右侧）。
    let bounds = leaders[0].path.bounds().expect("前导符应有包围盒");
    let number_x = page
        .text
        .items
        .iter()
        .find(|item| item.run.text == "7")
        .map(|item| item.bbox.x)
        .expect("页码");
    assert!(
        (bounds.right() - number_x).abs() < 0.6,
        "前导符右端 {}，页码起点 {number_x}",
        bounds.right()
    );
    assert!(bounds.x > MARGIN, "前导符左端 {}", bounds.x);
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
    // 默认步长 12.5mm 的整数倍格点。
    let step = 12.5;
    let grid = ((b - MARGIN) / step).round() * step + MARGIN;
    assert!((b - grid).abs() < 0.6, "B 落在 {b}，最近格点 {grid}");
    assert!(b > a, "制表符应把 B 推到 A 之后");
}
