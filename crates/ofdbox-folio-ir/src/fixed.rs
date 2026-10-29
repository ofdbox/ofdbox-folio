//! Fixed-layout document model (Fixed IR).
//!
//! Fixed IR is an immutable, cacheable collection of pages. Coordinates are in
//! millimeters, with the origin at the top-left corner of the page, x pointing right
//! and y pointing down; rotation has already been applied to the content, and
//! `PageInfo` retains the original page information for export.
//!
//! `FixedDocument` is the complete result of layout or export; `Page` is the unit the
//! reader loads. Both use the same page content tree, but the reader does not build
//! all pages at once.

use std::sync::Arc;

use crate::asset::{AssetId, FontAsset, ImageAsset, ResourceStore, TemplateId};
use crate::flow::{Attachment, LinkTarget};
use crate::geometry::{Matrix, Point, Rect, Rotation, Size};
use crate::meta::{Metadata, Report};
use crate::paint::{FillRule, Paint, StrokePaint};
use crate::path::Path;
use crate::text::{GlyphRun, TextLayer};

/// A complete fixed-layout document.
#[derive(Clone, Debug, PartialEq)]
pub struct FixedDocument {
    /// Document metadata.
    pub metadata: Metadata,
    /// Pages, in reading order.
    pub pages: Vec<Page>,
    /// Shared template layers.
    pub templates: Vec<Arc<TemplateLayer>>,
    /// Outline resolved to page numbers and coordinates.
    pub outline: Vec<OutlineItem>,
    /// Attachments.
    pub attachments: Vec<Attachment>,
    /// Font and image resources.
    pub resources: ResourceStore,
    /// Layout and degradation diagnostics.
    pub report: Report,
    /// Engine version and font fingerprints.
    pub engine: EngineInfo,
}

impl Default for FixedDocument {
    fn default() -> Self {
        Self {
            metadata: Metadata::default(),
            pages: Vec::new(),
            templates: Vec::new(),
            outline: Vec::new(),
            attachments: Vec::new(),
            resources: ResourceStore::new(),
            report: Report::new(),
            engine: EngineInfo::default(),
        }
    }
}

/// The fixed-layout content of a single page.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    /// Page information.
    pub info: PageInfo,
    /// Page content tree.
    pub content: Scene,
    /// Text layer in logical reading order.
    pub text: TextLayer,
    /// Background and foreground template layers.
    pub templates: Vec<Arc<TemplateLayer>>,
    /// Link areas.
    pub links: Vec<LinkArea>,
}

impl Page {
    /// Create an empty page.
    pub fn new(info: PageInfo, content: Scene) -> Self {
        Self {
            info,
            content,
            text: TextLayer::default(),
            templates: Vec::new(),
            links: Vec::new(),
        }
    }

    /// The page display size: the crop box size after rotation is applied.
    pub fn display_size(&self) -> Size {
        self.info.display_size()
    }

    /// Hit-test a text position.
    pub fn hit_test(&self, point: Point) -> Option<crate::text::TextPosition> {
        crate::text::hit_test(&self.text, point)
    }

    /// Hit-test a link area.
    pub fn link_at(&self, point: Point) -> Option<&LinkArea> {
        self.links.iter().find(|link| link.rect.contains(point))
    }

    /// Roughly estimate the number of bytes the page structure occupies, for the reader's cache budget.
    ///
    /// Font and image bytes are document-level shared resources and are not counted
    /// toward the per-page cost; the payload shared between text runs and the
    /// `TextLayer` is counted only once.
    pub fn estimated_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + scene_bytes(&self.content)
            + text_layer_bytes(&self.text)
            + self.links.len() * std::mem::size_of::<LinkArea>()
            + self
                .templates
                .iter()
                .map(|template| template.estimated_bytes())
                .sum::<usize>()
    }

    /// Validate the basic consistency of page geometry and text mapping.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !self.info.media_box.is_finite() || !self.info.crop_box.is_finite() {
            return Err(ValidationError::NonFiniteGeometry);
        }
        if !self.content.size.is_valid() || self.content.size.is_empty() {
            return Err(ValidationError::InvalidPageSize);
        }
        for item in &self.text.items {
            if !item.transform.is_finite() || !item.bbox.is_finite() {
                return Err(ValidationError::NonFiniteGeometry);
            }
            let char_count = item.run.text.chars().count();
            for cluster in &item.run.clusters {
                if cluster.text_start > cluster.text_end
                    || cluster.glyph_start > cluster.glyph_end
                    || cluster.text_end as usize > char_count
                    || cluster.glyph_end as usize > item.run.glyphs.len()
                {
                    return Err(ValidationError::InvalidTextCluster);
                }
            }
        }
        Ok(())
    }
}

