//! Render-time scene compaction.
//!
//! PDF-to-OFD converters often emit one text element per glyph. That is correct
//! but expensive to rasterize: the renderer has to traverse and build a glyph run
//! for every single glyph. [`Scene::compact`] produces a copy of a scene with
//! adjacent, visually equivalent text elements merged into one run, and disjoint
//! opaque paths with identical paint merged into one path.
//!
//! The original scene is never modified: the text layer, export and document
//! conversion paths keep using the original IR. This is a render-only optimization.

use std::sync::Arc;

use crate::fixed::{Element, Group, PathElement, Scene, TextElement};
use crate::geometry::{Matrix, Rect};
use crate::paint::{Paint, StrokePaint};
use crate::path::Path;
use crate::text::{Cluster, Glyph, GlyphRun};

/// Element counts before and after [`Scene::compact`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MergeStats {
    /// Number of elements in the original scene.
    pub before: usize,
    /// Number of elements in the compacted scene.
    pub after: usize,
}

impl MergeStats {
    /// Number of removed elements.
    pub fn removed(self) -> usize {
        self.before.saturating_sub(self.after)
    }
}

impl Scene {
    /// Returns a compacted copy of this scene.
    ///
    /// The original scene is left untouched; callers that only need rendering
    /// can use the returned copy and keep the original for text selection,
    /// search, and export.
    pub fn compact(&self) -> (Self, MergeStats) {
        let mut scene = self.clone();
        let before = count_elements(&scene.root);
        merge_group(&mut scene.root);
        let after = count_elements(&scene.root);
        (scene, MergeStats { before, after })
    }
}

impl Group {
    /// Merges adjacent render-equivalent text elements in this group and its
    /// descendants, returning the number of removed elements.
    pub fn merge_adjacent(&mut self) -> usize {
        let before = count_elements(self);
        merge_group(self);
        before.saturating_sub(count_elements(self))
    }
}

fn merge_group(group: &mut Group) {
    for child in &mut group.children {
        if let Element::Group(child) = child {
            merge_group(child);
        }
    }
    let mut index = 0;
    while index + 1 < group.children.len() {
        let second = group.children[index + 1].clone();
        let merged = match &mut group.children[index] {
            Element::Text(first) => match &second {
                Element::Text(second) => merge_text(first, second),
                _ => false,
            },
            Element::Path(first) => match &second {
                Element::Path(second) => merge_path(first, second),
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
}

/// Merge `second` into `first` when both are visually equivalent.
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
    use crate::fixed::PathElement;
    use crate::geometry::Point;
    use crate::paint::{FillRule, Rgba};
    use crate::path::Path;

    fn font() -> Arc<FontAsset> {
        Arc::new(FontAsset::new(Arc::from(&[][..]), 0))
    }

    fn text(run_text: &str, x: f64, glyph_x: f64) -> Element {
        text_with(&font(), run_text, x, glyph_x)
    }

    fn text_with(font: &Arc<FontAsset>, run_text: &str, x: f64, glyph_x: f64) -> Element {
        Element::Text(TextElement {
            font: Arc::clone(font),
            font_size: 10.0,
            transform: Matrix::translate(x, 0.0),
            run: Arc::new(GlyphRun::new(
                run_text,
                vec![Glyph {
                    id: 1,
                    x: glyph_x,
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

    #[test]
    fn merges_translation_adjacent_text() {
        let font = font();
        let (compacted, stats) = scene(vec![
            text_with(&font, "A", 0.0, 0.0),
            text_with(&font, "B", 10.0, 0.0),
        ])
        .compact();
        assert_eq!(stats.before, 2);
        assert_eq!(stats.after, 1);
        let Element::Text(text) = &compacted.root.children[0] else {
            panic!("expected text");
        };
        assert_eq!(text.run.text, "AB");
        assert_eq!(text.run.glyphs.len(), 2);
        assert_eq!(text.run.glyphs[1].x, 10.0);
        assert_eq!(text.run.clusters[1].text_start, 1);
        assert_eq!(text.run.clusters[1].glyph_start, 1);
    }

    #[test]
    fn does_not_merge_different_styles_or_rotations() {
        let mut second = text("B", 10.0, 0.0);
        let Element::Text(second_text) = &mut second else {
            panic!("expected text");
        };
        second_text.fill = Some(Paint::Color(Rgba::from_rgb(255, 0, 0)));
        let (_compacted, stats) = scene(vec![text("A", 0.0, 0.0), second]).compact();
        assert_eq!(stats.after, 2);

        let mut rotated = text("C", 20.0, 0.0);
        let Element::Text(rotated_text) = &mut rotated else {
            panic!("expected text");
        };
        rotated_text.transform = Matrix::scale(1.0, 1.0).then(Matrix::translate(20.0, 0.0));
        rotated_text.transform = Matrix {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            e: 20.0,
            f: 0.0,
        };
        let (_compacted, stats) = scene(vec![text("A", 0.0, 0.0), rotated]).compact();
        assert_eq!(stats.after, 2);
    }

    #[test]
    fn does_not_merge_across_paths() {
        let mut path = Path::new();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(1.0, 0.0));
        path.line_to(Point::new(1.0, 1.0));
        path.close();
        let path = Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: Some(Paint::Color(Rgba::BLACK)),
            stroke: None,
            rule: FillRule::NonZero,
        });
        let (compacted, stats) =
            scene(vec![text("A", 0.0, 0.0), path, text("B", 10.0, 0.0)]).compact();
        assert_eq!(stats.after, 3);
        assert_eq!(compacted.root.children.len(), 3);
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
}
