//! Writing Flow IR out as a DOCX (OPC) package.
//!
//! Only covers what Flow IR can express: sections and page setup, paragraph and
//! character formatting, tab stops, tables, inline and floating images, headers and
//! footers, page number fields, hyperlinks, footnotes/endnotes, automatic TOC fields and
//! metadata. Image masks and attachments are not supported yet; writing them leaves a
//! documented gap in the document (nothing is dropped silently).
//!
//! Output uses fixed ZIP timestamps, so identical input produces identical bytes.

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::sync::Arc;

use ofdbox_folio_ir::{
    Align, Block, Border, BorderStyle, Cell, ColumnWidth, Diagnostic, DiagnosticCode, Field,
    FieldKind, FlowDocument, FontFamily, HeaderFooter, ImageFormat, Inline, LineSpacing,
    LinkTarget, Margins, Metadata, NoteKind, PageSetup, ParaStyle, Paragraph, PositionFrom, Report,
    Row, TabAlign, TabLeader, TableBorders, TextStyle, TocBlock, Twips, WrapMode,
};

use crate::DocxError;

/// DOCX write options.
#[derive(Clone, Debug)]
pub struct DocxWriteOptions {
    /// Name of the generating tool.
    pub creator: String,
    /// Fixed ZIP timestamp, which keeps the output reproducible.
    pub deterministic: bool,
}

impl Default for DocxWriteOptions {
    fn default() -> Self {
        Self {
            creator: "OFDBox Folio".to_owned(),
            deterministic: true,
        }
    }
}

/// Write Flow IR into a DOCX package.
pub fn write_flow(
    document: &FlowDocument,
    options: &DocxWriteOptions,
) -> Result<Vec<u8>, DocxError> {
    Ok(write_flow_with_report(document, options)?.0)
}

/// Write a DOCX and return the downgrade diagnostics collected during the write (for
/// example an image mask that cannot be written).
pub fn write_flow_with_report(
    document: &FlowDocument,
    options: &DocxWriteOptions,
) -> Result<(Vec<u8>, Report), DocxError> {
    Writer::new(document, options).run()
}

/// Write Flow IR into a DOCX file.
pub fn write_flow_path(
    document: &FlowDocument,
    path: impl AsRef<std::path::Path>,
    options: &DocxWriteOptions,
) -> Result<(), DocxError> {
    let bytes = write_flow(document, options)?;
    std::fs::write(path, bytes).map_err(|error| DocxError::Io(error.to_string()))
}

/// A relationship.
struct Relationship {
    id: u32,
    kind: String,
    target: String,
    external: bool,
}

/// A header/footer part.
struct AreaPart {
    name: String,
    /// `header` or `footer`.
    kind: &'static str,
    xml: String,
}

/// State of the write process.
struct Writer<'a> {
    document: &'a FlowDocument,
    options: &'a DocxWriteOptions,
    body: String,
    relationships: Vec<Relationship>,
    next_rel: u32,
    next_doc_pr: u32,
    /// Image asset → (rId, package path).
    images: HashMap<u64, (u32, String)>,
    /// Media files: package path plus bytes.
    media: Vec<(String, Arc<[u8]>)>,
    /// Header/footer parts.
    areas: Vec<AreaPart>,
    next_area: u32,
    /// Registered bookmark names.
    anchors: HashMap<u64, String>,
    next_anchor: u32,
    /// Body width of the current section (twips); weighted table column widths are
    /// computed from it.
    content_width: i64,
    /// Downgrade diagnostics collected during the write.
    report: Report,
}

impl<'a> Writer<'a> {
    fn new(document: &'a FlowDocument, options: &'a DocxWriteOptions) -> Self {
        Self {
            document,
            options,
            body: String::new(),
            relationships: Vec::new(),
            next_rel: 1,
            next_doc_pr: 1,
            images: HashMap::new(),
            media: Vec::new(),
            areas: Vec::new(),
            next_area: 1,
            anchors: HashMap::new(),
            next_anchor: 1,
            content_width: 0,
            report: Report::new(),
        }
    }

    fn run(mut self) -> Result<(Vec<u8>, Report), DocxError> {
        if self.document.sections.is_empty() {
            return Err(DocxError::Structure(
                "no content to split into sections".to_owned(),
            ));
        }
        self.write_body();
        let notes = self.notes_xml()?;
        let report = std::mem::take(&mut self.report);
        let bytes = self.package(notes)?;
        Ok((bytes, report))
    }

    // ---- Body ----

    fn write_body(&mut self) {
        let sections = &self.document.sections;
        for (index, section) in sections.iter().enumerate() {
            self.content_width = (section.setup.width.0
                - section.setup.margins.left.0
                - section.setup.margins.right.0)
                .max(1);
            let mut blocks = String::new();
            self.blocks_xml(&section.blocks, &mut blocks);
            self.body.push_str(&blocks);
            let last = index + 1 == sections.len();
            if last {
                let sect = self.section_props(section.setup, &section.headers, &section.footers);
                self.body.push_str(&sect);
            } else {
                // The section break hangs off the paragraph's pPr.
                let sect = self.section_props(section.setup, &section.headers, &section.footers);
                self.body
                    .push_str(&format!("<w:p><w:pPr>{sect}</w:pPr></w:p>"));
            }
        }
    }

