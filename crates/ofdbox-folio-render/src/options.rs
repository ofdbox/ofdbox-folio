//! Bitmap rendering options.

use ofdbox_folio_ir::Rgba;

/// Bitmap rendering options.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderOptions {
    /// Page background color.
    pub background: Rgba,
    /// Maximum pixels allowed per image.
    pub max_image_pixels: u64,
    /// Byte budget for the decoded image cache; entries are evicted in insertion order once exceeded.
    ///
    /// In browsers, wasm linear memory is never returned to the system, so an
    /// unbounded cache would grow the process heap until allocation fails after
    /// long paging sessions; a clear budget is therefore provided by default.
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
