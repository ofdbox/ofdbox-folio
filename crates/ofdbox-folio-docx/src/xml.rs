//! quick-xml 小工具：本地名匹配、属性取值与字符引用还原。
//!
//! OOXML 用命名空间前缀区分词汇表（`w:`、`a:`、`wp:`、`r:`），这里一律按本地名
//! 匹配：同一份文档里同一个本地名不会同时属于两个互不相干的词汇表，按本地名
//! 取值比维护前缀映射更稳。

use quick_xml::events::BytesStart;
use quick_xml::reader::Reader;

use crate::DocxError;

/// 统一的事件reader类型：从 `&str` 读，事件借用输入，不分配缓冲区。
pub(crate) type XmlReader<'a> = Reader<&'a [u8]>;

/// 元素的本地名（去掉命名空间前缀）。
pub(crate) fn local_name(element: &BytesStart<'_>) -> String {
    element.local_name().as_ref().to_owned()
}

/// 元素本地名是否等于 `name`。
pub(crate) fn is_element(element: &BytesStart<'_>, name: &str) -> bool {
    element.local_name().as_ref() == name
}

/// 取属性值（按本地名匹配），已还原字符引用。
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

/// 取整数属性。
pub(crate) fn attr_i32(element: &BytesStart<'_>, name: &str) -> Option<i32> {
    attr(element, name)?.trim().parse().ok()
}

/// 取无符号整数属性。
pub(crate) fn attr_u32(element: &BytesStart<'_>, name: &str) -> Option<u32> {
    attr(element, name)?.trim().parse().ok()
}

/// 取浮点属性。
pub(crate) fn attr_f64(element: &BytesStart<'_>, name: &str) -> Option<f64> {
    attr(element, name)?.trim().parse().ok()
}

/// 按 OOXML 的 `ST_OnOff` 规则解释开关元素。
///
/// 没有 `val` 属性就是打开；`0/false/off` 是关闭；其余（含 `1/true/on`）是打开。
pub(crate) fn attr_on_off(element: &BytesStart<'_>) -> Option<bool> {
    Some(match attr(element, "val").as_deref() {
        None => true,
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off"
        ),
    })
}

/// 还原 `&name;` 或 `&#123;` 形式的字符引用；不认识的实体返回 `None`。
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

/// 跳过一个完整元素（含子树）。
///
/// 调用方应已读到该元素的 `Start`；本函数一直消费到与之配对的 `End`。计数必须
/// 覆盖所有嵌套的开始标签，否则会在第一个子元素的结束处提前返回。
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
                return Err(DocxError::Structure(format!("元素 {name} 没有结束标签")));
            }
            _ => {}
        }
    }
}

/// 把 quick-xml 错误转成 crate 错误。
pub(crate) fn xml_error(error: quick_xml::Error) -> DocxError {
    DocxError::Xml(error.to_string())
}
