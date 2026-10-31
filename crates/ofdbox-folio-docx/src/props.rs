//! Raw DOCX properties: parsing, merging and IR mapping of `pPr` / `rPr` / `sectPr` /
//! `tblPr`.
//!
//! OOXML styles cascade as "document defaults → table style → paragraph style chain →
//! character style chain → direct formatting". The IR named style has only a single
//! `based_on` merge and cannot express "explicitly written back default value", so on
//! read each field is merged based on **whether it appears** in the XML (`Option`
//! semantics), and the result is finally flattened into direct formatting in the Flow
//! IR.

use ofdbox_folio_ir::{
    Align, Border, BorderStyle, FontFamily, LineSpacing, Margins, PageSetup, ParaStyle, Rgba,
    TabAlign, TabLeader, TabStop, TableBorders, TableWidth, TextStyle, Twips,
};

use crate::DocxError;
use crate::xml::{
    XmlReader, attr, attr_f64, attr_i32, attr_on_off, attr_u32, is_element, local_name,
    skip_element,
};

/// Vertical alignment (superscript/subscript).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VertAlign {
    /// Superscript.
    Superscript,
    /// Subscript.
    Subscript,
}

/// Raw paragraph properties; `None` means the field does not appear in the XML.
#[derive(Clone, Debug, Default)]
pub(crate) struct RawParaProps {
    /// Named paragraph style ID.
    pub style_id: Option<String>,
    /// Alignment.
    pub align: Option<Align>,
    /// Line spacing.
    pub line_spacing: Option<LineSpacing>,
    /// Space before.
    pub space_before: Option<Twips>,
    /// Space after.
    pub space_after: Option<Twips>,
    /// First-line indent (may be negative, meaning hanging).
    pub first_line: Option<Twips>,
    /// Left indent.
    pub left: Option<Twips>,
    /// Right indent.
    pub right: Option<Twips>,
    /// Keep with the next paragraph.
    pub keep_next: Option<bool>,
    /// Keep the lines of a paragraph together.
    pub keep_lines: Option<bool>,
    /// Tab stops.
    pub tabs: Option<Vec<TabStop>>,
    /// Page break before the paragraph.
    pub page_break_before: Option<bool>,
    /// Numbering: `(numId, ilvl)`.
    pub numbering: Option<(u32, u8)>,
    /// Outline level (0-based).
    pub outline_level: Option<u8>,
    /// Character properties of the paragraph mark, used as the character style of an
    /// empty paragraph.
    pub mark: Option<RawRunProps>,
    /// Section properties embedded in the paragraph (section break at the paragraph
    /// end).
    pub section: Option<RawSectionProps>,
}

impl RawParaProps {
    /// Overwrite the fields present in `over`.
    pub fn merge(&mut self, over: &Self) {
        macro_rules! fill {
            ($($field:ident),* $(,)?) => {
                $(if over.$field.is_some() {
                    self.$field = over.$field.clone();
                })*
            };
        }
        fill!(
            style_id,
            align,
            line_spacing,
            space_before,
            space_after,
            first_line,
            left,
            right,
            keep_next,
            keep_lines,
            tabs,
            page_break_before,
            numbering,
            outline_level,
            section,
        );
        if let Some(mark) = &over.mark {
            match &mut self.mark {
                Some(base) => base.merge(mark),
                None => self.mark = Some(mark.clone()),
            }
        }
    }

    /// Build the Flow IR paragraph style (only non-default values are written).
    pub fn to_style(&self) -> ParaStyle {
        let mut style = ParaStyle::default();
        if let Some(align) = self.align {
            style.align = align;
        }
        if let Some(spacing) = self.line_spacing {
            style.line_spacing = spacing;
        }
        if let Some(value) = self.space_before {
            style.space_before = value;
        }
        if let Some(value) = self.space_after {
            style.space_after = value;
        }
        if let Some(value) = self.first_line {
            style.first_line_indent = value;
        }
        if let Some(value) = self.left {
            style.left_indent = value;
        }
        if let Some(value) = self.right {
            style.right_indent = value;
        }
        if let Some(value) = self.keep_next {
            style.keep_with_next = value;
        }
        if let Some(value) = self.keep_lines {
            style.keep_lines_together = value;
        }
        if let Some(tabs) = &self.tabs {
            style.tabs = tabs.clone();
        }
        style
    }
}

