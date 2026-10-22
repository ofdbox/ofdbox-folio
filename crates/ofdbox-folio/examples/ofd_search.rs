//! 在 OFD 中跨页搜索文本。
//!
//! ```text
//! cargo run -p ofdbox-folio --example ofd_search -- input.ofd 查询词
//! ```

use std::env;

use ofdbox_folio::{Document, OpenOptions, SearchQuery};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let input = args.next().ok_or("缺少输入 OFD 路径")?;
    let needle = args.next().ok_or("缺少查询词")?;
    let mut document = Document::open_path(&input, OpenOptions::default())?;
    let query = SearchQuery {
        text: needle,
        case_sensitive: false,
        whole_word: false,
    };
    let hits = document.search_all(&query)?;
    for hit in &hits {
        println!(
            "page {} item {} char {}..{}",
            hit.range.start.page + 1,
            hit.range.start.item,
            hit.range.start.char_offset,
            hit.range.end.char_offset
        );
    }
    println!("hits={} cache={:?}", hits.len(), document.cache_stats());
    Ok(())
}
