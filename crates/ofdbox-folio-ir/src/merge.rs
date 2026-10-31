//! Render-time scene compaction.
//!
//! PDF-to-OFD converters often emit one wrapper group per object, even when the
//! object is a single glyph or a tiny path. [`Scene::compact`] produces a copy of
//! a scene with wrapper groups flattened and some elements merged; the original
//! scene, its text layer and export paths are untouched.
//!
//! Compaction is configured with [`CompactOptions`]. The default is
//! [`CompactLevel::Safe`] and is byte-for-byte pixel identical on the regression
//! corpus; the more aggressive levels are opt-in because they can change
//! anti-aliasing by a few pixels.

use std::sync::Arc;

use crate::fixed::{Element, Group, PathElement, Scene, TextElement};
use crate::geometry::{Matrix, Rect};
use crate::paint::{Paint, StrokePaint};
use crate::path::Path;
use crate::text::{Cluster, Glyph, GlyphRun};

/// Compaction preset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompactLevel {
    /// Do not compact at all.
    Off,
    /// Pixel-safe transforms only: flatten wrappers without a clip and merge
    /// disjoint opaque paths. This is the default.
    #[default]
    Safe,
    /// Also flatten clip layers that cover the whole page.
    ///
    /// This can change anti-aliasing in a few hundred pixels on some documents.
    Balanced,
    /// Also merge text runs.
    ///
    /// Largest reduction; can change anti-aliasing in a few hundred pixels.
    Aggressive,
}

/// Runtime options for [`Scene::compact_with`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactOptions {
    /// Preset that supplied the defaults for the flags below.
    pub level: CompactLevel,
    /// Flatten groups whose clip covers the whole page.
    pub flatten_clip_layers: bool,
    /// Merge disjoint opaque paths with identical paint.
    pub merge_paths: bool,
    /// Merge adjacent equivalent text runs.
    pub merge_text_runs: bool,
    /// Skip compaction when the scene has fewer elements than this.
    pub min_elements: usize,
}

impl CompactOptions {
    /// Build options from a preset.
    pub fn for_level(level: CompactLevel) -> Self {
        let (flatten_clip_layers, merge_paths, merge_text_runs) = match level {
            CompactLevel::Off => (false, false, false),
            CompactLevel::Safe => (false, true, false),
            CompactLevel::Balanced => (true, true, false),
            CompactLevel::Aggressive => (true, true, true),
        };
        Self {
            level,
            flatten_clip_layers,
            merge_paths,
            merge_text_runs,
            min_elements: 0,
        }
    }
}

impl Default for CompactOptions {
    fn default() -> Self {
        Self::for_level(CompactLevel::Safe)
    }
}

/// Element counts before and after [`Scene::compact`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MergeStats {
    /// Number of elements in the original scene.
    pub before: usize,
    /// Number of elements in the compacted scene.
    pub after: usize,
    /// Number of wrapper groups flattened into their parent.
    pub flattened_groups: usize,
    /// Number of path elements merged away.
    pub merged_paths: usize,
    /// Number of text elements merged away.
    pub merged_text_runs: usize,
}

impl MergeStats {
    /// Number of removed elements.
    pub fn removed(self) -> usize {
        self.before.saturating_sub(self.after)
    }
}

impl Scene {
    /// Returns a compacted copy of this scene using [`CompactOptions::default`].
    ///
    /// The original scene is left untouched; callers that only need rendering
    /// can use the returned copy and keep the original for text selection,
    /// search, and export.
    pub fn compact(&self) -> (Self, MergeStats) {
        self.compact_with(&CompactOptions::default())
    }

    /// Returns a compacted copy of this scene with explicit options.
    pub fn compact_with(&self, options: &CompactOptions) -> (Self, MergeStats) {
        let mut scene = self.clone();
        let before = count_elements(&scene.root);
        if options.level == CompactLevel::Off || before < options.min_elements {
            return (
                scene,
                MergeStats {
                    before,
                    after: before,
                    ..MergeStats::default()
                },
            );
        }
        let page = Rect::new(0.0, 0.0, scene.size.width, scene.size.height);
        let mut stats = MergeStats {
            before,
            after: before,
            ..MergeStats::default()
        };
        stats.flattened_groups = flatten_group(&mut scene.root, Some(page), options);
        let merged = merge_group(&mut scene.root, options);
        stats.merged_paths = merged.paths;
        stats.merged_text_runs = merged.texts;
        stats.after = count_elements(&scene.root);
        (scene, stats)
    }
}

