//! DOCX 写出的端到端测试：构造 Flow IR → 写包 → 再读回来。

use std::sync::Arc;

use ofdbox_folio_docx::{DocxReadOptions, DocxWriteOptions, read_flow, write_flow};
use ofdbox_folio_ir::{
    Align, Block, Cell, Column, ColumnWidth, Field, FieldKind, FloatingObject, FlowDocument,
    FontFamily, HeaderFooter, ImageAsset, ImageBlock, ImageFormat, Inline, LineSpacing, Link,
    LinkTarget, Margins, Metadata, Note, NoteKind, NoteRef, PageSetup, ParaStyle, Paragraph, Point,
    Row, TabAlign, TabLeader, TabStop, Table, TableBorders, TextRun, TextStyle, TocBlock, Twips,
    WrapMode,
};

/// 1×1 的 RGBA PNG。
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

fn style(size: f64, bold: bool) -> TextStyle {
    TextStyle {
        size: Twips::from_pt(size),
        bold,
        ..TextStyle::default()
    }
}

fn paragraph(text: &str) -> Paragraph {
    Paragraph::plain(text, style(11.0, false))
}

fn read_back(document: &FlowDocument) -> FlowDocument {
    let bytes = write_flow(document, &DocxWriteOptions::default()).expect("write");
    read_flow(&bytes, &DocxReadOptions::default()).expect("read back")
}

#[test]
fn round_trips_paragraphs_and_run_formatting() {
    let mut flow = FlowDocument::new(Metadata {
        title: Some("往返样本".to_owned()),
        author: Some("测试".to_owned()),
        ..Metadata::default()
    });
    let mut heading = Paragraph::plain("标题一", style(16.0, true));
    heading.heading_level = Some(1);
    flow.push(Block::Paragraph(heading));

    let mut body = Paragraph::new(ParaStyle {
        align: Align::Justify,
        line_spacing: LineSpacing::Multiple(1.5),
        space_before: Twips::from_pt(6.0),
        space_after: Twips::from_pt(12.0),
        first_line_indent: Twips::from_mm(7.4),
        left_indent: Twips::from_mm(3.0),
        ..ParaStyle::default()
    });
    body.runs.push(Inline::Text(TextRun::new(
        "普通文字",
        TextStyle {
            family: FontFamily::new("SimSun"),
            ..style(11.0, false)
        },
    )));
    body.runs
        .push(Inline::Text(TextRun::new("加粗", style(12.0, true))));
    body.runs.push(Inline::Text(TextRun::new(
        "斜体",
        TextStyle {
            italic: true,
            color: ofdbox_folio_ir::Rgba::from_rgb(0xC8, 0x1E, 0x1E),
            ..style(11.0, false)
        },
    )));
    flow.push(Block::Paragraph(body));

    let back = read_back(&flow);
    assert_eq!(back.metadata.title.as_deref(), Some("往返样本"));
    assert_eq!(back.metadata.author.as_deref(), Some("测试"));

    let paragraphs: Vec<_> = back.sections[0]
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Paragraph(paragraph) => Some(paragraph),
            _ => None,
        })
        .collect();
    assert_eq!(paragraphs.len(), 2);

    let heading = paragraphs[0];
    assert_eq!(heading.heading_level, Some(1));
    assert_eq!(heading.style.align, Align::Left);
    match &heading.runs[0] {
        Inline::Text(run) => {
            assert_eq!(run.text, "标题一");
            assert!(run.style.bold);
            assert_eq!(run.style.size.to_pt(), 16.0);
        }
        other => panic!("期望文本 run: {other:?}"),
    }

    let body = paragraphs[1];
    assert_eq!(body.style.align, Align::Justify);
    assert!((body.style.first_line_indent.to_mm() - 7.4).abs() < 0.01);
    assert!((body.style.left_indent.to_mm() - 3.0).abs() < 0.01);
    assert!((body.style.space_before.to_pt() - 6.0).abs() < 0.01);
    assert!((body.style.space_after.to_pt() - 12.0).abs() < 0.01);
    assert!(
        matches!(body.style.line_spacing, LineSpacing::Multiple(factor) if (factor - 1.5).abs() < 0.01)
    );
    assert_eq!(body.runs.len(), 3);
    match &body.runs[0] {
        Inline::Text(run) => {
            assert_eq!(run.text, "普通文字");
            assert_eq!(run.style.family.name, "SimSun");
        }
        other => panic!("期望文本 run: {other:?}"),
    }
    match &body.runs[1] {
        Inline::Text(run) => {
            assert!(run.style.bold);
            assert_eq!(run.style.size.to_pt(), 12.0);
        }
        other => panic!("期望文本 run: {other:?}"),
    }
    match &body.runs[2] {
        Inline::Text(run) => {
            assert!(run.style.italic);
            assert_eq!(
                run.style.color,
                ofdbox_folio_ir::Rgba::from_rgb(0xC8, 0x1E, 0x1E)
            );
        }
        other => panic!("期望文本 run: {other:?}"),
    }
}

