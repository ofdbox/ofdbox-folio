//! wgpu compute 后端。
//!
//! W0 覆盖实色填充路径、描边、线性渐变、裁剪与文字轮廓；图片、
//! 径向渐变与混合模式尚未实现。

#[cfg(not(feature = "wgpu"))]
mod disabled;
#[cfg(feature = "wgpu")]
mod enabled;

#[cfg(not(feature = "wgpu"))]
pub use disabled::WgpuBackend;
#[cfg(feature = "wgpu")]
pub use enabled::{WgpuBackend, WgpuCapabilities, WgpuContext, WgpuTarget};
