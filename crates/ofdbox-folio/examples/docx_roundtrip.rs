//! DOCX → Flow IR → DOCX 的往返示例，用来检查写出结果。
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
        .ok_or("用法: docx_roundtrip <in.docx> [out.docx]")?;
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
        "读入: {} 节 {} 块 图片 {} 张 注释 {} 条 样式 {} 段/{} 字",
        flow.sections.len(),
        blocks,
        flow.resources.image_count(),
        flow.notes.len(),
        flow.styles.paragraph.len(),
        flow.styles.character.len()
    );

    let bytes = write_flow(&flow, &DocxWriteOptions::default())?;
    fs::write(&output, &bytes)?;
    println!("写出: {output} ({} 字节)", bytes.len());

    // 立刻读回一遍，确认包结构自洽。
    let back = read_flow_path(&output, &DocxReadOptions::default())?;
    let back_blocks: usize = back
        .sections
        .iter()
        .map(|section| section.blocks.len())
        .sum();
    println!(
        "回读: {} 节 {} 块 图片 {} 张 注释 {} 条",
        back.sections.len(),
        back_blocks,
        back.resources.image_count(),
        back.notes.len()
    );
    Ok(())
}