/// Raw character properties.
#[derive(Clone, Debug, Default)]
pub(crate) struct RawRunProps {
    /// Named character style ID.
    pub style_id: Option<String>,
    /// Latin font.
    pub ascii: Option<String>,
    /// Theme Latin font (`majorHAnsi` / `minorHAnsi`).
    pub ascii_theme: Option<String>,
    /// East Asian font.
    pub east_asia: Option<String>,
    /// Theme East Asian font.
    pub east_asia_theme: Option<String>,
    /// Font size (points).
    pub size: Option<f64>,
    /// Bold.
    pub bold: Option<bool>,
    /// Italic.
    pub italic: Option<bool>,
    /// Underline.
    pub underline: Option<bool>,
    /// Strikethrough.
    pub strike: Option<bool>,
    /// Color.
    pub color: Option<Rgba>,
    /// Vertical alignment.
    pub vert: Option<VertAlign>,
    /// Hidden text.
    pub hidden: Option<bool>,
}

impl RawRunProps {
    /// Overwrite the fields present in `over`.
    ///
    /// Fonts come as either an explicit name or a theme name: once some layer writes an
    /// explicit font, the theme font inherited from the layer below must be cleared,
    /// otherwise the theme font would in turn override the font explicitly written in
    /// the style.
    pub fn merge(&mut self, over: &Self) {
        macro_rules! fill {
            ($($field:ident),* $(,)?) => {
                $(if over.$field.is_some() {
                    self.$field = over.$field.clone();
                })*
            };
        }
        fill!(
            style_id, size, bold, italic, underline, strike, color, vert, hidden,
        );
        if over.ascii.is_some() {
            self.ascii = over.ascii.clone();
            self.ascii_theme = None;
        }
        if over.ascii_theme.is_some() {
            self.ascii_theme = over.ascii_theme.clone();
            self.ascii = None;
        }
        if over.east_asia.is_some() {
            self.east_asia = over.east_asia.clone();
            self.east_asia_theme = None;
        }
        if over.east_asia_theme.is_some() {
            self.east_asia_theme = over.east_asia_theme.clone();
            self.east_asia = None;
        }
    }

    /// Build the Flow IR character style.
    ///
    /// Theme fonts are resolved to concrete font names first; when a theme name cannot
    /// be resolved the explicit font is kept, so Chinese text is never handed to a theme
    /// font that only has Latin glyphs.
    pub fn to_style(&self, theme: &ThemeFonts) -> TextStyle {
        let mut style = TextStyle::default();
        let mut name = None;
        if let Some(family) = &self.ascii {
            name = Some(family.clone());
        }
        if let Some(theme_name) = &self.ascii_theme
            && let Some(family) = theme.resolve(theme_name)
        {
            name = Some(family);
        }
        let mut fallback = None;
        if let Some(family) = &self.east_asia {
            fallback = Some(family.clone());
        }
        if let Some(theme_name) = &self.east_asia_theme
            && let Some(family) = theme.resolve(theme_name)
        {
            fallback = Some(family);
        }
        let mut chain = Vec::new();
        if let Some(fallback) = fallback
            && Some(&fallback) != name.as_ref()
        {
            chain.push(fallback);
        }
        if name.is_some() || !chain.is_empty() {
            style.family = FontFamily {
                name: name.unwrap_or_default(),
                fallback: chain,
            };
        }
        if let Some(size) = self.size {
            style.size = Twips::from_pt(size);
        }
        if let Some(bold) = self.bold {
            style.bold = bold;
        }
        if let Some(italic) = self.italic {
            style.italic = italic;
        }
        if let Some(underline) = self.underline {
            style.underline = underline;
        }
        if let Some(strike) = self.strike {
            style.strike = strike;
        }
        if let Some(color) = self.color {
            style.color = color;
        }
        style.superscript = self.vert == Some(VertAlign::Superscript);
        style.subscript = self.vert == Some(VertAlign::Subscript);
        style
    }
}

