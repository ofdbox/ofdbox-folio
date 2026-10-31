//! `numbering.xml`: list numbering and bullets.

use std::collections::HashMap;

use crate::xml::{XmlReader, attr, attr_i32, attr_u32, is_element, local_name, skip_element};

/// Numbering format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MarkerFormat {
    /// Bullet symbol.
    #[default]
    Bullet,
    /// Arabic numerals.
    Decimal,
    /// Lowercase letters.
    LowerLetter,
    /// Uppercase letters.
    UpperLetter,
    /// Lowercase Roman numerals.
    LowerRoman,
    /// Uppercase Roman numerals.
    UpperRoman,
    /// No numbering displayed.
    None,
}

/// Definition of one numbering level.
#[derive(Clone, Debug)]
pub(crate) struct Level {
    /// Format.
    pub format: MarkerFormat,
    /// Numbering template, for example `%1.`.
    pub text: String,
    /// Start value.
    pub start: u32,
    /// Left indent (twip).
    pub left: Option<i64>,
    /// Hanging indent (twip).
    pub hanging: Option<i64>,
}

/// Per-level numbering definitions.
#[derive(Clone, Debug, Default)]
pub(crate) struct Numbering {
    /// `(numId, ilvl)` → definition.
    levels: HashMap<(u32, u8), Level>,
}

/// Numbering counters: the current value of each `(numId, ilvl)`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Counters {
    /// Current value.
    values: HashMap<(u32, u8), u32>,
}

impl Counters {
    /// Enter a numbered paragraph and return that paragraph's marker text.
    pub fn next(&mut self, numbering: &Numbering, num_id: u32, level: u8) -> Option<String> {
        let definition = numbering.levels.get(&(num_id, level))?;
        if definition.format == MarkerFormat::None {
            return None;
        }
        // When returning to a level, deeper levels restart their count.
        self.values
            .retain(|(id, ilvl), _| !(*id == num_id && *ilvl > level));
        let entry = self
            .values
            .entry((num_id, level))
            .or_insert(definition.start.saturating_sub(1));
        *entry += 1;
        let value = *entry;
        let mut marker = String::new();
        let mut chars = definition.text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '%' {
                marker.push(ch);
                continue;
            }
            let mut digits = String::new();
            while let Some(next) = chars.peek().copied().filter(char::is_ascii_digit) {
                digits.push(next);
                chars.next();
            }
            if digits.is_empty() {
                marker.push('%');
                continue;
            }
            let index: u8 = digits.parse::<u8>().unwrap_or(1).saturating_sub(1);
            let referenced = numbering.levels.get(&(num_id, index));
            let referenced_format = referenced
                .map(|level| level.format)
                .unwrap_or(definition.format);
            let number = if index == level {
                value
            } else if let Some(value) = self.values.get(&(num_id, index)) {
                *value
            } else {
                referenced
                    .map(|level| level.start)
                    .unwrap_or(definition.start)
            };
            marker.push_str(&format_number(referenced_format, number));
        }
        if definition.format == MarkerFormat::Bullet {
            marker = normalise_bullet(&marker);
        }
        if marker.trim().is_empty() {
            marker = match definition.format {
                MarkerFormat::Bullet => "•".to_owned(),
                _ => format_number(definition.format, value),
            };
        }
        Some(marker)
    }
}

/// Per-instance overrides of one numbering level.
#[derive(Clone, Debug, Default)]
struct LevelOverride {
    /// Replacement start value.
    start: Option<u32>,
    /// Replacement level definition.
    level: Option<Level>,
}

/// Parsed `w:num` entry.
#[derive(Clone, Debug, Default)]
struct NumDef {
    /// Referenced abstract numbering definition.
    abstract_id: Option<u32>,
    /// Per-level overrides.
    overrides: HashMap<u8, LevelOverride>,
}

