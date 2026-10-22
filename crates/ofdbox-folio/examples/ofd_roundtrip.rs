//! OFD → Fixed IR → OFD 往返验证。
//!
//! ```text
//! cargo run -p ofdbox-folio --example ofd_roundtrip -- input.ofd output.ofd
//! ```

use std::env;
use std::error::Error;
use std::fs;

use ofdbox_folio::ir::{DocumentSource, FixedDocument};
use ofdbox_folio::ofd::{OfdDocument, OfdReadOptions, OfdWriteOptions, write_fixed};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let input = args.next().ok_or("缺少输入 OFD")?;
    let output = args.next().ok_or("缺少输出 OFD")?;

    let mut source = OfdDocument::open_path(&input, OfdReadOptions::default())?;
    let page_count = source.page_count();
    let mut fixed = FixedDocument::default();
    for index in 0..page_count {
        fixed.pages.push(source.load_page(index)?);
    }
    let bytes = write_fixed(&fixed, &OfdWriteOptions::default())?;
    fs::write(&output, &bytes)?;

    let mut written = OfdDocument::open_path(&output, OfdReadOptions::default())?;
    println!(
        "pages: {page_count} -> {}  bytes: {}",
        written.page_count(),
        bytes.len()
    );
    if page_count > 0 {
        let page = written.load_page(0)?;
        println!("first page: {:?}", page.display_size());
    }
    Ok(())
}
