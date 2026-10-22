//! 阅读器契约。
//!
//! 格式适配器实现 [`DocumentSource`]，按需加载单页；门面在其上叠加页面缓存、
//! 搜索与选择。这里不包含任何格式解析或 I/O 实现。

use crate::asset::AttachmentId;
use crate::fixed::{OutlineItem, Page, PageInfo};
use crate::meta::{AttachmentInfo, Metadata};

/// 源文档格式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SourceFormat {
    /// OFD。
    #[default]
    Ofd,
    /// PDF。
    Pdf,
    /// DOCX。
    Docx,
}

/// 文档级信息，可在不加载页面内容时取得。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocumentInfo {
    /// 源格式。
    pub format: SourceFormat,
    /// 元数据。
    pub metadata: Metadata,
    /// 页面数量。
    pub page_count: usize,
    /// 大纲。
    pub outline: Vec<OutlineItem>,
    /// 附件元数据。
    pub attachments: Vec<AttachmentInfo>,
}

/// 不可信输入的规模限制。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// 最大页数。
    pub max_pages: usize,
    /// 单页内容最大字节数。
    pub max_page_bytes: usize,
    /// 文档总解压字节上限。
    pub max_total_bytes: u64,
    /// 单张图片最大像素数。
    pub max_image_pixels: u64,
    /// XML/对象解析最大递归深度。
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

/// 页面级错误。单页失败不应终止整个文档。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PageError {
    /// 页面索引越界。
    #[error("页面索引越界: {0}")]
    NotFound(usize),
    /// 页面解析失败。
    #[error("页面解析失败: {0}")]
    Decode(String),
    /// 页面包含不支持的特性。
    #[error("不支持的页面特性: {0}")]
    Unsupported(String),
    /// 超过规模限制。
    #[error("超过限制: {0}")]
    LimitExceeded(String),
    /// 读取底层数据失败。
    #[error("读取失败: {0}")]
    Io(String),
}

/// 打开或索引文档时的错误。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DocumentError {
    /// 无法识别文档格式。
    #[error("不支持的文档格式")]
    UnsupportedFormat,
    /// 文档结构损坏。
    #[error("文档损坏: {0}")]
    Corrupt(String),
    /// 文档已加密，需要密码。
    #[error("文档已加密，需要密码")]
    PasswordRequired,
    /// 读取底层数据失败。
    #[error("读取失败: {0}")]
    Io(String),
    /// 页面级错误。
    #[error(transparent)]
    Page(#[from] PageError),
}

/// 按需提供固定版式页面的文档源。
///
/// 当前不要求 `Send`：`ofdbox-core` 的包读取器尚未声明 `Send`，
/// 阅读器在单线程内使用；后续可通过约束收紧。
pub trait DocumentSource {
    /// 文档级信息。
    fn info(&self) -> &DocumentInfo;

    /// 页面信息，不加载页面内容。
    fn page_info(&self, index: usize) -> Result<PageInfo, PageError>;

    /// 加载单页内容。
    fn load_page(&mut self, index: usize) -> Result<Page, PageError>;

    /// 读取附件字节。
    fn load_attachment(&mut self, id: AttachmentId) -> Result<std::sync::Arc<[u8]>, PageError>;
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