#[test]
fn round_trips_tables_and_tabs() {
    let mut flow = FlowDocument::new(Metadata::default());
    let mut table = Table::new(vec![
        Column {
            width: ColumnWidth::Twips(Twips::from_mm(40.0)),
        },
        Column {
            width: ColumnWidth::Twips(Twips::from_mm(50.0)),
        },
    ]);
    table.rows = vec![
        Row {
            cells: vec![cell("表头甲"), cell("表头乙")],
            height: None,
            is_header: true,
        },
        Row::new(vec![cell("单元一"), cell("单元二")]),
    ];
    table.borders = TableBorders {
        outer: Some(ofdbox_folio_ir::Border::default()),
        horizontal: Some(ofdbox_folio_ir::Border::default()),
        vertical: Some(ofdbox_folio_ir::Border::default()),
    };
    table.cell_padding = Margins {
        top: Twips::from_mm(1.0),
        right: Twips::from_mm(2.0),
        bottom: Twips::from_mm(1.0),
        left: Twips::from_mm(2.0),
    };
    flow.push(Block::Table(table));

    let mut tabbed = Paragraph::new(ParaStyle {
        tabs: vec![TabStop::new(
            Twips::from_mm(100.0),
            TabAlign::Right,
            TabLeader::Dot,
        )],
        ..ParaStyle::default()
    });
    tabbed
        .runs
        .push(Inline::Text(TextRun::new("条目", style(11.0, false))));
    tabbed
        .runs
        .push(Inline::Text(TextRun::new("\t", style(11.0, false))));
    tabbed
        .runs
        .push(Inline::Text(TextRun::new("12", style(11.0, false))));
    flow.push(Block::Paragraph(tabbed));

    let back = read_back(&flow);
    let table = back.sections[0]
        .blocks
        .iter()
        .find_map(|block| match block {
            Block::Table(table) => Some(table),
            _ => None,
        })
        .expect("表格");
    assert_eq!(table.columns.len(), 2);
    assert_eq!(table.rows.len(), 2);
    assert_eq!(table.header_rows, 1);
    assert!(table.rows[0].is_header);
    assert!(table.borders.outer.is_some());
    assert!(table.cell_padding.left.0 > 0);

    let tabbed = back.sections[0]
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Paragraph(paragraph) => Some(paragraph),
            _ => None,
        })
        .find(|paragraph| {
            paragraph
                .runs
                .iter()
                .any(|inline| matches!(inline, Inline::Text(run) if run.text.contains('\t')))
        })
        .expect("制表段落");
    assert_eq!(tabbed.style.tabs.len(), 1);
    let stop = tabbed.style.tabs[0];
    assert_eq!(stop.align, TabAlign::Right);
    assert_eq!(stop.leader, TabLeader::Dot);
    assert!((stop.position.to_mm() - 100.0).abs() < 0.01);
}

fn cell(text: &str) -> Cell {
    Cell::new(vec![Block::Paragraph(paragraph(text))])
}