/// Theme fonts: `major` corresponds to `*Theme="major*"`, `minor` to `minor*`.
#[derive(Clone, Debug, Default)]
pub(crate) struct ThemeFonts {
    /// Heading Latin font.
    pub major_latin: Option<String>,
    /// Heading East Asian font.
    pub major_east_asia: Option<String>,
    /// Body Latin font.
    pub minor_latin: Option<String>,
    /// Body East Asian font.
    pub minor_east_asia: Option<String>,
}

impl ThemeFonts {
    /// Resolve a concrete font by theme name.
    pub fn resolve(&self, name: &str) -> Option<String> {
        let lower = name.to_ascii_lowercase();
        let major = lower.starts_with("major");
        let east_asia = lower.contains("eastasia") || lower.contains("bidi");
        let value = match (major, east_asia) {
            (true, false) => self.major_latin.as_ref(),
            (true, true) => self.major_east_asia.as_ref(),
            (false, false) => self.minor_latin.as_ref(),
            (false, true) => self.minor_east_asia.as_ref(),
        }?;
        if value.trim().is_empty() {
            None
        } else {
            Some(value.clone())
        }
    }
}

/// Raw section properties.
#[derive(Clone, Debug, Default)]
pub(crate) struct RawSectionProps {
    /// Page width.
    pub width: Option<Twips>,
    /// Page height.
    pub height: Option<Twips>,
    /// Top margin.
    pub top: Option<Twips>,
    /// Bottom margin.
    pub bottom: Option<Twips>,
    /// Left margin.
    pub left: Option<Twips>,
    /// Right margin.
    pub right: Option<Twips>,
    /// Header distance from the top edge.
    pub header: Option<Twips>,
    /// Footer distance from the bottom edge.
    pub footer: Option<Twips>,
    /// Different first page.
    pub title_page: Option<bool>,
    /// Header references: `(type, rId)`.
    pub headers: Vec<(String, String)>,
    /// Footer references: `(type, rId)`.
    pub footers: Vec<(String, String)>,
    /// Column count.
    pub columns: Option<u32>,
    /// Column gap.
    pub column_gap: Option<Twips>,
}

impl RawSectionProps {
    /// Merge another set of section properties on top.
    pub fn merge(&mut self, over: &Self) {
        macro_rules! fill {
            ($($field:ident),* $(,)?) => {
                $(if over.$field.is_some() {
                    self.$field = over.$field.clone();
                })*
            };
        }
        fill!(
            width, height, top, bottom, left, right, header, footer, title_page, columns,
            column_gap,
        );
        if !over.headers.is_empty() {
            self.headers = over.headers.clone();
        }
        if !over.footers.is_empty() {
            self.footers = over.footers.clone();
        }
    }

    /// Build the page setup; missing fields keep the A4 defaults.
    pub fn to_setup(&self) -> PageSetup {
        let mut setup = PageSetup::default();
        if let Some(value) = self.width {
            setup.width = value;
        }
        if let Some(value) = self.height {
            setup.height = value;
        }
        if let Some(value) = self.top {
            setup.margins.top = value;
        }
        if let Some(value) = self.bottom {
            setup.margins.bottom = value;
        }
        if let Some(value) = self.left {
            setup.margins.left = value;
        }
        if let Some(value) = self.right {
            setup.margins.right = value;
        }
        if let Some(value) = self.header {
            setup.header_distance = value;
        }
        if let Some(value) = self.footer {
            setup.footer_distance = value;
        }
        if let Some(value) = self.columns {
            setup.columns = u16::try_from(value).unwrap_or(1).max(1);
        }
        if let Some(value) = self.column_gap {
            setup.column_gap = value;
        }
        setup
    }
}

/// Table properties.
#[derive(Clone, Debug, Default)]
pub(crate) struct RawTableProps {
    /// Table style ID.
    pub style_id: Option<String>,
    /// Outer border.
    pub outer: Option<Border>,
    /// Horizontal inner border.
    pub horizontal: Option<Border>,
    /// Vertical inner border.
    pub vertical: Option<Border>,
    /// Cell margins.
    pub cell_margins: Option<Margins>,
    /// Horizontal alignment.
    pub alignment: Option<Align>,
    /// Left indent.
    pub indent: Option<Twips>,
    /// Declared table width.
    pub width: Option<TableWidth>,
    /// Whether the first-row conditional format is enabled.
    pub first_row: Option<bool>,
    /// Whether horizontal row banding is enabled.
    pub band_rows: Option<bool>,
    /// Whether column widths are content-driven (`tblLayout=autofit`).
    pub autofit: Option<bool>,
}

