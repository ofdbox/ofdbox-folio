//! Benchmarks the vello CPU and tiny-skia software renderers.

use std::env;
use std::time::Instant;

use ofdbox_folio::fonts::SystemFonts;
use ofdbox_folio::render::{
    CpuRenderer, RasterTarget, RenderOptions, RenderRegion, TinySkiaRenderer,
};
use ofdbox_folio::{Document, OpenOptions};

struct Stats {
    mean: f64,
    median: f64,
    min: f64,
    max: f64,
}

fn stats(mut values: Vec<f64>) -> Stats {
    values.sort_by(f64::total_cmp);
    let count = values.len();
    Stats {
        mean: values.iter().sum::<f64>() / count as f64,
        median: values[count / 2],
        min: values[0],
        max: values[count - 1],
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let input = args.next().ok_or("missing input document path")?;
    let page_index = args
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    let dpi = args
        .next()
        .map(|value| value.parse::<f64>())
        .transpose()?
        .unwrap_or(96.0);
    let iterations = args
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(10);
    let tile = args
        .next()
        .map(|value| value.parse::<u32>())
        .transpose()?
        .unwrap_or(0);

    let mut document = Document::open_path(
        &input,
        OpenOptions {
            font_provider: Some(Box::new(SystemFonts::new())),
            ..OpenOptions::default()
        },
    )?;
    let page = document.page(page_index)?;
    let scene = &page.content;
    let size = page.display_size();

    let mut vello = CpuRenderer::new(RenderOptions::default());
    let mut skia = TinySkiaRenderer::new(RenderOptions::default());

    let render_vello = |renderer: &mut CpuRenderer| -> Result<(), Box<dyn std::error::Error>> {
        if tile == 0 {
            let mut target = RasterTarget::from_size(size, dpi)?;
            renderer.render_scene(scene, &mut target)?;
        } else {
            let width = (size.width * dpi / 25.4).ceil() as u32;
            let height = (size.height * dpi / 25.4).ceil() as u32;
            let mut y = 0;
            while y < height {
                let mut x = 0;
                while x < width {
                    let w = (width - x).min(tile);
                    let h = (height - y).min(tile);
                    let mut target = RasterTarget::from_pixels(w, h, dpi)?;
                    renderer.render_scene_region(
                        scene,
                        &mut target,
                        RenderRegion::new((f64::from(x), f64::from(y)), dpi),
                    )?;
                    x += tile;
                }
                y += tile;
            }
        }
        Ok(())
    };

    let render_skia = |renderer: &mut TinySkiaRenderer| -> Result<(), Box<dyn std::error::Error>> {
        if tile == 0 {
            let mut target = RasterTarget::from_size(size, dpi)?;
            renderer.render_scene(scene, &mut target)?;
        } else {
            let width = (size.width * dpi / 25.4).ceil() as u32;
            let height = (size.height * dpi / 25.4).ceil() as u32;
            let mut y = 0;
            while y < height {
                let mut x = 0;
                while x < width {
                    let w = (width - x).min(tile);
                    let h = (height - y).min(tile);
                    let mut target = RasterTarget::from_pixels(w, h, dpi)?;
                    renderer.render_scene_region(
                        scene,
                        &mut target,
                        RenderRegion::new((f64::from(x), f64::from(y)), dpi),
                    )?;
                    x += tile;
                }
                y += tile;
            }
        }
        Ok(())
    };

    for _ in 0..2 {
        render_vello(&mut vello)?;
        render_skia(&mut skia)?;
    }

    let mut vello_times = Vec::with_capacity(iterations);
    let mut skia_times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        render_vello(&mut vello)?;
        vello_times.push(started.elapsed().as_secs_f64() * 1000.0);

        let started = Instant::now();
        render_skia(&mut skia)?;
        skia_times.push(started.elapsed().as_secs_f64() * 1000.0);
    }

    let vello_stats = stats(vello_times);
    let skia_stats = stats(skia_times);
    println!(
        "{input} page {page_index}, dpi {dpi}, tile {}, iterations {iterations}",
        if tile == 0 {
            "whole page".to_owned()
        } else {
            format!("{tile}px")
        }
    );
    println!(
        "vello_cpu  mean {:8.2} ms  median {:8.2}  min {:8.2}  max {:8.2}",
        vello_stats.mean, vello_stats.median, vello_stats.min, vello_stats.max
    );
    println!(
        "tiny-skia  mean {:8.2} ms  median {:8.2}  min {:8.2}  max {:8.2}",
        skia_stats.mean, skia_stats.median, skia_stats.min, skia_stats.max
    );
    println!(
        "speedup (vello/tiny): {:.2}x",
        vello_stats.median / skia_stats.median
    );
    Ok(())
}
