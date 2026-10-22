//! OFDBox Folio 的 Fixed IR 渲染层。
//!
//! 只消费 [`ofdbox_folio_ir::Page`] 等 Fixed IR 类型，不感知源文档格式。
//! 当前实现提供基于 `vello_cpu` 的 CPU 位图渲染器、图片解码和资源缓存。

mod error;
mod image;
mod options;
mod renderer;

pub use error::RenderError;
pub use image::{DecodedImage, decode_image, detect_format};
pub use options::RenderOptions;
pub use renderer::{CpuRenderer, RasterTarget};