impl RawTableProps {
    /// Overwrite the fields present in `over`.
    pub fn merge(&mut self, over: &Self) {
        if over.style_id.is_some() {
            self.style_id = over.style_id.clone();
        }
        if over.outer.is_some() {
            self.outer = over.outer;
        }
        if over.horizontal.is_some() {
            self.horizontal = over.horizontal;
        }
        if over.vertical.is_some() {
            self.vertical = over.vertical;
        }
        if over.cell_margins.is_some() {
            self.cell_margins = over.cell_margins;
        }
        if over.alignment.is_some() {
            self.alignment = over.alignment;
        }
        if over.indent.is_some() {
            self.indent = over.indent;
        }
        if over.width.is_some() {
            self.width = over.width;
        }
        if over.first_row.is_some() {
            self.first_row = over.first_row;
        }
        if over.band_rows.is_some() {
            self.band_rows = over.band_rows;
        }
        if over.autofit.is_some() {
            self.autofit = over.autofit;
        }
    }

    /// Build the IR border set.
    pub fn borders(&self) -> TableBorders {
        TableBorders {
            outer: self.outer,
            horizontal: self.horizontal,
            vertical: self.vertical,
        }
    }
}

// ---- Parsing ----

/// Parse `w:pPr`.
pub(crate) fn parse_para_props(
    reader: &mut XmlReader<'_>,
    _start: &quick_xml::events::BytesStart<'_>,
) -> Result<RawParaProps, DocxError> {
    let mut props = RawParaProps::default();
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "rPr" => props.mark = Some(parse_run_props(reader, &element)?),
                "sectPr" => props.section = Some(parse_section_props(reader, &element)?),
                "numPr" => props.numbering = parse_numbering_props(reader, &element)?,
                "tabs" => props.tabs = Some(parse_tabs(reader)?),
                _ => skip_element(reader, &element)?,
            },
            quick_xml::events::Event::Empty(element) => match local_name(&element).as_str() {
                "pStyle" => props.style_id = attr(&element, "val"),
                "jc" => props.align = attr(&element, "val").as_deref().and_then(parse_align),
                "spacing" => apply_spacing(&element, &mut props),
                "ind" => apply_indent(&element, &mut props),
                "numPr" => props.numbering = parse_numbering_props(reader, &element)?,
                "tabs" => props.tabs = Some(parse_tabs(reader)?),
                "outlineLvl" => props.outline_level = attr_u32(&element, "val").map(|l| l as u8),
                "keepNext" => props.keep_next = attr_on_off(&element),
                "keepLines" => props.keep_lines = attr_on_off(&element),
                "pageBreakBefore" => props.page_break_before = attr_on_off(&element),
                "contextualSpacing" => {}
                _ => {}
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => {
                return Err(DocxError::Structure("pPr has no end tag".to_owned()));
            }
            _ => {}
        }
    }
    Ok(props)
}

/// Parse `w:rPr`.
pub(crate) fn parse_run_props(
    reader: &mut XmlReader<'_>,
    _start: &quick_xml::events::BytesStart<'_>,
) -> Result<RawRunProps, DocxError> {
    let mut props = RawRunProps::default();
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "rFonts" => apply_fonts(&element, &mut props),
                _ => skip_element(reader, &element)?,
            },
            quick_xml::events::Event::Empty(element) => match local_name(&element).as_str() {
                "rStyle" => props.style_id = attr(&element, "val"),
                "rFonts" => apply_fonts(&element, &mut props),
                "b" => props.bold = attr_on_off(&element),
                "i" => props.italic = attr_on_off(&element),
                "u" => {
                    props.underline = attr(&element, "val")
                        .map(|value| value != "none" && value != "0")
                        .or(Some(true));
                }
                "strike" | "dstrike" => props.strike = attr_on_off(&element),
                "color" => props.color = attr(&element, "val").as_deref().and_then(parse_color),
                "sz" => {
                    props.size = attr_f64(&element, "val").map(|half_points| half_points / 2.0);
                }
                "vertAlign" => {
                    props.vert = match attr(&element, "val").as_deref() {
                        Some("superscript") => Some(VertAlign::Superscript),
                        Some("subscript") => Some(VertAlign::Subscript),
                        _ => None,
                    };
                }
                "vanish" => props.hidden = attr_on_off(&element),
                _ => {}
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => {
                return Err(DocxError::Structure("rPr has no end tag".to_owned()));
            }
            _ => {}
        }
    }
    Ok(props)
}

