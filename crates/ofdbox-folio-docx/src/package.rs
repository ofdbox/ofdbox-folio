//! DOCX (OPC) package access: read parts on demand, parse relationship tables.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use crate::DocxError;
use crate::DocxReadOptions;

/// A package relationship.
#[derive(Clone, Debug, Default)]
pub(crate) struct Relation {
    /// Target part path, already resolved against the source part's directory into a
    /// package-absolute path.
    pub target: String,
    /// Relationship type URI.
    pub kind: String,
    /// Whether this is an external link (`TargetMode="External"`).
    pub external: bool,
}

/// Read-only DOCX package.
pub(crate) struct Package<'a> {
    archive: zip::ZipArchive<Cursor<&'a [u8]>>,
    options: DocxReadOptions,
}

impl<'a> Package<'a> {
    /// Open the package; parts larger than `max_part_bytes` are refused.
    pub fn open(bytes: &'a [u8], options: &DocxReadOptions) -> Result<Self, DocxError> {
        let archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|error| DocxError::Package(error.to_string()))?;
        Ok(Self {
            archive,
            options: options.clone(),
        })
    }

    /// Read the bytes of one part; returns `None` when it does not exist.
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
                "part {name} is {size} bytes, over the limit of {}",
                self.options.max_part_bytes
            )));
        }
        let mut bytes = Vec::with_capacity(size as usize);
        file.read_to_end(&mut bytes)
            .map_err(|error| DocxError::Package(error.to_string()))?;
        if bytes.len() > self.options.max_part_bytes {
            return Err(DocxError::Limit(format!(
                "part {name} exceeds the limit of {} after decompression",
                self.options.max_part_bytes
            )));
        }
        Ok(Some(bytes))
    }

    /// Read an XML part and decode it into text.
    pub fn read_text(&mut self, name: &str) -> Result<Option<String>, DocxError> {
        let Some(bytes) = self.read(name)? else {
            return Ok(None);
        };
        Ok(Some(decode_xml(&bytes)))
    }

    /// Read the relationship table of a part.
    ///
    /// `part` is a package path such as `word/document.xml`; the relationship table lives
    /// in `_rels/<file name>.rels` in the same directory.
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

/// Decode XML bytes: strip the BOM, fall back to lossy decoding for invalid UTF-8.
fn decode_xml(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Parse `.rels` text.
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
                        // External links (web pages, mail) stay as-is and cannot be
                        // resolved as package paths.
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

/// Resolve a relationship target into a package-absolute path.
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
