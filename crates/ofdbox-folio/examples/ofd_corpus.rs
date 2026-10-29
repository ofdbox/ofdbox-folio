//! Corpus regression: open OFD files in bulk, optionally performing a read → write → read
//! round trip.
//!
//! ```text
//! cargo run -p ofdbox-folio --example ofd_corpus -- <directory> [--roundtrip]
//! ```

use std::error::Error;
use std::path::Path;

use ofdbox_folio::ir::{DocumentSource, FixedDocument};
use ofdbox_folio::ofd::{OfdDocument, OfdReadOptions, OfdWriteOptions, write_fixed};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let root = args.next().ok_or("missing directory")?;
    let roundtrip = args.any(|arg| arg == "--roundtrip");
    let mut files = Vec::new();
    collect(Path::new(&root), &mut files);
    files.sort();
    println!("files: {}", files.len());

    let mut ok = 0;
    let mut failed = Vec::new();
    let mut pages = 0usize;
    for path in &files {
        match run(path, roundtrip) {
            Ok(count) => {
                ok += 1;
                pages += count;
            }
            Err(error) => failed.push((path.display().to_string(), error.to_string())),
        }
    }
    println!("ok: {ok}  failed: {}  pages: {pages}", failed.len());
    for (path, error) in &failed {
        println!("FAIL {path}\n     {error}");
    }
    Ok(())
}

fn collect(path: &Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "ofd") {
            files.push(path);
        }
    }
}

fn run(path: &Path, roundtrip: bool) -> Result<usize, Box<dyn Error>> {
    let mut source = OfdDocument::open_path(path, OfdReadOptions::default())?;
    let page_count = source.page_count();
    let mut sizes = Vec::with_capacity(page_count);
    for index in 0..page_count {
        let page = source.load_page(index)?;
        sizes.push((page.content.size.width, page.content.size.height));
    }
    if !roundtrip {
        return Ok(page_count);
    }
    let info = source.info().clone();
    let mut fixed = FixedDocument {
        metadata: info.metadata,
        outline: info.outline,
        ..FixedDocument::default()
    };
    for index in 0..page_count {
        fixed.pages.push(source.load_page(index)?);
    }
    let bytes = write_fixed(&fixed, &OfdWriteOptions::default())?;
    let mut written = OfdDocument::open_bytes(bytes, OfdReadOptions::default())?;
    if written.page_count() != page_count {
        return Err(format!(
            "page count mismatch: {page_count} -> {}",
            written.page_count()
        )
        .into());
    }
    for (index, (width, height)) in sizes.iter().enumerate() {
        let page = written.load_page(index)?;
        if (page.content.size.width - width).abs() > 1e-3
            || (page.content.size.height - height).abs() > 1e-3
        {
            return Err(format!("page {index} size mismatch").into());
        }
    }
    Ok(page_count)
}
