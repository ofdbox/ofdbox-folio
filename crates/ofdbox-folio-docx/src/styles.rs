//! `styles.xml`, `theme1.xml` and style cascade resolution.

use std::collections::HashMap;

use ofdbox_folio_ir::Rgba;

use crate::DocxError;
use crate::props::{
    RawParaProps, RawRunProps, RawTableProps, ThemeFonts, parse_color, parse_para_props,
    parse_run_props, parse_table_props,
};
use crate::xml::{XmlReader, attr, local_name, skip_element};

/// A named paragraph style.
#[derive(Clone, Debug, Default)]
struct ParaStyleDef {
    /// Display name.
    name: String,
    /// Parent style ID.
    based_on: Option<String>,
    /// Properties.
    props: RawParaProps,
}

/// A named character style.
#[derive(Clone, Debug, Default)]
struct RunStyleDef {
    /// Parent style ID.
    based_on: Option<String>,
    /// Properties.
    props: RawRunProps,
}

/// Conditional table style overrides.
#[derive(Clone, Debug, Default)]
pub(crate) struct CellConditional {
    /// Conditional cell shading.
    pub shading: Option<Rgba>,
}

/// Parsed style sheet.
#[derive(Clone, Debug, Default)]
pub(crate) struct Styles {
    /// Document default paragraph properties.
    default_para: RawParaProps,
    /// Document default character properties.
    default_run: RawRunProps,
    /// Named paragraph styles.
    paragraph: HashMap<String, ParaStyleDef>,
    /// Named character styles.
    character: HashMap<String, RunStyleDef>,
    /// Named table styles.
    table: HashMap<String, RawTableProps>,
    /// Conditional table style overrides, keyed by style ID and condition type.
    table_conditional: HashMap<String, HashMap<String, CellConditional>>,
    /// Implicit default paragraph style ID (w:default="1").
    default_para_id: Option<String>,
    /// Implicit default character style ID.
    default_run_id: Option<String>,
    /// Theme fonts.
    pub theme: ThemeFonts,
}

