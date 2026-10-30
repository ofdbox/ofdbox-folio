//! Reader contracts.
//!
//! Format adapters implement [`DocumentSource`] and load individual pages on
//! demand; the facade layers page caching, search, and selection on top of it.
//! Nothing here contains any format parsing or I/O implementation.

use std::sync::Arc;

use crate::asset::AttachmentId;
use crate::fixed::{FixedDocument, OutlineItem, Page, PageInfo};
use crate::meta::{AttachmentInfo, Metadata};

/// Source document format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SourceFormat {
    /// OFD.
    #[default]
    Ofd,
    /// PDF.
    Pdf,
    /// DOCX.
    Docx,
}

/// Document-level information that can be obtained without loading page content.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocumentInfo {
    /// Source format.
    pub format: SourceFormat,
    /// Metadata.
    pub metadata: Metadata,
    /// Page count.
    pub page_count: usize,
    /// Outline.
    pub outline: Vec<OutlineItem>,
    /// Attachment metadata.
    pub attachments: Vec<AttachmentInfo>,
}

/// Size limits for untrusted input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Maximum page count.
    pub max_pages: usize,
    /// Maximum bytes of content per page.
    pub max_page_bytes: usize,
    /// Upper bound on total decompressed bytes for the document.
    pub max_total_bytes: u64,
    /// Maximum pixel count for a single image.
    pub max_image_pixels: u64,
    /// Maximum recursion depth for XML/object parsing.
    pub max_recursion_depth: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_pages: 100_000,
            max_page_bytes: 64 * 1024 * 1024,
            max_total_bytes: 4 * 1024 * 1024 * 1024,
            max_image_pixels: 64 * 1024 * 1024,
            max_recursion_depth: 64,
        }
    }
}

/// Page-level errors. A failure on one page should not abort the whole document.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PageError {
    /// Page index out of bounds.
    #[error("page index out of bounds: {0}")]
    NotFound(usize),
    /// Failed to parse the page.
    #[error("failed to parse page: {0}")]
    Decode(String),
    /// The page contains an unsupported feature.
    #[error("unsupported page feature: {0}")]
    Unsupported(String),
    /// A size limit was exceeded.
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),
    /// Failed to read underlying data.
    #[error("read failed: {0}")]
    Io(String),
}

/// Errors when opening or indexing a document.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DocumentError {
    /// The document format could not be recognized.
    #[error("unsupported document format")]
    UnsupportedFormat,
    /// The document structure is corrupt.
    #[error("corrupt document: {0}")]
    Corrupt(String),
    /// The document is encrypted and requires a password.
    #[error("document is encrypted and requires a password")]
    PasswordRequired,
    /// Failed to read underlying data.
    #[error("read failed: {0}")]
    Io(String),
    /// A page-level error.
    #[error(transparent)]
    Page(#[from] PageError),
}

/// A document source that provides fixed-layout pages on demand.
///
/// `Send` is currently not required: the package reader in `ofdbox-core` does not
/// yet declare `Send`, and the reader is used within a single thread; the bound
/// can be tightened later.
pub trait DocumentSource {
    /// Document-level information.
    fn info(&self) -> &DocumentInfo;

    /// Page information, without loading page content.
    fn page_info(&self, index: usize) -> Result<PageInfo, PageError>;

    /// Load a single page's content.
    fn load_page(&mut self, index: usize) -> Result<Page, PageError>;

    /// Read attachment bytes.
    fn load_attachment(&mut self, id: AttachmentId) -> Result<std::sync::Arc<[u8]>, PageError>;

    /// Font names declared by the document; returns an empty list if unsupported.
    fn font_names(&mut self) -> Result<Vec<String>, PageError> {
        Ok(Vec::new())
    }

    /// Clear the source's internal caches; called by the reader after fonts or policies change.
    fn clear_caches(&mut self) {}
}

/// A [`DocumentSource`] backed by an already laid-out [`FixedDocument`].
///
/// This bridges the layout output back into the reader stack, so a document
/// produced from DOCX (or any other Flow IR input) can use the same page cache,
/// selection, search and hit-testing APIs as native OFD/PDF readers.
pub struct FixedDocumentSource {
    document: FixedDocument,
    info: DocumentInfo,
}

impl FixedDocumentSource {
    /// Wrap a laid-out document and record its source format.
    pub fn new(document: FixedDocument, format: SourceFormat) -> Self {
        let info = DocumentInfo {
            format,
            metadata: document.metadata.clone(),
            page_count: document.pages.len(),
            outline: document.outline.clone(),
            attachments: document
                .attachments
                .iter()
                .map(|attachment| attachment.info.clone())
                .collect(),
        };
        Self { document, info }
    }

    /// Borrow the underlying fixed document.
    pub fn document(&self) -> &FixedDocument {
        &self.document
    }

    /// Consume the source and return the fixed document.
    pub fn into_inner(self) -> FixedDocument {
        self.document
    }
}

impl DocumentSource for FixedDocumentSource {
    fn info(&self) -> &DocumentInfo {
        &self.info
    }

    fn page_info(&self, index: usize) -> Result<PageInfo, PageError> {
        self.document
            .pages
            .get(index)
            .map(|page| page.info.clone())
            .ok_or(PageError::NotFound(index))
    }

    fn load_page(&mut self, index: usize) -> Result<Page, PageError> {
        self.document
            .pages
            .get(index)
            .cloned()
            .ok_or(PageError::NotFound(index))
    }

    fn load_attachment(&mut self, id: AttachmentId) -> Result<Arc<[u8]>, PageError> {
        self.document
            .attachments
            .iter()
            .find(|attachment| attachment.info.id == id)
            .map(|attachment| Arc::clone(&attachment.bytes))
            .ok_or_else(|| PageError::Decode(format!("attachment {} not found", id.get())))
    }

    fn font_names(&mut self) -> Result<Vec<String>, PageError> {
        let mut names: Vec<String> = Vec::new();
        for font in self.document.resources.fonts() {
            for name in [font.font_name.as_deref(), font.family.as_deref()]
                .into_iter()
                .flatten()
            {
                let name = name.trim();
                if !name.is_empty() && !names.iter().any(|known| known == name) {
                    names.push(name.to_owned());
                }
            }
        }
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::{DocumentError, Limits, PageError, SourceFormat};

    #[test]
    fn limits_have_bounded_defaults() {
        let limits = Limits::default();
        assert!(limits.max_pages > 0);
        assert!(limits.max_page_bytes > 0);
        assert!(limits.max_recursion_depth > 0);
    }

    #[test]
    fn default_format_is_ofd() {
        assert_eq!(SourceFormat::default(), SourceFormat::Ofd);
    }

    #[test]
    fn errors_are_page_scoped() {
        let error = DocumentError::from(PageError::NotFound(3));
        assert!(matches!(error, DocumentError::Page(PageError::NotFound(3))));
    }
}
