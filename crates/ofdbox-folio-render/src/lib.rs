//! Fixed IR rendering layer for OFDBox Folio.
//!
//! Consumes only Fixed IR types such as [`ofdbox_folio_ir::Page`] and is
//! unaware of the source document format. The current implementation provides
//! a `vello_cpu`-based CPU bitmap renderer, image decoding and resource caches.
//!
//! The optional GPU backend targets exactly one wgpu major version, selected at
//! build time with the `wgpu27` or `wgpu30` feature (the legacy `wgpu` feature
//! is an alias for `wgpu30`). Enabling both is a compile error: their
//! `wgpu::Device`/`wgpu::Queue` types are not interchangeable.

#[cfg(all(feature = "wgpu27", feature = "wgpu30"))]
compile_error!("enable exactly one of the `wgpu27` and `wgpu30` features");

mod error;
mod image;
mod options;
mod prepared;
mod renderer;
#[cfg(feature = "tiny-skia")]
mod tiny_skia_renderer;
mod wgpu;

pub use error::RenderError;
pub use image::{DecodedImage, decode_image, detect_format};
pub use options::RenderOptions;
pub use prepared::{PreparedClip, PreparedCommand, PreparedScene};
pub use renderer::{CpuRenderer, PixelRect, RasterTarget, RenderRegion};
#[cfg(feature = "tiny-skia")]
pub use tiny_skia_renderer::TinySkiaRenderer;
pub use wgpu::WgpuBackend;
#[cfg(any(feature = "wgpu27", feature = "wgpu30"))]
pub use wgpu::{WgpuCapabilities, WgpuContext, WgpuTarget};
