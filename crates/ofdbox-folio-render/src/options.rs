//! 位图渲染选项。

use ofdbox_folio_ir::Rgba;

/// 位图渲染选项。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderOptions {
    /// 页面背景色。
    pub background: Rgba,
    /// 单张图片允许的最大像素数。
    pub max_image_pixels: u64,
    /// 已解码图片缓存的字节上限；超出后按插入顺序淘汰。
    ///
    /// 浏览器里 wasm 的线性内存不会归还系统，缓存无上限时长时间翻页会把
    /// 进程堆到触发分配失败，因此默认给一个明确的预算。
    pub max_image_cache_bytes: u64,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            background: Rgba::WHITE,
            max_image_pixels: 64 * 1024 * 1024,
            max_image_cache_bytes: 64 * 1024 * 1024,
        }
    }
}
