//! `document.xml`、页眉页脚与核心属性的读取。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ofdbox_folio_ir::{
    Align, AnchorId, AssetId, Block, Cell, Column, ColumnWidth, Diagnostic, DiagnosticCode, Field,
    FieldKind, FloatingObject, FlowDocument, HeaderFooter, ImageAsset, ImageBlock, Inline,
    InlineImage, Link, LinkTarget, Metadata, Note, NoteKind, NoteRef, Paragraph, Point, Row,
    Section, Table, TextRun, TextStyle, TocBlock, Twips, WrapMode, detect_image_format,
    image_dimensions,
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

/// 读取 DOCX，产出 Flow IR。
pub(crate) fn read(bytes: &[u8], options: &DocxReadOptions) -> Result<FlowDocument, DocxError> {
    Builder::new(bytes, options)?.run()
}

/// 一个 `w:drawing` 的解析结果。
enum Drawing {
    /// 行内图片。
    Inline(InlineImage),
    /// 浮动图片。
    Floating(FloatingObject),
}

/// 读取过程的状态。
struct Builder<'a> {
    package: Package<'a>,
    styles: Styles,
    numbering: Numbering,
    relations: HashMap<String, Relation>,
    doc: FlowDocument,
    /// 当前分节的正文块。
    blocks: Vec<Block>,
    /// 当前分节属性（会向后续分节继承）。
    section_props: RawSectionProps,
    /// 每个已结束分节的属性，供最后回填页眉页脚。
    section_log: Vec<RawSectionProps>,
    counters: Counters,
    /// 书签名到锚点的映射。
    anchors: HashMap<String, AnchorId>,
    /// 已解析的图片：关系 ID → 资源。
    images: HashMap<String, Option<AssetId>>,
    /// 已报告过的诊断，避免同类问题刷屏。
    reported: HashSet<String>,
}

