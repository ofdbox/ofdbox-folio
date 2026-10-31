//! Render-time scene compaction.
//!
//! PDF-to-OFD converters often emit one wrapper group per object, even when the
//! object is a single glyph or a tiny path. That is correct but expensive to
//! rasterize: the renderer traverses thousands of groups and elements.
//! [`Scene::compact`] produces a copy of a scene with transparent wrapper groups
//! flattened and disjoint, opaque paths with identical paint merged.
//!
//! Text elements are intentionally not merged in the default path: combining
//! separate glyph runs changes anti-aliasing by a few pixels, which violates the
//! strict pixel-regression contract. Flattening alone already removes most of the
//! traversal cost for PDF-to-OFD documents.
//!
//! The original scene is never modified: the text layer, export and document
//! conversion paths keep using the original IR. This is a render-only optimization.

use std::sync::Arc;

use crate::fixed::{Element, Group, PathElement, Scene};
use crate::geometry::{Matrix, Rect};
use crate::paint::{Paint, StrokePaint};
use crate::path::Path;

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
        flatten_group(&mut scene.root);
        merge_group(&mut scene.root);
        let after = count_elements(&scene.root);
        (scene, MergeStats { before, after })
    }
}

impl Group {
    /// Merges adjacent disjoint opaque paths and flattens plain wrapper groups
    /// in this group and its descendants, returning the number of removed elements.
    pub fn merge_adjacent(&mut self) -> usize {
        let before = count_elements(self);
        flatten_group(self);
        merge_group(self);
        before.saturating_sub(count_elements(self))
    }
}

/// Remove groups that are only transform wrappers.
///
/// Groups without a clip or opacity do not create a layer; their children can be
/// spliced into the parent with the group transform baked into each child. This
/// exposes sequences of leaf elements inside the usual PDF-to-OFD wrapper shape
/// (object → transform group → clip group) to the path merge pass.
///
/// Groups with a clip are kept: dropping a clip layer changes anti-aliasing in
/// some documents, which would violate the strict pixel-regression contract.
fn flatten_group(group: &mut Group) {
    for child in &mut group.children {
        if let Element::Group(child) = child {
            flatten_group(child);
        }
    }
    let mut index = 0;
    while index < group.children.len() {
        let flattenable = match &group.children[index] {
            Element::Group(inner) => can_flatten(inner),
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
    }
}

fn can_flatten(group: &Group) -> bool {
    group.clip.is_none() && group.opacity >= 1.0 - f32::EPSILON
}

fn bake_transform(element: &mut Element, parent: &Matrix) {
    match element {
        Element::Text(text) => text.transform = text.transform.then(*parent),
        Element::Path(path) => path.transform = path.transform.then(*parent),
        Element::Image(image) => image.transform = image.transform.then(*parent),
        Element::Group(group) => group.transform = group.transform.then(*parent),
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
    use crate::fixed::PathElement;
    use crate::geometry::Point;
    use crate::paint::{FillRule, Rgba};
    use crate::path::Path;

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
}
