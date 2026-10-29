//! Reading of `document.xml`, headers/footers and core properties.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ofdbox_folio_ir::{
    Align, AnchorId, AssetId, Attachment, AttachmentId, AttachmentInfo, Block, Cell, Column,
    ColumnWidth, Diagnostic, DiagnosticCode, Field, FieldKind, FloatingObject, FlowDocument,
    HeaderFooter, ImageAsset, ImageBlock, Inline, InlineImage, Link, LinkTarget, Metadata, Note,
    NoteKind, NoteRef, Paragraph, Point, Row, Section, Table, TextRun, TextStyle, TocBlock, Twips,
    VMerge, WrapMode, detect_image_format, image_dimensions,
};

use crate::DocxError;
use crate::DocxReadOptions;
use crate::numbering::{Counters, Numbering};
use crate::package::{Package, Relation};
use crate::props::{
    RawParaProps, RawRunProps, RawSectionProps, RawTableProps, parse_para_props, parse_run_props,
    parse_section_props, parse_table_props,
};
use crate::styles::{Styles, heading_level_from_name, parse_theme};
use crate::xml::{
    XmlReader, attr, attr_f64, attr_i32, attr_u32, is_element, local_name, resolve_reference,
    skip_element,
};

/// Read a DOCX and produce Flow IR.
pub(crate) fn read(bytes: &[u8], options: &DocxReadOptions) -> Result<FlowDocument, DocxError> {
    Builder::new(bytes, options)?.run()
}

/// Result of parsing one `w:drawing`.
enum Drawing {
    /// Inline image.
    Inline(InlineImage),
    /// Floating image.
    Floating(FloatingObject),
}

/// State of the read process.
struct Builder<'a> {
    package: Package<'a>,
    styles: Styles,
    numbering: Numbering,
    relations: HashMap<String, Relation>,
    doc: FlowDocument,
    /// Body blocks of the current section.
    blocks: Vec<Block>,
    /// Properties of the current section (inherited by the following sections).
    section_props: RawSectionProps,
    /// Properties of every finished section, used at the end to fill in headers and
    /// footers.
    section_log: Vec<RawSectionProps>,
    counters: Counters,
    /// Mapping from bookmark name to anchor.
    anchors: HashMap<String, AnchorId>,
    /// Parsed images: relationship ID → asset.
    images: HashMap<String, Option<AssetId>>,
    /// Diagnostics already reported, so the same problem is not spammed.
    reported: HashSet<String>,
}

impl<'a> Builder<'a> {
    fn new(bytes: &'a [u8], options: &DocxReadOptions) -> Result<Self, DocxError> {
        let mut package = Package::open(bytes, options)?;
        let Some(document_xml) = package.read_text("word/document.xml")? else {
            return Err(DocxError::Structure(
                "missing word/document.xml, not a DOCX document".to_owned(),
            ));
        };
        let relations = package.relations("word/document.xml")?;
        let styles_target =
            target_by_kind(&relations, "/styles").unwrap_or_else(|| "word/styles.xml".to_owned());
        let theme_target = target_by_kind(&relations, "/theme")
            .unwrap_or_else(|| "word/theme/theme1.xml".to_owned());
        let numbering_target = target_by_kind(&relations, "/numbering")
            .unwrap_or_else(|| "word/numbering.xml".to_owned());

        let theme = match package.read_text(&theme_target)? {
            Some(text) => parse_theme(&text),
            None => Default::default(),
        };
        let styles = match package.read_text(&styles_target)? {
            Some(text) => Styles::parse(&text, theme)?,
            None => Styles::default(),
        };
        let numbering = match package.read_text(&numbering_target)? {
            Some(text) => Numbering::parse(&text),
            None => Numbering::default(),
        };

        let mut metadata = match package.read_text("docProps/core.xml")? {
            Some(text) => parse_core(&text),
            None => Metadata::default(),
        };
        if let Some(text) = package.read_text("docProps/app.xml")?
            && let Some(application) = find_element_text(&text, "Application")
        {
            metadata.creator = Some(application);
        }

        let anchors = collect_bookmarks(&document_xml);
        let mut doc = FlowDocument::new(metadata);
        doc.sections.clear();

        Ok(Self {
            package,
            styles,
            numbering,
            relations,
            doc,
            blocks: Vec::new(),
            section_props: RawSectionProps::default(),
            section_log: Vec::new(),
            counters: Counters::default(),
            anchors,
            images: HashMap::new(),
            reported: HashSet::new(),
        })
    }

    fn run(mut self) -> Result<FlowDocument, DocxError> {
        let document_xml = self
            .package
            .read_text("word/document.xml")?
            .ok_or_else(|| DocxError::Structure("missing word/document.xml".to_owned()))?;
        {
            let mut reader = XmlReader::from_str(&document_xml);
            let blocks = self.parse_blocks(&mut reader, Some("body"), true)?;
            self.blocks.extend(blocks);
        }
        self.finish_section();
        self.fill_headers_footers()?;
        self.parse_notes(NoteKind::Footnote)?;
        self.parse_notes(NoteKind::Endnote)?;
        self.parse_attachments()?;
        Ok(self.doc)
    }

    // ---- Block level ----

