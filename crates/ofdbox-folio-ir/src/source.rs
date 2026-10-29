//! Reader contracts.
//!
//! Format adapters implement [`DocumentSource`] and load individual pages on
//! demand; the facade layers page caching, search, and selection on top of it.
//! Nothing here contains any format parsing or I/O implementation.

use crate::asset::AttachmentId;
use crate::fixed::{OutlineItem, Page, PageInfo};
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