/// Parse `w:sectPr`.
pub(crate) fn parse_section_props(
    reader: &mut XmlReader<'_>,
    start: &quick_xml::events::BytesStart<'_>,
) -> Result<RawSectionProps, DocxError> {
    let mut props = RawSectionProps::default();
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "pgSz" | "pgMar" => apply_page_attrs(&element, &mut props),
                "cols" => apply_columns(&element, &mut props),
                _ => skip_element(reader, &element)?,
            },
            quick_xml::events::Event::Empty(element) => match local_name(&element).as_str() {
                "pgSz" | "pgMar" => apply_page_attrs(&element, &mut props),
                "titlePg" => props.title_page = attr_on_off(&element),
                "headerReference" => {
                    if let (Some(kind), Some(id)) = (attr(&element, "type"), attr(&element, "id")) {
                        props.headers.push((kind, id));
                    }
                }
                "footerReference" => {
                    if let (Some(kind), Some(id)) = (attr(&element, "type"), attr(&element, "id")) {
                        props.footers.push((kind, id));
                    }
                }
                "cols" => apply_columns(&element, &mut props),
                _ => {}
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => {
                let _ = start;
                return Err(DocxError::Structure("sectPr has no end tag".to_owned()));
            }
            _ => {}
        }
    }
    Ok(props)
}

/// Apply `w:cols`: column count and column gap.
fn apply_columns(element: &quick_xml::events::BytesStart<'_>, props: &mut RawSectionProps) {
    if let Some(count) = attr_u32(element, "num") {
        props.columns = Some(count);
    }
    if let Some(space) = attr_i32(element, "space") {
        props.column_gap = Some(Twips::new(i64::from(space)));
    }
}

/// Apply the size attributes on `pgSz` and `pgMar` (the two share one attribute
/// namespace).
fn apply_page_attrs(element: &quick_xml::events::BytesStart<'_>, props: &mut RawSectionProps) {
    let read = |name: &str| attr_i32(element, name).map(|value| Twips::new(i64::from(value)));
    if let Some(value) = read("w") {
        props.width = Some(value);
    }
    if let Some(value) = read("h") {
        props.height = Some(value);
    }
    if let Some(value) = read("top") {
        props.top = Some(value);
    }
    if let Some(value) = read("bottom") {
        props.bottom = Some(value);
    }
    if let Some(value) = read("left") {
        props.left = Some(value);
    }
    if let Some(value) = read("right") {
        props.right = Some(value);
    }
    if let Some(value) = read("header") {
        props.header = Some(value);
    }
    if let Some(value) = read("footer") {
        props.footer = Some(value);
    }
}