#[test]
fn weight_columns_get_real_widths() {
    let mut flow = FlowDocument::new(Metadata::default());
    let mut table = Table::new(vec![
        Column {
            width: ColumnWidth::Weight(1),
        },
        Column {
            width: ColumnWidth::Weight(2),
        },
    ]);
    table.rows = vec![Row::new(vec![cell("甲"), cell("乙")])];
    flow.push(Block::Table(table));

    let back = read_back(&flow);
    let table = back.sections[0]
        .blocks
        .iter()
        .find_map(|block| match block {
            Block::Table(table) => Some(table),
            _ => None,
        })
        .expect("表格");
    let widths: Vec<i64> = table
        .columns
        .iter()
        .map(|column| match column.width {
            ColumnWidth::Twips(value) => value.0,
            _ => 0,
        })
        .collect();
    assert!(
        widths.iter().all(|width| *width > 0),
        "权重列应换算成实际宽度: {widths:?}"
    );
    assert!(widths[1] > widths[0], "2:1 权重: {widths:?}");
}

#[test]
fn round_trips_images_positions_and_floating_objects() {
    let mut flow = FlowDocument::new(Metadata::default());
    let id = flow.resources.add_image(ImageAsset::new(
        ImageFormat::Png,
        Arc::from(TINY_PNG),
        32,
        16,
    ));
    let mut image = ImageBlock::new(id);
    image.size = Some((Twips::from_mm(40.0), Twips::from_mm(20.0)));
    image.align = Align::Center;
    flow.push(Block::Image(image));

    let floating_image = flow.resources.add_image(ImageAsset::new(
        ImageFormat::Png,
        Arc::from(TINY_PNG),
        10,
        10,
    ));
    let mut floating = FloatingObject::new(
        Point::new(30.0, 20.0),
        Block::Image(ImageBlock::new(floating_image)),
    );
    floating.wrap = WrapMode::Square;
    flow.push(Block::Floating(Box::new(floating)));

    let back = read_back(&flow);
    // 两张图内容相同会被资源表去重，这里只要求至少保留一张，并验证浮动对象还在。
    assert!(back.resources.image_count() >= 1, "图片应写出");

    let floating = back.sections[0]
        .blocks
        .iter()
        .find_map(|block| match block {
            Block::Floating(object) => Some(object),
            _ => None,
        })
        .expect("浮动对象");
    assert_eq!(floating.wrap, WrapMode::Square);
    assert!((floating.position.x - 30.0).abs() < 0.1);
    assert!((floating.position.y - 20.0).abs() < 0.1);
}