    fn section_props(
        &mut self,
        setup: PageSetup,
        headers: &ofdbox_folio_ir::HeaderFooterSet,
        footers: &ofdbox_folio_ir::HeaderFooterSet,
    ) -> String {
        let mut xml = String::from("<w:sectPr>");
        for (slot, area) in [
            ("default", &headers.default),
            ("first", &headers.first),
            ("even", &headers.even),
        ] {
            if let Some(area) = area {
                let id = self.area_part("header", area);
                xml.push_str(&format!(
                    "<w:headerReference w:type=\"{slot}\" r:id=\"rId{id}\"/>"
                ));
            }
        }
        for (slot, area) in [
            ("default", &footers.default),
            ("first", &footers.first),
            ("even", &footers.even),
        ] {
            if let Some(area) = area {
                let id = self.area_part("footer", area);
                xml.push_str(&format!(
                    "<w:footerReference w:type=\"{slot}\" r:id=\"rId{id}\"/>"
                ));
            }
        }
        xml.push_str(&format!(
            "<w:pgSz w:w=\"{}\" w:h=\"{}\"/>",
            setup.width.0, setup.height.0
        ));
        xml.push_str(&format!(
            "<w:pgMar w:top=\"{}\" w:right=\"{}\" w:bottom=\"{}\" w:left=\"{}\" w:header=\"{}\" w:footer=\"{}\"/>",
            setup.margins.top.0,
            setup.margins.right.0,
            setup.margins.bottom.0,
            setup.margins.left.0,
            setup.header_distance.0,
            setup.footer_distance.0
        ));
        if setup.columns > 1 {
            xml.push_str(&format!(
                "<w:cols w:num=\"{}\" w:space=\"{}\"/>",
                setup.columns, setup.column_gap.0
            ));
        }
        xml.push_str("</w:sectPr>");
        xml
    }