/// Parse `w:tblPr`.
pub(crate) fn parse_table_props(
    reader: &mut XmlReader<'_>,
    _start: &quick_xml::events::BytesStart<'_>,
) -> Result<RawTableProps, DocxError> {
    let mut props = RawTableProps::default();
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "tblBorders" => apply_table_borders(reader, &mut props)?,
                "tblCellMar" => {
                    props.cell_margins = Some(parse_cell_margins(reader, &element)?);
                }
                _ => skip_element(reader, &element)?,
            },
            quick_xml::events::Event::Empty(element) => {
                if is_element(&element, "tblStyle") {
                    props.style_id = attr(&element, "val");
                }
                if is_element(&element, "tblJc") {
                    props.alignment = attr(&element, "val").as_deref().and_then(parse_align);
                }
                if is_element(&element, "tblInd")
                    && let Some(value) = attr_i32(&element, "w")
                {
                    props.indent = Some(Twips::new(i64::from(value)));
                }
                if is_element(&element, "tblW") {
                    props.width = attr(&element, "w").and_then(|value| {
                        let width: f64 = value.parse().ok()?;
                        match attr(&element, "type").as_deref() {
                            Some("pct") => Some(TableWidth::Percent(width / 50.0)),
                            Some("dxa") => Some(TableWidth::Twips(Twips::new(width as i64))),
                            Some("auto") => Some(TableWidth::Auto),
                            _ => Some(TableWidth::Twips(Twips::new(width as i64))),
                        }
                    });
                }
                if is_element(&element, "tblLayout") {
                    props.autofit = attr(&element, "type").map(|value| value != "fixed");
                }
                if is_element(&element, "tblLook") {
                    let on = |value: Option<String>| {
                        value.map(|value| {
                            !matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "0" | "false" | "off"
                            )
                        })
                    };
                    props.first_row = on(attr(&element, "firstRow"));
                    props.band_rows = on(attr(&element, "noHBand")).map(|no_band| !no_band);
                }
            }
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => {
                return Err(DocxError::Structure("tblPr has no end tag".to_owned()));
            }
            _ => {}
        }
    }
    Ok(props)
}

/// Parse the tab stop definitions inside `w:tabs`.
fn parse_tabs(reader: &mut XmlReader<'_>) -> Result<Vec<TabStop>, DocxError> {
    let mut stops = Vec::new();
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => skip_element(reader, &element)?,
            quick_xml::events::Event::Empty(element) => {
                if !is_element(&element, "tab") {
                    continue;
                }
                let Some(position) = attr_i32(&element, "pos") else {
                    continue;
                };
                let align = match attr(&element, "val").as_deref() {
                    Some("center") => TabAlign::Center,
                    Some("right") => TabAlign::Right,
                    Some("decimal") => TabAlign::Decimal,
                    // clear/bar are not tab stops used for text alignment.
                    Some("clear") | Some("bar") => continue,
                    _ => TabAlign::Left,
                };
                let leader = match attr(&element, "leader").as_deref() {
                    Some("dot") | Some("middleDot") => TabLeader::Dot,
                    Some("hyphen") => TabLeader::Dash,
                    Some("underscore") | Some("heavy") => TabLeader::Line,
                    _ => TabLeader::None,
                };
                stops.push(TabStop::new(Twips::new(i64::from(position)), align, leader));
            }
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    stops.sort_by_key(|stop| stop.position);
    Ok(stops)
}

