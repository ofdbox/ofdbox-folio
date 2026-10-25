//! 逐字形字体回退：主字体缺字形时换兜底字体。

use std::sync::Arc;

use ofdbox_folio_ir::{
    Block, DiagnosticCode, FlowDocument, FontError, FontProvider, FontRequest, Metadata, Paragraph,
    TextStyle, Twips,
};
use ofdbox_folio_layout::layout;

/// 指定族名时给拉丁字体，不指定族名时给 CJK 兜底字体。
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

/// 拉丁字体（一定存在）。
fn latin_font() -> Option<Arc<[u8]>> {
    read_font(&[
        "C:/Windows/Fonts/arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ])
}

/// 带 CJK 字形的字体；机器上没有就跳过测试。
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
    // 样式指定 Arial（拉丁字体），文字却是中文：只能靠兜底字体出字形。
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

    // 文本层里每个字符都有字形（不是 .notdef）。
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
            .unwrap_or_else(|| panic!("字符 {ch} 缺失"));
        assert_ne!(id, 0, "字符 {ch} 应来自兜底字体，而不是 .notdef");
    }
    assert!(
        !fixed
            .report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::MissingFont),
        "回退成功时不应报缺字形: {:?}",
        fixed.report.diagnostics
    );
}

#[test]
fn both_fonts_missing_keeps_text_layer_and_reports() {
    let Some(latin) = latin_font() else {
        return;
    };
    // 兜底字体也没有这些字符（拉丁字体排中文）。
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
    // 文本层仍然保留字符（字形号 0），并有一条缺字形诊断。
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
        "两者都缺字形时应报诊断"
    );
}
