//! Multi-page pipeline benchmark.
//!
//! Usage: `cargo run --release --example pipeline_bench -- <document> [pages] [dpi] [size]`

use std::env;
use std::time::Instant;

use ofdbox_folio::service::{
    PipelineEvent, PipelineOptions, PipelinePriority, PipelineRequest, PipelineService,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let path = args.next().ok_or("missing document path")?;
    let pages = args.next().map_or(Ok(4_usize), |value| value.parse())?;
    let dpi = args.next().map_or(Ok(96.0), |value| value.parse())?;
    let size = args.next().map_or(Ok(512_u32), |value| value.parse())?;

    let mut pipeline = PipelineService::open_path(path)?;
    let started = Instant::now();
    let mut frames = 0_usize;
    for page in 0..pages {
        pipeline.request_with_options(
            PipelineRequest {
                page,
                width: size,
                height: size,
                dpi,
                origin_px: (0.0, 0.0),
            },
            PipelineOptions {
                priority: PipelinePriority::High,
                generation: 1,
            },
        )?;
        loop {
            match pipeline.try_recv() {
                Some(PipelineEvent::Frame { .. }) => {
                    frames += 1;
                    break;
                }
                Some(PipelineEvent::Error { message, .. }) => {
                    return Err(message.into());
                }
                Some(_) => {}
                None => std::thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
    }
    let elapsed = started.elapsed();
    println!(
        "{frames} pages in {:.2} ms ({:.2} ms/page)",
        elapsed.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1000.0 / frames.max(1) as f64
    );
    Ok(())
}
