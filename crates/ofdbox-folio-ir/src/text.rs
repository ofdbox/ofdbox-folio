//! The text layer: positioned glyphs, character mapping, selection, and search.
//!
//! Page drawing order and reading order are not necessarily the same. The text layer
//! stores lines and text items in logical reading order and handles hit-testing,
//! selection, copying, and search without re-parsing the page.

use std::ops::Range;
use std::sync::Arc;

use crate::asset::FontAsset;
use crate::geometry::{Matrix, Point, Rect};

/// A positioned glyph.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Glyph {
    /// Glyph ID within the font.
    pub id: u32,
    /// Baseline x coordinate in local space.
    pub x: f64,
    /// Baseline y coordinate in local space.
    pub y: f64,
    /// Horizontal advance to the next glyph.
    pub advance: f64,
}

/// Cluster mapping between characters and glyphs.
///
/// Text positions use character indices, not byte indices. A single cluster may span
/// multiple characters and multiple glyphs, for ligatures, combining marks, and font
/// fallback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Cluster {
    /// Starting character index.
    pub text_start: u32,
    /// Ending character index, exclusive.
    pub text_end: u32,
    /// Starting glyph index.
    pub glyph_start: u32,
    /// Ending glyph index, exclusive.
    pub glyph_end: u32,
}

/// A shaped run of text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlyphRun {
    /// Original text.
    pub text: String,
    /// Glyphs.
    pub glyphs: Vec<Glyph>,
    /// Cluster mapping between characters and glyphs.
    pub clusters: Vec<Cluster>,
}

impl GlyphRun {
    /// Create a glyph run.
    pub fn new(text: impl Into<String>, glyphs: Vec<Glyph>, clusters: Vec<Cluster>) -> Self {
        Self {
            text: text.into(),
            glyphs,
            clusters,
        }
    }

    /// The number of characters.
    pub fn char_count(&self) -> usize {
        self.text.chars().count()
    }
}

/// A text item within the text layer.
#[derive(Clone, Debug, PartialEq)]
pub struct TextItem {
    /// The shaped text.
    pub run: Arc<GlyphRun>,
    /// Transform from text local space to page space.
    pub transform: Matrix,
    /// Bounding box in page space.
    pub bbox: Rect,
    /// Font resource.
    pub font: Arc<FontAsset>,
    /// Font size, in millimeters.
    pub font_size: f64,
}

/// A line in logical reading order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextLine {
    /// The range of indices into `TextLayer::items` covered by this line.
    pub items: Range<u32>,
    /// Bounding box in page space.
    pub bbox: Rect,
    /// Baseline y coordinate.
    pub baseline: f64,
}

/// The text layer of one page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextLayer {
    /// Page index.
    pub page: usize,
    /// Text items arranged in logical reading order.
    pub items: Vec<TextItem>,
    /// Lines.
    pub lines: Vec<TextLine>,
}

impl TextLayer {
    /// Return plain text joined by line, with newlines between lines.
    pub fn plain_text(&self) -> String {
        if self.lines.is_empty() {
            return self
                .items
                .iter()
                .map(|item| item.run.text.as_str())
                .collect();
        }
        let mut text = String::new();
        for (line_index, line) in self.lines.iter().enumerate() {
            if line_index > 0 {
                text.push('\n');
            }
            for index in line.items.clone() {
                if let Some(item) = self.items.get(index as usize) {
                    text.push_str(&item.run.text);
                }
            }
        }
        text
    }

    /// The start position of the page text.
    pub fn start_position(&self) -> TextPosition {
        TextPosition::new(self.page, 0, 0)
    }

    /// The end position of the page text (the end of the last text item).
    pub fn end_position(&self) -> TextPosition {
        match self.items.len().checked_sub(1) {
            Some(index) => TextPosition::new(
                self.page,
                index as u32,
                self.items[index].run.char_count() as u32,
            ),
            None => self.start_position(),
        }
    }

    /// Whether the text layer is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// A text position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextPosition {
    /// Page index.
    pub page: usize,
    /// Text item index.
    pub item: u32,
    /// Character index.
    pub char_offset: u32,
}