    /// Read the block sequence up to the matching `end`; `top` means this is the section
    /// body level.
    fn parse_blocks(
        &mut self,
        reader: &mut XmlReader<'_>,
        end: Option<&str>,
        top: bool,
    ) -> Result<Vec<Block>, DocxError> {
        let mut blocks = Vec::new();
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                    "p" => {
                        let (mut produced, section) = self.parse_paragraph(reader)?;
                        blocks.append(&mut produced);
                        if top && let Some(section) = section {
                            self.section_props.merge(&section);
                            self.finish_section();
                        }
                    }
                    "tbl" => {
                        let table = self.parse_table(reader)?;
                        blocks.push(Block::Table(table));
                    }
                    "sectPr" if top => {
                        let props = parse_section_props(reader, &element)?;
                        self.section_props.merge(&props);
                    }
                    "sdt" => {
                        let mut produced = self.parse_sdt(reader)?;
                        blocks.append(&mut produced);
                    }
                    // Container elements themselves (`w:body`, `w:hdr`, `w:ftr`, etc.)
                    // carry no block content, so ignore them; the child elements are
                    // still handled by this loop.
                    _ => {}
                },
                quick_xml::events::Event::Empty(element) => {
                    if top && is_element(&element, "sectPr") {
                        let props = parse_section_props(reader, &element)?;
                        self.section_props.merge(&props);
                    }
                }
                quick_xml::events::Event::End(element) => {
                    let name = element.local_name().as_ref().to_owned();
                    if end == Some(name.as_str()) {
                        break;
                    }
                }
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        Ok(blocks)
    }

    /// Finish the current section; when it has no content and a section already exists,
    /// do not manufacture an extra empty section.
    fn finish_section(&mut self) {
        if self.blocks.is_empty() && !self.doc.sections.is_empty() {
            return;
        }
        let mut section = Section::new(self.section_props.to_setup());
        section.blocks = std::mem::take(&mut self.blocks);
        self.doc.sections.push(section);
        self.section_log.push(self.section_props.clone());
    }

    /// Parse a paragraph and return the blocks it produces (including in-paragraph page
    /// breaks) plus the section properties at its end.
    fn parse_paragraph(
        &mut self,
        reader: &mut XmlReader<'_>,
    ) -> Result<(Vec<Block>, Option<RawSectionProps>), DocxError> {
        let mut props = RawParaProps::default();
        // Resolve the style as soon as `pPr` is parsed: a run's character properties must
        // inherit the rPr of the paragraph style chain.
        let mut resolved: Option<RawParaProps> = None;
        let mut sink = InlineSink::default();
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                    "pPr" => {
                        props = parse_para_props(reader, &element)?;
                        resolved = Some(self.styles.resolve_para(&props));
                    }
                    _ => {
                        let para = resolved.as_ref().unwrap_or(&props);
                        self.parse_inlines(reader, &element, para, &mut sink)?;
                    }
                },
                quick_xml::events::Event::Empty(element) => {
                    if is_element(&element, "bookmarkStart") {
                        self.note_bookmark(&element, &mut sink);
                    }
                }
                quick_xml::events::Event::End(_) => break,
                quick_xml::events::Event::Eof => {
                    return Err(DocxError::Structure("paragraph has no end tag".to_owned()));
                }
                _ => {}
            }
        }

        let resolved = resolved.unwrap_or_else(|| self.styles.resolve_para(&props));
        let mut style = resolved.to_style();
        let mut marker = None;
        if let Some((num_id, level)) = resolved.numbering {
            marker = self.counters.next(&self.numbering, num_id, level);
            if let Some(definition) = self.numbering.level(num_id, level) {
                if let Some(left) = definition.left {
                    style.left_indent = Twips::new(left);
                }
                if let Some(hanging) = definition.hanging {
                    style.first_line_indent = Twips::new(-hanging);
                }
            }
        }
        let heading_level = self.heading_level(&resolved, &props);
        let marker_style = marker.as_ref().map(|_| {
            self.styles
                .resolve_run(&resolved, &RawRunProps::default())
                .to_style(&self.styles.theme)
        });

        let mut blocks = Vec::new();
        if resolved.page_break_before.unwrap_or(false) {
            blocks.push(Block::PageBreak);
        }
        // Floating objects inside the paragraph leave the text flow, so output them as
        // block-level elements first (each object carries its own position).
        for floating in std::mem::take(&mut sink.floatings) {
            blocks.push(Block::Floating(Box::new(floating)));
        }
        let mut segments: Vec<&[Inline]> = Vec::new();
        let mut segment_start = 0_usize;
        for boundary in sink.breaks.iter().copied() {
            segments.push(&sink.runs[segment_start..boundary]);
            segment_start = boundary;
        }
        segments.push(&sink.runs[segment_start..]);

        let mut pushed = false;
        for (index, segment) in segments.iter().enumerate() {
            if index > 0 {
                blocks.push(Block::PageBreak);
            }
            let mut runs = segment.to_vec();
            if index == 0
                && let (Some(marker), Some(marker_style)) = (&marker, &marker_style)
            {
                runs.insert(
                    0,
                    Inline::Text(TextRun::new(format!("{marker} "), marker_style.clone())),
                );
            }
            // Keep an empty paragraph only when the whole segment has no content, so an
            // in-paragraph page break does not conjure up a spurious blank line.
            if !runs.is_empty() || (!pushed && segments.len() == 1) {
                blocks.push(Block::Paragraph(Paragraph {
                    style: style.clone(),
                    runs,
                    heading_level,
                    anchor: sink.anchor,
                }));
                pushed = true;
            }
        }
        Ok((blocks, resolved.section))
    }

    fn heading_level(&self, resolved: &RawParaProps, props: &RawParaProps) -> Option<u8> {
        if let Some(level) = resolved.outline_level {
            return if level < 9 { Some(level + 1) } else { None };
        }
        let id = props.style_id.as_deref()?;
        let name = self.styles.paragraph_name(id)?;
        heading_level_from_name(name)
    }

    // ---- Inline level ----

    fn parse_inlines(
        &mut self,
        reader: &mut XmlReader<'_>,
        start: &quick_xml::events::BytesStart<'_>,
        para: &RawParaProps,
        sink: &mut InlineSink,
    ) -> Result<(), DocxError> {
        match local_name(start).as_str() {
            "r" => self.parse_run(reader, para, sink)?,
            "hyperlink" => self.parse_hyperlink(reader, start, para, sink)?,
            "fldSimple" => self.parse_simple_field(reader, start, para, sink)?,
            "sdt" => self.parse_sdt_inline(reader, para, sink)?,
            "ins" | "smartTag" | "customXml" | "moveTo" => {
                self.parse_inline_container(reader, para, sink)?;
            }
            "del" | "moveFrom" => skip_element(reader, start)?,
            "bookmarkStart" => {
                self.note_bookmark(start, sink);
                skip_element(reader, start)?;
            }
            _ => skip_element(reader, start)?,
        }
        Ok(())
    }

    /// Read the inline content of a container up to the matching `End`.
    fn parse_inline_container(
        &mut self,
        reader: &mut XmlReader<'_>,
        para: &RawParaProps,
        sink: &mut InlineSink,
    ) -> Result<(), DocxError> {
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => {
                    self.parse_inlines(reader, &element, para, sink)?;
                }
                quick_xml::events::Event::Empty(element) => {
                    if is_element(&element, "bookmarkStart") {
                        self.note_bookmark(&element, sink);
                    }
                }
                quick_xml::events::Event::End(_) => break,
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        Ok(())
    }

    fn parse_run(
        &mut self,
        reader: &mut XmlReader<'_>,
        para: &RawParaProps,
        sink: &mut InlineSink,
    ) -> Result<(), DocxError> {
        let mut props = RawRunProps::default();
        let mut text = String::new();
        let mut page_break = false;
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                    "rPr" => props = parse_run_props(reader, &element)?,
                    "t" => text.push_str(&self.read_text(reader)?),
                    "instrText" => {
                        let instr = self.read_text(reader)?;
                        if let Some(field) = &mut sink.field {
                            field.instr.push_str(&instr);
                        }
                    }
                    "delText" => {
                        let _ = self.read_text(reader)?;
                    }
                    "drawing" => match self.parse_drawing(reader)? {
                        Some(Drawing::Inline(image)) => sink.push_inline(Inline::Image(image)),
                        Some(Drawing::Floating(object)) => sink.floatings.push(object),
                        None => {}
                    },
                    "pict" => {
                        self.warn_once(
                            "pict",
                            DiagnosticCode::UnsupportedObject,
                            "VML graphics (w:pict) are not supported yet, skipped",
                        );
                        skip_element(reader, &element)?;
                    }
                    "fldChar" => {
                        self.apply_field_char(&element, sink);
                        skip_element(reader, &element)?;
                    }
                    "footnoteReference" | "endnoteReference" => {
                        self.push_note_reference(&element, sink);
                        skip_element(reader, &element)?;
                    }
                    "commentReference" => {
                        self.warn_once(
                            "commentReference",
                            DiagnosticCode::Unsupported,
                            "Comment references are not supported yet, ignored",
                        );
                        skip_element(reader, &element)?;
                    }
                    "sym" => {
                        self.warn_once(
                            "sym",
                            DiagnosticCode::Unsupported,
                            "Symbol fonts (w:sym) are not supported yet, skipped",
                        );
                        skip_element(reader, &element)?;
                    }
                    _ => skip_element(reader, &element)?,
                },
                quick_xml::events::Event::Empty(element) => match local_name(&element).as_str() {
                    "br" => {
                        if attr(&element, "type").as_deref() == Some("page") {
                            page_break = true;
                        } else {
                            text.push('\n');
                        }
                    }
                    "tab" => text.push('\t'),
                    "fldChar" => self.apply_field_char(&element, sink),
                    "noBreakHyphen" => text.push('-'),
                    "softHyphen" | "lastRenderedPageBreak" => {}
                    "sym" => {
                        self.warn_once(
                            "sym",
                            DiagnosticCode::Unsupported,
                            "Symbol fonts (w:sym) are not supported yet, skipped",
                        );
                    }
                    "footnoteReference" | "endnoteReference" => {
                        self.push_note_reference(&element, sink);
                    }
                    "commentReference" => {
                        self.warn_once(
                            "commentReference",
                            DiagnosticCode::Unsupported,
                            "Comment references are not supported yet, ignored",
                        );
                    }
                    _ => {}
                },
                quick_xml::events::Event::GeneralRef(reference) => {
                    if let Some(character) = resolve_reference(&reference) {
                        text.push(character);
                    }
                }
                quick_xml::events::Event::End(_) => break,
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }

        if !text.is_empty() {
            let style = self
                .styles
                .resolve_run(para, &props)
                .to_style(&self.styles.theme);
            sink.push_text(&style, &text);
        }
        if page_break {
            sink.push_break();
        }
        Ok(())
    }

    fn read_text(&mut self, reader: &mut XmlReader<'_>) -> Result<String, DocxError> {
        let mut text = String::new();
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Text(value) => text.push_str(&value),
                quick_xml::events::Event::CData(value) => text.push_str(&value),
                quick_xml::events::Event::GeneralRef(reference) => {
                    if let Some(character) = resolve_reference(&reference) {
                        text.push(character);
                    }
                }
                quick_xml::events::Event::End(_) => break,
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        Ok(text)
    }
    /// Read attachments: the word/attachments.xml manifest plus the original file parts
    /// (a convention of this library).
    fn parse_attachments(&mut self) -> Result<(), DocxError> {
        let Some(target) = target_by_kind(&self.relations, "urn:ofdbox:relationships/attachments")
        else {
            return Ok(());
        };
        let Some(text) = self.package.read_text(&target)? else {
            return Ok(());
        };
        let mut reader = XmlReader::from_str(&text);
        let mut index = 0_u64;
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element)
                | quick_xml::events::Event::Empty(element) => {
                    if local_name(&element) != "item" {
                        continue;
                    }
                    let Some(part) = attr(&element, "part") else {
                        continue;
                    };
                    let name = attr(&element, "name").unwrap_or_default();
                    let Some(bytes) = self.package.read(&part)? else {
                        continue;
                    };
                    index += 1;
                    self.doc.attachments.push(Attachment {
                        info: AttachmentInfo {
                            id: AttachmentId::new(index),
                            name,
                            format: None,
                            size: bytes.len() as u64,
                            created: None,
                        },
                        bytes: Arc::from(bytes.as_slice()),
                    });
                }
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        Ok(())
    }

    /// Read note bodies from word/footnotes.xml or word/endnotes.xml.
    fn parse_notes(&mut self, kind: NoteKind) -> Result<(), DocxError> {
        let suffix = match kind {
            NoteKind::Footnote => "/footnotes",
            NoteKind::Endnote => "/endnotes",
        };
        let Some(target) = target_by_kind(&self.relations, suffix) else {
            return Ok(());
        };
        let Some(text) = self.package.read_text(&target)? else {
            return Ok(());
        };
        let mut reader = XmlReader::from_str(&text);
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => {
                    let name = local_name(&element);
                    // The top-level w:footnotes/w:endnotes container carries no content
                    // itself, so ignore it.
                    if name != "footnote" && name != "endnote" {
                        continue;
                    }
                    let id = attr_u32(&element, "id");
                    // Pseudo-notes such as separator / continuationNotice have no body.
                    let note_type = attr(&element, "type").unwrap_or_default();
                    let blocks = self.parse_blocks(&mut reader, Some(&name), false)?;
                    if let (Some(id), true) = (id, note_type.is_empty())
                        && !blocks.is_empty()
                    {
                        self.doc.notes.push(Note::new(kind, id, blocks));
                    }
                }
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        Ok(())
    }

    /// Record w:footnoteReference / w:endnoteReference as a note reference.
    fn push_note_reference(
        &mut self,
        element: &quick_xml::events::BytesStart<'_>,
        sink: &mut InlineSink,
    ) {
        let kind = if is_element(element, "endnoteReference") {
            NoteKind::Endnote
        } else {
            NoteKind::Footnote
        };
        let Some(id) = attr_u32(element, "id") else {
            return;
        };
        // Word uses -1/0 for separator notes, which the body never references.
        sink.push_inline(Inline::Note(NoteRef::new(kind, id)));
    }

    /// Handle the begin / separate / end of a complex field.
    fn apply_field_char(
        &mut self,
        element: &quick_xml::events::BytesStart<'_>,
        sink: &mut InlineSink,
    ) {
        let Some(kind) = attr(element, "fldCharType") else {
            return;
        };
        match kind.as_str() {
            "begin" => sink.field = Some(FieldState::default()),
            "separate" => {
                if let Some(field) = &mut sink.field {
                    field.separated = true;
                }
            }
            "end" => {
                let Some(field) = sink.field.take() else {
                    return;
                };
                match field_kind(&field.instr) {
                    Some(kind) => {
                        let cached = runs_text(&field.result);
                        let mut inline = Field::new(kind);
                        if !cached.is_empty() {
                            inline.cached_text = Some(cached);
                        }
                        sink.runs.push(Inline::Field(inline));
                    }
                    None => sink.runs.extend(field.result),
                }
            }
            _ => {}
        }
    }

    fn note_bookmark(&self, element: &quick_xml::events::BytesStart<'_>, sink: &mut InlineSink) {
        if sink.anchor.is_some() {
            return;
        }
        if let Some(name) = attr(element, "name") {
            sink.anchor = self.anchors.get(&name).copied();
        }
    }

    fn parse_hyperlink(
        &mut self,
        reader: &mut XmlReader<'_>,
        start: &quick_xml::events::BytesStart<'_>,
        para: &RawParaProps,
        sink: &mut InlineSink,
    ) -> Result<(), DocxError> {
        let rid = attr(start, "id");
        let anchor_name = attr(start, "anchor");
        let mut inner = InlineSink::default();
        self.parse_inline_container(reader, para, &mut inner)?;
        let target = match (rid, anchor_name) {
            (Some(rid), _) => self
                .relations
                .get(&rid)
                .filter(|relation| relation.external)
                .map(|relation| LinkTarget::Uri(relation.target.clone())),
            (None, Some(name)) => self.anchors.get(&name).map(|id| LinkTarget::Anchor(*id)),
            _ => None,
        };
        match target {
            Some(target) => sink.push_inline(Inline::Link(Link::new(target, inner.runs))),
            None => {
                for inline in inner.runs {
                    sink.push_inline(inline);
                }
            }
        }
        Ok(())
    }

    /// Read `w:fldSimple`: the instruction is in an attribute and the content is the
    /// cached result.
    fn parse_simple_field(
        &mut self,
        reader: &mut XmlReader<'_>,
        start: &quick_xml::events::BytesStart<'_>,
        para: &RawParaProps,
        sink: &mut InlineSink,
    ) -> Result<(), DocxError> {
        let instr = attr(start, "instr").unwrap_or_default();
        let mut inner = InlineSink::default();
        self.parse_inline_container(reader, para, &mut inner)?;
        match field_kind(&instr) {
            Some(kind) => {
                let cached = runs_text(&inner.runs);
                let mut field = Field::new(kind);
                if !cached.is_empty() {
                    field.cached_text = Some(cached);
                }
                sink.push_inline(Inline::Field(field));
            }
            None => {
                for inline in inner.runs {
                    sink.push_inline(inline);
                }
            }
        }
        Ok(())
    }

    /// Read a content control; a table-of-contents content control is mapped wholesale
    /// to an automatic TOC block.
    fn parse_sdt(&mut self, reader: &mut XmlReader<'_>) -> Result<Vec<Block>, DocxError> {
        let mut gallery = None;
        let mut blocks = Vec::new();
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                    "sdtPr" => gallery = parse_sdt_gallery(reader)?,
                    "sdtContent" => {
                        blocks = self.parse_blocks(reader, Some("sdtContent"), false)?
                    }
                    _ => skip_element(reader, &element)?,
                },
                quick_xml::events::Event::End(_) => break,
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        if let Some(gallery) = gallery
            && is_toc_gallery(&gallery)
        {
            // The text inside a TOC control is the title (such as "Contents"); the
            // entries are recomputed by layout from the headings.
            let title = blocks.iter().find_map(|block| match block {
                Block::Paragraph(paragraph) => {
                    let text = runs_text(&paragraph.runs);
                    if text.trim().is_empty() {
                        None
                    } else {
                        Some(text.trim().to_owned())
                    }
                }
                _ => None,
            });
            let toc = TocBlock {
                title,
                ..TocBlock::default()
            };
            return Ok(vec![Block::Toc(toc)]);
        }
        Ok(blocks)
    }

    fn parse_sdt_inline(
        &mut self,
        reader: &mut XmlReader<'_>,
        para: &RawParaProps,
        sink: &mut InlineSink,
    ) -> Result<(), DocxError> {
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                    "sdtContent" => self.parse_inline_container(reader, para, sink)?,
                    _ => skip_element(reader, &element)?,
                },
                quick_xml::events::Event::End(_) => break,
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        Ok(())
    }

    // ---- Tables ----

    fn parse_table(&mut self, reader: &mut XmlReader<'_>) -> Result<Table, DocxError> {
        let mut props = RawTableProps::default();
        let mut columns = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                    "tblPr" => props = parse_table_props(reader, &element)?,
                    "tblGrid" => columns = self.parse_grid(reader)?,
                    "tr" => rows.push(self.parse_row(reader)?),
                    _ => skip_element(reader, &element)?,
                },
                quick_xml::events::Event::End(_) => break,
                quick_xml::events::Event::Eof => {
                    return Err(DocxError::Structure("table has no end tag".to_owned()));
                }
                _ => {}
            }
        }
        let resolved = self.styles.resolve_table(&props);
        let header_rows = rows.iter().take_while(|row| row.is_header).count() as u32;
        if columns.is_empty() {
            // Without a grid, divide the width evenly by the largest cell count.
            let count = rows.iter().map(|row| row.cells.len()).max().unwrap_or(0);
            columns = (0..count)
                .map(|_| Column {
                    width: ColumnWidth::Weight(1),
                })
                .collect();
        }
        let mut table = Table::new(columns);
        table.rows = rows;
        table.header_rows = header_rows;
        table.borders = resolved.borders();
        table.cell_padding = resolved.cell_margins.unwrap_or_default();
        Ok(table)
    }

    fn parse_grid(&mut self, reader: &mut XmlReader<'_>) -> Result<Vec<Column>, DocxError> {
        let mut columns = Vec::new();
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => skip_element(reader, &element)?,
                quick_xml::events::Event::Empty(element) => {
                    if is_element(&element, "gridCol")
                        && let Some(width) = attr_f64(&element, "w")
                    {
                        columns.push(Column {
                            width: ColumnWidth::Twips(Twips::new(width.round() as i64)),
                        });
                    }
                }
                quick_xml::events::Event::End(_) => break,
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        Ok(columns)
    }

    fn parse_row(&mut self, reader: &mut XmlReader<'_>) -> Result<Row, DocxError> {
        let mut cells = Vec::new();
        let mut is_header = false;
        let mut height = None;
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                    "trPr" => {
                        let (header, row_height) = parse_row_props(reader)?;
                        is_header = header;
                        height = row_height;
                    }
                    "tc" => {
                        if let Some(cell) = self.parse_cell(reader)? {
                            cells.push(cell);
                        }
                    }
                    _ => skip_element(reader, &element)?,
                },
                quick_xml::events::Event::End(_) => break,
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        let mut row = Row::new(cells);
        row.is_header = is_header;
        row.height = height;
        Ok(row)
    }

    fn parse_cell(&mut self, reader: &mut XmlReader<'_>) -> Result<Option<Cell>, DocxError> {
        let mut span = 1_u32;
        let mut v_merge = VMerge::None;
        let mut blocks = Vec::new();
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                    "tcPr" => {
                        let (cell_span, cell_merge) = parse_cell_props(reader)?;
                        span = cell_span;
                        v_merge = cell_merge;
                    }
                    "p" => {
                        let (mut produced, _) = self.parse_paragraph(reader)?;
                        blocks.append(&mut produced);
                    }
                    "tbl" => {
                        let table = self.parse_table(reader)?;
                        blocks.push(Block::Table(table));
                    }
                    "sdt" => {
                        let mut produced = self.parse_sdt(reader)?;
                        blocks.append(&mut produced);
                    }
                    _ => skip_element(reader, &element)?,
                },
                quick_xml::events::Event::End(_) => break,
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        if span > 1 {
            self.warn_once(
                "grid_span",
                DiagnosticCode::Approximation,
                "Merged cells (gridSpan) are not supported yet; treating them as ordinary cells",
            );
        }
        if v_merge == VMerge::Continue {
            // A vertically merged continuation cell stays empty and is covered by the
            // start cell above.
            return Ok(Some(
                Cell::new(Vec::new())
                    .with_span(span)
                    .with_v_merge(VMerge::Continue),
            ));
        }
        Ok(Some(
            Cell::new(blocks).with_span(span).with_v_merge(v_merge),
        ))
    }

    // ---- Images and floating objects ----

    /// Parse `w:drawing`: for inline returns `wp:inline`, for floating returns the
    /// position and wrap of `wp:anchor`.
    fn parse_drawing(&mut self, reader: &mut XmlReader<'_>) -> Result<Option<Drawing>, DocxError> {
        let mut extent: Option<(f64, f64)> = None;
        let mut embed = None;
        let mut alt = None;
        let mut anchor = false;
        let mut wrap = WrapMode::None;
        let mut offset_x: Option<f64> = None;
        let mut offset_y: Option<f64> = None;
        // Whether we are inside positionH / positionV, used to tell the two directions'
        // posOffset apart.
        let mut horizontal = false;
        let mut vertical = false;
        let mut from_h = ofdbox_folio_ir::PositionFrom::Margin;
        let mut from_v = ofdbox_folio_ir::PositionFrom::Margin;

        let mut depth = 0_usize;
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => {
                    match local_name(&element).as_str() {
                        "anchor" => anchor = true,
                        "positionH" => {
                            horizontal = true;
                            from_h = position_from(attr(&element, "relativeFrom").as_deref());
                        }
                        "positionV" => {
                            vertical = true;
                            from_v = position_from(attr(&element, "relativeFrom").as_deref());
                        }
                        "wrapSquare" => wrap = WrapMode::Square,
                        "wrapTopAndBottom" => wrap = WrapMode::TopBottom,
                        "wrapNone" => wrap = WrapMode::None,
                        "posOffset" => {
                            // The value of `wp:posOffset` is element text, not an
                            // attribute.
                            let text = self.read_text(reader)?;
                            if let Ok(value) = text.trim().parse::<f64>() {
                                if horizontal {
                                    offset_x = Some(value);
                                } else if vertical {
                                    offset_y = Some(value);
                                }
                            }
                            continue;
                        }
                        _ => {}
                    }
                    depth += 1;
                }
                quick_xml::events::Event::Empty(element) => match local_name(&element).as_str() {
                    "extent" => {
                        if let (Some(cx), Some(cy)) =
                            (attr_f64(&element, "cx"), attr_f64(&element, "cy"))
                        {
                            extent = Some((cx, cy));
                        }
                    }
                    "blip" => embed = attr(&element, "embed"),
                    "docPr" => {
                        alt = attr(&element, "descr")
                            .filter(|value| !value.trim().is_empty())
                            .or_else(|| attr(&element, "name"));
                    }
                    "posOffset" => {
                        if let Some(value) = attr_f64(&element, "body") {
                            if horizontal {
                                offset_x = Some(value);
                            } else if vertical {
                                offset_y = Some(value);
                            }
                        }
                    }
                    "wrapSquare" => wrap = WrapMode::Square,
                    "wrapTopAndBottom" => wrap = WrapMode::TopBottom,
                    "wrapNone" => wrap = WrapMode::None,
                    _ => {}
                },
                quick_xml::events::Event::End(element) => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    match element.local_name().as_ref() {
                        "positionH" => horizontal = false,
                        "positionV" => vertical = false,
                        _ => {}
                    }
                }
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }

        let Some(rid) = embed else {
            return Ok(None);
        };
        let Some(asset) = self.image_asset(&rid)? else {
            return Ok(None);
        };
        let size = extent.map(|(cx, cy)| (emu_to_twips(cx), emu_to_twips(cy)));
        if !anchor {
            let mut image = InlineImage::new(asset);
            image.size = size;
            image.alt = alt;
            return Ok(Some(Drawing::Inline(image)));
        }

        // Floating image: convert the position into millimetres relative to the top-left
        // corner of the text area.
        let mut image = ImageBlock::new(asset);
        image.size = size;
        image.alt = alt;
        image.align = Align::Left;
        let position = Point::new(
            offset_x.map(|value| value / 36000.0).unwrap_or(0.0),
            offset_y.map(|value| value / 36000.0).unwrap_or(0.0),
        );
        let mut object = FloatingObject::new(position, Block::Image(image));
        object.position_from = (from_h, from_v);
        object.width = size.map(|(width, _)| width);
        object.wrap = wrap;
        Ok(Some(Drawing::Floating(object)))
    }

    fn image_asset(&mut self, rid: &str) -> Result<Option<AssetId>, DocxError> {
        if let Some(cached) = self.images.get(rid) {
            return Ok(*cached);
        }
        let Some(relation) = self.relations.get(rid).cloned() else {
            self.images.insert(rid.to_owned(), None);
            return Ok(None);
        };
        let Some(bytes) = self.package.read(&relation.target)? else {
            self.images.insert(rid.to_owned(), None);
            return Ok(None);
        };
        let Some(format) = detect_image_format(&bytes) else {
            self.warn_once(
                &format!("image-format-{}", relation.target),
                DiagnosticCode::UnsupportedObject,
                format!("unsupported image format, skipped: {}", relation.target),
            );
            self.images.insert(rid.to_owned(), None);
            return Ok(None);
        };
        let Some((width, height)) = image_dimensions(&bytes) else {
            self.warn_once(
                &format!("image-size-{}", relation.target),
                DiagnosticCode::Image,
                format!("cannot read image dimensions, skipped: {}", relation.target),
            );
            self.images.insert(rid.to_owned(), None);
            return Ok(None);
        };
        let asset = ImageAsset::new(format, Arc::from(bytes.as_slice()), width, height);
        let id = self.doc.resources.add_image(asset);
        self.images.insert(rid.to_owned(), Some(id));
        Ok(Some(id))
    }

    // ---- Headers and footers ----

    fn fill_headers_footers(&mut self) -> Result<(), DocxError> {
        let mut requests: Vec<(usize, bool, String, String)> = Vec::new();
        for (index, props) in self.section_log.iter().enumerate() {
            for (slot, rid) in &props.headers {
                requests.push((index, true, slot.clone(), rid.clone()));
            }
            for (slot, rid) in &props.footers {
                requests.push((index, false, slot.clone(), rid.clone()));
            }
        }
        for (index, header, slot, rid) in requests {
            let Some(target) = self
                .relations
                .get(&rid)
                .map(|relation| relation.target.clone())
            else {
                continue;
            };
            let Some(text) = self.package.read_text(&target)? else {
                continue;
            };
            let mut reader = XmlReader::from_str(&text);
            let blocks = self.parse_blocks(&mut reader, None, false)?;
            let Some(section) = self.doc.sections.get_mut(index) else {
                continue;
            };
            let set = if header {
                &mut section.headers
            } else {
                &mut section.footers
            };
            let value = Some(HeaderFooter { blocks });
            match slot.as_str() {
                "first" => set.first = value,
                "even" => set.even = value,
                _ => set.default = value,
            }
        }
        Ok(())
    }

    /// Report the same kind of diagnostic only once.
    fn warn_once(&mut self, key: &str, code: DiagnosticCode, message: impl Into<String>) {
        if !self.reported.insert(key.to_owned()) {
            return;
        }
        self.doc.report.push(Diagnostic::warning(code, message));
    }
}

