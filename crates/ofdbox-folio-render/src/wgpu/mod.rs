//! wgpu compute backend.
//!
//! W0 covers solid-color fill paths, stroking, linear gradients, clipping and
//! text outlines; images, radial gradients and blend modes are not implemented
//! yet.

#[cfg(not(feature = "wgpu"))]
mod disabled;
#[cfg(feature = "wgpu")]
mod enabled;

#[cfg(not(feature = "wgpu"))]
pub use disabled::WgpuBackend;
#[cfg(feature = "wgpu")]
pub use enabled::{WgpuBackend, WgpuCapabilities, WgpuContext, WgpuTarget};
