//! OFDBox Folio 的 PDF 适配层。
//!
//! 负责把 PDF 页面解析为 Fixed IR，以及把 Fixed IR 写出为 PDF。
//! 对外不暴露 `lopdf` 的类型；PDF 阅读的图案、透明组、加密与复杂 CMap
//! 按路线图分期硬化。

#![allow(clippy::too_many_lines)]

mod backend;
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

/// 探测字节是否为 PDF。
///
/// PDF 允许文件头前存在少量垃圾字节，因此在前 1024 字节内查找 `%PDF-`。
pub fn probe(bytes: &[u8]) -> bool {
    bytes
        .get(..bytes.len().min(1024))
        .is_some_and(|head| head.windows(5).any(|window| window == b"%PDF-"))
}

/// PDF 文档源。
pub struct PdfDocument {
    document: Document,
    pages: Vec<ObjectId>,
    info: DocumentInfo,
    store: ofdbox_folio_ir::ResourceStore,
    font_cache: HashMap<Vec<u8>, Option<Arc<frontend::FontState>>>,
    font_provider: Option<Box<dyn FontProvider>>,
    options: PdfReadOptions,
}

impl PdfDocument {
    /// 从内存字节打开 PDF。
    pub fn open_bytes(bytes: Vec<u8>, options: PdfReadOptions) -> Result<Self, PdfError> {
        Self::open_bytes_with_fonts(bytes, options, None)
    }

    /// 从内存字节打开 PDF，并注入缺失字体的来源。
    pub fn open_bytes_with_fonts(
        bytes: Vec<u8>,
        options: PdfReadOptions,
        font_provider: Option<Box<dyn FontProvider>>,
    ) -> Result<Self, PdfError> {
        let document = Document::load_mem(&bytes).map_err(crate::error::parse_error)?;
        if document.is_encrypted() {
            return Err(PdfError::Encrypted);
        }
        let pages = frontend::page_ids(&document);
        let info = DocumentInfo {
            format: SourceFormat::Pdf,
            metadata: metadata(&document),
            page_count: pages.len(),
            outline: Vec::new(),
            attachments: Vec::new(),
        };
        Ok(Self {
            document,
            pages,
            info,
            store: ofdbox_folio_ir::ResourceStore::new(),
            font_cache: HashMap::new(),
            font_provider,
            options,
        })
    }

    /// 从文件打开 PDF。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_path(
        path: impl AsRef<std::path::Path>,
        options: PdfReadOptions,
    ) -> Result<Self, PdfError> {
        Self::open_path_with_fonts(path, options, None)
    }

    /// 从文件打开 PDF，并注入缺失字体的来源。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_path_with_fonts(
        path: impl AsRef<std::path::Path>,
        options: PdfReadOptions,
        font_provider: Option<Box<dyn FontProvider>>,
    ) -> Result<Self, PdfError> {
        let bytes = std::fs::read(path)?;
        Self::open_bytes_with_fonts(bytes, options, font_provider)
    }

    /// 页面数量。
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// 资源表。
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

    fn load_attachment(&mut self, _id: AttachmentId) -> Result<Arc<[u8]>, PageError> {
        Err(PageError::Unsupported("PDF 附件读取尚未实现".to_owned()))
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

/// 解码 PDF 文本字符串：UTF-16BE（带 BOM）或 PDFDocEncoding/latin-1。
fn decode_pdf_string(bytes: &[u8]) -> String {
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

/// 去掉 PDF 子集前缀，例如 `ABCDEF+SimSun` → `SimSun`。
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
