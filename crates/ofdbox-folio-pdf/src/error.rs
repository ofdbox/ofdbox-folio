//! PDF adapter layer errors.

use ofdbox_folio_ir::PageError;

/// PDF adapter layer error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PdfError {
    /// PDF structure parsing failed.
    #[error("PDF parse failed: {0}")]
    Parse(String),
    /// The document is encrypted and requires a password.
    #[error("PDF is encrypted and requires a password")]
    PasswordRequired,
    /// The supplied password is incorrect.
    #[error("PDF password is incorrect")]
    PasswordIncorrect,
    /// Page index out of bounds.
    #[error("page index out of bounds: {0}")]
    InvalidPage(usize),
    /// A feature the current implementation does not support.
    #[error("unsupported PDF feature: {0}")]
    Unsupported(String),
    /// Font processing failed.
    #[error("font processing failed: {0}")]
    Font(String),
    /// Image processing failed.
    #[error("image processing failed: {0}")]
    Image(String),
    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub(crate) fn parse_error(error: impl std::fmt::Display) -> PdfError {
    PdfError::Parse(error.to_string())
}

impl From<PdfError> for PageError {
    fn from(error: PdfError) -> Self {
        match error {
            PdfError::InvalidPage(index) => Self::NotFound(index),
            PdfError::Unsupported(message) => Self::Unsupported(message),
            other => Self::Decode(other.to_string()),
        }
    }
}
