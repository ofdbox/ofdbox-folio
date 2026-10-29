//! Additional A2 tests: tables, headers and footers.
//!
//! Like `layout.rs` these rely on the system's Arial for metrics and are skipped
//! when the font is unavailable.

use std::sync::Arc;

use ofdbox_folio_ir::{
    Align, Block, Border, Cell, Column, ColumnWidth, Field, FieldKind, FlowDocument, FontError,
    FontProvider, FontRequest, HeaderFooter, Inline, LineSpacing, Margins, Metadata, PageSetup,
    ParaStyle, Paragraph, Row, Table, TableBorders, TextRun, TextStyle, Twips,
};
use ofdbox_folio_layout::{LayoutOptions, layout, layout_with};

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

/// The text of each line in the page's text layer, the x of its first character
/// and the y of its line box.
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
            .unwrap_or_else(|| panic!("did not find {needle}: {text:?}"))
    };

    // First column: left margin 25.4 + padding 2; the second column is 50 further
    // right.
    let (first_x, first_y) = find("A1");
    assert!((first_x - 27.4).abs() < 0.5, "first column x={first_x}");
    let (second_x, _) = find("B1");
    assert!(
        (second_x - (first_x + 50.0)).abs() < 0.5,
        "second column x={second_x}"
    );
    // Cell top padding 1mm: the first line of text sits below the table top.
    assert!(first_y > 25.4 - 1.0, "first row y={first_y}");

    // The paragraph placed after the table must fall below the table.
    let (_, after_y) = find("表格之后");
    assert!(
        after_y > first_y + 3.0,
        "paragraph after the table y={after_y}"
    );

    // 4 edges per cell → 12 stroked paths for 3 cells.
    let strokes = page
        .content
        .root
        .children
        .iter()
        .filter(|element| matches!(element, ofdbox_folio_ir::Element::Path(_)))
        .count();
    assert_eq!(strokes, 12, "number of border strokes");
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
    assert!(fixed.pages.len() >= 2, "pages: {}", fixed.pages.len());
    for (index, page) in fixed.pages.iter().enumerate() {
        let text = texts(page);
        assert!(
            text.iter().any(|(value, _, _)| value == "表头甲"),
            "page {} is missing the repeated header: {text:?}",
            index + 1
        );
    }
}

#[test]
fn splits_a_row_taller_than_a_page_across_pages() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    let mut rows = vec![Row {
        cells: vec![cell("表头甲"), cell("表头乙")],
        is_header: true,
        height: None,
    }];
    // Stuff 90 short paragraphs into the cell so the whole row is far taller than
    // one page.
    let mut tall_cell = Vec::new();
    for index in 0..140 {
        tall_cell.push(Block::Paragraph(paragraph(&format!(
            "很长的单元格内容第 {index} 行"
        ))));
    }
    let mut row = Row::new(vec![Cell::new(tall_cell), cell("旁边")]);
    row.height = None;
    rows.push(row);
    rows.push(Row::new(vec![cell("表格最后一行"), cell("尾")]));

    let mut table = table(rows, 1);
    table.cell_padding = Margins::uniform(Twips::from_mm(1.0));
    // Single-column tables are more common: switch to two columns.
    table.columns = vec![
        Column {
            width: ColumnWidth::Twips(Twips::from_mm(90.0)),
        },
        Column {
            width: ColumnWidth::Twips(Twips::from_mm(60.0)),
        },
    ];
    flow.push(Block::Table(table));

    let fixed = layout_with(&flow, &mut fonts, LayoutOptions::default()).expect("layout");
    assert!(
        fixed.pages.len() >= 3,
        "an oversized row should continue across pages; actual page count: {}",
        fixed.pages.len()
    );

    // Every page carries the continued content of the oversized row and repeats
    // the header.
    for (index, page) in fixed.pages.iter().enumerate() {
        let text = texts(page);
        assert!(
            text.iter().any(|(value, _, _)| value == "表头甲"),
            "page {} is missing the header: {text:?}",
            index + 1
        );
        assert!(
            text.iter()
                .any(|(value, _, _)| value.starts_with("很长的单元格内容第")),
            "page {} is missing the continued cell content: {text:?}",
            index + 1
        );
    }
    // The first and last paragraphs land on different pages, which shows that a
    // single row was split and continued rather than moved wholesale.
    let page_of = |needle: &str| {
        fixed
            .pages
            .iter()
            .position(|page| texts(page).iter().any(|(value, _, _)| value == needle))
            .map(|index| index + 1)
    };
    let first_page = page_of("很长的单元格内容第 0 行").expect("first paragraph");
    let last_page = page_of("很长的单元格内容第 139 行").expect("last paragraph");
    assert!(
        last_page > first_page,
        "the first and last parts of the same row should land on different pages: {first_page} vs {last_page}"
    );

    // The last row falls entirely on the final page and is not repeated.
    let last = fixed.pages.last().expect("last page");
    assert!(
        texts(last)
            .iter()
            .any(|(value, _, _)| value == "表格最后一行"),
        "the last row should be fully laid out: {:?}",
        texts(last)
    );
    assert!(
        fixed
            .report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("table row spans pages")),
        "a row-spanning-pages diagnostic should be recorded"
    );
}