    /// Register a header/footer part and return its relationship ID.
    fn area_part(&mut self, kind: &'static str, area: &HeaderFooter) -> u32 {
        let index = self.next_area;
        self.next_area += 1;
        let name = format!("{kind}{index}.xml");
        let mut inner = String::new();
        self.blocks_xml(&area.blocks, &mut inner);
        let root = if kind == "header" { "hdr" } else { "ftr" };
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><w:{root} {NAMESPACES}>{inner}</w:{root}>"
        );
        let id = self.next_rel_id();
        self.relationships.push(Relationship {
            id,
            kind: format!(
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}"
            ),
            target: name.clone(),
            external: false,
        });
        self.areas.push(AreaPart { name, kind, xml });
        id
    }

    fn next_rel_id(&mut self) -> u32 {
        let id = self.next_rel;
        self.next_rel += 1;
        id
    }

    fn blocks_xml(&mut self, blocks: &[Block], out: &mut String) {
        for block in blocks {
            self.block_xml(block, out);
        }
    }

    fn block_xml(&mut self, block: &Block, out: &mut String) {
        match block {
            Block::Paragraph(paragraph) => out.push_str(&self.paragraph_xml(paragraph)),
            Block::Table(table) => self.table_xml(table, out),
            Block::Image(image) => {
                let paragraph = Paragraph {
                    style: ParaStyle {
                        align: image.align,
                        ..ParaStyle::default()
                    },
                    runs: vec![Inline::Image(ofdbox_folio_ir::InlineImage {
                        image: image.image,
                        size: image.size,
                        alt: image.alt.clone(),
                    })],
                    heading_level: None,
                    anchor: None,
                };
                out.push_str(&self.paragraph_xml(&paragraph));
            }
            Block::PageBreak => out.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>"),
            Block::Toc(toc) => self.toc_xml(toc, out),
            Block::Floating(floating) => self.floating_xml(floating, out),
            _ => {}
        }
    }

    // ---- Paragraphs and inlines ----

    fn paragraph_xml(&mut self, paragraph: &Paragraph) -> String {
        let mut xml = String::from("<w:p>");
        xml.push_str(&self.paragraph_props_with_heading(&paragraph.style, paragraph.heading_level));
        let bookmark = paragraph
            .anchor
            .map(|anchor| self.anchor_name(anchor))
            .map(|name| (name, self.next_doc_pr));
        if let Some((name, id)) = &bookmark {
            xml.push_str(&format!(
                "<w:bookmarkStart w:id=\"{id}\" w:name=\"{}\"/>",
                esc(name)
            ));
        }
        for inline in &paragraph.runs {
            self.inline_xml(inline, &mut xml);
        }
        if let Some((_, id)) = &bookmark {
            xml.push_str(&format!("<w:bookmarkEnd w:id=\"{id}\"/>"));
        }
        xml.push_str("</w:p>");
        xml
    }

    fn paragraph_props(&mut self, style: &ParaStyle) -> String {
        self.paragraph_props_with_heading(style, None)
    }

    fn paragraph_props_with_heading(&mut self, style: &ParaStyle, heading: Option<u8>) -> String {
        let mut props = String::new();
        if let Some(id) = &style.style_id {
            props.push_str(&format!("<w:pStyle w:val=\"{}\"/>", esc(id)));
        }
        if style.keep_with_next {
            props.push_str("<w:keepNext/>");
        }
        if style.keep_lines_together {
            props.push_str("<w:keepLines/>");
        }
        // Tab stops.
        if !style.tabs.is_empty() {
            let mut tabs = String::from("<w:tabs>");
            for stop in &style.tabs {
                let align = match stop.align {
                    TabAlign::Left => "left",
                    TabAlign::Center => "center",
                    TabAlign::Right => "right",
                    TabAlign::Decimal => "decimal",
                    _ => "left",
                };
                let leader = match stop.leader {
                    TabLeader::None => "",
                    TabLeader::Dot => " w:leader=\"dot\"",
                    TabLeader::Dash => " w:leader=\"hyphen\"",
                    TabLeader::Line => " w:leader=\"underscore\"",
                    _ => "",
                };
                tabs.push_str(&format!(
                    "<w:tab w:val=\"{align}\" w:pos=\"{}\"{leader}/>",
                    stop.position.0
                ));
            }
            tabs.push_str("</w:tabs>");
            props.push_str(&tabs);
        }
        let mut spacing = String::new();
        if style.space_before.0 != 0 {
            spacing.push_str(&format!(" w:before=\"{}\"", style.space_before.0));
        }
        if style.space_after.0 != 0 {
            spacing.push_str(&format!(" w:after=\"{}\"", style.space_after.0));
        }
        match style.line_spacing {
            LineSpacing::Single => {}
            LineSpacing::Multiple(factor) if factor.is_finite() && factor > 0.0 => {
                spacing.push_str(&format!(
                    " w:line=\"{}\" w:lineRule=\"auto\"",
                    (factor * 240.0).round() as i64
                ));
            }
            LineSpacing::Exact(value) => {
                spacing.push_str(&format!(" w:line=\"{}\" w:lineRule=\"exact\"", value.0));
            }
            LineSpacing::AtLeast(value) => {
                spacing.push_str(&format!(" w:line=\"{}\" w:lineRule=\"atLeast\"", value.0));
            }
            _ => {}
        }
        if !spacing.is_empty() {
            props.push_str(&format!("<w:spacing{spacing}/>"));
        }
        if style.first_line_indent.0 != 0 || style.left_indent.0 != 0 || style.right_indent.0 != 0 {
            let mut ind = String::from("<w:ind w:left=\"");
            ind.push_str(&style.left_indent.0.to_string());
            ind.push_str(&format!("\" w:right=\"{}\"", style.right_indent.0));
            if style.first_line_indent.0 != 0 {
                if style.first_line_indent.0 > 0 {
                    ind.push_str(&format!(" w:firstLine=\"{}\"", style.first_line_indent.0));
                } else {
                    ind.push_str(&format!(" w:hanging=\"{}\"", -style.first_line_indent.0));
                }
            }
            ind.push_str("/>");
            props.push_str(&ind);
        }
        if let Some(level) = heading {
            // Heading level: OOXML's outlineLvl is 0-based.
            props.push_str(&format!(
                "<w:outlineLvl w:val=\"{}\"/>",
                level.saturating_sub(1)
            ));
        }
        if style.align != Align::Left {
            let value = match style.align {
                Align::Center => "center",
                Align::Right => "right",
                Align::Justify => "both",
                _ => "left",
            };
            props.push_str(&format!("<w:jc w:val=\"{value}\"/>"));
        }
        if props.is_empty() {
            String::new()
        } else {
            format!("<w:pPr>{props}</w:pPr>")
        }
    }

    fn inline_xml(&mut self, inline: &Inline, out: &mut String) {
        match inline {
            Inline::Text(run) => {
                let props = self.run_props(&run.style, run.style_id.as_deref());
                let mut pieces = String::from("<w:r>");
                pieces.push_str(&props);
                let mut text = String::new();
                for ch in run.text.chars() {
                    match ch {
                        '\n' => {
                            flush_text(&mut pieces, &mut text);
                            pieces.push_str("<w:br/>");
                        }
                        '\t' => {
                            flush_text(&mut pieces, &mut text);
                            pieces.push_str("<w:tab/>");
                        }
                        _ => text.push(ch),
                    }
                }
                flush_text(&mut pieces, &mut text);
                pieces.push_str("</w:r>");
                out.push_str(&pieces);
            }
            Inline::Field(field) => self.field_xml(field, out),
            Inline::Note(note) => {
                let tag = match note.kind {
                    NoteKind::Footnote => "w:footnoteReference",
                    NoteKind::Endnote => "w:endnoteReference",
                };
                out.push_str(&format!(
                    "<w:r><w:rPr><w:vertAlign w:val=\"superscript\"/></w:rPr><{tag} w:id=\"{}\"/></w:r>",
                    note.id
                ));
            }
            Inline::Image(image) => {
                let run = self.drawing_xml(image.image, image.size, image.alt.as_deref(), None);
                out.push_str(&run);
            }
            Inline::Link(link) => {
                let mut body = String::new();
                for inner in &link.runs {
                    self.inline_xml(inner, &mut body);
                }
                match &link.target {
                    LinkTarget::Uri(uri) => {
                        let id = self.next_rel_id();
                        self.relationships.push(Relationship {
                            id,
                            kind: "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink".to_owned(),
                            target: uri.clone(),
                            external: true,
                        });
                        out.push_str(&format!(
                            "<w:hyperlink r:id=\"rId{id}\">{body}</w:hyperlink>"
                        ));
                    }
                    LinkTarget::Anchor(anchor) => {
                        let name = self.anchor_name(*anchor);
                        out.push_str(&format!(
                            "<w:hyperlink w:anchor=\"{}\">{body}</w:hyperlink>",
                            esc(&name)
                        ));
                    }
                    _ => out.push_str(&body),
                }
            }
            _ => {}
        }
    }

    fn field_xml(&mut self, field: &Field, out: &mut String) {
        let instruction = match &field.kind {
            FieldKind::PageNumber => " PAGE ".to_owned(),
            FieldKind::TotalPages => " NUMPAGES ".to_owned(),
            FieldKind::Date => " DATE ".to_owned(),
            FieldKind::Ref(anchor) => {
                let name = self.anchor_name(*anchor);
                format!(" REF {} ", name)
            }
            _ => return,
        };
        let cached = field.cached_text.clone().unwrap_or_default();
        out.push_str(&format!(
            "<w:fldSimple w:instr=\"{}\"><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:fldSimple>",
            esc(&instruction),
            esc(&cached)
        ));
    }

    fn run_props(&mut self, style: &TextStyle, style_id: Option<&str>) -> String {
        let mut props = String::new();
        if let Some(id) = style_id {
            props.push_str(&format!("<w:rStyle w:val=\"{}\"/>", esc(id)));
        }
        props.push_str(&font_xml(&style.family));
        if style.bold {
            props.push_str("<w:b/>");
        }
        if style.italic {
            props.push_str("<w:i/>");
        }
        if style.underline {
            props.push_str("<w:u w:val=\"single\"/>");
        }
        if style.strike {
            props.push_str("<w:strike/>");
        }
        if style.color != ofdbox_folio_ir::Rgba::BLACK {
            props.push_str(&format!(
                "<w:color w:val=\"{:02X}{:02X}{:02X}\"/>",
                style.color.r, style.color.g, style.color.b
            ));
        }
        let half_points = (style.size.to_pt() * 2.0).round() as i64;
        props.push_str(&format!("<w:sz w:val=\"{half_points}\"/>"));
        if style.superscript {
            props.push_str("<w:vertAlign w:val=\"superscript\"/>");
        } else if style.subscript {
            props.push_str("<w:vertAlign w:val=\"subscript\"/>");
        }
        if props.is_empty() {
            String::new()
        } else {
            format!("<w:rPr>{props}</w:rPr>")
        }
    }

    fn anchor_name(&mut self, anchor: ofdbox_folio_ir::AnchorId) -> String {
        if let Some(name) = self.anchors.get(&anchor.get()) {
            return name.clone();
        }
        let name = format!("_anchor{}", self.next_anchor);
        self.next_anchor += 1;
        self.anchors.insert(anchor.get(), name.clone());
        name
    }

    // ---- Tables ----

    fn table_xml(&mut self, table: &ofdbox_folio_ir::Table, out: &mut String) {
        let widths = self.column_widths(table);
        let total: i64 = widths.iter().sum();
        out.push_str(&format!(
            "<w:tbl><w:tblPr><w:tblW w:type=\"dxa\" w:w=\"{total}\"/>"
        ));
        out.push_str(&table_borders_xml(&table.borders));
        out.push_str(&margins_xml(&table.cell_padding));
        out.push_str("</w:tblPr><w:tblGrid>");
        for width in &widths {
            out.push_str(&format!("<w:gridCol w:w=\"{width}\"/>"));
        }
        out.push_str("</w:tblGrid>");
        for row in &table.rows {
            self.row_xml(row, out);
        }
        out.push_str("</w:tbl>");
    }

    /// Absolute width of each table column (twips): fixed columns keep their value and
    /// weighted columns share the body width.
    fn column_widths(&self, table: &ofdbox_folio_ir::Table) -> Vec<i64> {
        let mut widths = Vec::with_capacity(table.columns.len());
        let mut weights = 0_u32;
        let mut fixed = 0_i64;
        for column in &table.columns {
            match column.width {
                ColumnWidth::Twips(value) => {
                    let value = value.0.max(0);
                    fixed += value;
                    widths.push(value);
                }
                ColumnWidth::Weight(weight) => {
                    weights += u32::from(weight.max(1));
                    widths.push(0);
                }
                _ => {
                    weights += 1;
                    widths.push(0);
                }
            }
        }
        if weights > 0 {
            let remaining = (self.content_width - fixed).max(1);
            for (index, column) in table.columns.iter().enumerate() {
                let weight = match column.width {
                    ColumnWidth::Weight(weight) => u32::from(weight.max(1)),
                    ColumnWidth::Twips(_) => 0,
                    _ => 1,
                };
                if weight > 0 {
                    widths[index] = remaining * i64::from(weight) / i64::from(weights);
                }
            }
        }
        widths
    }

    fn row_xml(&mut self, row: &Row, out: &mut String) {
        out.push_str("<w:tr>");
        if row.is_header || row.height.is_some() {
            out.push_str("<w:trPr>");
            if row.is_header {
                out.push_str("<w:tblHeader/>");
            }
            if let Some(height) = row.height {
                out.push_str(&format!(
                    "<w:trHeight w:val=\"{}\" w:hRule=\"atLeast\"/>",
                    height.0
                ));
            }
            out.push_str("</w:trPr>");
        }
        for cell in &row.cells {
            self.cell_xml(cell, out);
        }
        out.push_str("</w:tr>");
    }

    fn cell_xml(&mut self, cell: &Cell, out: &mut String) {
        out.push_str("<w:tc><w:tcPr>");
        if cell.span > 1 {
            out.push_str(&format!("<w:gridSpan w:val=\"{}\"/>", cell.span));
        }
        match cell.v_merge {
            ofdbox_folio_ir::VMerge::Restart => out.push_str("<w:vMerge w:val=\"restart\"/>"),
            ofdbox_folio_ir::VMerge::Continue => out.push_str("<w:vMerge/>"),
            _ => {}
        }
        out.push_str("</w:tcPr>");
        let mut content = String::new();
        self.blocks_xml(&cell.blocks, &mut content);
        if content.is_empty()
            || !cell
                .blocks
                .last()
                .is_some_and(|block| matches!(block, Block::Paragraph(_)))
        {
            // OOXML requires a cell to end with a paragraph.
            content.push_str("<w:p/>");
        }
        out.push_str(&content);
        out.push_str("</w:tc>");
    }

    /// Build a `w:r` containing an inline or floating image.
    fn drawing_xml(
        &mut self,
        image: ofdbox_folio_ir::AssetId,
        size: Option<(Twips, Twips)>,
        alt: Option<&str>,
        anchor: Option<(f64, f64, WrapMode, (PositionFrom, PositionFrom))>,
    ) -> String {
        let Some((rid, name, extension)) = self.image_relationship(image) else {
            return String::new();
        };
        let (cx, cy) = self.image_extent(image, size);
        let doc_id = self.next_doc_pr;
        self.next_doc_pr += 1;
        let alt_text = esc(alt.unwrap_or(""));
        let picture = format!(
            "<a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:nvPicPr><pic:cNvPr id=\"{doc_id}\" name=\"{}\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"rId{rid}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic>",
            esc(&name)
        );
        let _ = extension;
        let drawing = match anchor {
            None => format!(
                "<w:drawing><wp:inline xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\"><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:docPr id=\"{doc_id}\" name=\"{}\" descr=\"{alt_text}\"/><wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" noChangeAspect=\"1\"/></wp:cNvGraphicFramePr>{picture}</wp:inline></w:drawing>",
                esc(&name)
            ),
            Some((x, y, wrap, from)) => {
                let wrap_xml = match wrap {
                    WrapMode::TopBottom => "<wp:wrapTopAndBottom/>".to_owned(),
                    WrapMode::Square => "<wp:wrapSquare wrapText=\"bothSides\"/>".to_owned(),
                    _ => "<wp:wrapNone/>".to_owned(),
                };
                let from_h = relative_from(from.0);
                let from_v = relative_from(from.1);
                format!(
                    "<w:drawing><wp:anchor xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" simplePos=\"0\" relativeHeight=\"{doc_id}\" behindDoc=\"1\" locked=\"0\" layoutInCell=\"1\" allowOverlap=\"1\"><wp:simplePos x=\"0\" y=\"0\"/><wp:positionH relativeFrom=\"{from_h}\"><wp:posOffset>{}</wp:posOffset></wp:positionH><wp:positionV relativeFrom=\"{from_v}\"><wp:posOffset>{}</wp:posOffset></wp:positionV><wp:extent cx=\"{cx}\" cy=\"{cy}\"/>{wrap_xml}<wp:docPr id=\"{doc_id}\" name=\"{}\" descr=\"{alt_text}\"/><wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" noChangeAspect=\"1\"/></wp:cNvGraphicFramePr>{picture}</wp:anchor></w:drawing>",
                    (x * 36000.0).round() as i64,
                    (y * 36000.0).round() as i64,
                    esc(&name)
                )
            }
        };
        format!("<w:r>{drawing}</w:r>")
    }

    /// Register an image asset and return (rId, name, extension).
    fn image_relationship(
        &mut self,
        asset: ofdbox_folio_ir::AssetId,
    ) -> Option<(u32, String, &'static str)> {
        if let Some((rid, path)) = self.images.get(&asset.get()) {
            let name = path.rsplit('/').next().unwrap_or(path).to_owned();
            return Some((*rid, name, ""));
        }
        let image = self.document.resources.image(asset)?;
        // With a mask, first try to bake the transparency into PNG; unsupported
        // combinations fall back to the base image and record a diagnostic.
        let (format, encoded) = match crate::image::composite_masked_png(image) {
            Some(png) => (ImageFormat::Png, Arc::from(png.as_slice())),
            None => {
                if image.mask.is_some() {
                    self.report.push(Diagnostic::warning(
                        DiagnosticCode::Image,
                        "Cannot write the image mask into DOCX (only a PNG base with a PNG mask is supported); wrote the base image",
                    ));
                }
                (image.format, Arc::clone(&image.encoded))
            }
        };
        let extension = extension_of(format);
        let index = self.media.len() + 1;
        let path = format!("word/media/image{index}.{extension}");
        let rid = self.next_rel_id();
        self.relationships.push(Relationship {
            id: rid,
            kind: "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image"
                .to_owned(),
            target: format!("media/image{index}.{extension}"),
            external: false,
        });
        let name = format!("image{index}.{extension}");
        self.media.push((path.clone(), encoded));
        self.images.insert(asset.get(), (rid, path));
        Some((rid, name, extension))
    }

    /// Display size of the image in the document (EMU).
    fn image_extent(
        &self,
        asset: ofdbox_folio_ir::AssetId,
        size: Option<(Twips, Twips)>,
    ) -> (i64, i64) {
        if let Some((width, height)) = size {
            return (
                (width.to_mm() * 36000.0).round() as i64,
                (height.to_mm() * 36000.0).round() as i64,
            );
        }
        let Some(image) = self.document.resources.image(asset) else {
            return (36000, 36000);
        };
        let (dpi_x, dpi_y) = image.dpi.unwrap_or((96.0, 96.0));
        (
            (f64::from(image.pixel_width) / dpi_x.max(1.0) * 914_400.0).round() as i64,
            (f64::from(image.pixel_height) / dpi_y.max(1.0) * 914_400.0).round() as i64,
        )
    }

    fn floating_xml(&mut self, floating: &ofdbox_folio_ir::FloatingObject, out: &mut String) {
        let (image, size, alt) = match floating.block.as_ref() {
            Block::Image(block) => (block.image, block.size, block.alt.as_deref()),
            _ => {
                // A floating block that is not an image degrades to an ordinary block.
                let mut inner = String::new();
                self.block_xml(&floating.block, &mut inner);
                out.push_str(&inner);
                return;
            }
        };
        let run = self.drawing_xml(
            image,
            size,
            alt,
            Some((
                floating.position.x,
                floating.position.y,
                floating.wrap,
                floating.position_from,
            )),
        );
        if run.is_empty() {
            return;
        }
        out.push_str("<w:p>");
        out.push_str(&run);
        out.push_str("</w:p>");
    }

    // ---- Table of contents ----

    fn toc_xml(&mut self, toc: &TocBlock, out: &mut String) {
        if let Some(title) = &toc.title
            && !title.trim().is_empty()
        {
            let mut paragraph = Paragraph::new(ParaStyle::default());
            paragraph
                .runs
                .push(Inline::Text(ofdbox_folio_ir::TextRun::new(
                    title.clone(),
                    toc.style.title_style.clone(),
                )));
            out.push_str(&self.paragraph_xml(&paragraph));
        }
        let instruction = format!(" TOC \\o \"1-{}\" \\h ", toc.max_level.max(1));
        out.push_str(&format!(
            "<w:p><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\">{}</w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>打开文档后更新域以生成目录</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>",
            esc(&instruction)
        ));
    }

    // ---- Notes ----

    /// Build `word/footnotes.xml` and `word/endnotes.xml` (each a separate part).
    fn notes_xml(&mut self) -> Result<(Option<String>, Option<String>), DocxError> {
        let has_footnote = self
            .document
            .notes
            .iter()
            .any(|note| note.kind == NoteKind::Footnote);
        let has_endnote = self
            .document
            .notes
            .iter()
            .any(|note| note.kind == NoteKind::Endnote);
        if !has_footnote && !has_endnote {
            return Ok((None, None));
        }
        let mut footnotes = has_footnote.then(|| {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><w:footnotes {NAMESPACES}><w:footnote w:type=\"separator\" w:id=\"-1\"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>"
            )
        });
        let mut endnotes = has_endnote.then(|| {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><w:endnotes {NAMESPACES}><w:endnote w:type=\"separator\" w:id=\"-1\"><w:p><w:r><w:separator/></w:r></w:p></w:endnote>"
            )
        });
        for note in &self.document.notes {
            let mut inner = String::new();
            self.blocks_xml(&note.blocks, &mut inner);
            if inner.is_empty() {
                inner.push_str("<w:p/>");
            }
            match note.kind {
                NoteKind::Footnote => {
                    if let Some(buffer) = &mut footnotes {
                        buffer.push_str(&format!(
                            "<w:footnote w:id=\"{}\">{inner}</w:footnote>",
                            note.id
                        ));
                    }
                }
                NoteKind::Endnote => {
                    if let Some(buffer) = &mut endnotes {
                        buffer.push_str(&format!(
                            "<w:endnote w:id=\"{}\">{inner}</w:endnote>",
                            note.id
                        ));
                    }
                }
            }
        }
        if let Some(buffer) = &mut footnotes {
            buffer.push_str("</w:footnotes>");
            let id = self.next_rel_id();
            self.relationships.push(Relationship {
                id,
                kind:
                    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes"
                        .to_owned(),
                target: "footnotes.xml".to_owned(),
                external: false,
            });
        }
        if let Some(buffer) = &mut endnotes {
            buffer.push_str("</w:endnotes>");
            let id = self.next_rel_id();
            self.relationships.push(Relationship {
                id,
                kind:
                    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes"
                        .to_owned(),
                target: "endnotes.xml".to_owned(),
                external: false,
            });
        }
        Ok((footnotes, endnotes))
    }

    // ---- Packaging ----

    fn package(mut self, notes: (Option<String>, Option<String>)) -> Result<Vec<u8>, DocxError> {
        let content_types = self.content_types_xml(&notes);
        let (footnotes, endnotes) = notes;
        let document_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><w:document {NAMESPACES_NODECL}><w:body>{}</w:body></w:document>",
            self.body
        );
        let styles_xml = self.styles_xml();
        let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/><Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties\" Target=\"docProps/app.xml\"/></Relationships>";
        let core = core_xml(&self.document.metadata);
        let app = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"><Application>{}</Application></Properties>",
            esc(&self.options.creator)
        );
        // Style relationship.
        let style_rel = self.next_rel_id();
        self.relationships.push(Relationship {
            id: style_rel,
            kind: "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
                .to_owned(),
            target: "styles.xml".to_owned(),
            external: false,
        });
        // Attachments: a manifest part plus the original file parts (DOCX has no
        // standard attachment concept; this is a convention of this library).
        let attachments: Vec<(String, String, Arc<[u8]>)> = self
            .document
            .attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| {
                let part = format!("word/attachments/{index}.bin");
                let name = attachment.info.name.clone();
                (part, name, Arc::clone(&attachment.bytes))
            })
            .collect();
        let attachments_manifest = if attachments.is_empty() {
            None
        } else {
            let mut manifest = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><attachments xmlns=\"urn:ofdbox:attachments\">",
            );
            for (part, name, bytes) in &attachments {
                manifest.push_str(&format!(
                    "<item part=\"{}\" name=\"{}\" size=\"{}\"/>",
                    esc(part),
                    esc(name),
                    bytes.len()
                ));
            }
            manifest.push_str("</attachments>");
            let id = self.next_rel_id();
            self.relationships.push(Relationship {
                id,
                kind: "urn:ofdbox:relationships/attachments".to_owned(),
                target: "attachments.xml".to_owned(),
                external: false,
            });
            Some(manifest)
        };
        let mut rels = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        );
        for relationship in &self.relationships {
            let mode = if relationship.external {
                " TargetMode=\"External\""
            } else {
                ""
            };
            rels.push_str(&format!(
                "<Relationship Id=\"rId{}\" Type=\"{}\" Target=\"{}\"{mode}/>",
                relationship.id,
                esc(&relationship.kind),
                esc(&relationship.target)
            ));
        }
        rels.push_str("</Relationships>");

        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let mut zip_options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        if self.options.deterministic {
            zip_options = zip_options.last_modified_time(zip::DateTime::default());
        }
        write_entry(
            &mut zip,
            "[Content_Types].xml",
            content_types.as_bytes(),
            zip_options,
        )?;
        write_entry(&mut zip, "_rels/.rels", root_rels.as_bytes(), zip_options)?;
        write_entry(
            &mut zip,
            "word/document.xml",
            document_xml.as_bytes(),
            zip_options,
        )?;
        write_entry(
            &mut zip,
            "word/_rels/document.xml.rels",
            rels.as_bytes(),
            zip_options,
        )?;
        write_entry(
            &mut zip,
            "word/styles.xml",
            styles_xml.as_bytes(),
            zip_options,
        )?;
        for area in &self.areas {
            write_entry(
                &mut zip,
                &format!("word/{}", area.name),
                area.xml.as_bytes(),
                zip_options,
            )?;
        }
        if let Some(xml) = &footnotes {
            write_entry(&mut zip, "word/footnotes.xml", xml.as_bytes(), zip_options)?;
        }
        if let Some(xml) = &endnotes {
            write_entry(&mut zip, "word/endnotes.xml", xml.as_bytes(), zip_options)?;
        }
        for (path, bytes) in &self.media {
            write_entry(&mut zip, path, bytes, zip_options)?;
        }
        if let Some(manifest) = &attachments_manifest {
            write_entry(
                &mut zip,
                "word/attachments.xml",
                manifest.as_bytes(),
                zip_options,
            )?;
            for (part, _, bytes) in &attachments {
                write_entry(&mut zip, part, bytes, zip_options)?;
            }
        }
        write_entry(&mut zip, "docProps/core.xml", core.as_bytes(), zip_options)?;
        write_entry(&mut zip, "docProps/app.xml", app.as_bytes(), zip_options)?;
        let cursor = zip
            .finish()
            .map_err(|error| DocxError::Io(error.to_string()))?;
        Ok(cursor.into_inner())
    }

    fn content_types_xml(&self, notes: &(Option<String>, Option<String>)) -> String {
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/>",
        );
        for (extension, mime) in [
            ("png", "image/png"),
            ("jpeg", "image/jpeg"),
            ("jpg", "image/jpeg"),
            ("bmp", "image/bmp"),
            ("tiff", "image/tiff"),
            ("bin", "application/octet-stream"),
        ] {
            xml.push_str(&format!(
                "<Default Extension=\"{extension}\" ContentType=\"{mime}\"/>"
            ));
        }
        let overrides = [
            (
                "/word/document.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
            ),
            (
                "/word/styles.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
            ),
            (
                "/docProps/core.xml",
                "application/vnd.openxmlformats-package.core-properties+xml",
            ),
            (
                "/docProps/app.xml",
                "application/vnd.openxmlformats-officedocument.extended-properties+xml",
            ),
        ];
        for (part, kind) in overrides {
            xml.push_str(&format!(
                "<Override PartName=\"{part}\" ContentType=\"{kind}\"/>"
            ));
        }
        for area in &self.areas {
            let kind = if area.kind == "header" {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"
            } else {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"
            };
            xml.push_str(&format!(
                "<Override PartName=\"/word/{}\" ContentType=\"{kind}\"/>",
                area.name
            ));
        }
        if notes.0.is_some() {
            xml.push_str("<Override PartName=\"/word/footnotes.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml\"/>");
        }
        if notes.1.is_some() {
            xml.push_str("<Override PartName=\"/word/endnotes.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml\"/>");
        }
        if !self.document.attachments.is_empty() {
            xml.push_str(
                "<Override PartName=\"/word/attachments.xml\" ContentType=\"application/vnd.ofdbox.attachments+xml\"/>",
            );
        }
        xml.push_str("</Types>");
        xml
    }

    fn styles_xml(&mut self) -> String {
        let styles = &self.document.styles;
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:docDefaults><w:rPrDefault>",
        );
        xml.push_str(&self.doc_default_run(&styles.default_character));
        xml.push_str("</w:rPrDefault><w:pPrDefault>");
        xml.push_str(&self.doc_default_para(&styles.default_paragraph));
        xml.push_str("</w:pPrDefault></w:docDefaults>");
        for named in &styles.paragraph {
            xml.push_str(&format!(
                "<w:style w:type=\"paragraph\" w:styleId=\"{}\"><w:name w:val=\"{}\"/>",
                esc(&named.id),
                esc(&named.name)
            ));
            if let Some(based) = &named.based_on {
                xml.push_str(&format!("<w:basedOn w:val=\"{}\"/>", esc(based)));
            }
            xml.push_str(&self.paragraph_props(&named.props));
            xml.push_str("</w:style>");
        }
        for named in &styles.character {
            xml.push_str(&format!(
                "<w:style w:type=\"character\" w:styleId=\"{}\"><w:name w:val=\"{}\"/>",
                esc(&named.id),
                esc(&named.name)
            ));
            if let Some(based) = &named.based_on {
                xml.push_str(&format!("<w:basedOn w:val=\"{}\"/>", esc(based)));
            }
            xml.push_str(&self.run_props(&named.props, None));
            xml.push_str("</w:style>");
        }
        xml.push_str("</w:styles>");
        xml
    }

    fn doc_default_run(&mut self, style: &TextStyle) -> String {
        format!(
            "<w:rPr>{}</w:rPr>",
            strip_wrapper(&self.run_props(style, None))
        )
    }

    fn doc_default_para(&mut self, style: &ParaStyle) -> String {
        format!(
            "<w:pPr>{}</w:pPr>",
            strip_wrapper(&self.paragraph_props(style))
        )
    }
}

