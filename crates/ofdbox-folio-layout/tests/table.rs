//! 追加的 A2 测试：表格与页眉页脚。
//!
//! 与 `layout.rs` 一样依赖系统里的 Arial 做度量，拿不到字体时跳过。

use std::sync::Arc;

use ofdbox_folio_ir::{
    Align, Block, Border, Cell, Column, ColumnWidth, Field, FieldKind, FlowDocument, FontError,
    FontProvider, FontRequest, HeaderFooter, Inline, LineSpacing, Margins, Metadata, PageSetup,
    ParaStyle, Paragraph, Row, Table, TableBorders, TextRun, TextStyle, Twips,
};
use ofdbox_folio_layout::{LayoutOptions, layout, layout_with};

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

fn cell(text: &str) -> Cell {
    Cell::new(vec![Block::Paragraph(paragraph(text))])
}

fn table(rows: Vec<Row>, header_rows: u32) -> Table {
    let columns = (0..3)
        .map(|_| Column {
            width: ColumnWidth::Twips(Twips::from_mm(50.0)),
        })
        .collect::<Vec<_>>();
    let mut table = Table::new(columns);
    table.rows = rows;
    table.header_rows = header_rows;
    let border = Border::default();
    table.borders = TableBorders {
        outer: Some(border),
        horizontal: Some(border),
        vertical: Some(border),
    };
    table.cell_padding = Margins {
        top: Twips::from_mm(1.0),
        right: Twips::from_mm(2.0),
        bottom: Twips::from_mm(1.0),
        left: Twips::from_mm(2.0),
    };
    table
}

/// 页面文本层里每一行的文字、首个字符的 x 与行框的 y。
fn texts(page: &ofdbox_folio_ir::Page) -> Vec<(String, f64, f64)> {
    page.text
        .lines
        .iter()
        .map(|line| {
            let start = line.items.start as usize;
            let end = line.items.end as usize;
            let items = &page.text.items[start..end];
            let text: String = items.iter().map(|item| item.run.text.as_str()).collect();
            let x = items.first().map(|item| item.bbox.x).unwrap_or(line.bbox.x);
            (text, x, line.bbox.y)
        })
        .collect()
}

#[test]
fn lays_out_table_cells_with_padding_and_borders() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Table(table(
        vec![Row::new(vec![cell("A1"), cell("B1"), cell("C1")])],
        0,
    )));
    flow.push(Block::Paragraph(paragraph("表格之后")));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let text = texts(page);
    let find = |needle: &str| {
        text.iter()
            .find(|(value, _, _)| value == needle)
            .map(|(_, x, y)| (*x, *y))
            .unwrap_or_else(|| panic!("没有找到 {needle}: {text:?}"))
    };

    // 第一列：左边距 25.4 + 内边距 2；第二列再向右 50。
    let (first_x, first_y) = find("A1");
    assert!((first_x - 27.4).abs() < 0.5, "首列 x={first_x}");
    let (second_x, _) = find("B1");
    assert!(
        (second_x - (first_x + 50.0)).abs() < 0.5,
        "第二列 x={second_x}"
    );
    // 单元格上内边距 1mm：首行文字落在表格顶部之下。
    assert!(first_y > 25.4 - 1.0, "首行 y={first_y}");

    // 表格之后的段落要落在表格下方。
    let (_, after_y) = find("表格之后");
    assert!(after_y > first_y + 3.0, "表格后段落 y={after_y}");

    // 每个单元格 4 条边 → 3 个单元格 12 条描边路径。
    let strokes = page
        .content
        .root
        .children
        .iter()
        .filter(|element| matches!(element, ofdbox_folio_ir::Element::Path(_)))
        .count();
    assert_eq!(strokes, 12, "边框描边数量");
}

#[test]
fn repeats_table_header_rows_on_the_next_page() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let mut rows = vec![Row {
        cells: vec![cell("表头甲"), cell("表头乙"), cell("表头丙")],
        is_header: true,
        height: None,
    }];
    for index in 0..80 {
        rows.push(Row::new(vec![
            cell(&format!("单元格 {index}")),
            cell("内容"),
            cell("内容"),
        ]));
    }
    let mut table = table(rows, 1);
    table.cell_padding = Margins::uniform(Twips::from_mm(1.0));
    flow.push(Block::Table(table));

    let fixed = layout_with(&flow, &mut fonts, LayoutOptions::default()).expect("layout");
    assert!(fixed.pages.len() >= 2, "页数: {}", fixed.pages.len());
    for (index, page) in fixed.pages.iter().enumerate() {
        let text = texts(page);
        assert!(
            text.iter().any(|(value, _, _)| value == "表头甲"),
            "第 {} 页缺少重复表头: {text:?}",
            index + 1
        );
    }
}