impl Styles {
    /// Parse `styles.xml`.
    pub fn parse(text: &str, theme: ThemeFonts) -> Result<Self, DocxError> {
        let mut styles = Self {
            theme,
            ..Self::default()
        };
        let mut reader = XmlReader::from_str(text);
        loop {
            match reader.read_event().map_err(crate::xml::xml_error)? {
                quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                    "docDefaults" => parse_doc_defaults(&mut reader, &mut styles)?,
                    "style" => parse_style(&mut reader, &element, &mut styles)?,
                    // Top-level containers such as `w:styles` carry no style
                    // definitions themselves, so ignore them.
                    _ => {}
                },
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
        Ok(styles)
    }

    /// Resolve paragraph properties: document defaults → style chain (root to leaf) →
    /// direct formatting.
    pub fn resolve_para(&self, direct: &RawParaProps) -> RawParaProps {
        let mut resolved = self.default_para.clone();
        let id = direct
            .style_id
            .as_deref()
            .or(self.default_para_id.as_deref());
        for style in self.para_chain(id).into_iter().rev() {
            resolved.merge(&style.props);
        }
        resolved.merge(direct);
        resolved
    }

    /// Resolve character properties: document defaults → paragraph style character
    /// properties → character style chain → direct formatting.
    pub fn resolve_run(&self, paragraph: &RawParaProps, direct: &RawRunProps) -> RawRunProps {
        let mut resolved = self.default_run.clone();
        if let Some(mark) = &paragraph.mark {
            resolved.merge(mark);
        }
        let id = direct
            .style_id
            .as_deref()
            .or(self.default_run_id.as_deref());
        for style in self.run_chain(id).into_iter().rev() {
            resolved.merge(&style.props);
        }
        resolved.merge(direct);
        resolved
    }

    /// Display name of a named paragraph style.
    pub fn paragraph_name(&self, id: &str) -> Option<&str> {
        self.paragraph.get(id).map(|style| style.name.as_str())
    }

    /// Resolve table properties: style chain (root to leaf) → direct formatting.
    pub fn resolve_table(&self, direct: &RawTableProps) -> RawTableProps {
        let mut resolved = RawTableProps::default();
        let mut chain: Vec<&RawTableProps> = Vec::new();
        let mut current = direct.style_id.as_deref().and_then(|id| self.table.get(id));
        while let Some(style) = current {
            if chain.len() >= 8 || chain.iter().any(|item| std::ptr::eq(*item, style)) {
                break;
            }
            chain.push(style);
            current = style.style_id.as_deref().and_then(|id| self.table.get(id));
        }
        for style in chain.into_iter().rev() {
            resolved.merge(style);
        }
        resolved.merge(direct);
        resolved
    }

    /// Conditional overrides of a named table style.
    pub fn table_conditional(&self, id: &str) -> Option<&HashMap<String, CellConditional>> {
        self.table_conditional.get(id)
    }

    fn para_chain(&self, id: Option<&str>) -> Vec<&ParaStyleDef> {
        let mut chain: Vec<&ParaStyleDef> = Vec::new();
        let mut current = id.and_then(|id| self.paragraph.get(id));
        while let Some(style) = current {
            if chain.len() >= 8 || chain.iter().any(|item| std::ptr::eq(*item, style)) {
                break;
            }
            chain.push(style);
            current = style
                .based_on
                .as_deref()
                .and_then(|id| self.paragraph.get(id));
        }
        chain
    }

    fn run_chain(&self, id: Option<&str>) -> Vec<&RunStyleDef> {
        let mut chain: Vec<&RunStyleDef> = Vec::new();
        let mut current = id.and_then(|id| self.character.get(id));
        while let Some(style) = current {
            if chain.len() >= 8 || chain.iter().any(|item| std::ptr::eq(*item, style)) {
                break;
            }
            chain.push(style);
            current = style
                .based_on
                .as_deref()
                .and_then(|id| self.character.get(id));
        }
        chain
    }
}

fn parse_doc_defaults(reader: &mut XmlReader<'_>, styles: &mut Styles) -> Result<(), DocxError> {
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "rPrDefault" => styles.default_run = parse_default_run(reader)?,
                "pPrDefault" => styles.default_para = parse_default_para(reader)?,
                _ => skip_element(reader, &element)?,
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

fn parse_default_run(reader: &mut XmlReader<'_>) -> Result<RawRunProps, DocxError> {
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "rPr" => return parse_run_props(reader, &element),
                _ => skip_element(reader, &element)?,
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok(RawRunProps::default())
}

fn parse_default_para(reader: &mut XmlReader<'_>) -> Result<RawParaProps, DocxError> {
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "pPr" => return parse_para_props(reader, &element),
                _ => skip_element(reader, &element)?,
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok(RawParaProps::default())
}

fn parse_style(
    reader: &mut XmlReader<'_>,
    start: &quick_xml::events::BytesStart<'_>,
    styles: &mut Styles,
) -> Result<(), DocxError> {
    let kind = attr(start, "type").unwrap_or_default();
    let Some(id) = attr(start, "styleId") else {
        return skip_element(reader, start);
    };
    // `w:default="1"` marks a default style applied implicitly; the on/off semantics of
    // the other attributes do not apply here.
    let is_default = matches!(attr(start, "default").as_deref(), Some("1" | "true" | "on"));

    let mut name = String::new();
    let mut based_on = None;
    let mut para = RawParaProps::default();
    let mut run = RawRunProps::default();
    let mut table = RawTableProps::default();
    let mut conditional: HashMap<String, CellConditional> = HashMap::new();
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "pPr" => para = parse_para_props(reader, &element)?,
                "rPr" => run = parse_run_props(reader, &element)?,
                "tblPr" => table = parse_table_props(reader, &element)?,
                "tblStylePr" => {
                    let (kind, cond) = parse_table_conditional(reader, &element)?;
                    conditional.insert(kind, cond);
                }
                _ => skip_element(reader, &element)?,
            },
            quick_xml::events::Event::Empty(element) => match local_name(&element).as_str() {
                "name" => name = attr(&element, "val").unwrap_or_default(),
                "basedOn" => based_on = attr(&element, "val"),
                _ => {}
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }

    match kind.as_str() {
        "paragraph" => {
            para.style_id = None;
            // The rPr inside a paragraph style holds that style's default character
            // properties, which runs inherit.
            para.mark = Some(run);
            if is_default {
                styles.default_para_id = Some(id.clone());
            }
            styles.paragraph.insert(
                id,
                ParaStyleDef {
                    name,
                    based_on,
                    props: para,
                },
            );
        }
        "character" => {
            run.style_id = None;
            if is_default {
                styles.default_run_id = Some(id.clone());
            }
            styles.character.insert(
                id,
                RunStyleDef {
                    based_on,
                    props: run,
                },
            );
        }
        "table" => {
            table.style_id = based_on;
            styles.table.insert(id.clone(), table);
            if !conditional.is_empty() {
                styles.table_conditional.insert(id, conditional);
            }
        }
        _ => {}
    }
    Ok(())
}

/// Parse one `w:tblStylePr` block and return its condition type.
fn parse_table_conditional(
    reader: &mut XmlReader<'_>,
    start: &quick_xml::events::BytesStart<'_>,
) -> Result<(String, CellConditional), DocxError> {
    let kind = attr(start, "type").unwrap_or_default();
    let mut conditional = CellConditional::default();
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "tcPr" => conditional.shading = parse_conditional_shading(reader)?,
                _ => skip_element(reader, &element)?,
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok((kind, conditional))
}

/// Parse the shading inside a conditional `w:tcPr`.
fn parse_conditional_shading(reader: &mut XmlReader<'_>) -> Result<Option<Rgba>, DocxError> {
    let mut shading = None;
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) => skip_element(reader, &element)?,
            quick_xml::events::Event::Empty(element) => {
                if local_name(&element).as_str() == "shd" {
                    shading = attr(&element, "fill").as_deref().and_then(parse_color);
                }
            }
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok(shading)
}