impl<'a> Builder<'a> {
    fn new(bytes: &'a [u8], options: &DocxReadOptions) -> Result<Self, DocxError> {
        let mut package = Package::open(bytes, options)?;
        let Some(document_xml) = package.read_text("word/document.xml")? else {
            return Err(DocxError::Structure(
                "缺少 word/document.xml，不是 DOCX 文档".to_owned(),
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
            .ok_or_else(|| DocxError::Structure("缺少 word/document.xml".to_owned()))?;
        {
            let mut reader = XmlReader::from_str(&document_xml);
            let blocks = self.parse_blocks(&mut reader, Some("body"), true)?;
            self.blocks.extend(blocks);
        }
        self.finish_section();
        self.fill_headers_footers()?;
        self.parse_notes(NoteKind::Footnote)?;
        self.parse_notes(NoteKind::Endnote)?;
        Ok(self.doc)
    }

    // ---- 块级 ----

    /// 读取到配对的 `end` 为止的块序列；`top` 表示这是分节正文层级。
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
                    // 容器元素本身（`w:body`、`w:hdr`、`w:ftr` 等）不承载块内容，
                    // 忽略即可，子元素会继续被本循环处理。
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

    /// 结束当前分节；没有内容且已有分节时不额外制造空分节。
    fn finish_section(&mut self) {
        if self.blocks.is_empty() && !self.doc.sections.is_empty() {
            return;
        }
        let mut section = Section::new(self.section_props.to_setup());
        section.blocks = std::mem::take(&mut self.blocks);
        self.doc.sections.push(section);
        self.section_log.push(self.section_props.clone());
    }

    /// 解析一个段落，返回它产生的块（含段内分页符）与段末分节属性。
    fn parse_paragraph(
        &mut self,
        reader: &mut XmlReader<'_>,
    ) -> Result<(Vec<Block>, Option<RawSectionProps>), DocxError> {
        let mut props = RawParaProps::default();
        // 解析出 `pPr` 就立刻解析样式：run 的字符属性要继承段落样式链的 rPr。
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
                    return Err(DocxError::Structure("段落没有结束标签".to_owned()));
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
        // 段落里的浮动对象脱离正文流，先作为块级元素输出（位置由对象自己带）。
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
            // 空段只有「整段没有内容」时才保留一个，避免段内分页符凭空多出空行。
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

    // ---- 行内 ----

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

    /// 读取容器里直到配对 `End` 为止的行内内容。
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
                            "VML 图形（w:pict）暂不支持，已跳过",
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
                            "批注引用暂不支持，已忽略",
                        );
                        skip_element(reader, &element)?;
                    }
                    "sym" => {
                        self.warn_once(
                            "sym",
                            DiagnosticCode::Unsupported,
                            "符号字体（w:sym）暂不支持，已跳过",
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
                            "符号字体（w:sym）暂不支持，已跳过",
                        );
                    }
                    "footnoteReference" | "endnoteReference" => {
                        self.push_note_reference(&element, sink);
                    }
                    "commentReference" => {
                        self.warn_once(
                            "commentReference",
                            DiagnosticCode::Unsupported,
                            "批注引用暂不支持，已忽略",
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

    /// 读取 word/footnotes.xml 或 word/endnotes.xml 里的注释正文。
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
                    // 顶层 w:footnotes/w:endnotes 容器本身不携带内容，忽略即可。
                    if name != "footnote" && name != "endnote" {
                        continue;
                    }
                    let id = attr_u32(&element, "id");
                    // separator / continuationNotice 之类的伪注释没有正文。
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

    /// 把 w:footnoteReference / w:endnoteReference 记成注释引用。
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
        // Word 用 -1/0 表示分隔符笔记，正文里不会引用。
        sink.push_inline(Inline::Note(NoteRef::new(kind, id)));
    }

    /// 处理复杂域的 begin / separate / end。
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

    /// 读取 `w:fldSimple`：指令在属性里，内容是缓存结果。
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

    /// 读取内容控件；目录内容控件整体映射成自动目录块。
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
            // 目录控件里那段文字是标题（如「目录」），条目由排版按标题重算。
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

    // ---- 表格 ----

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
                    return Err(DocxError::Structure("表格没有结束标签".to_owned()));
                }
                _ => {}
            }
        }
        let resolved = self.styles.resolve_table(&props);
        let header_rows = rows.iter().take_while(|row| row.is_header).count() as u32;
        if columns.is_empty() {
            // 没有 grid 时按最多的单元格数均分。
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
        let mut merged = false;
        let mut blocks = Vec::new();
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                    "tcPr" => {
                        let (cell_span, cell_merged) = parse_cell_props(reader)?;
                        span = cell_span;
                        merged = cell_merged;
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
                "合并单元格（gridSpan）暂不支持，按普通单元格处理",
            );
        }
        if merged {
            // 纵向合并的续格留空，保持列对齐。
            return Ok(Some(Cell::new(Vec::new())));
        }
        Ok(Some(Cell::new(blocks)))
    }

    // ---- 图片与浮动对象 ----

    /// 解析 `w:drawing`：行内返回 `wp:inline`，浮动返回 `wp:anchor` 的位置与绕排。
    fn parse_drawing(&mut self, reader: &mut XmlReader<'_>) -> Result<Option<Drawing>, DocxError> {
        let mut extent: Option<(f64, f64)> = None;
        let mut embed = None;
        let mut alt = None;
        let mut anchor = false;
        let mut wrap = WrapMode::None;
        let mut offset_x: Option<f64> = None;
        let mut offset_y: Option<f64> = None;
        // 当前在 positionH / positionV 里，用来区分两个方向的 posOffset。
        let mut horizontal = false;
        let mut vertical = false;
        let mut depth = 0_usize;
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => {
                    match local_name(&element).as_str() {
                        "anchor" => anchor = true,
                        "positionH" => horizontal = true,
                        "positionV" => vertical = true,
                        "wrapSquare" => wrap = WrapMode::Square,
                        "wrapTopAndBottom" => wrap = WrapMode::TopBottom,
                        "wrapNone" => wrap = WrapMode::None,
                        "posOffset" => {
                            // `wp:posOffset` 的值是元素文本，不是属性。
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

        // 浮动图片：位置换算成相对版心左上角的毫米。
        let mut image = ImageBlock::new(asset);
        image.size = size;
        image.alt = alt;
        image.align = Align::Left;
        let position = Point::new(
            offset_x.map(|value| value / 36000.0).unwrap_or(0.0),
            offset_y.map(|value| value / 36000.0).unwrap_or(0.0),
        );
        let mut object = FloatingObject::new(position, Block::Image(image));
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
                format!("不支持的图片格式，已跳过：{}", relation.target),
            );
            self.images.insert(rid.to_owned(), None);
            return Ok(None);
        };
        let Some((width, height)) = image_dimensions(&bytes) else {
            self.warn_once(
                &format!("image-size-{}", relation.target),
                DiagnosticCode::Image,
                format!("读不出图片尺寸，已跳过：{}", relation.target),
            );
            self.images.insert(rid.to_owned(), None);
            return Ok(None);
        };
        let asset = ImageAsset::new(format, Arc::from(bytes.as_slice()), width, height);
        let id = self.doc.resources.add_image(asset);
        self.images.insert(rid.to_owned(), Some(id));
        Ok(Some(id))
    }

    // ---- 页眉页脚 ----

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

    /// 只报告一次同类诊断。
    fn warn_once(&mut self, key: &str, code: DiagnosticCode, message: impl Into<String>) {
        if !self.reported.insert(key.to_owned()) {
            return;
        }
        self.doc.report.push(Diagnostic::warning(code, message));
    }
}