#[test]
fn lays_out_header_and_footer_with_page_fields() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let mut setup = PageSetup::a4();
    setup.header_distance = Twips::from_mm(10.0);
    setup.footer_distance = Twips::from_mm(10.0);
    flow.sections[0].setup = setup;

    let mut footer_paragraph = Paragraph::plain("第 ", body_style());
    footer_paragraph
        .runs
        .push(Inline::Field(Field::new(FieldKind::PageNumber)));
    footer_paragraph
        .runs
        .push(Inline::Text(TextRun::new(" 页 / 共 ", body_style())));
    footer_paragraph
        .runs
        .push(Inline::Field(Field::new(FieldKind::TotalPages)));
    footer_paragraph
        .runs
        .push(Inline::Text(TextRun::new(" 页", body_style())));

    flow.sections[0].headers.default = Some(HeaderFooter {
        blocks: vec![Block::Paragraph(paragraph("页眉文字"))],
    });
    flow.sections[0].footers.default = Some(HeaderFooter {
        blocks: vec![Block::Paragraph(footer_paragraph)],
    });
    flow.push(Block::Paragraph(paragraph("正文一")));
    flow.push(Block::PageBreak);
    flow.push(Block::Paragraph(paragraph("正文二")));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert_eq!(fixed.pages.len(), 2);

    let first = texts(&fixed.pages[0]);
    let header = first
        .iter()
        .find(|(value, _, _)| value == "页眉文字")
        .expect("页眉");
    assert!((header.1 - 25.4).abs() < 0.5, "页眉左边距: {}", header.1);
    assert!(
        (header.2 - 10.0).abs() < 3.0,
        "页眉 y 应贴近页眉距离: {}",
        header.2
    );

    let footer: String = first
        .iter()
        .map(|(value, _, _)| value.as_str())
        .collect::<Vec<_>>()
        .join("");
    assert!(footer.contains("第 1 页"), "页脚文字: {footer}");
    assert!(footer.contains("共 2 页"), "页脚文字: {footer}");
    let footer_y = first
        .iter()
        .filter(|(_, _, y)| *y > 250.0)
        .map(|(_, _, y)| *y)
        .fold(0.0_f64, f64::max);
    assert!(footer_y > 250.0, "页脚应在页面底部: {footer_y}");

    let second: String = texts(&fixed.pages[1])
        .iter()
        .map(|(value, _, _)| value.as_str())
        .collect::<Vec<_>>()
        .join("");
    assert!(second.contains("第 2 页"), "第二页页脚: {second}");
}

#[test]
fn aligns_centered_cells_and_keeps_row_height() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let mut centered = paragraph("居中");
    centered.style = ParaStyle {
        align: Align::Center,
        ..ParaStyle::default()
    };
    let mut row = Row::new(vec![Cell::new(vec![Block::Paragraph(centered)])]);
    row.height = Some(Twips::from_mm(20.0));
    let mut table = Table::new(vec![Column {
        width: ColumnWidth::Twips(Twips::from_mm(60.0)),
    }]);
    table.rows = vec![row];
    flow.push(Block::Table(table));
    flow.push(Block::Paragraph(paragraph("之后")));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let text = texts(page);
    let centered_x = text
        .iter()
        .find(|(value, _, _)| value == "居中")
        .map(|(_, x, y)| (*x, *y))
        .expect("居中文本");
    // 列宽 60mm、文本居中：起点应明显右于左边距。
    assert!(centered_x.0 > 25.4 + 15.0, "居中 x={}", centered_x.0);
    let after_y = text
        .iter()
        .find(|(value, _, _)| value == "之后")
        .map(|(_, _, y)| *y)
        .expect("后续段落");
    // 固定行高 20mm：后续段落的首行顶部在 +20mm 附近（行框从基线向上量）。
    assert!(after_y > 25.4 + 20.0 - 1.5, "后续段落 y={after_y}");
}

#[test]
fn reports_table_without_columns() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let table = Table {
        rows: vec![Row::new(vec![cell("孤零零")])],
        ..Table::default()
    };
    flow.push(Block::Table(table));
    flow.push(Block::Paragraph(paragraph("之后")));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert!(
        fixed
            .report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("表格没有行或列")),
        "诊断: {:?}",
        fixed.report.diagnostics
    );
    assert!(!fixed.pages.is_empty());
}

#[test]
fn justifies_and_indents_inside_cells() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let mut styled = paragraph(
        "This cell text is deliberately long so that it has to wrap several times inside \
         the column width, proving that indentation and line spacing still apply inside a \
         nested frame.",
    );
    styled.style = ParaStyle {
        left_indent: Twips::from_mm(3.0),
        line_spacing: LineSpacing::Multiple(1.5),
        ..ParaStyle::default()
    };
    let mut table = Table::new(vec![Column {
        width: ColumnWidth::Twips(Twips::from_mm(60.0)),
    }]);
    table.rows = vec![Row::new(vec![Cell::new(vec![Block::Paragraph(styled)])])];
    flow.push(Block::Table(table));

    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let lines = texts(page);
    // 单元格文本应在 3 行以上（60mm 列宽装不下整段）。
    assert!(lines.len() >= 3, "行数: {} / {lines:?}", lines.len());
    for line in &page.text.lines {
        assert!(
            line.bbox.right() <= 25.4 + 60.0 + 1.0,
            "行超出列宽: {} / {lines:?}",
            line.bbox.right()
        );
    }
}