/// OOXML namespaces (they must be declared on the element).
const NAMESPACES: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"";

/// Namespaces on `w:document` (`wp`/`a`/`pic` are declared in place on the drawing
/// elements).
const NAMESPACES_NODECL: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"";

fn strip_wrapper(xml: &str) -> &str {
    if let Some(inner) = xml
        .strip_prefix("<w:rPr>")
        .and_then(|rest| rest.strip_suffix("</w:rPr>"))
    {
        return inner;
    }
    if let Some(inner) = xml
        .strip_prefix("<w:pPr>")
        .and_then(|rest| rest.strip_suffix("</w:pPr>"))
    {
        return inner;
    }
    ""
}

fn flush_text(out: &mut String, text: &mut String) {
    if text.is_empty() {
        return;
    }
    out.push_str("<w:t xml:space=\"preserve\">");
    out.push_str(&esc(text));
    out.push_str("</w:t>");
    text.clear();
}

fn font_xml(family: &FontFamily) -> String {
    if family.name.is_empty() && family.fallback.is_empty() {
        return String::new();
    }
    let ascii = family.name.clone();
    let east_asia = family
        .fallback
        .first()
        .cloned()
        .unwrap_or_else(|| ascii.clone());
    format!(
        "<w:rFonts w:ascii=\"{}\" w:hAnsi=\"{}\" w:eastAsia=\"{}\"/>",
        esc(&ascii),
        esc(&ascii),
        esc(&east_asia)
    )
}

