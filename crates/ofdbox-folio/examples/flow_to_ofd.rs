//! Flow → Fixed → OFD 的最小示例。
//!
//! ```text
//! cargo run -p ofdbox-folio --example flow_to_ofd -- out.ofd [page.png]
//! ```

use std::error::Error;
use std::fs;

use ofdbox_folio::ir::{
    Align, Block, Field, FieldKind, FlowDocument, Inline, LineSpacing, Metadata, ParaStyle,
    Paragraph, TextRun, TextStyle, Twips,
};
use ofdbox_folio::layout::{LayoutOptions, layout_with};
use ofdbox_folio::render::{CpuRenderer, RasterTarget, RenderOptions};
use ofdbox_folio::{Document, OpenOptions};

mod common;

use common::SystemFonts;

fn heading(text: &str, points: f64) -> Paragraph {
    let mut paragraph = Paragraph::plain(
        text,
        TextStyle {
            size: Twips::from_pt(points),
            ..TextStyle::default()
        },
    );
    paragraph.heading_level = Some(1);
    paragraph
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let output = args.next().unwrap_or_else(|| "flow.ofd".to_owned());
    let png = args.next();

    let mut flow = FlowDocument::new(Metadata {
        title: Some("Flow 示例".to_owned()),
        creator: Some("OFDBox Folio".to_owned()),
        ..Metadata::default()
    });
    let body = ParaStyle {
        line_spacing: LineSpacing::Multiple(1.25),
        space_after: Twips::from_pt(6.0),
        align: Align::Justify,
        ..ParaStyle::default()
    };

    flow.push(Block::Paragraph(heading("创建链路示例", 22.0)));
    flow.push(Block::Paragraph(Paragraph {
        style: body.clone(),
        runs: vec![Inline::Text(TextRun::new(
            "这一段来自 Flow IR，由布局引擎换算成毫米并分页。断行会优先在空格与 \
             CJK 字符之间寻找机会，两端对齐只拉伸空格；最后一页最后一行按左对齐收尾。",
            TextStyle::default(),
        ))],
        heading_level: None,
        anchor: None,
    }));
    flow.push(Block::Paragraph(heading("分页与页码", 16.0)));
    for index in 0..40 {
        flow.push(Block::Paragraph(Paragraph {
            style: body.clone(),
            runs: vec![Inline::Text(TextRun::new(
                format!(
                    "第 {index} 段：这段文本用来把内容推到第二页，验证分页、段前段后间距与 \
                     行距是否按设置生效。"
                ),
                TextStyle::default(),
            ))],
            heading_level: None,
            anchor: None,
        }));
    }
    let mut footer = Paragraph::plain("第 ", TextStyle::default());
    footer
        .runs
        .push(Inline::Field(Field::new(FieldKind::PageNumber)));
    footer.runs.push(Inline::Text(TextRun::new(
        " 页 / 共 ",
        TextStyle::default(),
    )));
    footer
        .runs
        .push(Inline::Field(Field::new(FieldKind::TotalPages)));
    footer
        .runs
        .push(Inline::Text(TextRun::new(" 页", TextStyle::default())));
    flow.push(Block::Paragraph(footer));

    let mut fonts = SystemFonts::new();
    let fixed = layout_with(&flow, &mut fonts, LayoutOptions::default())?;
    println!(
        "pages: {}  outline: {}  diagnostics: {}",
        fixed.pages.len(),
        fixed.outline.len(),
        fixed.report.diagnostics.len()
    );
    for diagnostic in &fixed.report.diagnostics {
        println!("  [诊断] {}", diagnostic.message);
    }

    let bytes =
        ofdbox_folio::ofd::write_fixed(&fixed, &ofdbox_folio::ofd::OfdWriteOptions::default())?;
    fs::write(&output, &bytes)?;
    println!("wrote {output} ({} bytes)", bytes.len());

    if let Some(png) = png {
        let mut document = Document::open_path(&output, OpenOptions::default())?;
        let page = document.page(0)?;
        let mut target = RasterTarget::from_size(page.display_size(), 96.0)?;
        let mut renderer = CpuRenderer::new(RenderOptions::default());
        renderer.render_page(&page, &mut target)?;
        {
            let file = fs::File::create(&png)?;
            let writer = std::io::BufWriter::new(file);
            let mut encoder = png::Encoder::new(
                writer,
                u32::from(target.width()),
                u32::from(target.height()),
            );
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header()?;
            writer.write_image_data(target.data())?;
        }
        println!("wrote {png}");
    }
    Ok(())
}
