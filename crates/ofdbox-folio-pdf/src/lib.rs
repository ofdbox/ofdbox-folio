//! PDF adapter layer for OFDBox Folio.
//!
//! Parses PDF pages into Fixed IR and writes Fixed IR back out as PDF.
//! `lopdf` types are not exposed publicly; patterns, transparency groups,
//! encryption and complex CMaps on the PDF reading side are hardened in
//! stages per the roadmap.

#![allow(clippy::too_many_lines)]

mod backend;
mod catalog;
mod error;
mod frontend;

use std::collections::HashMap;
use std::sync::Arc;

use lopdf::{Document, Object, ObjectId};

use ofdbox_folio_ir::{
    AttachmentId, DocumentInfo, DocumentSource, FontProvider, Metadata, Page, PageError, PageInfo,
    SourceFormat,
};

pub use backend::{
    PdfBackend, PdfMetadata, PdfOptions, PdfOutline, PdfVersion, TextMode, Writer, write_fixed,
};
pub use error::PdfError;
pub use frontend::PdfReadOptions;

/// Probes whether the bytes are a PDF.
///
/// PDF allows a small amount of junk before the header, so `%PDF-` is
/// searched within the first 1024 bytes.
pub fn probe(bytes: &[u8]) -> bool {
    bytes
        .get(..bytes.len().min(1024))
        .is_some_and(|head| head.windows(5).any(|window| window == b"%PDF-"))
}

/// A PDF document source.
pub struct PdfDocument {
    document: Document,
    pages: Vec<ObjectId>,
    info: DocumentInfo,
    store: ofdbox_folio_ir::ResourceStore,
    font_cache: HashMap<Vec<u8>, Option<Arc<frontend::FontState>>>,
    font_provider: Option<Box<dyn FontProvider>>,
    options: PdfReadOptions,
    attachment_objects: Vec<(AttachmentId, ObjectId)>,
}

impl PdfDocument {
    /// Opens a PDF from in-memory bytes.
    pub fn open_bytes(bytes: Vec<u8>, options: PdfReadOptions) -> Result<Self, PdfError> {
        Self::open_bytes_with_fonts(bytes, options, None)
    }

    /// Opens a PDF from in-memory bytes and injects a source for missing fonts.
    pub fn open_bytes_with_fonts(
        bytes: Vec<u8>,
        options: PdfReadOptions,
        font_provider: Option<Box<dyn FontProvider>>,
    ) -> Result<Self, PdfError> {
        let document = load_document(&bytes, options.password.as_deref())?;
        let pages = frontend::page_ids(&document);
        let outline = catalog::outline(&document, &pages);
        let (attachments, attachment_objects) = catalog::attachments(&document);
        let info = DocumentInfo {
            format: SourceFormat::Pdf,
            metadata: metadata(&document),
            page_count: pages.len(),
            outline,
            attachments,
        };
        Ok(Self {
            document,
            pages,
            info,
            store: ofdbox_folio_ir::ResourceStore::new(),
            font_cache: HashMap::new(),
            font_provider,
            options,
            attachment_objects,
        })
    }

    /// Opens a PDF from a file.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_path(
        path: impl AsRef<std::path::Path>,
        options: PdfReadOptions,
    ) -> Result<Self, PdfError> {
        Self::open_path_with_fonts(path, options, None)
    }

    /// Opens a PDF from a file and injects a source for missing fonts.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_path_with_fonts(
        path: impl AsRef<std::path::Path>,
        options: PdfReadOptions,
        font_provider: Option<Box<dyn FontProvider>>,
    ) -> Result<Self, PdfError> {
        let bytes = std::fs::read(path)?;
        Self::open_bytes_with_fonts(bytes, options, font_provider)
    }

    /// Page count.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Resource store.
    pub fn resources(&self) -> &ofdbox_folio_ir::ResourceStore {
        &self.store
    }
}

impl DocumentSource for PdfDocument {
    fn info(&self) -> &DocumentInfo {
        &self.info
    }

    fn page_info(&self, index: usize) -> Result<PageInfo, PageError> {
        let page = self
            .pages
            .get(index)
            .copied()
            .ok_or(PageError::NotFound(index))?;
        frontend::page_info(&self.document, page).map_err(PageError::from)
    }