/// Parse the font scheme in `theme1.xml`.
pub(crate) fn parse_theme(text: &str) -> ThemeFonts {
    let mut theme = ThemeFonts::default();
    let mut reader = XmlReader::from_str(text);
    // Whether the major or the minor font group is currently being read.
    let mut major = false;
    while let Ok(event) = reader.read_event() {
        match event {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "majorFont" => major = true,
                "minorFont" => major = false,
                _ => {}
            },
            quick_xml::events::Event::Empty(element) => {
                let tag = local_name(&element);
                let Some(typeface) = attr(&element, "typeface") else {
                    continue;
                };
                if typeface.trim().is_empty() {
                    continue;
                }
                let hans = attr(&element, "script").as_deref() == Some("Hans");
                match (tag.as_str(), major) {
                    ("latin", true) => theme.major_latin = Some(typeface),
                    ("latin", false) => theme.minor_latin = Some(typeface),
                    ("ea", true) => theme.major_east_asia = Some(typeface),
                    ("ea", false) => theme.minor_east_asia = Some(typeface),
                    ("font", true) if hans => theme.major_east_asia = Some(typeface),
                    ("font", false) if hans => theme.minor_east_asia = Some(typeface),
                    _ => {}
                }
            }
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    theme
}

/// Whether the style name is a heading style; returns its level.
pub(crate) fn heading_level_from_name(name: &str) -> Option<u8> {
    let lower = name.trim().to_ascii_lowercase();
    if matches!(lower.as_str(), "title" | "标题" | "副标题" | "subtitle") {
        return None;
    }
    for pattern in ["heading", "标题 "] {
        if let Some(rest) = lower.strip_prefix(pattern) {
            let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
            if let Ok(level) = digits.parse::<u8>()
                && (1..=9).contains(&level)
            {
                return Some(level);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use ofdbox_folio_ir::{Align, Twips};

    use super::{Styles, heading_level_from_name, parse_theme};
    use crate::props::{RawParaProps, RawRunProps, parse_para_props};
    use crate::xml::XmlReader;

    const STYLES: &str = r#"
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:docDefaults>
    <w:rPrDefault><w:rPr><w:sz w:val="22"/><w:rFonts w:ascii="Calibri" w:eastAsia="宋体"/></w:rPr></w:rPrDefault>
    <w:pPrDefault><w:pPr><w:spacing w:after="200" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault>
  </w:docDefaults>
  <w:style w:type="paragraph" w:default="1" w:styleId="Normal">
    <w:name w:val="Normal"/><w:rPr><w:rFonts w:ascii="SimSun"/><w:sz w:val="22"/></w:rPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="Heading1">
    <w:name w:val="heading 1"/><w:basedOn w:val="Normal"/>
    <w:pPr><w:outlineLvl w:val="0"/><w:spacing w:before="480" w:after="0"/></w:pPr>
    <w:rPr><w:b/><w:sz w:val="28"/></w:rPr>
  </w:style>
</w:styles>"#;

    /// Parse the first `pPr` in `xml`.
    fn para_props(xml: &str) -> RawParaProps {
        let mut reader = XmlReader::from_str(xml);
        loop {
            match reader.read_event().expect("event") {
                quick_xml::events::Event::Start(element) => {
                    if crate::xml::is_element(&element, "pPr") {
                        return parse_para_props(&mut reader, &element).expect("pPr");
                    }
                }
                quick_xml::events::Event::Eof => panic!("no pPr"),
                _ => {}
            }
        }
    }

    #[test]
    fn resolves_style_chain_and_doc_defaults() {
        let styles = Styles::parse(STYLES, Default::default()).expect("styles");
        let props =
            para_props(r#"<w:pPr><w:pStyle w:val="Heading1"/><w:jc w:val="center"/></w:pPr>"#);
        let resolved = styles.resolve_para(&props);
        // The explicitly written back 0 overrides the document default 200, while space
        // before is inherited as 480 from the style.
        assert_eq!(resolved.space_after.map(|value| value.0), Some(0));
        assert_eq!(resolved.space_before.map(|value| value.0), Some(480));
        assert_eq!(resolved.align, Some(Align::Center));
        assert_eq!(resolved.outline_level, Some(0));

        let run = styles.resolve_run(&resolved, &RawRunProps::default());
        let style = run.to_style(&styles.theme);
        assert!(style.bold);
        assert_eq!(style.size, Twips::from_pt(14.0));
        // Normal's rPr specifies SimSun and Heading1 specifies no typeface, so it should
        // be inherited.
        assert_eq!(style.family.name, "SimSun");
    }

    #[test]
    fn parses_theme_fonts() {
        let xml = r#"
<a:theme>
 <a:fontScheme>
  <a:majorFont><a:latin typeface="Cambria"/><a:ea typeface=""/><a:font script="Hans" typeface="黑体"/></a:majorFont>
  <a:minorFont><a:latin typeface="Calibri"/><a:ea typeface=""/></a:minorFont>
 </a:fontScheme>
</a:theme>"#;
        let theme = parse_theme(xml);
        assert_eq!(theme.major_latin.as_deref(), Some("Cambria"));
        assert_eq!(theme.major_east_asia.as_deref(), Some("黑体"));
        assert_eq!(theme.minor_latin.as_deref(), Some("Calibri"));
        assert_eq!(theme.minor_east_asia, None);
    }

    #[test]
    fn recognises_heading_names() {
        assert_eq!(heading_level_from_name("heading 1"), Some(1));
        assert_eq!(heading_level_from_name("Heading 3"), Some(3));
        assert_eq!(heading_level_from_name("标题 2"), Some(2));
        assert_eq!(heading_level_from_name("Normal"), None);
        assert_eq!(heading_level_from_name("Title"), None);
    }
}

#[cfg(test)]
mod conditional_tests {
    use super::*;

    #[test]
    fn parses_conditional_table_shading() {
        let xml = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:style w:type="table" w:styleId="Grid">
                <w:tblPr/>
                <w:tblStylePr w:type="firstRow">
                    <w:tcPr><w:shd w:val="clear" w:fill="FF0000"/></w:tcPr>
                </w:tblStylePr>
            </w:style>
        </w:styles>"#;
        let styles = Styles::parse(xml, ThemeFonts::default()).expect("styles");
        let conditional = styles.table_conditional("Grid").expect("conditional");
        let shading = conditional
            .get("firstRow")
            .and_then(|value| value.shading)
            .expect("first row shading");
        assert_eq!((shading.r, shading.g, shading.b), (255, 0, 0));
    }
}
