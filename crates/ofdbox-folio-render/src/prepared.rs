//! Backend-neutral prepared scene.
//!
//! A [`RenderPlan`] is a flat command list derived from a Fixed IR
//! [`Scene`](ofdbox_folio_ir::Scene). It resolves group transforms, emits an
//! explicit layer stack for clips and group opacity, and keeps paints and
//! resources in page space. Rendering backends can consume the same list
//! instead of walking the semantic scene tree themselves.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use ofdbox_folio_ir::{
    Diagnostic, Element, FillRule, FontAsset, GlyphRun, Group, ImageAsset, LinkArea, Matrix, Page,
    PageInfo, Paint, Path, Rect, Scene, Size, StrokePaint, TextLayer,
};

/// A clip path resolved into page space.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderClip {
    /// Clipping path in page space.
    pub path: Arc<Path>,
    /// Fill rule for the clip path.
    pub rule: FillRule,
    /// Transform from the path's local space to page space.
    pub transform: Matrix,
}

/// One command in a prepared scene.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum RenderCommand {
    /// Begin an isolated layer with an optional clip and opacity.
    PushLayer {
        /// Clip path in page space, if the group was clipped.
        clip: Option<RenderClip>,
        /// Layer opacity in `0.0..=1.0`.
        opacity: f32,
    },
    /// End the most recently pushed layer.
    PopLayer,
    /// Fill a path.
    FillPath {
        /// Path in local space.
        path: Arc<Path>,
        /// Transform from local to page space.
        transform: Matrix,
        /// Fill paint.
        paint: Paint,
        /// Fill rule.
        rule: FillRule,
    },
    /// Stroke a path.
    StrokePath {
        /// Path in local space.
        path: Arc<Path>,
        /// Transform from local to page space.
        transform: Matrix,
        /// Stroke paint and style.
        stroke: StrokePaint,
    },
    /// Draw an image.
    DrawImage {
        /// Image resource.
        image: Arc<ImageAsset>,
        /// Image rectangle in local space.
        rect: Rect,
        /// Transform from local to page space.
        transform: Matrix,
        /// Image opacity in `0.0..=1.0`.
        alpha: f32,
    },
    /// Draw a shaped text run.
    DrawText {
        /// Font resource.
        font: Arc<FontAsset>,
        /// Font size in millimetres.
        font_size: f64,
        /// Transform from text local space to page space.
        transform: Matrix,
        /// Positioned glyphs and character mapping.
        run: Arc<GlyphRun>,
        /// Fill paint.
        fill: Option<Paint>,
        /// Stroke paint and style.
        stroke: Option<StrokePaint>,
    },
}

/// A page-sized, backend-neutral command list.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderPlan {
    /// Page display size, in millimetres.
    pub size: Size,
    /// Flattened commands in painting order.
    pub commands: Vec<RenderCommand>,
    /// Conservative page-space bounds of the whole plan.
    pub bounds: Option<Rect>,
}

impl RenderPlan {
    /// Resolves a Fixed IR scene into a prepared command list.
    pub fn from_scene(scene: &Scene) -> Self {
        let mut commands = Vec::new();
        walk_group(&scene.root, Matrix::IDENTITY, &mut commands);
        let bounds = (scene.size.is_valid() && !scene.size.is_empty()).then_some(Rect::new(
            0.0,
            0.0,
            scene.size.width,
            scene.size.height,
        ));
        Self {
            size: scene.size,
            commands,
            bounds,
        }
    }

    /// Number of emitted commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Whether the scene contains no commands.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// Alias kept for the previous display-list name.
pub type PreparedScene = RenderPlan;
/// Alias kept for the previous command name.
pub type PreparedCommand = RenderCommand;
/// Alias kept for the previous clip name.
pub type PreparedClip = RenderClip;

/// Immutable page data crossing the parse/render boundary.
#[derive(Clone, Debug)]
pub struct PageSnapshot {
    /// Page information.
    pub info: PageInfo,
    /// Backend-neutral render plan.
    pub plan: Arc<RenderPlan>,
    /// Text layer for selection, search and hit testing.
    pub text: Arc<TextLayer>,
    /// Link areas.
    pub links: Arc<[LinkArea]>,
    /// Page diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

impl PageSnapshot {
    /// Builds a snapshot, consuming the page so parser-owned state cannot cross threads.
    pub fn from_page(page: Page) -> Self {
        let plan = Arc::new(RenderPlan::from_scene(&page.content));
        Self {
            info: page.info,
            plan,
            text: Arc::new(page.text),
            links: page.links.into(),
            diagnostics: page.content.diagnostics,
        }
    }