/// Inline collector.
#[derive(Default)]
struct InlineSink {
    /// Completed runs.
    runs: Vec<Inline>,
    /// Positions of page breaks within `runs`.
    breaks: Vec<usize>,
    /// Paragraph anchor.
    anchor: Option<AnchorId>,
    /// The complex field in progress.
    field: Option<FieldState>,
    /// Floating objects appearing in the paragraph, laid out separately as block-level
    /// elements.
    floatings: Vec<FloatingObject>,
}

/// The complex field in progress.
#[derive(Default)]
struct FieldState {
    /// Field instruction.
    instr: String,
    /// Whether the result part has been entered.
    separated: bool,
    /// Result runs.
    result: Vec<Inline>,
}

impl InlineSink {
    /// Collection target: the field result or the ordinary run list; discarded while in
    /// the instruction phase.
    fn target(&mut self) -> Option<&mut Vec<Inline>> {
        match &mut self.field {
            Some(state) if state.separated => Some(&mut state.result),
            Some(_) => None,
            None => Some(&mut self.runs),
        }
    }

    fn push_text(&mut self, style: &TextStyle, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(runs) = self.target() {
            push_text_run(runs, style, text);
        }
    }

    fn push_inline(&mut self, inline: Inline) {
        if let Some(runs) = self.target() {
            runs.push(inline);
        }
    }

