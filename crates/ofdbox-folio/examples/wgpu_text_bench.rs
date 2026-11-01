//! Benchmarks wgpu tile rendering for a text-heavy page.

use std::env;
use std::time::Instant;

use ofdbox_folio::fonts::SystemFonts;
use ofdbox_folio::render::{WgpuBackend, WgpuContext, WgpuTarget};
use ofdbox_folio::{Document, OpenOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let input = args.next().ok_or("missing input")?;
    let page_index = args
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    let dpi = args
        .next()
        .map(|value| value.parse::<f64>())
        .transpose()?
        .unwrap_or(48.0);
    let tile = args
        .next()
        .map(|value| value.parse::<u32>())
        .transpose()?
        .unwrap_or(512);
    let mut document = Document::open_path(
        &input,
        OpenOptions {
            font_provider: Some(Box::new(SystemFonts::new())),
            ..OpenOptions::default()
        },
    )?;
    let page = document.page(page_index)?;
    let size = page.display_size();
    let width = (size.width * dpi / 25.4).ceil() as u32;
    let height = (size.height * dpi / 25.4).ceil() as u32;

    let started = Instant::now();
    let context = WgpuContext::new_blocking()?;
    let backend = WgpuBackend::new(context)?;
    println!(
        "backend init: {:.2} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );

    let mut targets = Vec::new();
    let mut y = 0;
    while y < height {
        let mut x = 0;
        while x < width {
            let w = (width - x).min(tile);
            let h = (height - y).min(tile);
            targets.push((x, y, WgpuTarget::from_pixels(backend.context(), w, h, dpi)?));
            x += tile;
        }
        y += tile;
    }

    let started = Instant::now();
    for (x, y, target) in &targets {
        backend.render_region(&page.content, target, (f64::from(*x), f64::from(*y)))?;
    }
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    println!(
        "page {page_index} {}x{} @ {dpi} dpi, {} tiles: {elapsed:.2} ms ({:.2} ms/tile)",
        width,
        height,
        targets.len(),
        elapsed / targets.len() as f64
    );
    Ok(())
}
