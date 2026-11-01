//! Compares the vello CPU renderer and the tiny-skia software renderer.

use std::env;
use std::fs::File;
use std::io::BufWriter;

use ofdbox_folio::fonts::SystemFonts;
use ofdbox_folio::render::{CpuRenderer, RasterTarget, RenderOptions, TinySkiaRenderer};
use ofdbox_folio::{Document, OpenOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let input = args.next().ok_or("missing input document path")?;
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
    let mut document = Document::open_path(
        &input,
        OpenOptions {
            font_provider: Some(Box::new(SystemFonts::new())),
            ..OpenOptions::default()
        },
    )?;
    let page = document.page(page_index)?;
    let size = page.display_size();

    let mut vello_target = RasterTarget::from_size(size, dpi)?;
    let mut vello = CpuRenderer::new(RenderOptions::default());
    vello.render_page(&page, &mut vello_target)?;
    let mut skia_target = RasterTarget::from_size(size, dpi)?;
    let mut skia = TinySkiaRenderer::with_defaults();
    skia.render_page(&page, &mut skia_target)?;

    let mut differing = 0usize;
    let mut max_delta = 0u8;
    for (a, b) in vello_target
        .data()
        .chunks_exact(4)
        .zip(skia_target.data().chunks_exact(4))
    {
        let delta = a
            .iter()
            .zip(b)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        if delta > 8 {
            differing += 1;
        }
        max_delta = max_delta.max(delta);
    }
    let width = u32::from(skia_target.width());
    let height = u32::from(skia_target.height());
    write_png("target/tiny-skia.png", width, height, skia_target.data())?;
    println!(
        "page {page_index}: {width}x{height}, differing pixels: {differing} ({:.3}%), max delta: {max_delta}",
        differing as f64 / (width * height) as f64 * 100.0
    );
    Ok(())
}

fn write_png(
    path: &str,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(data)?;
    Ok(())
}
