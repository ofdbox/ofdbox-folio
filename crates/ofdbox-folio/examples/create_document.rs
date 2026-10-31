//! Complete example that builds a Flow IR from scratch and writes out both DOCX and OFD.
//!
//! ```text
//! cargo run -p ofdbox-folio --example create_document -- out.docx
//! ```
//!
//! Covers: heading and body formatting, tab stops with dot leaders, tables, images,
//! footnotes, TOC fields, headers/footers with page-number fields, and floating images.
//! The same Flow IR is first written to DOCX, then laid out into Fixed IR and written to
//! OFD (fonts are subset automatically), and finally the DOCX is read back and its
//! structure printed.

use std::error::Error;
use std::sync::Arc;

use ofdbox_folio::docx::{DocxReadOptions, DocxWriteOptions, read_flow_path, write_flow_path};
use ofdbox_folio::ir::{
    Align, Block, Border, Cell, Column, ColumnWidth, Field, FieldKind, FloatingObject,
    FlowDocument, FontFamily, HeaderFooter, ImageAsset, ImageBlock, ImageFormat, Inline,
    LineSpacing, Link, LinkTarget, Margins, Metadata, Note, NoteKind, NoteRef, PageSetup,
    ParaStyle, Paragraph, Point, Row, TabAlign, TabLeader, TabStop, Table, TableBorders, TextRun,
    TextStyle, TocBlock, TocStyle, Twips, WrapMode,
};
use ofdbox_folio::layout::{LayoutOptions, layout_with};
use ofdbox_folio::ofd::{OfdWriteOptions, write_fixed};

mod common;

use common::SystemFonts;

/// Body text style.
fn style(size: f64) -> TextStyle {
    TextStyle {
        size: Twips::from_pt(size),
        family: FontFamily::new("SimSun"),
        ..TextStyle::default()
    }
}