    fn load_page(&mut self, index: usize) -> Result<Page, PageError> {
        let page = self
            .pages
            .get(index)
            .copied()
            .ok_or(PageError::NotFound(index))?;
        frontend::build_page(
            &self.document,
            page,
            index,
            &self.options,
            &mut self.store,
            &mut self.font_cache,
            &mut self.font_provider,
        )
        .map_err(PageError::from)
    }

    fn load_attachment(&mut self, id: AttachmentId) -> Result<Arc<[u8]>, PageError> {
        let object = self
            .attachment_objects
            .iter()
            .find(|(attachment, _)| *attachment == id)
            .map(|(_, object)| *object)
            .ok_or_else(|| PageError::Decode(format!("attachment not found: {}", id.get())))?;
        let object = self
            .document
            .get_object(object)
            .map_err(|error| PageError::Decode(error.to_string()))?;
        let stream = object
            .as_stream()
            .map_err(|error| PageError::Decode(error.to_string()))?;
        let bytes = stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone());
        Ok(Arc::from(bytes))
    }

    fn font_names(&mut self) -> Result<Vec<String>, PageError> {
        let mut names = std::collections::BTreeSet::new();
        for page in &self.pages {
            let Ok(page) = self.document.get_dictionary(*page) else {
                continue;
            };
            let Ok(resources) = page.get(b"Resources") else {
                continue;
            };
            let Ok((_, resources)) = self.document.dereference(resources) else {
                continue;
            };
            let Ok(resources) = resources.as_dict() else {
                continue;
            };
            let Ok(fonts) = resources.get(b"Font") else {
                continue;
            };
            let Ok((_, fonts)) = self.document.dereference(fonts) else {
                continue;
            };
            let Ok(fonts) = fonts.as_dict() else {
                continue;
            };
            for (_, font) in fonts.iter() {
                let Ok((_, font)) = self.document.dereference(font) else {
                    continue;
                };
                let Ok(font) = font.as_dict() else {
                    continue;
                };
                if let Ok(base) = font.get(b"BaseFont").and_then(Object::as_name) {
                    let name = String::from_utf8_lossy(base);
                    let name = strip_subset_prefix(&name);
                    if !name.is_empty() {
                        names.insert(name.to_owned());
                    }
                }
            }
        }
        Ok(names.into_iter().collect())
    }

    fn clear_caches(&mut self) {
        self.font_cache.clear();
    }
}

/// Loads and decrypts a PDF; distinguishes a wrong password from a general parse failure.
fn load_document(bytes: &[u8], password: Option<&str>) -> Result<Document, PdfError> {
    // Some generators write the /Encrypt dictionary inline in the trailer, while lopdf only recognizes indirect objects.
    let normalized = normalize_inline_encryption(bytes);
    let bytes = normalized.as_deref().unwrap_or(bytes);
    let load_options = lopdf::LoadOptions {
        // A PDF that only has an owner password unlocks with an empty user password.
        password: Some(password.unwrap_or_default().to_owned()),
        ..lopdf::LoadOptions::default()
    };
    match Document::load_mem_with_options(bytes, load_options) {
        Ok(document) if !document.is_encrypted() => Ok(document),
        Ok(_) => Err(password_error(password)),
        Err(error) => {
            if error.to_string().to_ascii_lowercase().contains("password") {
                return Err(password_error(password));
            }
            match Document::load_mem(bytes) {
                Ok(document) if document.is_encrypted() => Err(password_error(password)),
                _ => Err(crate::error::parse_error(error)),
            }
        }
    }
}

/// Rewrites a direct `/Encrypt <<...>>` in the trailer into an indirect object and forces an xref rebuild.
fn normalize_inline_encryption(bytes: &[u8]) -> Option<Vec<u8>> {
    let marker = b"/Encrypt";
    let mut search_end = bytes.len();
    while let Some(position) = bytes[..search_end]
        .windows(marker.len())
        .rposition(|window| window == marker)
    {
        let mut start = position + marker.len();
        while start < bytes.len() && bytes[start].is_ascii_whitespace() {
            start += 1;
        }
        if bytes.get(start..start + 2) == Some(b"<<")
            && let Some(end) = dictionary_end(bytes, start)
        {
            let dictionary = &bytes[start..end];
            let object_id = 999u32;
            let mut output = Vec::with_capacity(bytes.len() + dictionary.len() + 64);
            output.extend_from_slice(&bytes[..position]);
            output.extend_from_slice(format!("/Encrypt {object_id} 0 R").as_bytes());
            output.extend_from_slice(&bytes[end..]);
            output.extend_from_slice(
                format!(
                    "
{object_id} 0 obj
"
                )
                .as_bytes(),
            );
            output.extend_from_slice(dictionary);
            output.extend_from_slice(
                b"
endobj
",
            );
            force_xref_recovery(&mut output);
            return Some(output);
        }
        search_end = position;
    }
    None
}