    fn push_break(&mut self) {
        if self.field.is_some() {
            return;
        }
        self.breaks.push(self.runs.len());
    }
}

/// Append a text run; merge with an adjacent run of the same style to avoid
/// fragmentation.
fn push_text_run(runs: &mut Vec<Inline>, style: &TextStyle, text: &str) {
    if let Some(Inline::Text(last)) = runs.last_mut()
        && last.style == *style
    {
        last.text.push_str(text);
        return;
    }
    runs.push(Inline::Text(TextRun::new(text, style.clone())));
}

/// Field instruction to field kind.
fn field_kind(instr: &str) -> Option<FieldKind> {
    let token = instr.split_whitespace().next()?.to_ascii_uppercase();
    match token.as_str() {
        "PAGE" => Some(FieldKind::PageNumber),
        "NUMPAGES" | "SECTIONPAGES" => Some(FieldKind::TotalPages),
        "DATE" | "TIME" | "CREATEDATE" | "SAVEDATE" | "PRINTDATE" => Some(FieldKind::Date),
        _ => None,
    }
}

/// Flatten inline content into plain text, used as a field's cached result.
fn runs_text(runs: &[Inline]) -> String {
    let mut text = String::new();
    for inline in runs {
        match inline {
            Inline::Text(run) => text.push_str(&run.text),
            Inline::Link(link) => text.push_str(&runs_text(&link.runs)),
            Inline::Field(field) => {
                if let Some(cached) = &field.cached_text {
                    text.push_str(cached);
                }
            }
            _ => {}
        }
    }
    text
}

