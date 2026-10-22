//! 资源标识、字体、图片与文档级资源表。
//!
//! 资源 ID 在文档内分配，`0` 表示未分配。字体和图片按内容去重，页面元素
//! 通过共享句柄引用资源，不复制字节。

use std::sync::Arc;

/// 字体或图片资源标识。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetId(u64);

impl AssetId {
    /// 未分配标识。
    pub const UNASSIGNED: Self = Self(0);

    /// 以原始值构造。
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// 原始值。
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// 附件标识。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttachmentId(u64);

impl AttachmentId {
    /// 未分配标识。
    pub const UNASSIGNED: Self = Self(0);

    /// 以原始值构造。
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// 原始值。
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// 模板层标识。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TemplateId(u64);

impl TemplateId {
    /// 未分配标识。
    pub const UNASSIGNED: Self = Self(0);

    /// 以原始值构造。
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// 原始值。
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// 标题或书签锚点标识。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnchorId(u64);

impl AnchorId {
    /// 未分配标识。
    pub const UNASSIGNED: Self = Self(0);

    /// 以原始值构造。
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// 原始值。
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// 内嵌字体资源。
#[derive(Debug, PartialEq)]
pub struct FontAsset {
    id: AssetId,
    /// 字体文件字节。
    pub data: Arc<[u8]>,
    /// 字体集合内的 face 索引。
    pub face_index: u32,
    /// 字体名称，对应 OFD `FontName`。
    pub font_name: Option<String>,
    /// 字体家族名。
    pub family: Option<String>,
    /// PostScript 名称。
    pub postscript_name: Option<String>,
}

impl FontAsset {
    /// 创建字体资源，ID 由资源表分配。
    pub fn new(data: Arc<[u8]>, face_index: u32) -> Self {
        Self {
            id: AssetId::UNASSIGNED,
            data,
            face_index,
            font_name: None,
            family: None,
            postscript_name: None,
        }
    }

    /// 设置字体名称。
    pub fn with_font_name(mut self, name: impl Into<String>) -> Self {
        self.font_name = Some(name.into());
        self
    }

    /// 设置字体家族名。
    pub fn with_family(mut self, family: impl Into<String>) -> Self {
        self.family = Some(family.into());
        self
    }

    /// 设置 PostScript 名称。
    pub fn with_postscript_name(mut self, name: impl Into<String>) -> Self {
        self.postscript_name = Some(name.into());
        self
    }

    /// 资源标识。
    pub fn id(&self) -> AssetId {
        self.id
    }

    /// 字体文件字节。
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// 字体集合内的 face 索引。
    pub fn face_index(&self) -> u32 {
        self.face_index
    }
}

/// 图片编码格式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageFormat {
    /// PNG。
    Png,
    /// JPEG。
    Jpeg,
    /// BMP。
    Bmp,
    /// TIFF。
    Tiff,
    /// JBIG2。
    Jbig2,
    /// CCITT Group 3/4。
    Ccitt,
    /// 未知格式。
    #[default]
    Unknown,
}

/// 图片资源。
#[derive(Debug, PartialEq)]
pub struct ImageAsset {
    id: AssetId,
    /// 编码格式。
    pub format: ImageFormat,
    /// 原始编码字节。
    pub encoded: Arc<[u8]>,
    /// 像素宽度。
    pub pixel_width: u32,
    /// 像素高度。
    pub pixel_height: u32,
    /// 可选 DPI。
    pub dpi: Option<(f64, f64)>,
    /// 可选透明度蒙版。
    pub mask: Option<Arc<ImageAsset>>,
}

impl ImageAsset {
    /// 创建图片资源，ID 由资源表分配。
    pub fn new(
        format: ImageFormat,
        encoded: Arc<[u8]>,
        pixel_width: u32,
        pixel_height: u32,
    ) -> Self {
        Self {
            id: AssetId::UNASSIGNED,
            format,
            encoded,
            pixel_width,
            pixel_height,
            dpi: None,
            mask: None,
        }
    }

    /// 设置 DPI。
    pub fn with_dpi(mut self, x: f64, y: f64) -> Self {
        self.dpi = Some((x, y));
        self
    }

    /// 设置透明度蒙版。
    pub fn with_mask(mut self, mask: Arc<ImageAsset>) -> Self {
        self.mask = Some(mask);
        self
    }

    /// 资源标识。
    pub fn id(&self) -> AssetId {
        self.id
    }
}

/// 文档级字体与图片资源表。
///
/// 相同内容只保存一份，并分配稳定 ID。ID 从 1 开始，`0` 保留给未分配状态。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResourceStore {
    next_id: u64,
    fonts: Vec<Arc<FontAsset>>,
    images: Vec<Arc<ImageAsset>>,
}

impl ResourceStore {
    /// 创建空资源表。
    pub fn new() -> Self {
        Self::default()
    }