    /// The render plan.
    pub fn plan(&self) -> &RenderPlan {
        &self.plan
    }
}

fn walk_group(group: &Group, parent: Matrix, commands: &mut Vec<RenderCommand>) {
    let transform = group.transform.then(parent);
    let layered = group.clip.is_some() || group.opacity < 1.0;
    if layered {
        commands.push(RenderCommand::PushLayer {
            clip: group.clip.as_ref().map(|clip| RenderClip {
                path: Arc::clone(&clip.path),
                rule: clip.rule,
                transform,
            }),
            opacity: group.opacity.clamp(0.0, 1.0),
        });
    }
    for element in &group.children {
        match element {
            Element::Group(child) => walk_group(child, transform, commands),
            Element::Path(path) => {
                let transform = path.transform.then(transform);
                if let Some(paint) = &path.fill {
                    commands.push(RenderCommand::FillPath {
                        path: Arc::clone(&path.path),
                        transform,
                        paint: paint.clone(),
                        rule: path.rule,
                    });
                }
                if let Some(stroke) = &path.stroke {
                    commands.push(RenderCommand::StrokePath {
                        path: Arc::clone(&path.path),
                        transform,
                        stroke: stroke.clone(),
                    });
                }
            }
            Element::Image(image) => commands.push(RenderCommand::DrawImage {
                image: Arc::clone(&image.image),
                rect: image.rect,
                transform: image.transform.then(transform),
                alpha: image.alpha.clamp(0.0, 1.0),
            }),
            Element::Text(text) => commands.push(RenderCommand::DrawText {
                font: Arc::clone(&text.font),
                font_size: text.font_size,
                transform: text.transform.then(transform),
                run: Arc::clone(&text.run),
                fill: text.fill.clone(),
                stroke: text.stroke.clone(),
            }),
            _ => {}
        }
    }
    if layered {
        commands.push(RenderCommand::PopLayer);
    }
}

pub(crate) fn scene_fingerprint(scene: &Scene) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    scene.size.width.to_bits().hash(&mut hasher);
    scene.size.height.to_bits().hash(&mut hasher);
    hash_group(&scene.root, &mut hasher);
    hasher.finish()
}

fn hash_group(group: &Group, hasher: &mut impl Hasher) {
    for value in [
        group.transform.a,
        group.transform.b,
        group.transform.c,
        group.transform.d,
        group.transform.e,
        group.transform.f,
    ] {
        value.to_bits().hash(hasher);
    }
    group.opacity.to_bits().hash(hasher);
    group
        .clip
        .as_ref()
        .map(|clip| Arc::as_ptr(&clip.path) as usize)
        .hash(hasher);
    group.children.len().hash(hasher);
    for element in &group.children {
        match element {
            Element::Group(child) => {
                1_u8.hash(hasher);
                hash_group(child, hasher);
            }
            Element::Path(path) => {
                2_u8.hash(hasher);
                (Arc::as_ptr(&path.path) as usize).hash(hasher);
                for value in [
                    path.transform.a,
                    path.transform.b,
                    path.transform.c,
                    path.transform.d,
                    path.transform.e,
                    path.transform.f,
                ] {
                    value.to_bits().hash(hasher);
                }
            }
            Element::Text(text) => {
                3_u8.hash(hasher);
                (Arc::as_ptr(&text.font) as usize).hash(hasher);
                (Arc::as_ptr(&text.run) as usize).hash(hasher);
                text.font_size.to_bits().hash(hasher);
                for value in [
                    text.transform.a,
                    text.transform.b,
                    text.transform.c,
                    text.transform.d,
                    text.transform.e,
                    text.transform.f,
                ] {
                    value.to_bits().hash(hasher);
                }
            }
            Element::Image(image) => {
                4_u8.hash(hasher);
                (Arc::as_ptr(&image.image) as usize).hash(hasher);
                for value in [
                    image.rect.x,
                    image.rect.y,
                    image.rect.width,
                    image.rect.height,
                ] {
                    value.to_bits().hash(hasher);
                }
            }
            _ => 0_u8.hash(hasher),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ofdbox_folio_ir::{Clip, PathElement};

    fn path_element(x: f64) -> Element {
        let mut path = Path::new();
        path.move_to(ofdbox_folio_ir::Point::new(x, 0.0));
        path.line_to(ofdbox_folio_ir::Point::new(x + 1.0, 0.0));
        Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::translate(1.0, 2.0),
            fill: Some(Paint::Color(ofdbox_folio_ir::Rgba::BLACK)),
            stroke: None,
            rule: FillRule::NonZero,
        })
    }

    fn scene(children: Vec<Element>) -> Scene {
        Scene {
            size: Size::new(10.0, 10.0),
            root: Group {
                children,
                ..Group::default()
            },
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn page_snapshot_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PageSnapshot>();
    }

    #[test]
    fn page_snapshot_owns_the_plan_and_text_layer() {
        let page = Page::new(PageInfo::default(), scene(vec![path_element(0.0)]));
        let snapshot = PageSnapshot::from_page(page);
        assert_eq!(snapshot.plan.commands.len(), 1);
        assert!(snapshot.plan.bounds.is_some());
        assert!(snapshot.text.items.is_empty());
    }

    #[test]
    fn resolves_nested_transforms() {
        let group = Group {
            transform: Matrix::translate(3.0, 4.0),
            children: vec![path_element(0.0)],
            ..Group::default()
        };
        let prepared = RenderPlan::from_scene(&scene(vec![Element::Group(group)]));
        let RenderCommand::FillPath { transform, .. } = &prepared.commands[0] else {
            panic!("expected a fill command");
        };
        let point = transform.apply(ofdbox_folio_ir::Point::new(0.0, 0.0));
        assert!((point.x - 4.0).abs() < 1e-9);
        assert!((point.y - 6.0).abs() < 1e-9);
    }

    #[test]
    fn emits_one_layer_per_clipped_or_translucent_group() {
        let clip = Arc::new(Clip {
            path: Arc::new(Path::new()),
            rule: FillRule::EvenOdd,
        });
        let clipped = Group {
            clip: Some(clip),
            children: vec![path_element(0.0)],
            ..Group::default()
        };
        let translucent = Group {
            opacity: 0.5,
            children: vec![path_element(2.0)],
            ..Group::default()
        };
        let prepared = RenderPlan::from_scene(&scene(vec![
            Element::Group(clipped),
            Element::Group(translucent),
        ]));
        assert_eq!(prepared.len(), 6);
        assert!(matches!(
            prepared.commands[0],
            RenderCommand::PushLayer { clip: Some(_), .. }
        ));
        assert!(matches!(prepared.commands[2], RenderCommand::PopLayer));
        assert!(matches!(
            prepared.commands[3],
            RenderCommand::PushLayer { clip: None, opacity } if (opacity - 0.5).abs() < 1e-6
        ));
        assert!(matches!(prepared.commands[5], RenderCommand::PopLayer));
    }

    #[test]
    fn splits_fill_and_stroke_into_ordered_commands() {
        let mut path = Path::new();
        path.move_to(ofdbox_folio_ir::Point::new(0.0, 0.0));
        path.line_to(ofdbox_folio_ir::Point::new(1.0, 1.0));
        let element = Element::Path(PathElement {
            path: Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: Some(Paint::Color(ofdbox_folio_ir::Rgba::BLACK)),
            stroke: Some(StrokePaint {
                paint: Paint::Color(ofdbox_folio_ir::Rgba::BLACK),
                style: Default::default(),
            }),
            rule: FillRule::NonZero,
        });
        let prepared = RenderPlan::from_scene(&scene(vec![element]));
        assert!(matches!(
            prepared.commands[0],
            RenderCommand::FillPath { .. }
        ));
        assert!(matches!(
            prepared.commands[1],
            RenderCommand::StrokePath { .. }
        ));
    }
}