/// Page information.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageInfo {
    /// Original media box.
    pub media_box: Rect,
    /// Crop box, which determines the display area.
    pub crop_box: Rect,
    /// Original page rotation.
    pub rotation: Rotation,
    /// Page label; `None` means use the physical page number.
    pub label: Option<String>,
}

impl PageInfo {
    /// The display size after applying the rotation.
    pub fn display_size(&self) -> Size {
        self.rotation
            .apply_to_size(Size::new(self.crop_box.width, self.crop_box.height))
    }
}

/// The page content tree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    /// Page display size.
    pub size: Size,
    /// Root group.
    pub root: Group,
    /// Diagnostics produced while building this page.
    pub diagnostics: Vec<crate::meta::Diagnostic>,
}

/// A group with a transform, clip, and opacity.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// Transform from the group local space to the parent space.
    pub transform: Matrix,
    /// Optional clipping region.
    pub clip: Option<Arc<Clip>>,
    /// Group opacity.
    pub opacity: f32,
    /// Child elements.
    pub children: Vec<Element>,
}

impl Group {
    /// Create an empty group.
    pub fn new(transform: Matrix) -> Self {
        Self {
            transform,
            clip: None,
            opacity: 1.0,
            children: Vec::new(),
        }
    }
}

impl Default for Group {
    fn default() -> Self {
        Self::new(Matrix::IDENTITY)
    }
}

/// A clipping path.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    /// Clipping path.
    pub path: Arc<Path>,
    /// Fill rule.
    pub rule: FillRule,
}

/// A page element.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Element {
    /// A child group.
    Group(Group),
    /// A path.
    Path(PathElement),
    /// Text.
    Text(TextElement),
    /// An image.
    Image(ImageElement),
}

/// A path element.
#[derive(Clone, Debug, PartialEq)]
pub struct PathElement {
    /// Path.
    pub path: Arc<Path>,
    /// Transform from the element local space to the parent space.
    pub transform: Matrix,
    /// Fill paint.
    pub fill: Option<Paint>,
    /// Stroke.
    pub stroke: Option<StrokePaint>,
    /// Fill rule.
    pub rule: FillRule,
}

/// A text element.
#[derive(Clone, Debug, PartialEq)]
pub struct TextElement {
    /// Font resource.
    pub font: Arc<FontAsset>,
    /// Font size, in millimeters.
    pub font_size: f64,
    /// Transform from the text local space to the parent space.
    pub transform: Matrix,
    /// Positioned glyphs and character mapping.
    pub run: Arc<GlyphRun>,
    /// Fill paint.
    pub fill: Option<Paint>,
    /// Stroke.
    pub stroke: Option<StrokePaint>,
}

/// An image element.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageElement {
    /// Image resource.
    pub image: Arc<ImageAsset>,
    /// Rectangle in image local space.
    pub rect: Rect,
    /// Transform from the image local space to the parent space.
    pub transform: Matrix,
    /// Image opacity, ranging from 0.0 to 1.0.
    pub alpha: f32,
}

/// A shared template layer.
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateLayer {
    /// Template identifier.
    pub id: TemplateId,
    /// Template name.
    pub name: Option<String>,
    /// Drawing level.
    pub z_order: TemplateZOrder,
    /// Template content.
    pub scene: Scene,
}

/// The drawing level of a template.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TemplateZOrder {
    /// Drawn below the page content.
    #[default]
    Background,
    /// Drawn above the page content.
    Foreground,
}

/// A link area.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkArea {
    /// Clickable rectangle.
    pub rect: Rect,
    /// Link target.
    pub target: LinkTarget,
}

/// An outline item.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineItem {
    /// Title.
    pub title: String,
    /// Jump destination.
    pub destination: Destination,
    /// Child items.
    pub children: Vec<OutlineItem>,
}

