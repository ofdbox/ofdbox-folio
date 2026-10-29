//! quick-xml helpers: local-name matching, attribute access, and character reference
//! resolution.
//!
//! OOXML uses namespace prefixes to separate vocabularies (`w:`, `a:`, `wp:`, `r:`), but
//! everything here is matched by local name: a single local name never belongs to two
//! unrelated vocabularies within one document, so matching by local name is more robust
//! than maintaining a prefix map.

use quick_xml::events::BytesStart;
use quick_xml::reader::Reader;

use crate::DocxError;

/// Unified event reader type: reads from `&str`, events borrow the input, no buffer
/// allocation.
pub(crate) type XmlReader<'a> = Reader<&'a [u8]>;

/// Local name of an element (namespace prefix removed).
pub(crate) fn local_name(element: &BytesStart<'_>) -> String {
    element.local_name().as_ref().to_owned()
}

/// Whether an element's local name equals `name`.
pub(crate) fn is_element(element: &BytesStart<'_>, name: &str) -> bool {
    element.local_name().as_ref() == name
}

/// Get an attribute value (matched by local name) with character references resolved.
pub(crate) fn attr(element: &BytesStart<'_>, name: &str) -> Option<String> {
    element.attributes().flatten().find_map(|attribute| {
        if attribute.key.local_name().as_ref() == name {
            attribute
                .normalized_value(quick_xml::XmlVersion::Explicit1_0)
                .ok()
                .map(|value| value.into_owned())
        } else {
            None
        }
    })
}

/// Get an integer attribute.
pub(crate) fn attr_i32(element: &BytesStart<'_>, name: &str) -> Option<i32> {
    attr(element, name)?.trim().parse().ok()
}

/// Get an unsigned integer attribute.
pub(crate) fn attr_u32(element: &BytesStart<'_>, name: &str) -> Option<u32> {
    attr(element, name)?.trim().parse().ok()
}

/// Get a floating-point attribute.
pub(crate) fn attr_f64(element: &BytesStart<'_>, name: &str) -> Option<f64> {
    attr(element, name)?.trim().parse().ok()
}

/// Interpret an on/off element per the OOXML `ST_OnOff` rules.
///
/// No `val` attribute means on; `0/false/off` means off; anything else (including
/// `1/true/on`) means on.
pub(crate) fn attr_on_off(element: &BytesStart<'_>) -> Option<bool> {
    Some(match attr(element, "val").as_deref() {
        None => true,
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off"
        ),
    })
}

/// Resolve a character reference of the form `&name;` or `&#123;`; unknown entities
/// return `None`.
pub(crate) fn resolve_reference(raw: &str) -> Option<char> {
    match raw {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let digits = raw.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse::<u32>().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Skip a complete element (including its subtree).
///
/// The caller should already have read the element's `Start`; this function consumes up
/// to the matching `End`. The count must cover every nested start tag, otherwise it
/// would return early at the first child element's end.
pub(crate) fn skip_element(
    reader: &mut XmlReader<'_>,
    start: &BytesStart<'_>,
) -> Result<(), DocxError> {
    let name = local_name(start);
    let mut depth = 0_usize;
    loop {
        match reader.read_event().map_err(xml_error)? {
            quick_xml::events::Event::Start(_) => depth += 1,
            quick_xml::events::Event::End(_) => {
                if depth == 0 {
                    return Ok(());
                }
                depth -= 1;
            }
            quick_xml::events::Event::Eof => {
                return Err(DocxError::Structure(format!(
                    "element {name} has no end tag"
                )));
            }
            _ => {}
        }
    }
}

/// Convert a quick-xml error into a crate error.
pub(crate) fn xml_error(error: quick_xml::Error) -> DocxError {
    DocxError::Xml(error.to_string())
}