fn dictionary_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut in_hex = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b')' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if in_hex {
            if byte == b'>' {
                in_hex = false;
            }
            index += 1;
            continue;
        }
        match byte {
            b'(' => in_string = true,
            b'<' if bytes.get(index..index + 2) == Some(b"<<") => {
                depth += 1;
                index += 2;
                continue;
            }
            b'<' => in_hex = true,
            b'>' if bytes.get(index..index + 2) == Some(b">>") => {
                depth = depth.saturating_sub(1);
                index += 2;
                if depth == 0 {
                    return Some(index);
                }
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// After appending objects the original xref is no longer complete, so make lopdf scan and rebuild it.
fn force_xref_recovery(bytes: &mut [u8]) {
    let marker = b"startxref";
    let Some(position) = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
    else {
        return;
    };
    let mut index = position + marker.len();
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    if index < bytes.len() && bytes[index].is_ascii_digit() {
        bytes[index] = b'0';
        for byte in &mut bytes[index + 1..] {
            if byte.is_ascii_digit() {
                *byte = b' ';
            } else {
                break;
            }
        }
    }
}

fn password_error(password: Option<&str>) -> PdfError {
    if password.is_some() {
        PdfError::PasswordIncorrect
    } else {
        PdfError::PasswordRequired
    }
}

fn metadata(document: &Document) -> Metadata {
    let Ok(info) = document.trailer.get(b"Info") else {
        return Metadata::default();
    };
    let Ok((_, info)) = document.dereference(info) else {
        return Metadata::default();
    };
    let Ok(info) = info.as_dict() else {
        return Metadata::default();
    };
    let text = |key: &[u8]| {
        info.get(key)
            .ok()
            .and_then(|object| document.dereference(object).ok())
            .and_then(|(_, object)| object.as_str().ok())
            .map(decode_pdf_string)
            .filter(|value| !value.is_empty())
    };
    Metadata {
        title: text(b"Title"),
        author: text(b"Author"),
        subject: text(b"Subject"),
        keywords: text(b"Keywords")
            .map(|value| {
                value
                    .split([';', ','])
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        creator: text(b"Creator"),
        created: None,
        modified: None,
        language: None,
    }
}

/// Decodes a PDF text string: UTF-16BE (with BOM) or PDFDocEncoding/latin-1.
pub(crate) fn decode_pdf_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair));
        char::decode_utf16(units)
            .map(|value| value.unwrap_or('\u{FFFD}'))
            .collect()
    } else {
        bytes.iter().map(|byte| char::from(*byte)).collect()
    }
}

/// Strips the PDF subset prefix, e.g. `ABCDEF+SimSun` → `SimSun`.
fn strip_subset_prefix(name: &str) -> &str {
    match name.split_once('+') {
        Some((prefix, rest))
            if prefix.len() == 6 && prefix.bytes().all(|byte| byte.is_ascii_uppercase()) =>
        {
            rest
        }
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::{PdfDocument, PdfReadOptions, probe, strip_subset_prefix};

    #[test]
    fn probes_pdf_signature_with_leading_bytes() {
        assert!(probe(b"%PDF-1.7\n"));
        assert!(probe(b"junk%PDF-1.4"));
        assert!(!probe(b"PK\x03\x04"));
    }

    #[test]
    fn strips_six_letter_subset_prefix() {
        assert_eq!(strip_subset_prefix("ABCDEF+SimSun"), "SimSun");
        assert_eq!(strip_subset_prefix("SimSun"), "SimSun");
        assert_eq!(strip_subset_prefix("abc+SimSun"), "abc+SimSun");
    }

    #[test]
    fn rejects_invalid_pdf() {
        let error = PdfDocument::open_bytes(b"not a pdf".to_vec(), PdfReadOptions::default())
            .err()
            .expect("invalid pdf should fail");
        assert!(matches!(error, super::PdfError::Parse(_)));
    }
}