    fn allocate(&mut self) -> AssetId {
        self.next_id += 1;
        AssetId(self.next_id)
    }

    /// 添加字体；相同字节和 face 索引会复用已有资源。
    pub fn add_font(&mut self, data: Arc<[u8]>, face_index: u32) -> AssetId {
        if let Some(existing) = self
            .fonts
            .iter()
            .find(|font| font.face_index == face_index && font.data == data)
        {
            return existing.id;
        }
        let id = self.allocate();
        self.fonts.push(Arc::new(FontAsset {
            id,
            data,
            face_index,
            font_name: None,
            family: None,
            postscript_name: None,
        }));
        id
    }

    /// 添加带元数据的字体；相同字节、face 索引与名称会复用已有资源。
    pub fn add_font_asset(&mut self, mut asset: FontAsset) -> AssetId {
        if let Some(existing) = self.fonts.iter().find(|font| {
            font.face_index == asset.face_index
                && font.data == asset.data
                && font.font_name == asset.font_name
                && font.family == asset.family
                && font.postscript_name == asset.postscript_name
        }) {
            return existing.id;
        }
        asset.id = self.allocate();
        let id = asset.id;
        self.fonts.push(Arc::new(asset));
        id
    }

    /// 添加图片；相同编码、尺寸和蒙版会复用已有资源。
    pub fn add_image(&mut self, mut image: ImageAsset) -> AssetId {
        if let Some(existing) = self
            .images
            .iter()
            .find(|candidate| same_image(candidate, &image))
        {
            return existing.id;
        }
        image.id = self.allocate();
        let id = image.id;
        self.images.push(Arc::new(image));
        id
    }

    /// 按 ID 查找字体。
    pub fn font(&self, id: AssetId) -> Option<&Arc<FontAsset>> {
        self.fonts.iter().find(|font| font.id == id)
    }

    /// 按 ID 查找图片。
    pub fn image(&self, id: AssetId) -> Option<&Arc<ImageAsset>> {
        self.images.iter().find(|image| image.id == id)
    }

    /// 遍历全部字体。
    pub fn fonts(&self) -> impl Iterator<Item = &Arc<FontAsset>> {
        self.fonts.iter()
    }

    /// 遍历全部图片。
    pub fn images(&self) -> impl Iterator<Item = &Arc<ImageAsset>> {
        self.images.iter()
    }

    /// 字体数量。
    pub fn font_count(&self) -> usize {
        self.fonts.len()
    }

    /// 图片数量。
    pub fn image_count(&self) -> usize {
        self.images.len()
    }
}

fn same_image(a: &ImageAsset, b: &ImageAsset) -> bool {
    a.format == b.format
        && a.pixel_width == b.pixel_width
        && a.pixel_height == b.pixel_height
        && a.dpi == b.dpi
        && a.encoded == b.encoded
        && match (&a.mask, &b.mask) {
            (None, None) => true,
            (Some(a), Some(b)) => a.format == b.format && a.encoded == b.encoded,
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::{AssetId, FontAsset, ImageAsset, ImageFormat, ResourceStore};
    use std::sync::Arc;

    #[test]
    fn fonts_are_deduplicated_by_content() {
        let mut store = ResourceStore::new();
        let first = store.add_font(Arc::from(&b"font"[..]), 0);
        let second = store.add_font(Arc::from(&b"font"[..]), 0);
        let other_face = store.add_font(Arc::from(&b"font"[..]), 1);
        assert_eq!(first, second);
        assert_ne!(first, other_face);
        assert_eq!(store.font_count(), 2);
        assert!(first.get() >= 1);
        assert!(store.font(first).is_some());
    }

    #[test]
    fn fonts_keep_distinct_names_for_same_bytes() {
        let mut store = ResourceStore::new();
        let bytes: Arc<[u8]> = Arc::from(&b"font"[..]);
        let first = store.add_font_asset(FontAsset::new(Arc::clone(&bytes), 0).with_family("A"));
        let second = store.add_font_asset(FontAsset::new(Arc::clone(&bytes), 0).with_family("B"));
        let same = store.add_font_asset(FontAsset::new(bytes, 0).with_family("A"));
        assert_ne!(first, second);
        assert_eq!(first, same);
        assert_eq!(store.font_count(), 2);
    }

    #[test]
    fn images_are_deduplicated_by_content() {
        let mut store = ResourceStore::new();
        let image = || ImageAsset::new(ImageFormat::Png, Arc::from(&b"png"[..]), 10, 20);
        let first = store.add_image(image());
        let second = store.add_image(image());
        assert_eq!(first, second);
        assert_eq!(store.image_count(), 1);
        assert_eq!(store.image(first).map(|image| image.pixel_width), Some(10));
    }

    #[test]
    fn unassigned_id_is_zero() {
        assert_eq!(AssetId::default(), AssetId::UNASSIGNED);
        assert_eq!(AssetId::UNASSIGNED.get(), 0);
    }
}