impl Group {
    /// Compact this group with the default (safe) options.
    pub fn merge_adjacent(&mut self) -> usize {
        self.merge_adjacent_with(&CompactOptions::default())
    }

    /// Compact this group with explicit options and return the removed count.
    pub fn merge_adjacent_with(&mut self, options: &CompactOptions) -> usize {
        let before = count_elements(self);
        flatten_group(self, None, options);
        merge_group(self, options);
        before.saturating_sub(count_elements(self))
    }
}

/// Remove groups that are only transform wrappers.
///
/// Groups without a clip or opacity do not create a layer; their children can be
/// spliced into the parent with the group transform baked into each child. When
/// `flatten_clip_layers` is set, page-covering clip layers are flattened too.
fn flatten_group(group: &mut Group, page: Option<Rect>, options: &CompactOptions) -> usize {
    let mut flattened = 0;
    for child in &mut group.children {
        if let Element::Group(child) = child {
            flattened += flatten_group(child, page, options);
        }
    }
    let mut index = 0;
    while index < group.children.len() {
        let flattenable = match &group.children[index] {
            Element::Group(inner) => can_flatten(inner, page, options),
            _ => false,
        };
        if !flattenable {
            index += 1;
            continue;
        }
        let Element::Group(mut inner) = group.children.remove(index) else {
            index += 1;
            continue;
        };
        for mut child in inner.children.drain(..) {
            bake_transform(&mut child, &inner.transform);
            group.children.insert(index, child);
            index += 1;
        }
        flattened += 1;
    }
    flattened
}

fn can_flatten(group: &Group, page: Option<Rect>, options: &CompactOptions) -> bool {
    if group.opacity < 1.0 - f32::EPSILON {
        return false;
    }
    match &group.clip {
        None => true,
        Some(clip) => {
            options.flatten_clip_layers
                && group.transform == Matrix::IDENTITY
                && page.is_some_and(|page| {
                    clip.path
                        .bounds()
                        .is_some_and(|bounds| clip_covers_page(bounds, page))
                })
        }
    }
}

fn clip_covers_page(bounds: Rect, page: Rect) -> bool {
    const EPSILON: f64 = 0.01;
    bounds.x <= EPSILON
        && bounds.y <= EPSILON
        && bounds.right() >= page.width - EPSILON
        && bounds.bottom() >= page.height - EPSILON
}

