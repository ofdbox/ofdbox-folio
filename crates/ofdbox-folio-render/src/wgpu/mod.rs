//! wgpu compute backend.
//!
//! W0 covers solid-color fill paths, stroking, linear gradients, clipping and
//! text outlines; images, radial gradients and blend modes are not implemented
//! yet.

#[cfg(any(feature = "wgpu27", feature = "wgpu30"))]
mod atlas;
#[cfg(not(any(feature = "wgpu27", feature = "wgpu30")))]
mod disabled;
#[cfg(any(feature = "wgpu27", feature = "wgpu30"))]
mod enabled;

#[cfg(not(any(feature = "wgpu27", feature = "wgpu30")))]
pub use disabled::WgpuBackend;
#[cfg(any(feature = "wgpu27", feature = "wgpu30"))]
pub use enabled::{WgpuBackend, WgpuCapabilities, WgpuContext, WgpuTarget};
