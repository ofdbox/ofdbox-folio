//! Glyph name and encoding helpers for text layer recovery.
//!
//! Simple fonts without a `/ToUnicode` map still carry enough information to recover Unicode:
//! the character code is mapped to a glyph name by the PDF `/Encoding` (or a base encoding),
//! and the glyph name follows the Adobe naming conventions. This module resolves that chain.

use std::collections::HashMap;

use lopdf::Object;

use crate::glyph_names_data::{AGL, MACROMANENCODING, STANDARDENCODING, WINANSIENCODING};

/// PDF base encodings that are used when a simple font does not define `/Differences`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BaseEncoding {
    /// Adobe StandardEncoding (Type 1 default).
    Standard,
    /// WinAnsiEncoding.
    WinAnsi,
    /// MacRomanEncoding.
    MacRoman,
}

impl BaseEncoding {
    /// Resolves the encoding from a PDF name such as `WinAnsiEncoding`.
    pub(crate) fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"StandardEncoding" => Some(Self::Standard),
            b"WinAnsiEncoding" => Some(Self::WinAnsi),
            b"MacRomanEncoding" => Some(Self::MacRoman),
            _ => None,
        }
    }

    fn glyph_name(self, code: u8) -> Option<&'static str> {
        let table: &[Option<&'static str>; 256] = match self {
            Self::Standard => &STANDARDENCODING,
            Self::WinAnsi => &WINANSIENCODING,
            Self::MacRoman => &MACROMANENCODING,
        };
        table[usize::from(code)]
    }
}

/// Resolves a glyph name to a single Unicode character, following the Adobe conventions.
pub(crate) fn name_to_unicode(name: &str) -> Option<char> {
    if name.is_empty() {
        return None;
    }
    // Strip a stylistic suffix such as `A.sc` or `a.alt`; try the verbatim name first.
    if let Some(character) = raw_name_to_unicode(name) {
        return Some(character);
    }
    if let Some((base, _)) = name.split_once('.')
        && !base.is_empty()
    {
        return raw_name_to_unicode(base);
    }
    None
}

fn raw_name_to_unicode(name: &str) -> Option<char> {
    if let Some(rest) = name.strip_prefix("uni") {
        // A single `uniXXXX` name maps to one character; ligature forms hold several.
        if rest.len() == 4
            && let Ok(value) = u32::from_str_radix(rest, 16)
            && let Some(character) = char::from_u32(value)
        {
            return Some(character);
        }
        return None;
    }
    if let Some(rest) = name.strip_prefix('u')
        && (4..=6).contains(&rest.len())
        && rest.chars().all(|character| character.is_ascii_hexdigit())
        && let Ok(value) = u32::from_str_radix(rest, 16)
        && let Some(character) = char::from_u32(value)
    {
        return Some(character);
    }
    AGL.binary_search_by(|(candidate, _)| (*candidate).cmp(name))
        .ok()
        .map(|index| AGL[index].1)
}

/// Resolves a simple-font character code to Unicode through `/Encoding` and glyph names.
pub(crate) fn code_to_unicode(
    code: u8,
    base: Option<BaseEncoding>,
    differences: &HashMap<u8, String>,
) -> Option<char> {
    if let Some(name) = differences.get(&code) {
        return name_to_unicode(name);
    }
    base.and_then(|base| base.glyph_name(code))
        .and_then(name_to_unicode)
}

/// Parses a simple font `/Encoding` value into its base encoding and `/Differences` names.
pub(crate) fn parse_encoding(
    encoding: &Object,
    default_base: BaseEncoding,
) -> (BaseEncoding, HashMap<u8, String>) {
    let mut base = default_base;
    let mut differences = HashMap::new();
    let dictionary = match encoding {
        Object::Name(name) => {
            if let Some(found) = BaseEncoding::from_name(name) {
                base = found;
            }
            return (base, differences);
        }
        Object::Dictionary(dictionary) => dictionary,
        _ => return (base, differences),
    };
    if let Ok(object) = dictionary.get(b"BaseEncoding")
        && let Ok(name) = object.as_name()
        && let Some(found) = BaseEncoding::from_name(name)
    {
        base = found;
    }
    if let Some(array) = dictionary
        .get(b"Differences")
        .ok()
        .and_then(|object| object.as_array().ok())
    {
        let mut code = 0_u8;
        for object in array {
            if let Ok(value) = object.as_i64() {
                code = u8::try_from(value.clamp(0, 255)).unwrap_or(0);
            } else if let Ok(name) = object.as_name() {
                differences.insert(code, String::from_utf8_lossy(name).into_owned());
                code = code.saturating_add(1);
            }
        }
    }
    (base, differences)
}

