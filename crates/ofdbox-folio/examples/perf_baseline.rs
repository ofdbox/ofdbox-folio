//! OFD → PNG 性能基线。
//!
//! ```text
//! cargo run --release -p ofdbox-folio --example perf_baseline -- ../OFD文件 96
//! ```

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ofdbox_folio::ir::SourceFormat;
use ofdbox_folio::render::{CpuRenderer, RasterTarget};
use ofdbox_folio::{Document, OpenOptions};

struct DocResult {
    path: PathBuf,
    pages: usize,
    read_ms: f64,
    open_ms: f64,
    total_ms: f64,
    png_bytes: usize,
    cache: ofdbox_folio::CacheStats,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let root = args.next().ok_or("缺少 OFD 目录")?;
    let dpi = args
        .next()
        .map(|value| value.parse::<f64>())
        .transpose()?
        .unwrap_or(96.0);

    let mut files = Vec::new();
    collect_ofd(Path::new(&root), &mut files)?;
    files.sort();

    let mut docs = Vec::new();
    let mut failures = Vec::new();
    let mut page_times = Vec::new();
    let started = Instant::now();

    for path in &files {
        match measure_document(path, dpi, &mut page_times) {
            Ok(result) => docs.push(result),
            Err(error) => failures.push((path.clone(), error)),
        }
    }

    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    let pages: usize = docs.iter().map(|doc| doc.pages).sum();
    let png_bytes: usize = docs.iter().map(|doc| doc.png_bytes).sum();
    let mut open_times: Vec<f64> = docs.iter().map(|doc| doc.open_ms).collect();
    let mut doc_times: Vec<f64> = docs.iter().map(|doc| doc.total_ms).collect();
    let mut read_times: Vec<f64> = docs.iter().map(|doc| doc.read_ms).collect();
    open_times.sort_by(|a, b| a.total_cmp(b));
    doc_times.sort_by(|a, b| a.total_cmp(b));
    read_times.sort_by(|a, b| a.total_cmp(b));
    page_times.sort_by(|a, b| a.total_cmp(b));

    println!("== OFD → PNG baseline ==");
    println!(
        "cpu threads: {}",
        std::thread::available_parallelism().map_or(0, |n| n.get())
    );
    println!("dpi: {dpi}");
    println!(
        "files: {}  ok: {}  failed: {}",
        files.len(),
        docs.len(),
        failures.len()
    );
    println!(
        "pages: {pages}  png: {:.2} MiB",
        png_bytes as f64 / 1024.0 / 1024.0
    );
    println!(
        "wall: {:.1} ms  docs/s: {:.2}  pages/s: {:.2}",
        elapsed,
        1000.0 * docs.len() as f64 / elapsed.max(1.0),
        1000.0 * pages as f64 / elapsed.max(1.0)
    );
    println!(
        "read ms: mean {:.2}  p50 {:.2}  p95 {:.2}",
        mean(&read_times),
        percentile(&read_times, 50),
        percentile(&read_times, 95)
    );
    println!(
        "open ms: mean {:.2}  p50 {:.2}  p95 {:.2}",
        mean(&open_times),
        percentile(&open_times, 50),
        percentile(&open_times, 95)
    );
    println!(
        "page ms: mean {:.2}  p50 {:.2}  p95 {:.2}",
        mean(&page_times),
        percentile(&page_times, 50),
        percentile(&page_times, 95)
    );
    println!(
        "doc ms:  mean {:.1}  p50 {:.1}  p95 {:.1}",
        mean(&doc_times),
        percentile(&doc_times, 50),
        percentile(&doc_times, 95)
    );

    let cache_pages: usize = docs.iter().map(|doc| doc.cache.pages).sum();
    let cache_bytes: usize = docs.iter().map(|doc| doc.cache.bytes).sum();
    let hits: u64 = docs.iter().map(|doc| doc.cache.hits).sum();
    let misses: u64 = docs.iter().map(|doc| doc.cache.misses).sum();
    println!(
        "page cache: pages={cache_pages}  bytes={:.2} MiB  hits={hits}  misses={misses}",
        cache_bytes as f64 / 1024.0 / 1024.0
    );

    docs.sort_by(|a, b| b.total_ms.total_cmp(&a.total_ms));
    println!("slowest docs:");
    for doc in docs.iter().take(8) {
        println!(
            "  {:>8.1} ms  {:>3} p  {}",
            doc.total_ms,
            doc.pages,
            doc.path.display()
        );
    }

    if !failures.is_empty() {
        println!("failures:");
        for (path, error) in &failures {
            println!("  {}: {}", path.display(), error);
        }
    }

    Ok(())
}

fn measure_document(
    path: &Path,
    dpi: f64,
    page_times: &mut Vec<f64>,
) -> Result<DocResult, Box<dyn Error>> {
    let read_started = Instant::now();
    let bytes = fs::read(path)?;
    let read_ms = read_started.elapsed().as_secs_f64() * 1000.0;

    let open_started = Instant::now();
    let mut document = Document::open_bytes(
        bytes,
        OpenOptions {
            format: Some(SourceFormat::Ofd),
            ..OpenOptions::default()
        },
    )?;
    let open_ms = open_started.elapsed().as_secs_f64() * 1000.0;
    let pages = document.page_count();
    let mut renderer = CpuRenderer::with_defaults();
    let mut png_bytes = 0usize;
    let total_started = Instant::now();

    for index in 0..pages {
        let page_started = Instant::now();
        let page = document.page(index)?;
        let mut target = RasterTarget::from_size(page.display_size(), dpi)?;
        renderer.render_page(page.as_ref(), &mut target)?;
        png_bytes += encode_png(&target)?.len();
        page_times.push(page_started.elapsed().as_secs_f64() * 1000.0);
    }

    Ok(DocResult {
        path: path.to_owned(),
        pages,
        read_ms,
        open_ms,
        total_ms: total_started.elapsed().as_secs_f64() * 1000.0,
        png_bytes,
        cache: document.cache_stats(),
    })
}

fn encode_png(target: &RasterTarget) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(
            &mut png,
            u32::from(target.width()),
            u32::from(target.height()),
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(target.data())?;
    }
    Ok(png)
}

fn collect_ofd(dir: &Path, output: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_ofd(&path, output)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("ofd"))
        {
            output.push(path);
        }
    }
    Ok(())
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn percentile(sorted: &[f64], percentile: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = (sorted.len() - 1) * percentile / 100;
    sorted[index]
}
