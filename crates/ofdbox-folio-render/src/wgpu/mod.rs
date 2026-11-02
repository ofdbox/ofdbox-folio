//! wgpu compute backend.
//!
//! Paths and strokes are rasterized by a compute pass; text glyphs and images are
//! drawn by small render pipelines on top. Radial gradients and blend modes are
//! not implemented yet.

#[cfg(any(feature = "wgpu27", feature = "wgpu30"))]
mod atlas;
#[cfg(not(any(feature = "wgpu27", feature = "wgpu30")))]
mod disabled;
#[cfg(any(feature = "wgpu27", feature = "wgpu30"))]
mod enabled;
#[cfg(any(feature = "wgpu27", feature = "wgpu30"))]
mod image;
#[cfg(any(feature = "wgpu27", feature = "wgpu30"))]
mod text;

#[cfg(not(any(feature = "wgpu27", feature = "wgpu30")))]
pub use disabled::WgpuBackend;
#[cfg(any(feature = "wgpu27", feature = "wgpu30"))]
pub use enabled::{WgpuBackend, WgpuCapabilities, WgpuContext, WgpuOptions, WgpuTarget};
