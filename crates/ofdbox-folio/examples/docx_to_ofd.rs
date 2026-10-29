//! Minimal pipeline example: DOCX → Flow IR → Fixed IR → OFD.
//!
//! ```text
//! cargo run -p ofdbox-folio --example docx_to_ofd -- in.docx out.ofd [page.png]
//! ```

use std::error::Error;
use std::fs;

use ofdbox_folio::docx::{DocxReadOptions, read_flow_path};
use ofdbox_folio::layout::{LayoutOptions, layout_with};
use ofdbox_folio::ofd::{OfdWriteOptions, write_fixed};
use ofdbox_folio::render::{CpuRenderer, RasterTarget, RenderOptions};
use ofdbox_folio::{Document, OpenOptions};

mod common;

use common::SystemFonts;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .ok_or("usage: docx_to_ofd <in.docx> <out.ofd> [page.png]")?;
    let output = args.next().ok_or("missing output path")?;
    let png = args.next();

    let flow = read_flow_path(&input, &DocxReadOptions::default())?;
    let block_count: usize = flow
        .sections
        .iter()
        .map(|section| section.blocks.len())
        .sum();
    println!(
        "read: {} sections, {} blocks, {} images",
        flow.sections.len(),
        block_count,
        flow.resources.image_count()
    );

    let mut fonts = SystemFonts::new();
    let fixed = layout_with(&flow, &mut fonts, LayoutOptions::default())?;
    println!(
        "layout: {} pages, {} outline entries, {} diagnostics",
        fixed.pages.len(),
        fixed.outline.len(),
        fixed.report.diagnostics.len()
    );
    for diagnostic in &fixed.report.diagnostics {
        println!("  [layout diagnostic] {}", diagnostic.message);
    }
    for diagnostic in &flow.report.diagnostics {
        println!("  [read diagnostic] {}", diagnostic.message);
    }

    let bytes = write_fixed(&fixed, &OfdWriteOptions::default())?;
    fs::write(&output, &bytes)?;
    println!("wrote: {output} ({} bytes)", bytes.len());

    if let Some(png) = png {
        let mut document = Document::open_path(&output, OpenOptions::default())?;
        let page = document.page(0)?;
        let mut target = RasterTarget::from_size(page.display_size(), 96.0)?;
        let mut renderer = CpuRenderer::new(RenderOptions::default());
        renderer.render_page(&page, &mut target)?;
        {
            let file = fs::File::create(&png)?;
            let writer = std::io::BufWriter::new(file);
            let mut encoder = png::Encoder::new(
                writer,
                u32::from(target.width()),
                u32::from(target.height()),
            );
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header()?;
            writer.write_image_data(target.data())?;
        }
        println!("rendered: {png}");
    }
    Ok(())
}