impl Numbering {
    /// Parse `numbering.xml`.
    pub fn parse(text: &str) -> Self {
        let mut numbering = Self::default();
        // abstractNumId → per-level definitions.
        let mut abstracts: HashMap<u32, HashMap<u8, Level>> = HashMap::new();
        // numId → parsed num definition.
        let mut nums: HashMap<u32, NumDef> = HashMap::new();
        let mut reader = XmlReader::from_str(text);
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Start(element)) => {
                    match local_name(&element).as_str() {
                        "abstractNum" => {
                            let Some(id) = attr_u32(&element, "abstractNumId") else {
                                continue;
                            };
                            let levels = parse_abstract(&mut reader).unwrap_or_default();
                            abstracts.insert(id, levels);
                        }
                        "num" => {
                            let Some(id) = attr_u32(&element, "numId") else {
                                continue;
                            };
                            if let Some(definition) = parse_num(&mut reader) {
                                nums.insert(id, definition);
                            }
                        }
                        _ => {
                            // Top-level containers (such as `w:numbering`) carry no
                            // information themselves, so ignore them; only `abstractNum`
                            // and `num` matter.
                        }
                    }
                }
                Ok(quick_xml::events::Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
        for (num_id, definition) in nums {
            let Some(abstract_id) = definition.abstract_id else {
                continue;
            };
            if let Some(levels) = abstracts.get(&abstract_id) {
                for (level, base) in levels {
                    let mut resolved = base.clone();
                    if let Some(over) = definition.overrides.get(level) {
                        if let Some(start) = over.start {
                            resolved.start = start;
                        }
                        if let Some(replacement) = &over.level {
                            resolved = replacement.clone();
                            if let Some(start) = over.start {
                                resolved.start = start;
                            }
                        }
                    }
                    numbering.levels.insert((num_id, *level), resolved);
                }
            }
        }
        numbering
    }

    /// Get the definition of this `(numId, ilvl)`.
    pub fn level(&self, num_id: u32, level: u8) -> Option<&Level> {
        self.levels.get(&(num_id, level))
    }
}

/// Map Symbol/Wingdings private-use code points to ordinary characters.
///
/// Word often writes bullets as `<w:lvlText w:val="\u{F0B7}">` with a Symbol font,
/// which would come out as a missing glyph if emitted directly; map them to common
/// symbols here.
fn normalise_bullet(text: &str) -> String {
    let mut result = String::new();
    for ch in text.chars() {
        result.push(match ch {
            '\u{F0B7}' => '•',
            '\u{F0A7}' => '▪',
            '\u{F0D8}' => '➢',
            '\u{F0FC}' => '✓',
            '\u{F0A8}' => '◆',
            other if ('\u{F000}'..='\u{F0FF}').contains(&other) => '•',
            other => other,
        });
    }
    result
}

fn parse_abstract(reader: &mut XmlReader<'_>) -> Option<HashMap<u8, Level>> {
    let mut levels = HashMap::new();
    loop {
        match reader.read_event().ok()? {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "lvl" => {
                    let level = attr_u32(&element, "ilvl").unwrap_or(0) as u8;
                    if let Some(definition) = parse_level(reader) {
                        levels.insert(level, definition);
                    }
                }
                _ => {
                    let _ = skip_element(reader, &element);
                }
            },
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Some(levels)
}

fn parse_level(reader: &mut XmlReader<'_>) -> Option<Level> {
    let mut definition = Level {
        format: MarkerFormat::Bullet,
        text: String::new(),
        start: 1,
        left: None,
        hanging: None,
    };
    loop {
        match reader.read_event().ok()? {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "pPr" => parse_level_para(reader, &mut definition),
                _ => {
                    let _ = skip_element(reader, &element);
                }
            },
            quick_xml::events::Event::Empty(element) => {
                let tag = local_name(&element);
                match tag.as_str() {
                    "start" => definition.start = attr_u32(&element, "val").unwrap_or(1),
                    "numFmt" => {
                        definition.format = match attr(&element, "val").as_deref() {
                            Some("bullet") => MarkerFormat::Bullet,
                            Some("decimal") | Some("decimalZero") => MarkerFormat::Decimal,
                            Some("lowerLetter") => MarkerFormat::LowerLetter,
                            Some("upperLetter") => MarkerFormat::UpperLetter,
                            Some("lowerRoman") => MarkerFormat::LowerRoman,
                            Some("upperRoman") => MarkerFormat::UpperRoman,
                            Some("none") => MarkerFormat::None,
                            _ => MarkerFormat::Decimal,
                        };
                    }
                    "lvlText" => definition.text = attr(&element, "val").unwrap_or_default(),
                    _ => {}
                }
            }
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Some(definition)
}

/// Read the indent inside a numbering level (`w:lvl/w:pPr/w:ind`).
fn parse_level_para(reader: &mut XmlReader<'_>, definition: &mut Level) {
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(element)) => {
                let _ = skip_element(reader, &element);
            }
            Ok(quick_xml::events::Event::Empty(element)) => {
                if is_element(&element, "ind") {
                    if let Some(left) = attr_i32(&element, "left") {
                        definition.left = Some(i64::from(left));
                    }
                    if let Some(hanging) = attr_i32(&element, "hanging") {
                        definition.hanging = Some(i64::from(hanging));
                    }
                }
            }
            Ok(quick_xml::events::Event::End(_)) | Err(_) => break,
            _ => {}
        }
    }
}

