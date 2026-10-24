//! `numbering.xml`：列表编号与项目符号。

use std::collections::HashMap;

use crate::xml::{XmlReader, attr, attr_i32, attr_u32, is_element, local_name, skip_element};

/// 编号格式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MarkerFormat {
    /// 项目符号。
    #[default]
    Bullet,
    /// 阿拉伯数字。
    Decimal,
    /// 小写字母。
    LowerLetter,
    /// 大写字母。
    UpperLetter,
    /// 小写罗马数字。
    LowerRoman,
    /// 大写罗马数字。
    UpperRoman,
    /// 不显示编号。
    None,
}

/// 一级编号定义。
#[derive(Clone, Debug)]
pub(crate) struct Level {
    /// 格式。
    pub format: MarkerFormat,
    /// 编号模板，例如 `%1.`。
    pub text: String,
    /// 起始值。
    pub start: u32,
    /// 左缩进（twip）。
    pub left: Option<i64>,
    /// 悬挂缩进（twip）。
    pub hanging: Option<i64>,
}

/// 一级的编号定义。
#[derive(Clone, Debug, Default)]
pub(crate) struct Numbering {
    /// `(numId, ilvl)` → 定义。
    levels: HashMap<(u32, u8), Level>,
}

/// 编号计数器：每个 `(numId, ilvl)` 当前的值。
#[derive(Clone, Debug, Default)]
pub(crate) struct Counters {
    /// 当前值。
    values: HashMap<(u32, u8), u32>,
}

impl Counters {
    /// 进入一个编号段落，返回该段的标记文本。
    pub fn next(&mut self, numbering: &Numbering, num_id: u32, level: u8) -> Option<String> {
        let definition = numbering.levels.get(&(num_id, level))?;
        if definition.format == MarkerFormat::None {
            return None;
        }
        // 回到某级时，更深层重新计数。
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
            let number = if index == level {
                value
            } else {
                self.values
                    .get(&(num_id, index))
                    .copied()
                    .unwrap_or(definition.start)
            };
            marker.push_str(&format_number(definition.format, number));
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

impl Numbering {
    /// 解析 `numbering.xml`。
    pub fn parse(text: &str) -> Self {
        let mut numbering = Self::default();
        // abstractNumId → 各级定义。
        let mut abstracts: HashMap<u32, HashMap<u8, Level>> = HashMap::new();
        // numId → abstractNumId。
        let mut nums: HashMap<u32, u32> = HashMap::new();
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
                            if let Some(abstract_id) = parse_num(&mut reader) {
                                nums.insert(id, abstract_id);
                            }
                        }
                        _ => {
                            // 顶层容器（如 `w:numbering`）本身不携带信息，直接忽略；
                            // 只关心 `abstractNum` 与 `num`。
                        }
                    }
                }
                Ok(quick_xml::events::Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
        for (num_id, abstract_id) in nums {
            if let Some(levels) = abstracts.get(&abstract_id) {
                for (level, definition) in levels {
                    numbering
                        .levels
                        .insert((num_id, *level), definition.clone());
                }
            }
        }
        numbering
    }

    /// 取该 `(numId, ilvl)` 的定义。
    pub fn level(&self, num_id: u32, level: u8) -> Option<&Level> {
        self.levels.get(&(num_id, level))
    }
}

/// 把 Symbol/Wingdings 的私用区码位换成普通字符。
///
/// Word 的项目符号常写成 `<w:lvlText w:val="\u{F0B7}">` 并配 Symbol 字体，
/// 直接输出会变成缺字形；这里映射成常用符号。
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

/// 读取编号级别里的缩进（`w:lvl/w:pPr/w:ind`）。
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

fn parse_num(reader: &mut XmlReader<'_>) -> Option<u32> {
    let mut abstract_id = None;
    loop {
        match reader.read_event().ok()? {
            quick_xml::events::Event::Start(element) => {
                let _ = skip_element(reader, &element);
            }
            quick_xml::events::Event::Empty(element) => {
                if is_element(&element, "abstractNumId") {
                    abstract_id = attr_u32(&element, "val");
                }
            }
            quick_xml::events::Event::End(_) => break,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    abstract_id
}

/// 把计数器值格式化成该编号格式的文本。
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
        // 回到 0 级后，1 级重新从 a 开始。
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