#[test]
fn merged_cell_spans_multiple_columns() {
    let Some(mut fonts) = fonts() else {
        return;
    };
    let mut flow = FlowDocument::new(Metadata::default());
    // Two 40mm columns: a cell spanning both has 80mm available, so the short text
    // fits on one line; a single 40mm column would wrap it to two lines.
    let mut table = table(
        vec![
            Row::new(vec![
                cell("跨列单元格里的文字刚好一行放得下就说明占满两列").with_span(2),
            ]),
            Row::new(vec![cell("左"), cell("右")]),
        ],
        0,
    );
    table.columns = vec![
        ofdbox_folio_ir::Column {
            width: ofdbox_folio_ir::ColumnWidth::Twips(Twips::from_mm(40.0)),
        },
        ofdbox_folio_ir::Column {
            width: ofdbox_folio_ir::ColumnWidth::Twips(Twips::from_mm(40.0)),
        },
    ];
    table.cell_padding = Margins::uniform(Twips::from_mm(1.0));
    flow.push(Block::Table(table));

    let fixed = layout_with(&flow, &mut fonts, LayoutOptions::default()).expect("layout");
    let page = &fixed.pages[0];
    let lines: Vec<String> = page
        .text
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
        .collect();
    let span_lines = lines
        .iter()
        .filter(|line| line.contains("跨列单元格"))
        .count();
    assert_eq!(
        span_lines, 1,
        "a spanning cell should fit on one line: {lines:?}"
    );
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
        .expect("header");
    assert!(
        (header.1 - 25.4).abs() < 0.5,
        "header left margin: {}",
        header.1
    );
    assert!(
        (header.2 - 10.0).abs() < 3.0,
        "header y should be close to the header distance: {}",
        header.2
    );

    let footer: String = first
        .iter()
        .map(|(value, _, _)| value.as_str())
        .collect::<Vec<_>>()
        .join("");
    assert!(footer.contains("第 1 页"), "footer text: {footer}");
    assert!(footer.contains("共 2 页"), "footer text: {footer}");
    let footer_y = first
        .iter()
        .filter(|(_, _, y)| *y > 250.0)
        .map(|(_, _, y)| *y)
        .fold(0.0_f64, f64::max);
    assert!(
        footer_y > 250.0,
        "the footer should be at the bottom of the page: {footer_y}"
    );

    let second: String = texts(&fixed.pages[1])
        .iter()
        .map(|(value, _, _)| value.as_str())
        .collect::<Vec<_>>()
        .join("");
    assert!(second.contains("第 2 页"), "second page footer: {second}");
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
        .expect("centered text");
    // Column width 60mm with centered text: the start should be clearly right of
    // the left margin.
    assert!(centered_x.0 > 25.4 + 15.0, "centered x={}", centered_x.0);
    let after_y = text
        .iter()
        .find(|(value, _, _)| value == "之后")
        .map(|(_, _, y)| *y)
        .expect("following paragraph");
    // Fixed row height 20mm: the first line top of the following paragraph is
    // around +20mm (line boxes are measured upward from the baseline).
    assert!(
        after_y > 25.4 + 20.0 - 1.5,
        "following paragraph y={after_y}"
    );
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
            .any(|diagnostic| diagnostic.message.contains("table has no rows or columns")),
        "diagnostics: {:?}",
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
    // The cell text should take more than 3 lines (60mm of column width cannot
    // hold the whole paragraph).
    assert!(lines.len() >= 3, "line count: {} / {lines:?}", lines.len());
    for line in &page.text.lines {
        assert!(
            line.bbox.right() <= 25.4 + 60.0 + 1.0,
            "line exceeds the column width: {} / {lines:?}",
            line.bbox.right()
        );
    }
}
