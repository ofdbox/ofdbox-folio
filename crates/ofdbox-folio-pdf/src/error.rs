//! PDF 适配层错误。

use ofdbox_folio_ir::PageError;

/// PDF 适配层错误。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PdfError {
    /// PDF 结构解析失败。
    #[error("PDF 解析失败: {0}")]
    Parse(String),
    /// 文档已加密，需要密码。
    #[error("PDF 已加密，需要密码")]
    PasswordRequired,
    /// 提供的密码不正确。
    #[error("PDF 密码不正确")]
    PasswordIncorrect,
    /// 页面索引越界。
    #[error("页面索引越界: {0}")]
    InvalidPage(usize),
    /// 当前实现不支持的特性。
    #[error("不支持的 PDF 特性: {0}")]
    Unsupported(String),
    /// 字体处理失败。
    #[error("字体处理失败: {0}")]
    Font(String),
    /// 图片处理失败。
    #[error("图片处理失败: {0}")]
    Image(String),
    /// I/O 错误。
    #[error("I/O 错误: {0}")]
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