/// Builds the full code-to-Unicode map for a simple font.
pub(crate) fn code_unicode_map(
    encoding: Option<&Object>,
    default_base: BaseEncoding,
) -> HashMap<u32, char> {
    let (base, differences) = match encoding {
        Some(encoding) => parse_encoding(encoding, default_base),
        None => (default_base, HashMap::new()),
    };
    let mut map = HashMap::new();
    for code in 0..=255_u8 {
        if let Some(character) = code_to_unicode(code, Some(base), &differences) {
            map.insert(u32::from(code), character);
        }
    }
    // Differences may reference glyph names without a base table entry.
    for code in differences.keys() {
        if !map.contains_key(&u32::from(*code))
            && let Some(character) = code_to_unicode(*code, None, &differences)
        {
            map.insert(u32::from(*code), character);
        }
    }
    map
}

/// Picks the default base encoding for a simple font subtype.
pub(crate) fn default_base_for(subtype: &[u8]) -> BaseEncoding {
    match subtype {
        b"TrueType" => BaseEncoding::WinAnsi,
        _ => BaseEncoding::Standard,
    }
}

#[cfg(test)]
mod tests {
    use super::{BaseEncoding, code_unicode_map, name_to_unicode, parse_encoding};
    use lopdf::{Dictionary, Object};

    #[test]
    fn resolves_conventional_glyph_names() {
        assert_eq!(name_to_unicode("uni4E2D"), Some('中'));
        assert_eq!(name_to_unicode("u1F600"), Some('😀'));
        assert_eq!(name_to_unicode("A"), Some('A'));
        assert_eq!(name_to_unicode("space"), Some(' '));
        assert_eq!(name_to_unicode("quotesingle"), Some('\''));
        assert_eq!(name_to_unicode("A.sc"), Some('A'));
        assert_eq!(name_to_unicode("G21"), None);
        assert_eq!(name_to_unicode("cid1234"), None);
    }

    #[test]
    fn resolves_base_encoding_and_differences() {
        let mut dictionary = Dictionary::new();
        dictionary.set("BaseEncoding", Object::Name(b"WinAnsiEncoding".to_vec()));
        dictionary.set(
            "Differences",
            Object::Array(vec![
                Object::Integer(33),
                Object::Name(b"uni4E2D".to_vec()),
                Object::Name(b"uni6587".to_vec()),
            ]),
        );
        let map = code_unicode_map(
            Some(&Object::Dictionary(dictionary.clone())),
            BaseEncoding::Standard,
        );
        assert_eq!(map.get(&33), Some(&'中'));
        assert_eq!(map.get(&34), Some(&'文'));
        assert_eq!(map.get(&36), Some(&'$'));

        let (base, differences) =
            parse_encoding(&Object::Dictionary(dictionary), BaseEncoding::Standard);
        assert_eq!(base, BaseEncoding::WinAnsi);
        assert_eq!(differences.get(&33).map(String::as_str), Some("uni4E2D"));
    }

    #[test]
    fn opaque_names_do_not_invent_unicode() {
        let mut dictionary = Dictionary::new();
        dictionary.set(
            "Differences",
            Object::Array(vec![
                Object::Integer(33),
                Object::Name(b"G21".to_vec()),
                Object::Name(b"G22".to_vec()),
            ]),
        );
        let map = code_unicode_map(
            Some(&Object::Dictionary(dictionary)),
            BaseEncoding::Standard,
        );
        assert!(!map.contains_key(&33));
        assert!(!map.contains_key(&34));
        // Codes without /Differences still resolve through StandardEncoding.
        assert_eq!(map.get(&65), Some(&'A'));
    }
}
