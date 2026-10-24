//! 字体解析与度量。
//!
//! 布局不扫描系统字体，只通过 [`FontProvider`] 取字节；每个「家族 + 粗体 + 斜体」
//! 组合解析一次，把用到的字符的字形编号与宽度缓存成 em 比例，之后的行布局不再
//! 触碰字体文件。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ofdbox_folio_ir::{FontAsset, FontProvider, FontRequest};
use ttf_parser::Face;

/// 字体键：家族名 + 粗体 + 斜体。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FontKey {
    /// 首选家族名。
    pub family: String,
    /// 粗体。
    pub bold: bool,
    /// 斜体。
    pub italic: bool,
}

impl FontKey {
    /// 由文本样式构造字体键。
    pub fn new(family: &str, bold: bool, italic: bool) -> Self {
        Self {
            family: family.to_owned(),
            bold,
            italic,
        }
    }
}

/// 单个字体的度量结果。
pub(crate) struct FontData {
    /// 字体字节，用于构造 Fixed IR 的字体资源。
    pub bytes: Arc<[u8]>,
    /// 集合内的 face 索引。
    pub face_index: u32,
    /// 家族名。
    pub family: String,

    /// 字符到 em 宽度。
    advances: HashMap<char, f64>,
    /// 字符到字形编号。
    glyphs: HashMap<char, u32>,
    /// 字体里没有的字符。
    missing: HashSet<char>,
}

impl FontData {
    /// 构造供 Fixed IR 使用的字体资源。
    pub fn asset(&self) -> FontAsset {
        FontAsset::new(Arc::clone(&self.bytes), self.face_index).with_family(self.family.clone())
    }

    /// 字符的 em 宽度；缺字形时按 0.5 em 兜底。
    pub fn advance(&self, ch: char) -> f64 {
        self.advances.get(&ch).copied().unwrap_or(0.5)
    }

    /// 字形编号；缺字形时为 0（.notdef）。
    pub fn glyph(&self, ch: char) -> u32 {
        self.glyphs.get(&ch).copied().unwrap_or(0)
    }

    /// 字体是否缺少该字符。
    pub fn is_missing(&self, ch: char) -> bool {
        self.missing.contains(&ch)
    }
}

/// 按家族名解析并缓存字体。
#[derive(Default)]
pub(crate) struct FontStore {
    fonts: HashMap<FontKey, Option<FontData>>,
}

impl FontStore {
    /// 预解析一批字体：`chars` 是要用到的字符集合。
    pub fn prepare(
        &mut self,
        provider: &mut dyn FontProvider,
        key: &FontKey,
        fallbacks: &[String],
        chars: &HashSet<char>,
    ) {
        if self.fonts.contains_key(key) {
            return;
        }
        let bytes = self.load(provider, key, fallbacks);
        let data = bytes.and_then(|bytes| Self::build(bytes, key, chars));
        self.fonts.insert(key.clone(), data);
    }

    /// 取已解析字体。
    pub fn get(&self, key: &FontKey) -> Option<&FontData> {
        self.fonts.get(key).and_then(Option::as_ref)
    }

    fn load(
        &self,
        provider: &mut dyn FontProvider,
        key: &FontKey,
        fallbacks: &[String],
    ) -> Option<Arc<[u8]>> {
        let mut candidates = Vec::with_capacity(fallbacks.len() + 1);
        candidates.push(key.family.as_str());
        candidates.extend(fallbacks.iter().map(String::as_str));
        for family in candidates {
            if family.is_empty() {
                continue;
            }
            let request = FontRequest {
                family_name: Some(family),
                font_name: Some(family),
                bold: key.bold,
                italic: key.italic,
                ..FontRequest::default()
            };
            if let Ok(Some(bytes)) = provider.load(&request) {
                return Some(bytes);
            }
        }
        // 家族名不可用时退化为「任意可用字体」，由调用方提供兜底。
        let request = FontRequest {
            bold: key.bold,
            italic: key.italic,
            ..FontRequest::default()
        };
        provider.load(&request).ok().flatten()
    }

    fn build(bytes: Arc<[u8]>, key: &FontKey, chars: &HashSet<char>) -> Option<FontData> {
        let index = FACE_INDEX;
        // `Face` 借用字体字节，先算完度量再移动 `bytes`。
        let (advances, glyphs, missing) = {
            let face = Face::parse(&bytes, index).ok()?;
            let units = f64::from(face.units_per_em()).max(1.0);
            let mut advances = HashMap::with_capacity(chars.len());
            let mut glyphs = HashMap::with_capacity(chars.len());
            let mut missing = HashSet::new();
            for ch in chars {
                match face.glyph_index(*ch) {
                    Some(glyph) => {
                        let advance = face
                            .glyph_hor_advance(glyph)
                            .map(|advance| f64::from(advance) / units)
                            .unwrap_or(0.5);
                        advances.insert(*ch, advance);
                        glyphs.insert(*ch, u32::from(glyph.0));
                    }
                    None => {
                        missing.insert(*ch);
                    }
                }
            }
            (advances, glyphs, missing)
        };
        Some(FontData {
            bytes,
            face_index: index,
            family: key.family.clone(),
            advances,
            glyphs,
            missing,
        })
    }
}

/// 字体集合里固定取第一个 face。
const FACE_INDEX: u32 = 0;