fn table_borders_xml(borders: &TableBorders) -> String {
    let mut xml = String::new();
    let mut inner = String::new();
    let mut push = |name: &str, border: Option<Border>| {
        let Some(border) = border else {
            return;
        };
        let value = match border.style {
            BorderStyle::Dashed => "dashed",
            BorderStyle::Dotted => "dotted",
            BorderStyle::Double => "double",
            _ => "single",
        };
        let size = (border.width.to_pt() * 8.0).round().max(1.0) as i64;
        inner.push_str(&format!(
            "<w:{name} w:val=\"{value}\" w:sz=\"{size}\" w:space=\"0\" w:color=\"{:02X}{:02X}{:02X}\"/>",
            border.color.r, border.color.g, border.color.b
        ));
    };
    push("top", borders.outer);
    push("left", borders.outer);
    push("bottom", borders.outer);
    push("right", borders.outer);
    push("insideH", borders.horizontal);
    push("insideV", borders.vertical);
    if !inner.is_empty() {
        xml.push_str(&format!("<w:tblBorders>{inner}</w:tblBorders>"));
    }
    xml
}

fn margins_xml(margins: &Margins) -> String {
    if margins.top.0 == 0 && margins.right.0 == 0 && margins.bottom.0 == 0 && margins.left.0 == 0 {
        return String::new();
    }
    format!(
        "<w:tblCellMar><w:top w:w=\"{}\" w:type=\"dxa\"/><w:left w:w=\"{}\" w:type=\"dxa\"/><w:bottom w:w=\"{}\" w:type=\"dxa\"/><w:right w:w=\"{}\" w:type=\"dxa\"/></w:tblCellMar>",
        margins.top.0, margins.left.0, margins.bottom.0, margins.right.0
    )
}

