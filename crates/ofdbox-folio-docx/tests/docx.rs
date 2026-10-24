//! DOCX → Flow IR 的端到端测试。
//!
//! 测试在内存里拼一个最小 DOCX，覆盖段落样式链、直接格式、列表编号、表格、
//! 行内图片、页眉页脚与页码域。

use std::io::{Cursor, Write};

use ofdbox_folio_docx::{DocxReadOptions, read_flow};
use ofdbox_folio_ir::{Block, FieldKind, Inline};

/// 1×1 的 RGBA PNG。
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// 拼一个最小 DOCX。
fn build(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in parts {
        zip.start_file(*name, options).expect("start");
        zip.write_all(data).expect("write");
    }
    let cursor = zip.finish().expect("finish");
    cursor.into_inner()
}

/// 拼一个带完整正文的 DOCX。
fn sample() -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:r="{R_NS}"
  xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
  xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
  xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
  <w:body>
    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>标题一</w:t></w:r></w:p>
    <w:p>
      <w:pPr>
        <w:spacing w:before="120" w:after="240" w:line="360" w:lineRule="auto"/>
        <w:ind w:firstLine="420"/>
        <w:jc w:val="both"/>
      </w:pPr>
      <w:r><w:rPr><w:b/><w:sz w:val="24"/></w:rPr><w:t>加粗</w:t></w:r>
      <w:r><w:rPr><w:i/><w:color w:val="C81E1E"/></w:rPr><w:t>斜体红字</w:t></w:r>
    </w:p>
    <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>列表项</w:t></w:r></w:p>
    <w:tbl>
      <w:tblPr><w:tblStyle w:val="TableGrid"/></w:tblPr>
      <w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>
      <w:tr><w:trPr><w:tblHeader/></w:trPr>
        <w:tc><w:p><w:r><w:t>表头一</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>表头二</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:p><w:r><w:t>单元一</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>单元二</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
    <w:p><w:r><w:drawing><wp:inline>
      <wp:extent cx="360000" cy="360000"/>
      <wp:docPr id="1" name="图 1" descr="示例图"/>
      <a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">
        <pic:pic><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic>
      </a:graphicData></a:graphic>
    </wp:inline></w:drawing></w:r></w:p>
    <w:p><w:r><w:br w:type="page"/></w:r></w:p>
    <w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t>第二页</w:t></w:r></w:p>
    <w:sectPr>
      <w:headerReference w:type="default" r:id="rId4"/>
      <w:footerReference w:type="default" r:id="rId5"/>
      <w:cols w:num="2" w:space="425"/>
      <w:pgSz w:w="11906" w:h="16838"/>
      <w:pgMar w:top="1440" w:bottom="1440" w:left="1440" w:right="1440" w:header="720" w:footer="720"/>
    </w:sectPr>
  </w:body>
</w:document>"#
    );
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{W_NS}">
  <w:docDefaults>
    <w:rPrDefault><w:rPr><w:rFonts w:asciiTheme="minorHAnsi" w:eastAsiaTheme="minorEastAsia"/><w:sz w:val="22"/></w:rPr></w:rPrDefault>
    <w:pPrDefault><w:pPr><w:spacing w:after="200" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault>
  </w:docDefaults>
  <w:style w:type="paragraph" w:default="1" w:styleId="Normal">
    <w:name w:val="Normal"/>
    <w:rPr><w:rFonts w:ascii="SimSun" w:eastAsia="宋体"/><w:sz w:val="22"/></w:rPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="Heading1">
    <w:name w:val="heading 1"/><w:basedOn w:val="Normal"/>
    <w:pPr><w:outlineLvl w:val="0"/><w:spacing w:before="480" w:after="0"/></w:pPr>
    <w:rPr><w:b/><w:sz w:val="28"/></w:rPr>
  </w:style>
  <w:style w:type="table" w:styleId="TableGrid">
    <w:name w:val="Table Grid"/>
    <w:tblPr>
      <w:tblBorders>
        <w:top w:val="single" w:sz="4"/><w:left w:val="single" w:sz="4"/>
        <w:bottom w:val="single" w:sz="4"/><w:right w:val="single" w:sz="4"/>
        <w:insideH w:val="single" w:sz="4"/><w:insideV w:val="single" w:sz="4"/>
      </w:tblBorders>
      <w:tblCellMar><w:left w:w="108" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar>
    </w:tblPr>
  </w:style>
</w:styles>"#
    );
    let numbering = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="{W_NS}">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="&#xF0B7;"/>
      <w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#
    );
    let footer = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="{W_NS}">
  <w:p><w:pPr><w:jc w:val="center"/></w:pPr>
    <w:r><w:t xml:space="preserve">第 </w:t></w:r>
    <w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple>
    <w:r><w:t xml:space="preserve"> 页 / 共 </w:t></w:r>
    <w:fldSimple w:instr=" NUMPAGES "><w:r><w:t>2</w:t></w:r></w:fldSimple>
    <w:r><w:t xml:space="preserve"> 页</w:t></w:r>
  </w:p>
</w:ftr>"#
    );
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="{W_NS}"><w:p><w:r><w:t>页眉</w:t></w:r></w:p></w:hdr>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>
  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
  <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>
  <Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
</Relationships>"#;
    build(&[
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
        ("word/footer1.xml", footer.as_bytes()),
        ("word/header1.xml", header.as_bytes()),
        ("word/_rels/document.xml.rels", rels.as_bytes()),
        ("word/media/image1.png", TINY_PNG),
    ])
}

