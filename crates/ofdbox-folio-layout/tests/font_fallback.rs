//! Per-glyph font fallback: switch to the fallback font when the primary font
//! lacks a glyph.

use std::sync::Arc;

use ofdbox_folio_ir::{
    Block, DiagnosticCode, FlowDocument, FontError, FontProvider, FontRequest, Metadata, Paragraph,
    TextStyle, Twips,
};
use ofdbox_folio_layout::layout;

/// Return the Latin font when a family name is given, and the CJK fallback font
/// when none is given.
struct SplitFonts {
    latin: Arc<[u8]>,
    cjk: Option<Arc<[u8]>>,
}

impl FontProvider for SplitFonts {
    fn load(&mut self, request: &FontRequest<'_>) -> Result<Option<Arc<[u8]>>, FontError> {
        if request.family_name.is_some() {
            Ok(Some(Arc::clone(&self.latin)))
        } else {
            Ok(self.cjk.clone())
        }
    }
}

fn read_font(candidates: &[&str]) -> Option<Arc<[u8]>> {
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(Arc::from(bytes));
        }
    }
    None
}

/// Latin font (always present).
fn latin_font() -> Option<Arc<[u8]>> {
    read_font(&[
        "C:/Windows/Fonts/arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ])
}

/// Font with CJK glyphs; skip the test when the machine has none.
fn cjk_font() -> Option<Arc<[u8]>> {
    read_font(&[
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/simsun.ttc",
        "C:/Windows/Fonts/msyh.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    ])
}

#[test]
fn missing_glyphs_use_the_fallback_font() {
    let Some(latin) = latin_font() else {
        return;
    };
    let Some(cjk) = cjk_font() else {
        return;
    };

    let mut flow = FlowDocument::new(Metadata::default());
    // The style specifies Arial (a Latin font) but the text is Chinese: glyphs
    // can only come from the fallback font.
    let mut paragraph = Paragraph::plain(
        "中文字形回退",
        TextStyle {
            size: Twips::from_pt(11.0),
            family: ofdbox_folio_ir::FontFamily::new("Arial"),
            ..TextStyle::default()
        },
    );
    paragraph.heading_level = None;
    flow.push(Block::Paragraph(paragraph));

    let mut fonts = SplitFonts {
        latin,
        cjk: Some(cjk),
    };
    let fixed = layout(&flow, &mut fonts).expect("layout");
    let page = &fixed.pages[0];

    // Every character in the text layer has a glyph (not .notdef).
    let glyphs: Vec<(char, u32)> = page
        .text
        .items
        .iter()
        .flat_map(|item| {
            item.run
                .text
                .chars()
                .zip(item.run.glyphs.iter().map(|glyph| glyph.id))
        })
        .collect();
    for ch in "中文字形回退".chars() {
        let id = glyphs
            .iter()
            .find(|(candidate, _)| *candidate == ch)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("character {ch} is missing"));
        assert_ne!(
            id, 0,
            "character {ch} should come from the fallback font, not .notdef"
        );
    }
    assert!(
        !fixed
            .report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::MissingFont),
        "a successful fallback should not report missing glyphs: {:?}",
        fixed.report.diagnostics
    );
}

#[test]
fn both_fonts_missing_keeps_text_layer_and_reports() {
    let Some(latin) = latin_font() else {
        return;
    };
    // The fallback font lacks these characters too (laying out Chinese with a
    // Latin font).
    let mut flow = FlowDocument::new(Metadata::default());
    flow.push(Block::Paragraph(Paragraph::plain(
        "中文",
        TextStyle {
            size: Twips::from_pt(11.0),
            family: ofdbox_folio_ir::FontFamily::new("Arial"),
            ..TextStyle::default()
        },
    )));
    let mut fonts = SplitFonts { latin, cjk: None };
    let fixed = layout(&flow, &mut fonts).expect("layout");
    // The text layer still keeps the characters (glyph id 0) and one missing-glyph
    // diagnostic is reported.
    let text: String = fixed.pages[0]
        .text
        .items
        .iter()
        .map(|item| item.run.text.as_str())
        .collect();
    assert_eq!(text, "中文");
    assert!(
        fixed
            .report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::MissingFont),
        "a diagnostic should be reported when both fonts lack the glyphs"
    );
}
