//! Rendering errors.

/// Bitmap rendering error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RenderError {
    /// A rendering feature the current implementation cannot express.
    #[error("unsupported rendering feature: {0}")]
    Unsupported(String),
    /// A referenced resource is missing.
    #[error("missing rendering resource: {0}")]
    MissingResource(String),
    /// Image decoding failed.
    #[error("image decoding failed: {0}")]
    Image(String),
    /// Font parsing failed.
    #[error("font parsing failed: {0}")]
    Font(String),
    /// Invalid page size.
    #[error("invalid page size")]
    InvalidPage,
    /// Backend execution failed.
    #[error("render backend error: {0}")]
    Backend(String),
    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
