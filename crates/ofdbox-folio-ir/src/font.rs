//! 字体提供者契约。
//!
//! 格式适配器和布局引擎通过本 trait 获取缺失的字体字节。库本身不扫描
//! 系统字体，调用方可以注入系统字体、目录索引或远程字体服务。

use std::sync::Arc;

/// 字体加载请求。
#[derive(Clone, Debug, Default)]
pub struct FontRequest<'a> {
    /// 源格式中的字体资源 ID。
    pub font_id: Option<&'a str>,
    /// 字体名称。
    pub font_name: Option<&'a str>,
    /// 字体家族名。
    pub family_name: Option<&'a str>,
    /// 字符集。
    pub charset: Option<&'a str>,
    /// 是否粗体。
    pub bold: bool,
    /// 是否斜体。
    pub italic: bool,
    /// 是否衬线体。
    pub serif: bool,
    /// 是否等宽体。
    pub fixed_width: bool,
    /// 源文件声明的字体文件路径。
    pub font_file: Option<&'a str>,
}

/// 字体提供者。
pub trait FontProvider {
    /// 返回字体文件字节；找不到时返回 `None`。
    fn load(&mut self, request: &FontRequest<'_>) -> Result<Option<Arc<[u8]>>, FontError>;
}

/// 字体提供者错误。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("字体加载失败: {0}")]
pub struct FontError(pub String);

impl FontError {
    /// 创建错误。
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}