/// 行内收集器。
#[derive(Default)]
struct InlineSink {
    /// 已完成的 run。
    runs: Vec<Inline>,
    /// 分页符在 `runs` 中的位置。
    breaks: Vec<usize>,
    /// 段落锚点。
    anchor: Option<AnchorId>,
    /// 进行中的复杂域。
    field: Option<FieldState>,
    /// 段落里出现的浮动对象，按块级元素另排。
    floatings: Vec<FloatingObject>,
}

/// 进行中的复杂域。
#[derive(Default)]
struct FieldState {
    /// 域指令。
    instr: String,
    /// 是否已进入结果部分。
    separated: bool,
    /// 结果 run。
    result: Vec<Inline>,
}

impl InlineSink {
    /// 收集目标：域结果或普通 run 列表；处于指令阶段时丢弃。
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

/// 追加文本 run；样式相同且相邻时合并，避免碎片化。
fn push_text_run(runs: &mut Vec<Inline>, style: &TextStyle, text: &str) {
    if let Some(Inline::Text(last)) = runs.last_mut()
        && last.style == *style
    {
        last.text.push_str(text);
        return;
    }
    runs.push(Inline::Text(TextRun::new(text, style.clone())));
}

/// 域指令到字段类型。
fn field_kind(instr: &str) -> Option<FieldKind> {
    let token = instr.split_whitespace().next()?.to_ascii_uppercase();
    match token.as_str() {
        "PAGE" => Some(FieldKind::PageNumber),
        "NUMPAGES" | "SECTIONPAGES" => Some(FieldKind::TotalPages),
        "DATE" | "TIME" | "CREATEDATE" | "SAVEDATE" | "PRINTDATE" => Some(FieldKind::Date),
        _ => None,
    }
}

/// 把行内内容拍平成纯文本，作为域的缓存结果。
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

/// 收集文档里的书签名，按出现顺序分配锚点。
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

/// 读取 `sdtPr` 里的内容控件库分类。
fn parse_sdt_gallery(reader: &mut XmlReader<'_>) -> Result<Option<String>, DocxError> {
    let mut gallery = None;
    // docPartGallery 藏在 docPartObj 里，必须按深度读到配对 End，不能跳子树。
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

/// 内容控件分类是否为目录。
fn is_toc_gallery(gallery: &str) -> bool {
    let lower = gallery.to_ascii_lowercase();
    lower.contains("table of contents") || gallery.contains("目录")
}

/// 行属性（`w:trPr`）。
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

/// 单元格属性（`w:tcPr`）：返回 `(列跨度, 是否为纵向合并续格)`。
fn parse_cell_props(reader: &mut XmlReader<'_>) -> Result<(u32, bool), DocxError> {
    let mut span = 1;
    let mut merged = false;
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
                    merged = attr(&element, "val").as_deref() != Some("restart");
                }
                _ => {}
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok((span, merged))
}

/// 从关系表里按类型后缀取目标部件。
fn target_by_kind(relations: &HashMap<String, Relation>, suffix: &str) -> Option<String> {
    relations
        .values()
        .find(|relation| relation.kind.ends_with(suffix))
        .map(|relation| relation.target.clone())
}

/// 读取 `docProps/core.xml` 的元数据。
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
        // 只对已知字段读取文本；容器元素（如 cp:coreProperties）直接忽略，
        // 否则会把整棵子树当成一个字段的文本，什么也匹配不到。
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

/// 取第一个同名元素的文本内容。
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

/// EMU（1/914400 英寸）换算成 twip。
fn emu_to_twips(emu: f64) -> Twips {
    Twips::new((emu / 635.0).round() as i64)
}