impl TextPosition {
    /// Create a text position.
    pub const fn new(page: usize, item: u32, char_offset: u32) -> Self {
        Self {
            page,
            item,
            char_offset,
        }
    }
}

/// A text range; the end position is exclusive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextRange {
    /// Start.
    pub start: TextPosition,
    /// End, exclusive.
    pub end: TextPosition,
}

impl TextRange {
    /// Create a text range.
    pub const fn new(start: TextPosition, end: TextPosition) -> Self {
        Self { start, end }
    }

    /// Whether the range is empty.
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Return the direction with the start before the end; swap if reversed.
    pub fn normalized(self) -> Self {
        if self.start <= self.end {
            self
        } else {
            Self {
                start: self.end,
                end: self.start,
            }
        }
    }
}

/// A selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Selection {
    /// Selection anchor.
    pub anchor: TextPosition,
    /// Selection focus.
    pub focus: TextPosition,
}

impl Selection {
    /// Convert to a normalized text range.
    pub fn range(self) -> TextRange {
        TextRange::new(self.anchor, self.focus).normalized()
    }
}

/// Search criteria.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchQuery {
    /// Search text.
    pub text: String,
    /// Whether the search is case-sensitive.
    pub case_sensitive: bool,
    /// Whether whole-word matching is required.
    pub whole_word: bool,
}

/// A single search hit.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    /// Hit range.
    pub range: TextRange,
    /// Rectangles in page space.
    pub rects: Vec<Rect>,
}

/// Hit-test within the text layer.
///
/// This implementation uses the page bounding box of text items and approximate
/// glyph boxes derived from clusters; it does not depend on font outlines.
pub fn hit_test(layer: &TextLayer, point: Point) -> Option<TextPosition> {
    for line in &layer.lines {
        if !line.bbox.contains(point) {
            continue;
        }
        for index in line.items.clone() {
            let Some(item) = layer.items.get(index as usize) else {
                continue;
            };
            if !item.bbox.contains(point) {
                continue;
            }
            let local = item.transform.invert()?.transform_point(point);
            if let Some(offset) = char_offset_at(item, local) {
                return Some(TextPosition::new(layer.page, index, offset));
            }
        }
    }
    None
}

/// Return the page rectangles covered by a selection range.
pub fn selection_rects(layer: &TextLayer, range: &TextRange) -> Vec<Rect> {
    let range = range.normalized();
    let mut rects = Vec::new();
    for (index, item) in layer.items.iter().enumerate() {
        let item_index = index as u32;
        if item_index < range.start.item || item_index > range.end.item {
            continue;
        }
        let char_len = item.run.char_count() as u32;
        let start = if item_index == range.start.item {
            range.start.char_offset
        } else {
            0
        };
        let end = if item_index == range.end.item {
            range.end.char_offset
        } else {
            char_len
        };
        if start >= end {
            continue;
        }
        let mut bounds: Option<Rect> = None;
        for cluster in &item.run.clusters {
            if cluster.text_end <= start || cluster.text_start >= end {
                continue;
            }
            if let Some(rect) = cluster_rect(item, *cluster) {
                bounds = Some(match bounds {
                    Some(current) => union(current, rect),
                    None => rect,
                });
            }
        }
        if let Some(rect) = bounds {
            rects.push(rect);
        }
    }
    rects
}

/// Return the text corresponding to a selection range.
pub fn selection_text(layer: &TextLayer, range: &TextRange) -> String {
    let range = range.normalized();
    let mut text = String::new();
    let mut previous_line = None;
    for (index, item) in layer.items.iter().enumerate() {
        let item_index = index as u32;
        if item_index < range.start.item || item_index > range.end.item {
            continue;
        }
        let line = line_of_item(layer, item_index);
        if let Some(line) = line
            && previous_line.is_some_and(|previous| previous != line)
        {
            text.push('\n');
        }
        previous_line = line.or(previous_line);
        let char_len = item.run.char_count() as u32;
        let start = if item_index == range.start.item {
            range.start.char_offset
        } else {
            0
        };
        let end = if item_index == range.end.item {
            range.end.char_offset
        } else {
            char_len
        };
        if start < end {
            text.extend(
                item.run
                    .text
                    .chars()
                    .skip(start as usize)
                    .take((end - start) as usize),
            );
        }
    }
    text
}