fn bake_transform(element: &mut Element, parent: &Matrix) {
    match element {
        Element::Text(text) => text.transform = text.transform.then(*parent),
        Element::Path(path) => path.transform = path.transform.then(*parent),
        Element::Image(image) => image.transform = image.transform.then(*parent),
        Element::Group(group) => group.transform = group.transform.then(*parent),
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct MergeCounts {
    paths: usize,
    texts: usize,
}

#[allow(clippy::collapsible_match)]
fn merge_group(group: &mut Group, options: &CompactOptions) -> MergeCounts {
    let mut counts = MergeCounts::default();
    for child in &mut group.children {
        if let Element::Group(child) = child {
            let inner = merge_group(child, options);
            counts.paths += inner.paths;
            counts.texts += inner.texts;
        }
    }
    let mut index = 0;
    while index + 1 < group.children.len() {
        let second = group.children[index + 1].clone();
        let merged = match &mut group.children[index] {
            Element::Text(first) if options.merge_text_runs => match &second {
                Element::Text(second) => {
                    if merge_text(first, second) {
                        counts.texts += 1;
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            },
            Element::Path(first) if options.merge_paths => match &second {
                Element::Path(second) => {
                    if merge_path(first, second) {
                        counts.paths += 1;
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            },
            _ => false,
        };
        if merged {
            group.children.remove(index + 1);
        } else {
            index += 1;
        }
    }
    counts
}

/// Merge two adjacent, visually equivalent text runs.
fn merge_text(first: &mut TextElement, second: &TextElement) -> bool {
    if !Arc::ptr_eq(&first.font, &second.font)
        || first.font_size != second.font_size
        || first.fill != second.fill
        || first.stroke != second.stroke
        || !opaque_fill(&first.fill)
        || !opaque_stroke(&first.stroke)
        || first.run.glyphs.is_empty()
        || second.run.glyphs.is_empty()
    {
        return false;
    }
    let Some((dx, dy)) = translation_delta(&first.transform, &second.transform) else {
        return false;
    };
    let char_offset = u32::try_from(first.run.text.chars().count()).unwrap_or(u32::MAX);
    let glyph_offset = u32::try_from(first.run.glyphs.len()).unwrap_or(u32::MAX);
    let mut text = first.run.text.clone();
    text.push_str(&second.run.text);
    let mut glyphs = first.run.glyphs.clone();
    glyphs.extend(second.run.glyphs.iter().map(|glyph| Glyph {
        x: glyph.x + dx,
        y: glyph.y + dy,
        ..*glyph
    }));
    let mut clusters = first.run.clusters.clone();
    clusters.extend(second.run.clusters.iter().map(|cluster| Cluster {
        text_start: cluster.text_start.saturating_add(char_offset),
        text_end: cluster.text_end.saturating_add(char_offset),
        glyph_start: cluster.glyph_start.saturating_add(glyph_offset),
        glyph_end: cluster.glyph_end.saturating_add(glyph_offset),
    }));
    first.run = Arc::new(GlyphRun::new(text, glyphs, clusters));
    true
}

/// Translation delta when both matrices are pure translations; `None` otherwise.
fn translation_delta(first: &Matrix, second: &Matrix) -> Option<(f64, f64)> {
    if first == second {
        return Some((0.0, 0.0));
    }
    (is_translation(first) && is_translation(second))
        .then_some((second.e - first.e, second.f - first.f))
}

fn is_translation(matrix: &Matrix) -> bool {
    matrix.a == 1.0 && matrix.b == 0.0 && matrix.c == 0.0 && matrix.d == 1.0
}

/// Merge two disjoint, opaque paths with identical paint.
///
/// Overlapping paths are rejected: a combined nonzero/even-odd fill can cancel
/// an overlap that would have been painted by two separate opaque fills.
fn merge_path(first: &mut PathElement, second: &PathElement) -> bool {
    if first.transform != second.transform
        || first.rule != second.rule
        || first.fill != second.fill
        || first.stroke != second.stroke
        || !opaque_fill(&first.fill)
        || !opaque_stroke(&first.stroke)
        || (first.fill.is_none() && first.stroke.is_none())
    {
        return false;
    }
    let (Some(first_bounds), Some(second_bounds)) = (first.path.bounds(), second.path.bounds())
    else {
        return false;
    };
    // Include stroke extent so two strokes cannot meet at their antialiased edges.
    let pad = first
        .stroke
        .as_ref()
        .map(|stroke| stroke.style.width.max(0.0))
        .unwrap_or(0.0)
        .max(
            second
                .stroke
                .as_ref()
                .map(|stroke| stroke.style.width.max(0.0))
                .unwrap_or(0.0),
        )
        + 0.5;
    let first_bounds = expand(first_bounds, pad);
    let second_bounds = expand(second_bounds, pad);
    if first_bounds.intersects(second_bounds) {
        return false;
    }
    let mut segments = first.path.segments().to_vec();
    segments.extend_from_slice(second.path.segments());
    first.path = Arc::new(Path::from_segments(segments));
    true
}

fn expand(rect: Rect, pad: f64) -> Rect {
    Rect::new(
        rect.x - pad,
        rect.y - pad,
        rect.width + pad * 2.0,
        rect.height + pad * 2.0,
    )
}

/// Only opaque solid colors are merged: translucent fills/strokes composite
/// differently when drawn as separate elements.
fn opaque_fill(fill: &Option<Paint>) -> bool {
    match fill {
        None => true,
        Some(Paint::Color(color)) => color.a == 255,
        Some(_) => false,
    }
}

fn opaque_stroke(stroke: &Option<StrokePaint>) -> bool {
    match stroke {
        None => true,
        Some(stroke) => match &stroke.paint {
            Paint::Color(color) => color.a == 255,
            _ => false,
        },
    }
}

fn count_elements(group: &Group) -> usize {
    group
        .children
        .iter()
        .map(|child| match child {
            Element::Group(child) => 1 + count_elements(child),
            _ => 1,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontAsset;
    use crate::fixed::{Clip, PathElement};
    use crate::geometry::Point;
    use crate::paint::{FillRule, Rgba};
    use crate::path::Path;
    use crate::text::{Cluster, Glyph, GlyphRun};

    fn scene(children: Vec<Element>) -> Scene {
        Scene {
            size: crate::geometry::Size::new(100.0, 100.0),
            root: Group {
                children,
                ..Group::default()
            },
            diagnostics: Vec::new(),
        }
    }

    fn rect_path(x: f64, y: f64, width: f64, height: f64) -> Element {
        let mut path = Path::new();
        path.move_to(Point::new(x, y));
        path.line_to(Point::new(x + width, y));
        path.line_to(Point::new(x + width, y + height));
        path.line_to(Point::new(x, y + height));
        path.close();
        Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: Some(Paint::Color(Rgba::BLACK)),
            stroke: None,
            rule: FillRule::NonZero,
        })
    }

    #[test]
    fn merges_disjoint_opaque_paths() {
        let (compacted, stats) = scene(vec![
            rect_path(0.0, 0.0, 10.0, 10.0),
            rect_path(20.0, 0.0, 10.0, 10.0),
        ])
        .compact();
        assert_eq!(stats.after, 1);
        let Element::Path(path) = &compacted.root.children[0] else {
            panic!("expected path");
        };
        assert_eq!(path.path.segments().len(), 10);
    }

    #[test]
    fn does_not_merge_overlapping_paths() {
        let (compacted, stats) = scene(vec![
            rect_path(0.0, 0.0, 10.0, 10.0),
            rect_path(5.0, 5.0, 10.0, 10.0),
        ])
        .compact();
        assert_eq!(stats.after, 2);
        assert_eq!(compacted.root.children.len(), 2);
    }

    fn text_element(font: &Arc<FontAsset>, text: &str, x: f64) -> Element {
        Element::Text(TextElement {
            font: Arc::clone(font),
            font_size: 10.0,
            transform: Matrix::translate(x, 0.0),
            run: Arc::new(GlyphRun::new(
                text,
                vec![Glyph {
                    id: 1,
                    x: 0.0,
                    y: 0.0,
                    advance: 10.0,
                }],
                vec![Cluster {
                    text_start: 0,
                    text_end: 1,
                    glyph_start: 0,
                    glyph_end: 1,
                }],
            )),
            fill: Some(Paint::Color(Rgba::BLACK)),
            stroke: None,
        })
    }

    #[test]
    fn safe_keeps_text_runs_separate() {
        let font = Arc::new(FontAsset::new(Arc::from(&[][..]), 0));
        let scene = scene(vec![
            text_element(&font, "A", 0.0),
            text_element(&font, "B", 10.0),
        ]);
        let (compacted, stats) = scene.compact();
        assert_eq!(stats.after, 2);
        assert_eq!(compacted.root.children.len(), 2);
    }

    #[test]
    fn aggressive_merges_text_runs() {
        let font = Arc::new(FontAsset::new(Arc::from(&[][..]), 0));
        let scene = scene(vec![
            text_element(&font, "A", 0.0),
            text_element(&font, "B", 10.0),
        ]);
        let options = CompactOptions::for_level(CompactLevel::Aggressive);
        let (compacted, stats) = scene.compact_with(&options);
        assert_eq!(stats.after, 1);
        assert_eq!(stats.merged_text_runs, 1);
        let Element::Text(text) = &compacted.root.children[0] else {
            panic!("expected text");
        };
        assert_eq!(text.run.text, "AB");
        assert_eq!(text.run.glyphs[1].x, 10.0);
    }

    #[test]
    fn off_returns_the_scene_unchanged() {
        let scene = scene(vec![
            rect_path(0.0, 0.0, 10.0, 10.0),
            rect_path(20.0, 0.0, 10.0, 10.0),
        ]);
        let (compacted, stats) = scene.compact_with(&CompactOptions::for_level(CompactLevel::Off));
        assert_eq!(stats.after, stats.before);
        assert_eq!(compacted.root.children.len(), 2);
    }

    #[test]
    fn balanced_flattens_page_covering_clip_groups() {
        let mut clip_path = Path::new();
        clip_path.move_to(Point::new(0.0, 0.0));
        clip_path.line_to(Point::new(100.0, 0.0));
        clip_path.line_to(Point::new(100.0, 100.0));
        clip_path.line_to(Point::new(0.0, 100.0));
        clip_path.close();
        let clipped = Element::Group(Group {
            clip: Some(Arc::new(Clip {
                path: Arc::new(clip_path),
                rule: FillRule::NonZero,
            })),
            children: vec![Element::Group(Group {
                children: vec![rect_path(10.0, 10.0, 20.0, 20.0)],
                ..Group::default()
            })],
            ..Group::default()
        });
        let scene = scene(vec![clipped]);
        let (safe, safe_stats) = scene.compact();
        assert!(matches!(safe.root.children[0], Element::Group(_)));
        assert_eq!(safe_stats.flattened_groups, 1);

        let balanced = CompactOptions::for_level(CompactLevel::Balanced);
        let (compacted, stats) = scene.compact_with(&balanced);
        assert!(matches!(compacted.root.children[0], Element::Path(_)));
        assert_eq!(stats.flattened_groups, 2);
    }
}