/// Collect bookmark names in the document and assign anchors in order of appearance.
fn collect_bookmarks(xml: &str) -> HashMap<String, AnchorId> {
    let mut anchors = HashMap::new();
    let mut reader = XmlReader::from_str(xml);
    let mut next = 1_u64;
    while let Ok(event) = reader.read_event() {
        match event {
            quick_xml::events::Event::Start(element) | quick_xml::events::Event::Empty(element) => {
                if is_element(&element, "bookmarkStart")
                    && let Some(name) = attr(&element, "name")
                    && !name.starts_with('_')
                {
                    anchors.entry(name).or_insert_with(|| {
                        let id = AnchorId::new(next);
                        next += 1;
                        id
                    });
                }
            }
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    anchors
}

/// Read the content-control gallery from `sdtPr`.
fn parse_sdt_gallery(reader: &mut XmlReader<'_>) -> Result<Option<String>, DocxError> {
    let mut gallery = None;
    // docPartGallery hides inside docPartObj, so the matching End must be reached by
    // depth; the subtree cannot be skipped.
    let mut depth = 0_usize;
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => {
                depth += 1;
                let _ = element;
            }
            quick_xml::events::Event::Empty(element) => {
                if is_element(&element, "docPartGallery") {
                    gallery = attr(&element, "val");
                }
            }
            quick_xml::events::Event::End(_) => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok(gallery)
}

/// Whether the content-control gallery denotes a table of contents.
fn is_toc_gallery(gallery: &str) -> bool {
    let lower = gallery.to_ascii_lowercase();
    lower.contains("table of contents") || gallery.contains("目录")
}

/// Row properties (`w:trPr`).
fn parse_row_props(reader: &mut XmlReader<'_>) -> Result<(bool, Option<Twips>), DocxError> {
    let mut header = false;
    let mut height = None;
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => skip_element(reader, &element)?,
            quick_xml::events::Event::Empty(element) => match local_name(&element).as_str() {
                "tblHeader" => header = crate::xml::attr_on_off(&element).unwrap_or(true),
                "trHeight" => {
                    if let Some(value) = attr_i32(&element, "val") {
                        height = Some(Twips::new(i64::from(value)));
                    }
                }
                _ => {}
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok((header, height))
}

/// Cell properties (`w:tcPr`): returns `(column span, whether this is a vertically
/// merged continuation cell)`.
fn parse_cell_props(reader: &mut XmlReader<'_>) -> Result<(u32, VMerge), DocxError> {
    let mut span = 1;
    let mut v_merge = VMerge::None;
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => skip_element(reader, &element)?,
            quick_xml::events::Event::Empty(element) => match local_name(&element).as_str() {
                "gridSpan" => {
                    if let Some(value) = attr_i32(&element, "val") {
                        span = u32::try_from(value).unwrap_or(1).max(1);
                    }
                }
                "vMerge" => {
                    v_merge = match attr(&element, "val").as_deref() {
                        Some("restart") => VMerge::Restart,
                        _ => VMerge::Continue,
                    };
                }
                _ => {}
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok((span, v_merge))
}

/// Get a target part from the relationship table by type suffix.
fn target_by_kind(relations: &HashMap<String, Relation>, suffix: &str) -> Option<String> {
    relations
        .values()
        .find(|relation| relation.kind.ends_with(suffix))
        .map(|relation| relation.target.clone())
}

/// Read the metadata of `docProps/core.xml`.
fn parse_core(text: &str) -> Metadata {
    let mut metadata = Metadata::default();
    let mut reader = XmlReader::from_str(text);
    while let Ok(event) = reader.read_event() {
        let quick_xml::events::Event::Start(element) = event else {
            if matches!(event, quick_xml::events::Event::Eof) {
                break;
            }
            continue;
        };
        let tag = local_name(&element);
        // Read text only for known fields; container elements (such as cp:coreProperties)
        // are ignored outright, otherwise the whole subtree would be taken as the text of
        // one field and nothing would match.
        if !matches!(
            tag.as_str(),
            "title" | "creator" | "subject" | "keywords" | "language"
        ) {
            continue;
        }
        let mut value = String::new();
        let mut depth = 0_usize;
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Text(text)) => value.push_str(&text),
                Ok(quick_xml::events::Event::GeneralRef(reference)) => {
                    if let Some(character) = resolve_reference(&reference) {
                        value.push(character);
                    }
                }
                Ok(quick_xml::events::Event::Start(_)) => depth += 1,
                Ok(quick_xml::events::Event::End(_)) => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                Ok(quick_xml::events::Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
        let value = value.trim().to_owned();
        if value.is_empty() {
            continue;
        }
        match tag.as_str() {
            "title" => metadata.title = Some(value),
            "creator" => metadata.author = Some(value),
            "subject" => metadata.subject = Some(value),
            "keywords" => {
                metadata.keywords = value
                    .split([';', ','])
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            "language" => metadata.language = Some(value),
            _ => {}
        }
    }
    metadata
}

/// Get the text content of the first element with the same name.
fn find_element_text(text: &str, name: &str) -> Option<String> {
    let mut reader = XmlReader::from_str(text);
    while let Ok(event) = reader.read_event() {
        match event {
            quick_xml::events::Event::Start(element) => {
                if local_name(&element) != name {
                    continue;
                }
                let mut value = String::new();
                loop {
                    match reader.read_event() {
                        Ok(quick_xml::events::Event::Text(text)) => value.push_str(&text),
                        Ok(quick_xml::events::Event::End(_)) => break,
                        Ok(quick_xml::events::Event::Eof) | Err(_) => break,
                        _ => {}
                    }
                }
                let value = value.trim().to_owned();
                return if value.is_empty() { None } else { Some(value) };
            }
            quick_xml::events::Event::Eof => return None,
            _ => {}
        }
    }
    None
}

/// Convert EMU (1/914400 inch) into twips.
///
/// `relativeFrom` attribute → position reference.
fn position_from(value: Option<&str>) -> ofdbox_folio_ir::PositionFrom {
    match value.unwrap_or_default() {
        "page" => ofdbox_folio_ir::PositionFrom::Page,
        "column" => ofdbox_folio_ir::PositionFrom::Column,
        "paragraph" => ofdbox_folio_ir::PositionFrom::Paragraph,
        _ => ofdbox_folio_ir::PositionFrom::Margin,
    }
}

fn emu_to_twips(emu: f64) -> Twips {
    Twips::new((emu / 635.0).round() as i64)
}
