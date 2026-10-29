//! Line height and baseline are computed from font metrics.

use std::sync::Arc;

use ofdbox_folio_ir::{
    Block, FlowDocument, FontError, FontProvider, FontRequest, Metadata, Paragraph, TextStyle,
    Twips,
};
use ofdbox_folio_layout::layout;

struct SystemFonts {
    bytes: Arc<[u8]>,
}

impl FontProvider for SystemFonts {
    fn load(&mut self, _request: &FontRequest<'_>) -> Result<Option<Arc<[u8]>>, FontError> {
        Ok(Some(Arc::clone(&self.bytes)))
    }
}

fn font_bytes() -> Option<Vec<u8>> {
    let path = if cfg!(windows) {
        "C:/Windows/Fonts/arial.ttf"
    } else {
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
    };
    std::fs::read(path).ok()
}

fn paragraph(text: &str) -> Paragraph {
    Paragraph::plain(
        text,
        TextStyle {
            size: Twips::from_pt(11.0),
            ..TextStyle::default()
        },
    )
}

#[test]
fn single_line_height_matches_font_metrics() {
    let Some(bytes) = font_bytes() else {
        return;
    };
    let face = ttf_parser::Face::parse(&bytes, 0).expect("parse font");
    let units = f64::from(face.units_per_em()).max(1.0);
    let expected_em = (f64::from(face.ascender()) - f64::from(face.descender())
        + f64::from(face.line_gap()))
        / units;
    let ascender_em = f64::from(face.ascender()) / units;

    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Paragraph(paragraph("第一行")));
    flow.push(Block::Paragraph(paragraph("第二行")));

    let mut fonts = SystemFonts {
        bytes: Arc::from(bytes.as_slice()),
    };
    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];
    let baselines: Vec<f64> = page.text.lines.iter().map(|line| line.baseline).collect();
    assert_eq!(baselines.len(), 2, "there should be two lines");

    let size_mm = 11.0 * 25.4 / 72.0;
    let line_height = baselines[1] - baselines[0];
    let expected = size_mm * expected_em;
    assert!(
        (line_height - expected).abs() < 0.15,
        "line height should equal the font metric {expected:.3}mm, got {line_height:.3}mm (1.2x the font size is {:.3})",
        size_mm * 1.2
    );

    // The baseline is shifted down by the ascent rather than a fixed 0.8 line
    // box.
    let first_baseline = baselines[0];
    let expected_baseline = 25.4 + size_mm * ascender_em;
    assert!(
        (first_baseline - expected_baseline).abs() < 0.2,
        "the first baseline should be near {expected_baseline:.3}mm, got {first_baseline:.3}mm"
    );
}

#[test]
fn multiple_spacing_scales_font_line_height() {
    let Some(bytes) = font_bytes() else {
        return;
    };
    let face = ttf_parser::Face::parse(&bytes, 0).expect("parse font");
    let units = f64::from(face.units_per_em()).max(1.0);
    let expected_em = (f64::from(face.ascender()) - f64::from(face.descender())
        + f64::from(face.line_gap()))
        / units;
    let size_mm = 11.0 * 25.4 / 72.0;

    let mut flow = FlowDocument::new(Metadata::default());
    for text in ["一", "二", "三"] {
        let mut paragraph = paragraph(text);
        paragraph.style.line_spacing = ofdbox_folio_ir::LineSpacing::Multiple(1.5);
        flow.push(Block::Paragraph(paragraph));
    }
    let mut fonts = SystemFonts {
        bytes: Arc::from(bytes.as_slice()),
    };
    let fixed = layout(&flow, &mut fonts).expect("layout");
    let baselines: Vec<f64> = fixed.pages[0]
        .text
        .lines
        .iter()
        .map(|line| line.baseline)
        .collect();
    let gap = baselines[1] - baselines[0];
    let expected = size_mm * expected_em * 1.5;
    assert!(
        (gap - expected).abs() < 0.2,
        "1.5x line spacing should be {expected:.3}mm, got {gap:.3}mm"
    );
}

#[test]
fn document_without_any_font_still_lays_out() {
    /// Font provider that always returns None.
    struct NoFonts;

    impl FontProvider for NoFonts {
        fn load(&mut self, _request: &FontRequest<'_>) -> Result<Option<Arc<[u8]>>, FontError> {
            Ok(None)
        }
    }

    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Paragraph(paragraph("第一行")));
    flow.push(Block::Paragraph(paragraph("第二行")));
    let mut fonts = NoFonts;
    // With no usable font, fall back to empirical metrics: no panic, pages are
    // still produced.
    let fixed = layout(&flow, &mut fonts).expect("layout");
    assert_eq!(fixed.pages.len(), 1);
    assert!(fixed.report.diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic.code,
            ofdbox_folio_ir::DiagnosticCode::MissingFont
        )
    }));
}
