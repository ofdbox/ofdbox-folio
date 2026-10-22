//! 门面统一错误。

use ofdbox_folio_ir::{DocumentError, PageError};

/// 门面错误。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// 文档打开或页面读取错误。
    #[error(transparent)]
    Document(#[from] DocumentError),
    /// 页面级错误。
    #[error(transparent)]
    Page(#[from] PageError),
    /// OFD 适配层错误。
    #[cfg(feature = "ofd")]
    #[error(transparent)]
    Ofd(#[from] ofdbox_folio_ofd::OfdError),
    /// PDF 适配层错误。
    #[cfg(feature = "pdf")]
    #[error(transparent)]
    Pdf(#[from] ofdbox_folio_pdf::PdfError),
    /// 渲染错误。
    #[cfg(any(feature = "cpu", feature = "gpu"))]
    #[error(transparent)]
    Render(#[from] ofdbox_folio_render::RenderError),
    /// 尚未接入的格式。
    #[error("尚未实现的格式: {0}")]
    UnsupportedFormat(String),
    /// I/O 错误。
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),
}
