//! Placeholder implementation used when the `wgpu` feature is not enabled.

use crate::RenderError;
use ofdbox_folio_ir::Scene;

/// Placeholder type for the wgpu backend.
#[derive(Debug, Default)]
pub struct WgpuBackend;

impl WgpuBackend {
    /// Creates the placeholder backend.
    pub fn new() -> Self {
        Self
    }

    /// Always returns unsupported while wgpu is not enabled.
    pub fn draw(&mut self, _scene: &Scene) -> Result<(), RenderError> {
        Err(RenderError::Unsupported(
            "wgpu backend is not enabled; enable the render/wgpu feature".to_owned(),
        ))
    }
}
