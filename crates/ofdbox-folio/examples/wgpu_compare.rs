//! Renders one page with both the CPU and wgpu backends and reports the pixel
//! difference.
//!
//! ```text
//! cargo run -p ofdbox-folio --features gpu30 --example wgpu_compare -- \
//!     input.ofd output-prefix [page] [dpi]
//! ```
//!
//! Writes `<prefix>-cpu.png` and `<prefix>-gpu.png` and prints the number of
//! pixels that differ by more than eight levels.

use std::env;
use std::fs::File;
use std::io::BufWriter;

use ofdbox_folio::render::{
    CpuRenderer, RasterTarget, RenderOptions, WgpuBackend, WgpuContext, WgpuTarget,
};
use ofdbox_folio::{Document, OpenOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let input = args.next().ok_or("missing input document path")?;
    let prefix = args.next().ok_or("missing output prefix")?;
    let page_index = args
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    let dpi = args
        .next()
        .map(|value| value.parse::<f64>())
        .transpose()?
        .unwrap_or(96.0);

    let mut document = Document::open_path(&input, OpenOptions::default())?;
    let page = document.page(page_index)?;
    let size = page.display_size();

    let mut cpu_target = RasterTarget::from_size(size, dpi)?;
    let mut cpu = CpuRenderer::new(RenderOptions::default());
    cpu.render_scene(&page.content, &mut cpu_target)?;

    let context = WgpuContext::new_blocking()?;
    let backend = WgpuBackend::new(context)?;
    let gpu_target = WgpuTarget::from_size(backend.context(), size, dpi)?;
    backend.render_page(&page, &gpu_target)?;
    let gpu_pixels = gpu_target.read_rgba(backend.context())?;

    let cpu_pixels = cpu_target.data();
    let width = cpu_target.width() as usize;
    let height = cpu_target.height() as usize;
    let mut differing = 0_usize;
    let mut max_delta = 0_u8;
    for (cpu, gpu) in cpu_pixels.chunks_exact(4).zip(gpu_pixels.chunks_exact(4)) {
        let delta = cpu
            .iter()
            .zip(gpu)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        if delta > 8 {
            differing += 1;
        }
        max_delta = max_delta.max(delta);
    }

    write_png(&format!("{prefix}-cpu.png"), width, height, cpu_pixels)?;
    write_png(&format!("{prefix}-gpu.png"), width, height, &gpu_pixels)?;
    println!(
        "page {page_index}: {width}x{height}, differing pixels: {differing} ({:.3}%), max delta: {max_delta}",
        differing as f64 / (width * height) as f64 * 100.0
    );
    Ok(())
}

fn write_png(
    path: &str,
    width: usize,
    height: usize,
    data: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(data)?;
    Ok(())
}