fn read() -> ofdbox_folio_ir::FlowDocument {
    read_flow(&sample(), &DocxReadOptions::default()).expect("read")
}

#[test]
fn reads_sections_and_page_setup() {
    let flow = read();
    assert_eq!(flow.sections.len(), 1);
    let setup = flow.sections[0].setup;
    assert_eq!(setup.width.0, 11906);
    assert_eq!(setup.height.0, 16838);
    assert_eq!(setup.margins.left.0, 1440);
    assert_eq!(setup.header_distance.0, 720);
    assert_eq!(setup.columns, 2);
    assert_eq!(setup.column_gap.0, 425);
}

#[test]
fn maps_heading_paragraphs_and_run_formatting() {
    let flow = read();
    let paragraphs: Vec<_> = flow.sections[0]
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Paragraph(paragraph) => Some(paragraph),
            _ => None,
        })
        .collect();

    let heading = paragraphs.first().expect("标题段");
    assert_eq!(heading.heading_level, Some(1));
    // Heading1 继承 Normal 的字体，字号来自 Heading1 自己的 rPr。
    match &heading.runs[0] {
        Inline::Text(run) => {
            assert_eq!(run.text, "标题一");
            assert!(run.style.bold);
            assert_eq!(run.style.size.to_pt(), 14.0);
            assert_eq!(run.style.family.name, "SimSun");
        }
        other => panic!("期望文本 run: {other:?}"),
    }

    let body = paragraphs.get(1).expect("正文段");
    assert_eq!(body.style.align, ofdbox_folio_ir::Align::Justify);
    assert_eq!(body.style.first_line_indent.0, 420);
    assert_eq!(body.style.space_before.0, 120);
    assert_eq!(body.style.space_after.0, 240);
    assert_eq!(body.runs.len(), 2);
    match &body.runs[0] {
        Inline::Text(run) => {
            assert!(run.style.bold);
            assert_eq!(run.style.size.to_pt(), 12.0);
        }
        other => panic!("期望文本 run: {other:?}"),
    }
    match &body.runs[1] {
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
fn turns_numbering_into_marker_and_hanging_indent() {
    let flow = read();
    let bullet = flow.sections[0]
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
                .any(|inline| matches!(inline, Inline::Text(run) if run.text.contains('•')))
        })
        .expect("列表段");
    assert_eq!(bullet.style.left_indent.0, 720);
    assert_eq!(bullet.style.first_line_indent.0, -360);
    match &bullet.runs[0] {
        Inline::Text(run) => assert!(run.text.starts_with('•')),
        other => panic!("期望标记 run: {other:?}"),
    }
}

#[test]
fn reads_tables_with_header_rows_and_borders() {
    let flow = read();
    let table = flow.sections[0]
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
    assert!(table.borders.horizontal.is_some());
    assert!(table.cell_padding.left.0 > 0);
    assert_eq!(table.rows[1].cells.len(), 2);
}

#[test]
fn reads_inline_pictures_with_display_size() {
    let flow = read();
    let image = flow.sections[0]
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Paragraph(paragraph) => Some(paragraph),
            _ => None,
        })
        .flat_map(|paragraph| paragraph.runs.iter())
        .find_map(|inline| match inline {
            Inline::Image(image) => Some(image),
            _ => None,
        })
        .expect("行内图片");
    // 360000 EMU = 10 mm。
    let (width, height) = image.size.expect("显示尺寸");
    assert_eq!(width.to_pt(), 28.35);
    assert_eq!(height.to_pt(), 28.35);
    assert_eq!(image.alt.as_deref(), Some("示例图"));
    assert_eq!(flow.resources.image_count(), 1);
    let asset = flow.resources.image(image.image).expect("图片资源");
    assert_eq!((asset.pixel_width, asset.pixel_height), (1, 1));
}

#[test]
fn reads_page_break_and_footer_fields() {
    let flow = read();
    let breaks = flow.sections[0]
        .blocks
        .iter()
        .filter(|block| matches!(block, Block::PageBreak))
        .count();
    assert_eq!(breaks, 1);

    let footer = flow.sections[0].footers.default.as_ref().expect("页脚");
    assert_eq!(footer.blocks.len(), 1);
    let Block::Paragraph(paragraph) = &footer.blocks[0] else {
        panic!("页脚应是段落");
    };
    let kinds: Vec<_> = paragraph
        .runs
        .iter()
        .filter_map(|inline| match inline {
            Inline::Field(field) => Some(field.kind.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec![FieldKind::PageNumber, FieldKind::TotalPages]);

    let header = flow.sections[0].headers.default.as_ref().expect("页眉");
    assert_eq!(header.blocks.len(), 1);
}

#[test]
fn rejects_bytes_that_are_not_docx() {
    assert!(read_flow(b"not a zip", &DocxReadOptions::default()).is_err());
    let zipped = build(&[("other.xml", b"<x/>")]);
    assert!(read_flow(&zipped, &DocxReadOptions::default()).is_err());
    assert!(!ofdbox_folio_docx::probe(b"not a zip"));
    assert!(ofdbox_folio_docx::probe(&sample()));
}

#[test]
fn honours_part_size_limit() {
    let options = DocxReadOptions { max_part_bytes: 16 };
    assert!(read_flow(&sample(), &options).is_err());
}