fn paragraph(text: &str) -> Paragraph {
    Paragraph::plain(text, style(11.0))
}

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "example.docx".to_owned());

    let mut flow = FlowDocument::new(Metadata {
        title: Some("创建 DOCX 示例".to_owned()),
        author: Some("OFDBox".to_owned()),
        subject: Some("Flow IR → DOCX".to_owned()),
        keywords: vec!["示例".to_owned(), "docx".to_owned()],
        ..Metadata::default()
    });
    flow.sections[0].setup = PageSetup::a4();

    // ---- Heading and body ----
    let mut title = paragraph("创建 DOCX 示例");
    title.style = ParaStyle {
        align: Align::Center,
        space_after: Twips::from_pt(18.0),
        ..ParaStyle::default()
    };
    title.runs = vec![Inline::Text(TextRun::new(
        "创建 DOCX 示例",
        TextStyle {
            size: Twips::from_pt(22.0),
            bold: true,
            ..TextStyle::default()
        },
    ))];
    title.heading_level = Some(1);
    flow.push(Block::Paragraph(title));

    let mut body = paragraph("这一段演示字符格式：");
    body.runs.push(Inline::Text(TextRun::new(
        "加粗",
        TextStyle {
            bold: true,
            ..style(11.0)
        },
    )));
    body.runs
        .push(Inline::Text(TextRun::new("、", style(11.0))));
    body.runs.push(Inline::Text(TextRun::new(
        "斜体",
        TextStyle {
            italic: true,
            ..style(11.0)
        },
    )));
    body.runs
        .push(Inline::Text(TextRun::new("、", style(11.0))));
    body.runs.push(Inline::Text(TextRun::new(
        "红色",
        TextStyle {
            color: ofdbox_folio::ir::Rgba::from_rgb(0xC0, 0x1C, 0x28),
            ..style(11.0)
        },
    )));
    body.runs
        .push(Inline::Text(TextRun::new("、上标", style(11.0))));
    body.runs.push(Inline::Text(TextRun::new(
        "2",
        TextStyle {
            superscript: true,
            ..style(8.0)
        },
    )));
    body.runs
        .push(Inline::Text(TextRun::new("，以及", style(11.0))));
    body.runs.push(Inline::Link(Link::new(
        LinkTarget::Uri("https://example.com".to_owned()),
        vec![Inline::Text(TextRun::new("一个超链接", style(11.0)))],
    )));
    body.runs
        .push(Inline::Text(TextRun::new("。", style(11.0))));
    // Footnote reference: inserted in the body; the footnote text lives in the notes at
    // the end of the document.
    body.runs
        .push(Inline::Note(NoteRef::new(NoteKind::Footnote, 1)));
    flow.push(Block::Paragraph(body));

    // ---- Tab stop + dot leader (a TOC-style line) ----
    let mut leader_line = Paragraph::new(ParaStyle {
        tabs: vec![TabStop::new(
            Twips::from_mm(150.0),
            TabAlign::Right,
            TabLeader::Dot,
        )],
        ..ParaStyle::default()
    });
    leader_line
        .runs
        .push(Inline::Text(TextRun::new("第一章 总则", style(11.0))));
    leader_line
        .runs
        .push(Inline::Text(TextRun::new("\t", style(11.0))));
    leader_line
        .runs
        .push(Inline::Text(TextRun::new("1", style(11.0))));
    flow.push(Block::Paragraph(leader_line));

    let mut second =
        paragraph("第二段用于确认段前段后与行距：行距 1.5 倍，段前 12 磅、段后 6 磅。");
    second.style = ParaStyle {
        line_spacing: LineSpacing::Multiple(1.5),
        space_before: Twips::from_pt(12.0),
        space_after: Twips::from_pt(6.0),
        first_line_indent: Twips::from_mm(7.4),
        align: Align::Justify,
        ..ParaStyle::default()
    };
    flow.push(Block::Paragraph(second));

    // ---- Table ----
    let mut table = Table::new(vec![
        Column {
            width: ColumnWidth::Weight(2),
        },
        Column {
            width: ColumnWidth::Weight(3),
        },
        Column {
            width: ColumnWidth::Weight(2),
        },
    ]);
    table.borders = TableBorders {
        outer: Some(Border::default()),
        horizontal: Some(Border::default()),
        vertical: Some(Border::default()),
    };
    table.cell_padding = Margins {
        top: Twips::from_mm(1.0),
        right: Twips::from_mm(2.0),
        bottom: Twips::from_mm(1.0),
        left: Twips::from_mm(2.0),
    };
    let header = Row {
        cells: vec![cell("列一"), cell("列二"), cell("列三")],
        height: None,
        is_header: true,
        ..Row::new(Vec::new())
    };
    let data = Row::new(vec![cell("R1C0"), cell("第二列的内容"), cell("R1C2")]);
    table.rows = vec![
        header,
        data,
        Row::new(vec![cell("R2C0"), cell("R2C1"), cell("R2C2")]),
    ];
    flow.push(Block::Table(table));

    // ---- Images (inline + floating) ----
    let (png, width, height) = sample_png();
    let id = flow.resources.add_image(ImageAsset::new(
        ImageFormat::Png,
        Arc::from(png.as_slice()),
        width,
        height,
    ));
    let mut image = ImageBlock::new(id);
    image.size = Some((Twips::from_mm(60.0), Twips::from_mm(40.0)));
    image.align = Align::Center;
    image.alt = Some("渐变色块".to_owned());
    flow.push(Block::Image(image));

    let mut floating_image = ImageBlock::new(id);
    floating_image.size = Some((Twips::from_mm(30.0), Twips::from_mm(20.0)));
    let mut floating = FloatingObject::new(Point::new(120.0, 190.0), Block::Image(floating_image));
    floating.wrap = WrapMode::Square;
    flow.push(Block::Floating(Box::new(floating)));

    // ---- TOC field ----
    flow.push(Block::Toc(TocBlock {
        max_level: 3,
        title: Some("目录".to_owned()),
        style: TocStyle::default(),
    }));

    // ---- Footnote text ----
    flow.notes.push(Note::new(
        NoteKind::Footnote,
        1,
        vec![Block::Paragraph(paragraph(
            "这是脚注正文，会排在引用所在页的页底。",
        ))],
    ));

    // ---- Header and footer ----
    let mut header = Paragraph::plain("OFDBox Folio 示例", style(9.0));
    header.style.align = Align::Right;
    flow.sections[0].headers.default = Some(HeaderFooter {
        blocks: vec![Block::Paragraph(header)],
    });
    let mut footer = Paragraph::new(ParaStyle {
        align: Align::Center,
        ..ParaStyle::default()
    });
    footer
        .runs
        .push(Inline::Text(TextRun::new("第 ", style(9.0))));
    footer
        .runs
        .push(Inline::Field(Field::new(FieldKind::PageNumber)));
    footer
        .runs
        .push(Inline::Text(TextRun::new(" 页 / 共 ", style(9.0))));
    footer
        .runs
        .push(Inline::Field(Field::new(FieldKind::TotalPages)));
    footer
        .runs
        .push(Inline::Text(TextRun::new(" 页", style(9.0))));
    flow.sections[0].footers.default = Some(HeaderFooter {
        blocks: vec![Block::Paragraph(footer)],
    });

    // ---- Write DOCX ----
    write_flow_path(&flow, &path, &DocxWriteOptions::default())?;
    let size = std::fs::metadata(&path)?.len();
    println!("Wrote DOCX: {path} ({size} bytes)");

    // ---- Lay out the same Flow IR into Fixed IR, then write OFD ----
    let mut fonts = SystemFonts::new();
    let fixed = layout_with(&flow, &mut fonts, LayoutOptions::default())?;
    for diagnostic in &fixed.report.diagnostics {
        eprintln!("  layout diagnostic: {}", diagnostic.message);
    }
    let ofd_path = std::path::Path::new(&path)
        .with_extension("ofd")
        .to_string_lossy()
        .to_string();
    let ofd = write_fixed(&fixed, &OfdWriteOptions::default())?;
    std::fs::write(&ofd_path, &ofd)?;
    println!(
        "Wrote OFD: {ofd_path} ({} bytes, {} pages)",
        ofd.len(),
        fixed.pages.len()
    );

    // ---- Read the DOCX back ----
    let back = read_flow_path(&path, &DocxReadOptions::default())?;
    let blocks: usize = back
        .sections
        .iter()
        .map(|section| section.blocks.len())
        .sum();
    println!(
        "Read back: {} sections, {} blocks, {} images, {} notes",
        back.sections.len(),
        blocks,
        back.resources.image_count(),
        back.notes.len()
    );
    Ok(())
}

/// Cell: one paragraph.
fn cell(text: &str) -> Cell {
    Cell::new(vec![Block::Paragraph(paragraph(text))])
}

/// Generates a 240×160 gradient PNG so the example does not depend on external image files.
fn sample_png() -> (Vec<u8>, u32, u32) {
    let width = 240_u32;
    let height = 160_u32;
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            pixels.extend([(x * 255 / width) as u8, (y * 255 / height) as u8, 200, 255]);
        }
    }
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer.write_image_data(&pixels).expect("PNG data");
    }
    (png, width, height)
}