/// Search within the text layer.
pub fn search(layer: &TextLayer, query: &SearchQuery) -> Vec<SearchHit> {
    if query.text.is_empty() {
        return Vec::new();
    }
    let needle: Vec<char> = query.text.chars().collect();
    let mut hits = Vec::new();
    for line in &layer.lines {
        let mut chars = Vec::new();
        let mut map = Vec::new();
        for index in line.items.clone() {
            let Some(item) = layer.items.get(index as usize) else {
                continue;
            };
            for (offset, character) in item.run.text.chars().enumerate() {
                chars.push(character);
                map.push((index, offset as u32));
            }
        }
        collect_hits(layer, &mut hits, &chars, &map, &needle, query);
    }
    if layer.lines.is_empty() {
        let mut chars = Vec::new();
        let mut map = Vec::new();
        for (index, item) in layer.items.iter().enumerate() {
            for (offset, character) in item.run.text.chars().enumerate() {
                chars.push(character);
                map.push((index as u32, offset as u32));
            }
        }
        collect_hits(layer, &mut hits, &chars, &map, &needle, query);
    }
    hits
}

fn collect_hits(
    layer: &TextLayer,
    hits: &mut Vec<SearchHit>,
    chars: &[char],
    map: &[(u32, u32)],
    needle: &[char],
    query: &SearchQuery,
) {
    for (start, end) in find_matches(chars, needle, query) {
        let Some((start_item, start_offset)) = map.get(start).copied() else {
            continue;
        };
        let Some((end_item, end_offset)) = map.get(end.saturating_sub(1)).copied() else {
            continue;
        };
        let range = TextRange::new(
            TextPosition::new(layer.page, start_item, start_offset),
            TextPosition::new(layer.page, end_item, end_offset + 1),
        );
        let rects = selection_rects(layer, &range);
        hits.push(SearchHit { range, rects });
    }
}

fn find_matches(chars: &[char], needle: &[char], query: &SearchQuery) -> Vec<(usize, usize)> {
    if needle.is_empty() || chars.len() < needle.len() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    for start in 0..=chars.len() - needle.len() {
        let end = start + needle.len();
        if !char_eq(&chars[start..end], needle, query.case_sensitive) {
            continue;
        }
        if query.whole_word {
            let before_is_word = start > 0 && is_word_char(chars[start - 1]);
            let after_is_word = end < chars.len() && is_word_char(chars[end]);
            if before_is_word || after_is_word {
                continue;
            }
        }
        matches.push((start, end));
    }
    matches
}

fn char_eq(candidate: &[char], needle: &[char], case_sensitive: bool) -> bool {
    if case_sensitive {
        candidate == needle
    } else {
        candidate
            .iter()
            .zip(needle)
            .all(|(a, b)| fold_char(*a) == fold_char(*b))
    }
}