/// Parse `w:numPr` and return `(numId, ilvl)`.
fn parse_numbering_props(
    reader: &mut XmlReader<'_>,
    _start: &quick_xml::events::BytesStart<'_>,
) -> Result<Option<(u32, u8)>, DocxError> {
    let mut num_id = None;
    let mut level = 0_u8;
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => skip_element(reader, &element)?,
            quick_xml::events::Event::Empty(element) => match local_name(&element).as_str() {
                "numId" => num_id = attr_u32(&element, "val"),
                "ilvl" => {
                    if let Some(value) = attr_u32(&element, "val") {
                        level = value as u8;
                    }
                }
                _ => {}
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok(num_id.map(|num_id| (num_id, level)))
}

fn apply_spacing(element: &quick_xml::events::BytesStart<'_>, props: &mut RawParaProps) {
    if let Some(value) = attr_i32(element, "before") {
        props.space_before = Some(Twips::new(i64::from(value)));
    }
    if let Some(value) = attr_i32(element, "after") {
        props.space_after = Some(Twips::new(i64::from(value)));
    }
    let Some(line) = attr_i32(element, "line") else {
        return;
    };
    let rule = attr(element, "lineRule").unwrap_or_else(|| "auto".to_owned());
    props.line_spacing = Some(match rule.as_str() {
        "exact" => LineSpacing::Exact(Twips::new(i64::from(line))),
        "atLeast" => LineSpacing::AtLeast(Twips::new(i64::from(line))),
        _ => LineSpacing::Multiple(f64::from(line) / 240.0),
    });
}

fn apply_indent(element: &quick_xml::events::BytesStart<'_>, props: &mut RawParaProps) {
    let first = attr_i32(element, "firstLine");
    let hanging = attr_i32(element, "hanging");
    if let Some(value) = first {
        props.first_line = Some(Twips::new(i64::from(value)));
    } else if let Some(value) = hanging {
        props.first_line = Some(Twips::new(-i64::from(value)));
    }
    let left = attr_i32(element, "left").or_else(|| attr_i32(element, "start"));
    if let Some(value) = left {
        props.left = Some(Twips::new(i64::from(value)));
    }
    let right = attr_i32(element, "right").or_else(|| attr_i32(element, "end"));
    if let Some(value) = right {
        props.right = Some(Twips::new(i64::from(value)));
    }
}

fn apply_fonts(element: &quick_xml::events::BytesStart<'_>, props: &mut RawRunProps) {
    if let Some(value) = attr(element, "ascii") {
        props.ascii = Some(value);
    }
    if let Some(value) = attr(element, "asciiTheme") {
        props.ascii_theme = Some(value);
    }
    if let Some(value) = attr(element, "eastAsia") {
        props.east_asia = Some(value);
    }
    if let Some(value) = attr(element, "eastAsiaTheme") {
        props.east_asia_theme = Some(value);
    }
}

fn apply_table_borders(
    reader: &mut XmlReader<'_>,
    props: &mut RawTableProps,
) -> Result<(), DocxError> {
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => skip_element(reader, &element)?,
            quick_xml::events::Event::Empty(element) => {
                let Some(border) = parse_border(&element) else {
                    continue;
                };
                match local_name(&element).as_str() {
                    "top" | "left" | "bottom" | "right" | "start" | "end" => {
                        props.outer = Some(border);
                    }
                    "insideH" => props.horizontal = Some(border),
                    "insideV" => props.vertical = Some(border),
                    _ => {}
                }
            }
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

fn parse_border(element: &quick_xml::events::BytesStart<'_>) -> Option<Border> {
    let style = attr(element, "val")?;
    if matches!(style.as_str(), "none" | "nil") {
        return None;
    }
    let width = attr_f64(element, "sz")
        .map(|eighths| Twips::from_pt(eighths / 8.0))
        .unwrap_or_else(|| Twips::from_pt(0.5));
    let color = attr(element, "color").as_deref().and_then(parse_color);
    Some(Border {
        width,
        color: color.unwrap_or(Rgba::BLACK),
        style: match style.as_str() {
            "dashed" | "dashSmallGap" | "dashDot" => BorderStyle::Dashed,
            "dotted" => BorderStyle::Dotted,
            "double" | "thickThinSmallGap" | "thinThickSmallGap" => BorderStyle::Double,
            _ => BorderStyle::Solid,
        },
    })
}

pub(crate) fn parse_cell_margins(
    reader: &mut XmlReader<'_>,
    start: &quick_xml::events::BytesStart<'_>,
) -> Result<Margins, DocxError> {
    let _ = start;
    let mut margins = Margins::default();
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => skip_element(reader, &element)?,
            quick_xml::events::Event::Empty(element) => {
                let Some(value) = attr_i32(&element, "w") else {
                    continue;
                };
                // Only dxa (twip) can be used directly; other units (such as pct) are
                // approximated as twips.
                let twips = Twips::new(i64::from(value));
                match local_name(&element).as_str() {
                    "top" => margins.top = twips,
                    "bottom" => margins.bottom = twips,
                    "left" | "start" => margins.left = twips,
                    "right" | "end" => margins.right = twips,
                    _ => {}
                }
            }
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok(margins)
}

/// Parse the value of `w:jc`.
pub(crate) fn parse_align(value: &str) -> Option<Align> {
    Some(match value {
        "center" => Align::Center,
        "right" | "end" => Align::Right,
        "both" | "justify" | "distribute" | "thaiDistribute" => Align::Justify,
        "left" | "start" => Align::Left,
        _ => return None,
    })
}

/// Parse the hexadecimal value of `w:color`.
pub(crate) fn parse_color(value: &str) -> Option<Rgba> {
    let hex = value.trim().trim_start_matches('#');
    if hex.eq_ignore_ascii_case("auto") || hex.len() != 6 {
        return None;
    }
    let red = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let green = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Rgba::from_rgb(red, green, blue))
}

#[cfg(test)]
mod tests {
    use ofdbox_folio_ir::{Align, LineSpacing, Twips};

    use super::{RawParaProps, parse_para_props};
    use crate::xml::XmlReader;

    /// Parse the first `pPr` in `xml`.
    fn parse(xml: &str) -> RawParaProps {
        let mut reader = XmlReader::from_str(xml);
        loop {
            match reader.read_event().expect("event") {
                quick_xml::events::Event::Start(element) => {
                    if crate::xml::is_element(&element, "pPr") {
                        return parse_para_props(&mut reader, &element).expect("props");
                    }
                }
                quick_xml::events::Event::Eof => panic!("no pPr"),
                _ => {}
            }
        }
    }

    #[test]
    fn parses_common_paragraph_properties() {
        let props = parse(
            r#"<w:pPr><w:pStyle w:val="Body"/><w:spacing w:before="120" w:after="240" w:line="360" w:lineRule="auto"/><w:ind w:firstLine="420" w:left="200" w:right="100"/><w:jc w:val="both"/><w:outlineLvl w:val="1"/><w:keepNext/></w:pPr>"#,
        );
        assert_eq!(props.style_id.as_deref(), Some("Body"));
        assert_eq!(props.align, Some(Align::Justify));
        assert_eq!(props.space_before, Some(Twips::new(120)));
        assert_eq!(props.space_after, Some(Twips::new(240)));
        assert_eq!(props.line_spacing, Some(LineSpacing::Multiple(1.5)));
        assert_eq!(props.first_line, Some(Twips::new(420)));
        assert_eq!(props.left, Some(Twips::new(200)));
        assert_eq!(props.right, Some(Twips::new(100)));
        assert_eq!(props.outline_level, Some(1));
        assert_eq!(props.keep_next, Some(true));
    }

    #[test]
    fn hanging_indent_becomes_negative_first_line() {
        let props = parse(r#"<w:pPr><w:ind w:hanging="720" w:left="1440"/></w:pPr>"#);
        assert_eq!(props.first_line, Some(Twips::new(-720)));
        assert_eq!(props.left, Some(Twips::new(1440)));
    }

    #[test]
    fn merges_only_present_fields() {
        let mut base = parse(r#"<w:pPr><w:jc w:val="center"/><w:spacing w:after="240"/></w:pPr>"#);
        let over = parse(r#"<w:pPr><w:spacing w:after="0"/></w:pPr>"#);
        base.merge(&over);
        // An explicitly written back 0 must take effect too.
        assert_eq!(base.space_after, Some(Twips::new(0)));
        assert_eq!(base.align, Some(Align::Center));
    }

    #[test]
    fn maps_run_properties_to_text_style() {
        let xml = r#"<w:rPr><w:b/><w:i w:val="0"/><w:u w:val="single"/><w:strike/><w:color w:val="C81E1E"/><w:sz w:val="24"/><w:vertAlign w:val="superscript"/><w:rFonts w:ascii="Arial" w:eastAsia="宋体"/></w:rPr>"#;
        let mut reader = XmlReader::from_str(xml);
        let props = loop {
            match reader.read_event().expect("event") {
                quick_xml::events::Event::Start(element) => {
                    if crate::xml::is_element(&element, "rPr") {
                        break super::parse_run_props(&mut reader, &element).expect("props");
                    }
                }
                quick_xml::events::Event::Eof => panic!("no rPr"),
                _ => {}
            }
        };
        let style = props.to_style(&super::ThemeFonts::default());
        assert!(style.bold);
        assert!(!style.italic);
        assert!(style.underline);
        assert!(style.strike);
        assert!(style.superscript);
        assert_eq!(style.size, Twips::from_pt(12.0));
        assert_eq!(
            style.color,
            ofdbox_folio_ir::Rgba::from_rgb(0xC8, 0x1E, 0x1E)
        );
        assert_eq!(style.family.name, "Arial");
        assert_eq!(style.family.fallback, vec!["宋体".to_owned()]);
    }
}
