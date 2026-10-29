//! DOCX → Flow IR → DOCX round-trip example, used to check the written result.
//!
//! ```text
//! cargo run -p ofdbox-folio --example docx_roundtrip -- in.docx out.docx
//! ```

use std::error::Error;
use std::fs;

use ofdbox_folio::docx::{DocxReadOptions, DocxWriteOptions, read_flow_path, write_flow};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .ok_or("usage: docx_roundtrip <in.docx> [out.docx]")?;
    let output = args
        .next()
        .unwrap_or_else(|| input.replace(".docx", "_roundtrip.docx"));

    let flow = read_flow_path(&input, &DocxReadOptions::default())?;
    let blocks: usize = flow
        .sections
        .iter()
        .map(|section| section.blocks.len())
        .sum();
    println!(
        "read: {} sections, {} blocks, {} images, {} notes, {} paragraph styles / {} character styles",
        flow.sections.len(),
        blocks,
        flow.resources.image_count(),
        flow.notes.len(),
        flow.styles.paragraph.len(),
        flow.styles.character.len()
    );

    let bytes = write_flow(&flow, &DocxWriteOptions::default())?;
    fs::write(&output, &bytes)?;
    println!("wrote: {output} ({} bytes)", bytes.len());

    // Read it back right away to confirm the package structure is self-consistent.
    let back = read_flow_path(&output, &DocxReadOptions::default())?;
    let back_blocks: usize = back
        .sections
        .iter()
        .map(|section| section.blocks.len())
        .sum();
    println!(
        "read back: {} sections, {} blocks, {} images, {} notes",
        back.sections.len(),
        back_blocks,
        back.resources.image_count(),
        back.notes.len()
    );
    Ok(())
}
