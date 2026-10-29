//! Fixed IR rendering layer for OFDBox Folio.
//!
//! Consumes only Fixed IR types such as [`ofdbox_folio_ir::Page`] and is
//! unaware of the source document format. The current implementation provides
//! a `vello_cpu`-based CPU bitmap renderer, image decoding and resource caches.

mod error;
mod image;
mod options;
mod renderer;
mod wgpu;

pub use error::RenderError;
pub use image::{DecodedImage, decode_image, detect_format};
pub use options::RenderOptions;
pub use renderer::{CpuRenderer, RasterTarget, RenderRegion};
pub use wgpu::WgpuBackend;
#[cfg(feature = "wgpu")]
pub use wgpu::{WgpuCapabilities, WgpuContext, WgpuTarget};