fn parse_num(reader: &mut XmlReader<'_>) -> Option<NumDef> {
    let mut definition = NumDef::default();
    loop {
        match reader.read_event().ok()? {
            quick_xml::events::Event::Start(element) => {
                if local_name(&element).as_str() == "lvlOverride"
                    && let Some((level, over)) = parse_lvl_override(reader)
                {
                    definition.overrides.insert(level, over);
                } else {
                    let _ = skip_element(reader, &element);
                }
            }
            quick_xml::events::Event::Empty(element) => {
                if is_element(&element, "abstractNumId") {
                    definition.abstract_id = attr_u32(&element, "val");
                }
            }
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Some(definition)
}

fn parse_lvl_override(reader: &mut XmlReader<'_>) -> Option<(u8, LevelOverride)> {
    let mut level = 0_u8;
    let mut over = LevelOverride::default();
    loop {
        match reader.read_event().ok()? {
            quick_xml::events::Event::Start(element) => match local_name(&element).as_str() {
                "lvl" => {
                    level = attr_u32(&element, "ilvl").unwrap_or(0) as u8;
                    if let Some(definition) = parse_level(reader) {
                        over.level = Some(definition);
                    }
                }
                _ => {
                    let _ = skip_element(reader, &element);
                }
            },
            quick_xml::events::Event::Empty(element) => {
                if is_element(&element, "startOverride") {
                    over.start = attr_u32(&element, "val");
                }
            }
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Some((level, over))
}

/// Format a counter value as text in that numbering format.
fn format_number(format: MarkerFormat, value: u32) -> String {
    match format {
        MarkerFormat::Decimal => value.to_string(),
        MarkerFormat::LowerLetter => letter(value, false),
        MarkerFormat::UpperLetter => letter(value, true),
        MarkerFormat::LowerRoman => roman(value).to_lowercase(),
        MarkerFormat::UpperRoman => roman(value),
        MarkerFormat::Bullet | MarkerFormat::None => String::new(),
    }
}

fn letter(value: u32, upper: bool) -> String {
    let index = value.clamp(1, 26) as u8;
    let base = if upper { b'A' } else { b'a' };
    char::from(base + index - 1).to_string()
}

fn roman(value: u32) -> String {
    const TABLE: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut rest = value;
    let mut text = String::new();
    for (amount, symbol) in TABLE {
        while rest >= amount {
            text.push_str(symbol);
            rest -= amount;
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{Counters, MarkerFormat, Numbering};

    const NUMBERING: &str = r#"
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="·"/></w:lvl>
    <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="o"/></w:lvl>
  </w:abstractNum>
  <w:abstractNum w:abstractNumId="1">
    <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
    <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%2)"/></w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
  <w:num w:numId="5"><w:abstractNumId w:val="1"/></w:num>
</w:numbering>"#;

    #[test]
    fn counts_decimal_markers() {
        let numbering = Numbering::parse(NUMBERING);
        let mut counters = Counters::default();
        assert_eq!(counters.next(&numbering, 5, 0).as_deref(), Some("1."));
        assert_eq!(counters.next(&numbering, 5, 0).as_deref(), Some("2."));
        assert_eq!(counters.next(&numbering, 5, 0).as_deref(), Some("3."));
    }

    #[test]
    fn resets_deeper_levels() {
        let numbering = Numbering::parse(NUMBERING);
        let mut counters = Counters::default();
        assert_eq!(counters.next(&numbering, 5, 0).as_deref(), Some("1."));
        assert_eq!(counters.next(&numbering, 5, 1).as_deref(), Some("a)"));
        assert_eq!(counters.next(&numbering, 5, 1).as_deref(), Some("b)"));
        // After returning to level 0, level 1 starts again from a.
        assert_eq!(counters.next(&numbering, 5, 0).as_deref(), Some("2."));
        assert_eq!(counters.next(&numbering, 5, 1).as_deref(), Some("a)"));
    }

    #[test]
    fn bullets_keep_template_text() {
        let numbering = Numbering::parse(NUMBERING);
        let mut counters = Counters::default();
        assert_eq!(counters.next(&numbering, 1, 0).as_deref(), Some("·"));
        assert_eq!(counters.next(&numbering, 1, 1).as_deref(), Some("o"));
    }

    #[test]
    fn formats_roman_and_letters() {
        assert_eq!(super::format_number(MarkerFormat::LowerRoman, 4), "iv");
        assert_eq!(super::format_number(MarkerFormat::UpperRoman, 9), "IX");
        assert_eq!(super::format_number(MarkerFormat::LowerLetter, 2), "b");
        assert_eq!(super::format_number(MarkerFormat::UpperLetter, 27), "Z");
    }
}

#[cfg(test)]
mod multi_level_tests {
    use super::*;

    #[test]
    fn marker_uses_each_levels_own_format() {
        let xml = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:abstractNum w:abstractNumId="0">
                <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
                <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%1.%2"/></w:lvl>
            </w:abstractNum>
            <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
        </w:numbering>"#;
        let numbering = Numbering::parse(xml);
        let mut counters = Counters::default();
        assert_eq!(counters.next(&numbering, 1, 0), Some("1.".to_owned()));
        assert_eq!(
            counters.next(&numbering, 1, 1),
            Some("1.a".to_owned()),
            "level 2 should use lowerLetter for its own placeholder"
        );
    }

    #[test]
    fn start_override_restarts_a_list_instance() {
        let xml = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:abstractNum w:abstractNumId="0">
                <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
            </w:abstractNum>
            <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
            <w:num w:numId="2">
                <w:abstractNumId w:val="0"/>
                <w:lvlOverride w:ilvl="0"><w:startOverride w:val="5"/></w:lvlOverride>
            </w:num>
        </w:numbering>"#;
        let numbering = Numbering::parse(xml);
        let mut counters = Counters::default();
        assert_eq!(counters.next(&numbering, 1, 0), Some("1.".to_owned()));
        assert_eq!(counters.next(&numbering, 2, 0), Some("5.".to_owned()));
        assert_eq!(counters.next(&numbering, 2, 0), Some("6.".to_owned()));
    }

    #[test]
    fn full_level_override_replaces_format_and_start() {
        let xml = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:abstractNum w:abstractNumId="0">
                <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
            </w:abstractNum>
            <w:num w:numId="1">
                <w:abstractNumId w:val="0"/>
                <w:lvlOverride w:ilvl="0">
                    <w:lvl w:ilvl="0"><w:start w:val="2"/><w:numFmt w:val="upperRoman"/><w:lvlText w:val="(%1)"/></w:lvl>
                </w:lvlOverride>
            </w:num>
        </w:numbering>"#;
        let numbering = Numbering::parse(xml);
        let mut counters = Counters::default();
        assert_eq!(counters.next(&numbering, 1, 0), Some("(II)".to_owned()));
    }
}
