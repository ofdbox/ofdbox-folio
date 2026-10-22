//! 固定版式文档模型（Fixed IR）。
//!
//! Fixed IR 是不可变、可缓存的页面集合。坐标使用毫米，原点在页面左上角，
//! x 向右、y 向下；旋转已经应用到内容，`PageInfo` 保留原始页面信息供导出。
//!
//! `FixedDocument` 是布局或导出的完整结果；`Page` 是阅读器的加载单位。
//! 两者使用同一套页面内容树，但阅读器不会一次性构建全部页面。

use std::sync::Arc;

use crate::asset::{AssetId, FontAsset, ImageAsset, ResourceStore, TemplateId};
use crate::flow::{Attachment, LinkTarget};
use crate::geometry::{Matrix, Point, Rect, Rotation, Size};
use crate::meta::{Metadata, Report};
use crate::paint::{FillRule, Paint, StrokePaint};
use crate::path::Path;
use crate::text::{GlyphRun, TextLayer};

/// 完整的固定版式文档。
#[derive(Clone, Debug, PartialEq)]
pub struct FixedDocument {
    /// 文档元数据。
    pub metadata: Metadata,
    /// 页面，按阅读顺序排列。
    pub pages: Vec<Page>,
    /// 共享模板层。
    pub templates: Vec<Arc<TemplateLayer>>,
    /// 已解析到页码和坐标的大纲。
    pub outline: Vec<OutlineItem>,
    /// 附件。
    pub attachments: Vec<Attachment>,
    /// 字体与图片资源。
    pub resources: ResourceStore,
    /// 布局与降级诊断。
    pub report: Report,
    /// 引擎版本与字体指纹。
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

/// 单页固定版式内容。
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    /// 页面信息。
    pub info: PageInfo,
    /// 页面内容树。
    pub content: Scene,
    /// 逻辑阅读顺序的文本层。
    pub text: TextLayer,
    /// 背景与前景模板层。
    pub templates: Vec<Arc<TemplateLayer>>,
    /// 链接区域。
    pub links: Vec<LinkArea>,
}

impl Page {
    /// 创建空页面。
    pub fn new(info: PageInfo, content: Scene) -> Self {
        Self {
            info,
            content,
            text: TextLayer::default(),
            templates: Vec::new(),
            links: Vec::new(),
        }
    }

    /// 页面显示尺寸：裁剪框应用旋转后的尺寸。
    pub fn display_size(&self) -> Size {
        self.info.display_size()
    }

    /// 命中文本位置。
    pub fn hit_test(&self, point: Point) -> Option<crate::text::TextPosition> {
        crate::text::hit_test(&self.text, point)
    }

    /// 命中链接区域。
    pub fn link_at(&self, point: Point) -> Option<&LinkArea> {
        self.links.iter().find(|link| link.rect.contains(point))
    }

    /// 校验页面几何与文本映射的基本一致性。
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

/// 页面信息。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageInfo {
    /// 原始媒体框。
    pub media_box: Rect,
    /// 裁剪框，决定显示区域。
    pub crop_box: Rect,
    /// 原始页面旋转。
    pub rotation: Rotation,
    /// 页码标签；`None` 表示使用物理页码。
    pub label: Option<String>,
}

impl PageInfo {
    /// 应用旋转后的显示尺寸。
    pub fn display_size(&self) -> Size {
        self.rotation
            .apply_to_size(Size::new(self.crop_box.width, self.crop_box.height))
    }
}

/// 页面内容树。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    /// 页面显示尺寸。
    pub size: Size,
    /// 根组。
    pub root: Group,
    /// 构建该页面时产生的诊断。
    pub diagnostics: Vec<crate::meta::Diagnostic>,
}

/// 带变换、裁剪和透明度的组。
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// 组局部空间到父空间的变换。
    pub transform: Matrix,
    /// 可选裁剪区域。
    pub clip: Option<Arc<Clip>>,
    /// 组透明度。
    pub opacity: f32,
    /// 子元素。
    pub children: Vec<Element>,
}

impl Group {
    /// 创建空组。
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

/// 裁剪路径。
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    /// 裁剪路径。
    pub path: Arc<Path>,
    /// 填充规则。
    pub rule: FillRule,
}

/// 页面元素。
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Element {
    /// 子组。
    Group(Group),
    /// 路径。
    Path(PathElement),
    /// 文本。
    Text(TextElement),
    /// 图片。
    Image(ImageElement),
}

/// 路径元素。
#[derive(Clone, Debug, PartialEq)]
pub struct PathElement {
    /// 路径。
    pub path: Arc<Path>,
    /// 元素局部空间到父空间的变换。
    pub transform: Matrix,
    /// 填充画笔。
    pub fill: Option<Paint>,
    /// 描边。
    pub stroke: Option<StrokePaint>,
    /// 填充规则。
    pub rule: FillRule,
}

/// 文本元素。
#[derive(Clone, Debug, PartialEq)]
pub struct TextElement {
    /// 字体资源。
    pub font: Arc<FontAsset>,
    /// 字号，单位毫米。
    pub font_size: f64,
    /// 文本局部空间到父空间的变换。
    pub transform: Matrix,
    /// 已定位的字形与字符映射。
    pub run: Arc<GlyphRun>,
    /// 填充画笔。
    pub fill: Option<Paint>,
    /// 描边。
    pub stroke: Option<StrokePaint>,
}

/// 图片元素。
#[derive(Clone, Debug, PartialEq)]
pub struct ImageElement {
    /// 图片资源。
    pub image: Arc<ImageAsset>,
    /// 图片局部矩形。
    pub rect: Rect,
    /// 图片局部空间到父空间的变换。
    pub transform: Matrix,
    /// 图片透明度，范围 0.0 到 1.0。
    pub alpha: f32,
}

/// 共享模板层。
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateLayer {
    /// 模板标识。
    pub id: TemplateId,
    /// 模板名称。
    pub name: Option<String>,
    /// 绘制层级。
    pub z_order: TemplateZOrder,
    /// 模板内容。
    pub scene: Scene,
}

/// 模板绘制层级。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TemplateZOrder {
    /// 位于页面内容之下。
    #[default]
    Background,
    /// 位于页面内容之上。
    Foreground,
}

/// 链接区域。
#[derive(Clone, Debug, PartialEq)]
pub struct LinkArea {
    /// 可点击矩形。
    pub rect: Rect,
    /// 链接目标。
    pub target: LinkTarget,
}

/// 大纲条目。
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineItem {
    /// 标题。
    pub title: String,
    /// 跳转目标。
    pub destination: Destination,
    /// 子条目。
    pub children: Vec<OutlineItem>,
}

/// 跳转目标。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Destination {
    /// 页面索引。
    pub page: usize,
    /// 页面坐标。
    pub point: Point,
}

/// 引擎版本与输入指纹。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineInfo {
    /// 布局引擎版本。
    pub layout_version: &'static str,
    /// 参与布局的字体内容指纹。
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

/// 字体内容指纹。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontFingerprint {
    /// 资源标识。
    pub asset: AssetId,
    /// 字体字节的内容哈希。
    pub content_hash: [u8; 32],
}

/// 页面校验错误。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ValidationError {
    /// 页面尺寸非法。
    #[error("页面尺寸非法")]
    InvalidPageSize,
    /// 坐标包含非有限值。
    #[error("几何数据包含非有限值")]
    NonFiniteGeometry,
    /// 文本簇映射越界或倒序。
    #[error("文本簇映射越界或倒序")]
    InvalidTextCluster,
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