fn extension_of(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        _ => "bin",
    }
}

fn core_xml(metadata: &Metadata) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">",
    );
    if let Some(title) = &metadata.title {
        xml.push_str(&format!("<dc:title>{}</dc:title>", esc(title)));
    }
    if let Some(author) = &metadata.author {
        xml.push_str(&format!("<dc:creator>{}</dc:creator>", esc(author)));
    }
    if let Some(subject) = &metadata.subject {
        xml.push_str(&format!("<dc:subject>{}</dc:subject>", esc(subject)));
    }
    if !metadata.keywords.is_empty() {
        xml.push_str(&format!(
            "<cp:keywords>{}</cp:keywords>",
            esc(&metadata.keywords.join("; "))
        ));
    }
    xml.push_str("</cp:coreProperties>");
    xml
}

/// Values of the `relativeFrom` attribute.
fn relative_from(value: PositionFrom) -> &'static str {
    match value {
        PositionFrom::Page => "page",
        PositionFrom::Column => "column",
        PositionFrom::Paragraph => "paragraph",
        _ => "margin",
    }
}

/// XML escaping.
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn write_entry(
    zip: &mut zip::ZipWriter<Cursor<Vec<u8>>>,
    name: &str,
    bytes: &[u8],
    options: zip::write::SimpleFileOptions,
) -> Result<(), DocxError> {
    zip.start_file(name, options)
        .map_err(|error| DocxError::Io(error.to_string()))?;
    zip.write_all(bytes)
        .map_err(|error| DocxError::Io(error.to_string()))
}