fn fold_char(character: char) -> char {
    character.to_lowercase().next().unwrap_or(character)
}

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn char_offset_at(item: &TextItem, local: Point) -> Option<u32> {
    let char_len = item.run.char_count() as u32;
    if char_len == 0 {
        return None;
    }
    if !item.run.clusters.is_empty() {
        for cluster in &item.run.clusters {
            let Some(rect) = cluster_local_rect(item, *cluster) else {
                continue;
            };
            if local.x <= rect.right() + 0.001 {
                let count = cluster.text_end.saturating_sub(cluster.text_start);
                if count == 0 {
                    return Some(cluster.text_start.min(char_len.saturating_sub(1)));
                }
                let fraction = if rect.width > 0.0 {
                    ((local.x - rect.x) / rect.width).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let offset = cluster.text_start + (fraction * count as f64).floor() as u32;
                return Some(offset.min(char_len.saturating_sub(1)));
            }
        }
        return Some(char_len - 1);
    }
    Some(0)
}

fn cluster_rect(item: &TextItem, cluster: Cluster) -> Option<Rect> {
    let local = cluster_local_rect(item, cluster)?;
    Some(item.transform.transform_rect(local))
}

fn cluster_local_rect(item: &TextItem, cluster: Cluster) -> Option<Rect> {
    let glyphs = item
        .run
        .glyphs
        .get(cluster.glyph_start as usize..cluster.glyph_end as usize)?;
    let first = glyphs.first()?;
    let mut min_x = first.x;
    let mut max_x = first.x + first.advance;
    let mut baseline = first.y;
    for glyph in glyphs.iter().skip(1) {
        min_x = min_x.min(glyph.x);
        max_x = max_x.max(glyph.x + glyph.advance);
        baseline = baseline.max(glyph.y);
    }
    let height = item.font_size.max(0.1);
    Some(Rect::new(
        min_x,
        baseline - height,
        max_x - min_x,
        height * 1.2,
    ))
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect::new(x, y, right - x, bottom - y)
}

fn line_of_item(layer: &TextLayer, item: u32) -> Option<usize> {
    layer
        .lines
        .iter()
        .position(|line| line.items.contains(&item))
}

#[cfg(test)]
mod tests {
    use super::{
        Cluster, Glyph, GlyphRun, SearchQuery, TextItem, TextLayer, TextLine, TextPosition,
        TextRange, hit_test, search, selection_rects, selection_text,
    };
    use crate::asset::FontAsset;
    use crate::geometry::{Matrix, Point, Rect};
    use std::sync::Arc;

    fn sample_layer() -> TextLayer {
        let text = "Hello world";
        let glyphs = text
            .chars()
            .enumerate()
            .map(|(index, _)| Glyph {
                id: index as u32,
                x: index as f64 * 5.0,
                y: 10.0,
                advance: 5.0,
            })
            .collect::<Vec<_>>();
        let clusters = text
            .chars()
            .enumerate()
            .map(|(index, _)| Cluster {
                text_start: index as u32,
                text_end: index as u32 + 1,
                glyph_start: index as u32,
                glyph_end: index as u32 + 1,
            })
            .collect::<Vec<_>>();
        let run = Arc::new(GlyphRun::new(text, glyphs, clusters));
        let font = Arc::new(FontAsset::new(Arc::from(&b""[..]), 0));
        TextLayer {
            page: 0,
            items: vec![TextItem {
                run,
                transform: Matrix::IDENTITY,
                bbox: Rect::new(0.0, 0.0, 60.0, 12.0),
                font,
                font_size: 10.0,
            }],
            lines: vec![TextLine {
                items: 0..1,
                bbox: Rect::new(0.0, 0.0, 60.0, 12.0),
                baseline: 10.0,
            }],
        }
    }

    #[test]
    fn plain_text_joins_lines() {
        assert_eq!(sample_layer().plain_text(), "Hello world");
    }

    #[test]
    fn hit_test_maps_to_character() {
        let hit = hit_test(&sample_layer(), Point::new(1.0, 5.0)).expect("hit");
        assert_eq!(hit, TextPosition::new(0, 0, 0));
        let hit = hit_test(&sample_layer(), Point::new(27.0, 5.0)).expect("hit");
        assert_eq!(hit.char_offset, 5);
    }

    #[test]
    fn selection_returns_text_and_rects() {
        let range = TextRange::new(TextPosition::new(0, 0, 0), TextPosition::new(0, 0, 5));
        assert_eq!(selection_text(&sample_layer(), &range), "Hello");
        assert_eq!(selection_rects(&sample_layer(), &range).len(), 1);
    }

    #[test]
    fn search_finds_case_insensitive_matches() {
        let layer = sample_layer();
        let query = SearchQuery {
            text: "WORLD".to_owned(),
            case_sensitive: false,
            whole_word: true,
        };
        let hits = search(&layer, &query);
        assert_eq!(hits.len(), 1);
        assert_eq!(selection_text(&layer, &hits[0].range), "world");
    }
}
