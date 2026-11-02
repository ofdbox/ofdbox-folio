//! End-to-end page snapshot + render example.
//!
//! Usage: `cargo run --example pipeline -- <document> [page] [dpi] [size] [out.png]`

use std::env;
use std::fs::File;
use std::io::BufWriter;

use ofdbox_folio::service::{PipelineEvent, PipelineRequest, PipelineService};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let path = args.next().ok_or("missing document path")?;
    let page = args.next().map_or(Ok(0), |value| value.parse())?;
    let dpi = args.next().map_or(Ok(96.0), |value| value.parse())?;
    let size = args.next().map_or(Ok(512_u32), |value| value.parse())?;
    let out = args
        .next()
        .unwrap_or_else(|| "target/pipeline.png".to_owned());

    let mut pipeline = PipelineService::open_path(path)?;
    let id = pipeline.request(PipelineRequest {
        page,
        width: size,
        height: size,
        dpi,
    });

    loop {
        match pipeline.try_recv() {
            Some(PipelineEvent::Page { request, snapshot }) => {
                println!(
                    "request {request}: page {} snapshot with {} commands",
                    page,
                    snapshot.plan.commands.len()
                );
            }
            Some(PipelineEvent::Started { request }) => {
                println!("request {request}: rendering");
            }
            Some(PipelineEvent::Frame {
                request,
                width,
                height,
                rgba,
            }) => {
                println!("request {request}: frame {width}x{height}");
                let file = File::create(&out)?;
                let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.write_header()?.write_image_data(&rgba)?;
                break;
            }
            Some(PipelineEvent::Error { request, message }) => {
                return Err(format!("request {request}: {message}").into());
            }
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    println!("wrote {out} (request {id})");
    Ok(())
}