#[test]
fn round_trips_sections_headers_footers_and_fields() {
    let mut flow = FlowDocument::new(Metadata::default());
    let mut setup = PageSetup::a4();
    setup.columns = 2;
    setup.column_gap = Twips::from_mm(8.0);
    setup.margins.left = Twips::from_mm(30.0);
    flow.sections[0].setup = setup;

    let mut footer_paragraph = Paragraph::plain("第 ", style(9.0, false));
    footer_paragraph
        .runs
        .push(Inline::Field(Field::new(FieldKind::PageNumber)));
    footer_paragraph
        .runs
        .push(Inline::Text(TextRun::new(" 页", style(9.0, false))));
    flow.sections[0].footers.default = Some(HeaderFooter {
        blocks: vec![Block::Paragraph(footer_paragraph)],
    });
    flow.sections[0].headers.default = Some(HeaderFooter {
        blocks: vec![Block::Paragraph(paragraph("页眉"))],
    });
    flow.push(Block::Paragraph(paragraph("正文")));

    let back = read_back(&flow);
    let section = &back.sections[0];
    assert_eq!(section.setup.columns, 2);
    assert!((section.setup.column_gap.to_mm() - 8.0).abs() < 0.01);
    assert!((section.setup.margins.left.to_mm() - 30.0).abs() < 0.01);

    let header = section.headers.default.as_ref().expect("页眉");
    let text: String = header.blocks.iter().map(block_text).collect();
    assert!(text.contains("页眉"), "页眉文字: {text}");

    let footer = section.footers.default.as_ref().expect("页脚");
    let kinds: Vec<FieldKind> = footer
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Paragraph(paragraph) => Some(paragraph),
            _ => None,
        })
        .flat_map(|paragraph| paragraph.runs.iter())
        .filter_map(|inline| match inline {
            Inline::Field(field) => Some(field.kind.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec![FieldKind::PageNumber]);
}

fn block_text(block: &Block) -> String {
    let Block::Paragraph(paragraph) = block else {
        return String::new();
    };
    fn collect(inlines: &[Inline], text: &mut String) {
        for inline in inlines {
            match inline {
                Inline::Text(run) => text.push_str(&run.text),
                Inline::Link(link) => collect(&link.runs, text),
                Inline::Field(field) => {
                    if let Some(cached) = &field.cached_text {
                        text.push_str(cached);
                    }
                }
                _ => {}
            }
        }
    }
    let mut text = String::new();
    collect(&paragraph.runs, &mut text);
    text
}

#[test]
fn round_trips_footnotes_links_and_toc() {
    let mut flow = FlowDocument::new(Metadata::default());
    flow.notes.push(Note::new(
        NoteKind::Footnote,
        1,
        vec![Block::Paragraph(paragraph("脚注正文"))],
    ));
    let mut body = paragraph("带脚注的段落");
    body.runs
        .push(Inline::Note(NoteRef::new(NoteKind::Footnote, 1)));
    flow.push(Block::Paragraph(body));

    let anchor = ofdbox_folio_ir::AnchorId::new(1);
    let mut target = paragraph("锚点段落");
    target.anchor = Some(anchor);
    flow.push(Block::Paragraph(target));

    flow.push(Block::Paragraph(Paragraph {
        style: ParaStyle::default(),
        runs: vec![Inline::Link(Link::new(
            LinkTarget::Uri("https://example.com".to_owned()),
            vec![Inline::Text(TextRun::new("外链", style(11.0, false)))],
        ))],
        heading_level: None,
        anchor: None,
    }));

    flow.push(Block::Toc(TocBlock {
        max_level: 2,
        title: Some("目录".to_owned()),
        style: ofdbox_folio_ir::TocStyle::default(),
    }));

    let back = read_back(&flow);
    assert_eq!(back.notes.len(), 1, "注释: {:?}", back.notes);
    assert_eq!(back.notes[0].id, 1);
    assert_eq!(back.notes[0].kind, NoteKind::Footnote);

    let text: String = back.sections[0].blocks.iter().map(block_text).collect();
    assert!(text.contains("带脚注的段落"), "正文: {text}");
    assert!(text.contains("外链"), "链接: {text}");
    assert!(text.contains("目录"), "目录标题: {text}");

    let has_note_ref = back.sections[0]
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Paragraph(paragraph) => Some(paragraph),
            _ => None,
        })
        .flat_map(|paragraph| paragraph.runs.iter())
        .any(|inline| matches!(inline, Inline::Note(_)));
    assert!(has_note_ref, "应保留脚注引用");

    let has_link = back.sections[0]
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Paragraph(paragraph) => Some(paragraph),
            _ => None,
        })
        .flat_map(|paragraph| paragraph.runs.iter())
        .any(|inline| matches!(inline, Inline::Link(_)));
    assert!(has_link, "应保留超链接");
}

#[test]
fn output_is_deterministic_and_valid_zip() {
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Paragraph(paragraph("确定性")));
    let first = write_flow(&flow, &DocxWriteOptions::default()).expect("write");
    let second = write_flow(&flow, &DocxWriteOptions::default()).expect("write again");
    assert_eq!(first, second, "相同输入应产生相同字节");

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&first)).expect("zip");
    let names: Vec<String> = (0..archive.len())
        .map(|index| {
            archive
                .by_index(index)
                .expect("entry")
                .name()
                .map(|name| name.to_string())
                .unwrap_or_default()
        })
        .collect();
    for expected in [
        "[Content_Types].xml",
        "_rels/.rels",
        "word/document.xml",
        "word/_rels/document.xml.rels",
        "word/styles.xml",
        "docProps/core.xml",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "缺少 {expected}: {names:?}"
        );
    }
}