/// A jump destination.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Destination {
    /// Page index.
    pub page: usize,
    /// Page coordinates.
    pub point: Point,
}

/// Engine version and input fingerprints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineInfo {
    /// Layout engine version.
    pub layout_version: &'static str,
    /// Fingerprints of the font content that participated in layout.
    pub font_fingerprints: Vec<FontFingerprint>,
}

impl Default for EngineInfo {
    fn default() -> Self {
        Self {
            layout_version: env!("CARGO_PKG_VERSION"),
            font_fingerprints: Vec::new(),
        }
    }
}

/// A font content fingerprint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontFingerprint {
    /// Resource identifier.
    pub asset: AssetId,
    /// Content hash of the font bytes.
    pub content_hash: [u8; 32],
}

/// A page validation error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ValidationError {
    /// Invalid page size.
    #[error("invalid page size")]
    InvalidPageSize,
    /// Coordinates contain non-finite values.
    #[error("geometry data contains non-finite values")]
    NonFiniteGeometry,
    /// Text cluster mapping is out of bounds or reversed.
    #[error("text cluster mapping is out of bounds or reversed")]
    InvalidTextCluster,
}

impl TemplateLayer {
    /// Roughly estimate the number of bytes the template structure occupies.
    pub fn estimated_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.name.as_ref().map_or(0, String::len)
            + scene_bytes(&self.scene)
    }
}

fn scene_bytes(scene: &Scene) -> usize {
    std::mem::size_of::<Scene>()
        + group_bytes(&scene.root)
        + scene
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.len() + 32)
            .sum::<usize>()
}

fn group_bytes(group: &Group) -> usize {
    let clip = group.clip.as_ref().map_or(0, |clip| path_bytes(&clip.path));
    clip + group
        .children
        .iter()
        .map(|element| match element {
            Element::Group(group) => std::mem::size_of::<Group>() + group_bytes(group),
            Element::Path(path) => std::mem::size_of::<PathElement>() + path_bytes(&path.path),
            Element::Text(text) => {
                std::mem::size_of::<TextElement>()
                    + text.run.text.len()
                    + text.run.glyphs.len() * std::mem::size_of::<crate::text::Glyph>()
                    + text.run.clusters.len() * std::mem::size_of::<crate::text::Cluster>()
            }
            Element::Image(_) => std::mem::size_of::<ImageElement>(),
        })
        .sum::<usize>()
}

fn path_bytes(path: &Path) -> usize {
    std::mem::size_of_val(path.segments())
}

fn text_layer_bytes(layer: &TextLayer) -> usize {
    std::mem::size_of::<TextLayer>()
        + layer.items.len() * std::mem::size_of::<crate::text::TextItem>()
        + layer.lines.len() * std::mem::size_of::<crate::text::TextLine>()
}

#[cfg(test)]
mod tests {
    use super::{Page, PageInfo, Scene, ValidationError};
    use crate::geometry::{Rect, Rotation, Size};

    #[test]
    fn empty_page_is_valid() {
        let page = Page::new(
            PageInfo {
                media_box: Rect::new(0.0, 0.0, 210.0, 297.0),
                crop_box: Rect::new(0.0, 0.0, 210.0, 297.0),
                rotation: Rotation::None,
                label: None,
            },
            Scene {
                size: Size::new(210.0, 297.0),
                ..Scene::default()
            },
        );
        assert!(page.validate().is_ok());
        assert_eq!(page.display_size(), Size::new(210.0, 297.0));
        assert!(page.estimated_bytes() > 0);
    }

    #[test]
    fn invalid_size_is_rejected() {
        let page = Page::new(PageInfo::default(), Scene::default());
        assert_eq!(page.validate(), Err(ValidationError::InvalidPageSize));
    }

    #[test]
    fn link_and_hit_test_are_empty_by_default() {
        let page = Page::new(PageInfo::default(), Scene::default());
        assert!(
            page.link_at(crate::geometry::Point::new(0.0, 0.0))
                .is_none()
        );
        assert!(
            page.hit_test(crate::geometry::Point::new(0.0, 0.0))
                .is_none()
        );
    }
}
