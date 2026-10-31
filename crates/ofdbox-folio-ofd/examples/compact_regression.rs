//! Render-time scene compaction regression check.
//!
//! Usage:
//!
//! ```text
//! cargo run -p ofdbox-folio-ofd --example compact_regression -- path/to/a.ofd path/to/b.ofd
//! ```
//!
//! For every page the tool renders the original scene and the `Scene::compact`
//! copy at the same DPI, compares the RGBA buffers byte for byte, and prints the
//! element counts and render times. It exits with a non-zero status if any page
//! differs, so it can be used as a manual corpus gate.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use ofdbox_folio_ir::{CompactLevel, CompactOptions, DocumentSource, Element, Group};
use ofdbox_folio_ofd::{OfdDocument, OfdReadOptions};
use ofdbox_folio_render::{CpuRenderer, RasterTarget, RenderOptions};

fn main() -> ExitCode {
    match run() {
        Ok(failures) => {
            if failures > 0 {
                eprintln!("{failures} page(s) differ");
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<usize, Box<dyn std::error::Error>> {
    let mut dpi = 96.0_f64;
    let mut paths = Vec::new();
    let mut inspect = false;
    let mut level = CompactLevel::Safe;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dpi" => {
                let Some(value) = args.next() else {
                    return Err("--dpi requires a value".into());
                };
                dpi = value.parse()?;
            }
            "--level" => {
                let Some(value) = args.next() else {
                    return Err("--level requires off|safe|balanced|aggressive".into());
                };
                level = match value.as_str() {
                    "off" => CompactLevel::Off,
                    "safe" => CompactLevel::Safe,
                    "balanced" => CompactLevel::Balanced,
                    "aggressive" => CompactLevel::Aggressive,
                    _ => return Err(format!("unknown level: {value}").into()),
                };
            }
            "--inspect" => inspect = true,
            _ => paths.push(arg),
        }
    }
    if paths.is_empty() {
        eprintln!(
            "usage: compact_regression [--dpi 96] [--level off|safe|balanced|aggressive] <file.ofd>..."
        );
        return Ok(0);
    }
    if !(dpi.is_finite() && dpi > 0.0) {
        return Err("invalid DPI".into());
    }

    let mut failures = 0_usize;
    for path in paths {
        let mut document = OfdDocument::open_path(&path, OfdReadOptions::default())?;
        println!("{path}");
        for index in 0..document.page_count() {
            let page = document.load_page(index)?;
            if inspect {
                inspect_scene(&page.content.root, index);
            }
            let size = page.display_size();
            let mut renderer = CpuRenderer::new(RenderOptions::default());
            let mut original = RasterTarget::from_size(size, dpi)?;
            let start = Instant::now();
            renderer.render_scene(&page.content, &mut original)?;
            let original_time = start.elapsed();
            let (compacted, stats) = page.content.compact_with(&CompactOptions::for_level(level));
            let mut merged = RasterTarget::from_size(size, dpi)?;
            let start = Instant::now();
            renderer.render_scene(&compacted, &mut merged)?;
            let merged_time = start.elapsed();
            let diff = diff_pixels(original.data(), merged.data());
            println!(
                "  page {index}: elements {} -> {} ({:.1}%), flattened {}, merged paths {}, text runs {}, diff pixels {}, render {:.1} ms -> {:.1} ms",
                stats.before,
                stats.after,
                reduction(stats.before, stats.after),
                stats.flattened_groups,
                stats.merged_paths,
                stats.merged_text_runs,
                diff,
                original_time.as_secs_f64() * 1000.0,
                merged_time.as_secs_f64() * 1000.0,
            );
            if diff > 0 {
                failures += 1;
            }
        }
    }
    Ok(failures)
}

fn reduction(before: usize, after: usize) -> f64 {
    if before == 0 {
        0.0
    } else {
        (1.0 - after as f64 / before as f64) * 100.0
    }
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn diff_pixels(first: &[u8], second: &[u8]) -> usize {
    first
        .chunks_exact(4)
        .zip(second.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count()
}

fn inspect_scene(scene: &Group, page: usize) {
    let mut text = 0;
    let mut path = 0;
    let mut images = 0;
    let mut groups = 0;
    let mut children = Vec::new();
    collect(scene, &mut text, &mut path, &mut images, &mut groups);
    for child in &scene.children {
        children.push(kind(child));
    }
    println!(
        "  page {page} scene: text={text} path={path} image={images} group={groups} order={:?}",
        &children[..children.len().min(20)]
    );
    let mut lines = 0;
    print_tree(scene, 0, &mut lines);
    let mut texts = Vec::new();
    collect_texts(scene, &mut texts);
    for element in texts.iter().take(8) {
        println!(
            "    leaf text {:?} glyphs={} font={:p} data={:p} size={} fill={:?} transform={:?}",
            element.run.text,
            element.run.glyphs.len(),
            Arc::as_ptr(&element.font),
            Arc::as_ptr(&element.font.data),
            element.font_size,
            element.fill,
            element.transform,
        );
    }
    for child in &scene.children {
        if let Element::Group(inner) = child {
            if let Some(clip) = &inner.clip {
                println!(
                    "    first clip bounds={:?} segments={:?}",
                    clip.path.bounds(),
                    clip.path.segments()
                );
            }
            break;
        }
    }
}

fn collect(
    group: &Group,
    text: &mut usize,
    path: &mut usize,
    images: &mut usize,
    groups: &mut usize,
) {
    for child in &group.children {
        match child {
            Element::Text(_) => *text += 1,
            Element::Path(_) => *path += 1,
            Element::Image(_) => *images += 1,
            Element::Group(inner) => {
                *groups += 1;
                collect(inner, text, path, images, groups);
            }
            _ => {}
        }
    }
}

fn collect_texts<'a>(group: &'a Group, out: &mut Vec<&'a ofdbox_folio_ir::TextElement>) {
    for child in &group.children {
        match child {
            Element::Text(element) => out.push(element),
            Element::Group(inner) => collect_texts(inner, out),
            _ => {}
        }
    }
}

fn kind(element: &Element) -> &'static str {
    match element {
        Element::Text(_) => "T",
        Element::Path(_) => "P",
        Element::Image(_) => "I",
        Element::Group(_) => "G",
        _ => "?",
    }
}

fn print_tree(group: &Group, depth: usize, lines: &mut usize) {
    if *lines >= 36 {
        return;
    }
    let indent = "  ".repeat(depth);
    println!(
        "{indent}group clip={} opacity={} transform={:?} children={}",
        group.clip.is_some(),
        group.opacity,
        group.transform,
        group.children.len()
    );
    *lines += 1;
    for child in &group.children {
        if *lines >= 36 {
            return;
        }
        match child {
            Element::Group(inner) => print_tree(inner, depth + 1, lines),
            Element::Text(element) => {
                println!(
                    "{indent}  text {:?} glyphs={} font={:p} data={:p} size={} fill={:?} transform={:?}",
                    element.run.text,
                    element.run.glyphs.len(),
                    Arc::as_ptr(&element.font),
                    Arc::as_ptr(&element.font.data),
                    element.font_size,
                    element.fill,
                    element.transform,
                );
                *lines += 1;
            }
            Element::Path(_) => {
                println!("{indent}  path");
                *lines += 1;
            }
            _ => {}
        }
    }
}
