//! Unified facade error.

use ofdbox_folio_ir::{DocumentError, PageError};

/// Facade error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Document open or page read error.
    #[error(transparent)]
    Document(#[from] DocumentError),
    /// Page-level error.
    #[error(transparent)]
    Page(#[from] PageError),
    /// OFD adapter error.
    #[cfg(feature = "ofd")]
    #[error(transparent)]
    Ofd(#[from] ofdbox_folio_ofd::OfdError),
    /// PDF adapter error.
    #[cfg(feature = "pdf")]
    #[error(transparent)]
    Pdf(#[from] ofdbox_folio_pdf::PdfError),
    /// Render error.
    #[cfg(any(feature = "cpu", feature = "gpu"))]
    #[error(transparent)]
    Render(#[from] ofdbox_folio_render::RenderError),
    /// A format that is not wired up yet.
    #[error("format not implemented yet: {0}")]
    UnsupportedFormat(String),
    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
