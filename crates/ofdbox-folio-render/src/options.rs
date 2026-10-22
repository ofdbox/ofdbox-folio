//! 位图渲染选项。

use ofdbox_folio_ir::Rgba;

/// 位图渲染选项。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderOptions {
    /// 页面背景色。
    pub background: Rgba,
    /// 单张图片允许的最大像素数。
    pub max_image_pixels: u64,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            background: Rgba::WHITE,
            max_image_pixels: 64 * 1024 * 1024,
        }
    }
}
