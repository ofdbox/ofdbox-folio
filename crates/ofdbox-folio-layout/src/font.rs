//! Font parsing and metrics.
//!
//! Layout does not scan system fonts; it only fetches bytes through
//! [`FontProvider`]. Each "family + bold + italic" combination is parsed once,
//! caching the glyph ids and widths of the characters used as em ratios, so
//! later line layout no longer touches the font files.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ofdbox_folio_ir::{FontAsset, FontProvider, FontRequest};
use ttf_parser::Face;

/// Font key: family name + bold + italic.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FontKey {
    /// Preferred family name.
    pub family: String,
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
}

impl FontKey {
    /// Build a font key from a text style.
    pub fn new(family: &str, bold: bool, italic: bool) -> Self {
        Self {
            family: family.to_owned(),
            bold,
            italic,
        }
    }
}

/// Metrics result for a single font.
pub(crate) struct FontData {
    /// Font bytes, used to construct font resources for the Fixed IR.
    pub bytes: Arc<[u8]>,
    /// Face index within the collection.
    pub face_index: u32,
    /// Family name.
    pub family: String,

    /// Character to em advance.
    advances: HashMap<char, f64>,
    /// Character to glyph id.
    glyphs: HashMap<char, u32>,
    /// Characters missing from the font.
    missing: HashSet<char>,
    /// Ascender height (em).
    pub ascender: f64,
    /// Descender height (em, negative).
    pub descender: f64,
    /// Line gap (em).
    pub line_gap: f64,
}

impl FontData {
    /// Build the font resource used by the Fixed IR.
    pub fn asset(&self) -> FontAsset {
        FontAsset::new(Arc::clone(&self.bytes), self.face_index).with_family(self.family.clone())
    }

    /// The character's em advance; when the glyph is missing, falls back to 0.5
    /// em.
    pub fn advance(&self, ch: char) -> f64 {
        self.advances.get(&ch).copied().unwrap_or(0.5)
    }

    /// Glyph id; 0 (.notdef) when the glyph is missing.
    pub fn glyph(&self, ch: char) -> u32 {
        self.glyphs.get(&ch).copied().unwrap_or(0)
    }

    /// Whether the font is missing this character.
    pub fn is_missing(&self, ch: char) -> bool {
        self.missing.contains(&ch)
    }

    /// Height of single-spaced line (em): ascent - descent + lineGap.
    pub fn line_height(&self) -> f64 {
        (self.ascender - self.descender + self.line_gap).max(0.5)
    }
}

/// Resolve and cache fonts by family name.
#[derive(Default)]
pub(crate) struct FontStore {
    fonts: HashMap<FontKey, Option<FontData>>,
    /// Per-glyph fallback font: its glyphs are used when the primary font
    /// lacks a glyph.
    fallback: Option<FontData>,
}

impl FontStore {
    /// Pre-resolve a batch of fonts: `chars` is the set of characters that will
    /// be used.
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

    /// Pre-resolve the fallback font: a request without a family name, leaving
    /// it to the provider to return a generic font.
    pub fn prepare_fallback(&mut self, provider: &mut dyn FontProvider, chars: &HashSet<char>) {
        if self.fallback.is_some() {
            return;
        }
        let request = FontRequest::default();
        let Ok(Some(bytes)) = provider.load(&request) else {
            return;
        };
        let key = FontKey::new("", false, false);
        self.fallback = Self::build(bytes, &key, chars);
    }

    /// The fallback font.
    pub fn fallback(&self) -> Option<&FontData> {
        self.fallback.as_ref()
    }

    /// Fetch an already resolved font.
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
        // When the family name is unavailable, degrade to "any available font";
        // the caller supplies the fallback.
        let request = FontRequest {
            bold: key.bold,
            italic: key.italic,
            ..FontRequest::default()
        };
        provider.load(&request).ok().flatten()
    }

    fn build(bytes: Arc<[u8]>, key: &FontKey, chars: &HashSet<char>) -> Option<FontData> {
        let index = FACE_INDEX;
        // `Face` borrows the font bytes, so compute all metrics before moving
        // `bytes`.
        let (advances, glyphs, missing, ascender, descender, line_gap) = {
            let face = Face::parse(&bytes, index).ok()?;
            let units = f64::from(face.units_per_em()).max(1.0);
            let ascender = f64::from(face.ascender()) / units;
            let descender = f64::from(face.descender()) / units;
            let line_gap = f64::from(face.line_gap()) / units;
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
            (advances, glyphs, missing, ascender, descender, line_gap)
        };
        Some(FontData {
            bytes,
            face_index: index,
            family: key.family.clone(),
            advances,
            glyphs,
            missing,
            ascender,
            descender,
            line_gap,
        })
    }
}

/// Always take the first face in a font collection.
const FACE_INDEX: u32 = 0;
