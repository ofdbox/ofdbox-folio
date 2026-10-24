//! DOCX（OPC）包访问：按需读取部分、解析关系表。

use std::collections::HashMap;
use std::io::{Cursor, Read};

use crate::DocxError;
use crate::DocxReadOptions;

/// 一条包关系。
#[derive(Clone, Debug, Default)]
pub(crate) struct Relation {
    /// 目标部件路径，已按源部件所在目录解析成包内绝对路径。
    pub target: String,
    /// 关系类型 URI。
    pub kind: String,
    /// 是否为外部链接（`TargetMode="External"`）。
    pub external: bool,
}

/// 只读的 DOCX 包。
pub(crate) struct Package<'a> {
    archive: zip::ZipArchive<Cursor<&'a [u8]>>,
    options: DocxReadOptions,
}

impl<'a> Package<'a> {
    /// 打开包；`max_part_bytes` 之外的部分拒绝读取。
    pub fn open(bytes: &'a [u8], options: &DocxReadOptions) -> Result<Self, DocxError> {
        let archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|error| DocxError::Package(error.to_string()))?;
        Ok(Self {
            archive,
            options: options.clone(),
        })
    }

    /// 读取一个部件的字节；不存在时返回 `None`。
    pub fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, DocxError> {
        let Some(index) = self.archive.index_for_name(name) else {
            return Ok(None);
        };
        let mut file = self
            .archive
            .by_index(index)
            .map_err(|error| DocxError::Package(error.to_string()))?;
        let size = file.size();
        if size > self.options.max_part_bytes as u64 {
            return Err(DocxError::Limit(format!(
                "部件 {name} 有 {size} 字节，超过上限 {}",
                self.options.max_part_bytes
            )));
        }
        let mut bytes = Vec::with_capacity(size as usize);
        file.read_to_end(&mut bytes)
            .map_err(|error| DocxError::Package(error.to_string()))?;
        if bytes.len() > self.options.max_part_bytes {
            return Err(DocxError::Limit(format!(
                "部件 {name} 解压后超过上限 {}",
                self.options.max_part_bytes
            )));
        }
        Ok(Some(bytes))
    }

    /// 读取一个 XML 部件并解码成文本。
    pub fn read_text(&mut self, name: &str) -> Result<Option<String>, DocxError> {
        let Some(bytes) = self.read(name)? else {
            return Ok(None);
        };
        Ok(Some(decode_xml(&bytes)))
    }

    /// 读取某个部件的关系表。
    ///
    /// `part` 是包内路径，例如 `word/document.xml`；关系表位于同目录的
    /// `_rels/<文件名>.rels`。
    pub fn relations(&mut self, part: &str) -> Result<HashMap<String, Relation>, DocxError> {
        let (directory, file) = match part.rsplit_once('/') {
            Some((directory, file)) => (directory, file),
            None => ("", part),
        };
        let rels_name = if directory.is_empty() {
            format!("_rels/{file}.rels")
        } else {
            format!("{directory}/_rels/{file}.rels")
        };
        let Some(text) = self.read_text(&rels_name)? else {
            return Ok(HashMap::new());
        };
        parse_relations(&text, directory)
    }
}

/// 解码 XML 字节：去掉 BOM，非法 UTF-8 走有损解码。
fn decode_xml(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// 解析 `.rels` 文本。
fn parse_relations(text: &str, base: &str) -> Result<HashMap<String, Relation>, DocxError> {
    let mut reader = crate::xml::XmlReader::from_str(text);
    let mut relations = HashMap::new();
    loop {
        match reader.read_event().map_err(crate::xml::xml_error)? {
            quick_xml::events::Event::Start(element) | quick_xml::events::Event::Empty(element) => {
                if !crate::xml::is_element(&element, "Relationship") {
                    continue;
                }
                let Some(id) = crate::xml::attr(&element, "Id") else {
                    continue;
                };
                let target = crate::xml::attr(&element, "Target").unwrap_or_default();
                let kind = crate::xml::attr(&element, "Type").unwrap_or_default();
                let external =
                    crate::xml::attr(&element, "TargetMode").as_deref() == Some("External");
                relations.insert(
                    id,
                    Relation {
                        // 外部链接（网页、邮件）保持原样，不能按包内路径解析。
                        target: if external {
                            target
                        } else {
                            resolve_target(base, &target)
                        },
                        kind,
                        external,
                    },
                );
            }
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    Ok(relations)
}

/// 把关系目标解析成包内绝对路径。
fn resolve_target(base: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.trim_start_matches('/').to_owned();
    }
    let mut parts: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::{Relation, resolve_target};

    #[test]
    fn resolves_relative_targets() {
        assert_eq!(
            resolve_target("word", "media/image1.png"),
            "word/media/image1.png"
        );
        assert_eq!(
            resolve_target("word", "../customXml/item1.xml"),
            "customXml/item1.xml"
        );
        assert_eq!(resolve_target("", "word/document.xml"), "word/document.xml");
        assert_eq!(
            resolve_target("word", "/word/media/a.png"),
            "word/media/a.png"
        );
    }

    #[test]
    fn relation_defaults_to_internal() {
        let relation = Relation::default();
        assert!(!relation.external);
        assert!(relation.target.is_empty());
    }
}
