//! Convert a Markdown file to OFD via Flow IR and the layout engine.
//!
//! ```text
//! cargo run -p ofdbox-folio --example markdown_to_ofd -- README.md
//! cargo run -p ofdbox-folio --example markdown_to_ofd -- README.md out.ofd
//! ```

use std::error::Error;
use std::path::Path;

use ofdbox_folio::layout::{LayoutOptions, layout_with};
use ofdbox_folio::markdown::{MarkdownOptions, read_markdown_path};
use ofdbox_folio::ofd::{OfdWriteOptions, write_fixed};

mod common;

use common::SystemFonts;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        return Err("usage: markdown_to_ofd <input.md> [output.ofd]".into());
    };
    let output = args
        .next()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("target")
                .join("markdown.ofd")
        });
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let flow = read_markdown_path(&input, MarkdownOptions::default())?;
    for diagnostic in &flow.report.diagnostics {
        eprintln!("  markdown diagnostic: {}", diagnostic.message);
    }
    let fixed = layout_with(&flow, &mut SystemFonts::new(), LayoutOptions::default())?;
    for diagnostic in &fixed.report.diagnostics {
        eprintln!("  layout diagnostic: {}", diagnostic.message);
    }
    let bytes = write_fixed(&fixed, &OfdWriteOptions::default())?;
    std::fs::write(&output, &bytes)?;
    println!(
        "Wrote {} ({} bytes, {} pages)",
        output.display(),
        bytes.len(),
        fixed.pages.len()
    );
    Ok(())
}
