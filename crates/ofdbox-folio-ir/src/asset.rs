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

/// 把灰度蒙版乘入 RGBA8 像素的 alpha 通道。
///
/// 蒙版与图片描述同一块物理区域，但生产方常按不同分辨率存放（例如 25×25 的
/// 图片配 75×75 的 1 bit 蒙版）。尺寸不同时把蒙版按面积平均重采样到图片像素
/// 网格，因此调用方不需要自己处理缩放。
///
/// 返回的错误说明蒙版为什么不可用；调用方应按无蒙版降级或报错，而不是使用半张蒙版。
pub fn apply_image_mask(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    mask: &[u8],
    mask_width: u32,
    mask_height: u32,
) -> Result<(), &'static str> {
    if mask_width == 0 || mask_height == 0 {
        return Err("蒙版尺寸为 0");
    }
    if mask.len() < mask_width as usize * mask_height as usize * 4 {
        return Err("蒙版像素数据不完整");
    }
    if rgba.len() < width as usize * height as usize * 4 {
        return Err("图像像素数据不完整");
    }
    if mask_width == width && mask_height == height {
        for (pixel, mask_pixel) in rgba
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(mask.as_chunks::<4>().0)
        {
            pixel[3] = apply_factor(pixel[3], mask_factor(mask_pixel));
        }
        return Ok(());
    }
    let mask_width = mask_width as usize;
    for y in 0..height {
        let (row_start, row_end) = sample_range(y, height, mask_height);
        for x in 0..width {
            let (column_start, column_end) = sample_range(x, width, mask_width as u32);
            let mut sum = 0_u64;
            let mut count = 0_u64;
            for row in row_start..row_end {
                for column in column_start..column_end {
                    let index = (row as usize * mask_width + column as usize) * 4;
                    sum += u64::from(mask[index])
                        + u64::from(mask[index + 1])
                        + u64::from(mask[index + 2]);
                    count += 1;
                }
            }
            let factor = if count == 0 {
                255
            } else {
                (sum / (count * 3)) as u32
            };
            let index = (y as usize * width as usize + x as usize) * 4 + 3;
            rgba[index] = apply_factor(rgba[index], factor);
        }
    }
    Ok(())
}

/// 蒙版像素的灰度。
fn mask_factor(mask_pixel: &[u8]) -> u32 {
    (u32::from(mask_pixel[0]) + u32::from(mask_pixel[1]) + u32::from(mask_pixel[2])) / 3
}

/// 把 0–255 的灰度系数乘到 alpha 上。
fn apply_factor(alpha: u8, factor: u32) -> u8 {
    ((u32::from(alpha) * factor) / 255) as u8
}

/// 目标像素 `index` 在源像素网格上覆盖的半开区间。
fn sample_range(index: u32, dst_len: u32, src_len: u32) -> (u32, u32) {
    let start = (u64::from(index) * u64::from(src_len) / u64::from(dst_len)) as u32;
    let start = start.min(src_len - 1);
    let end = ((u64::from(index) + 1) * u64::from(src_len) / u64::from(dst_len)) as u32;
    (start, end.max(start + 1).min(src_len))
}

/// 按文件头识别图片编码格式。
///
/// 只看魔数，不做完整解码；无法识别时返回 `None`。
pub fn detect_image_format(data: &[u8]) -> Option<ImageFormat> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(ImageFormat::Png)
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFormat::Jpeg)
    } else if data.starts_with(b"BM") {
        Some(ImageFormat::Bmp)
    } else if data.starts_with(&[0x97, b'J', b'B', b'2']) || data.first() == Some(&0x97) {
        Some(ImageFormat::Jbig2)
    } else if data.starts_with(&[0x49, 0x49, 0x2A, 0x00])
        || data.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
    {
        Some(ImageFormat::Tiff)
    } else {
        None
    }
}

/// 从编码字节读出像素尺寸。
///
/// 支持 PNG、JPEG、BMP、TIFF；只需读文件头，不解码像素。
pub fn image_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        if data.len() < 24 {
            return None;
        }
        let width = u32::from_be_bytes(data.get(16..20)?.try_into().ok()?);
        let height = u32::from_be_bytes(data.get(20..24)?.try_into().ok()?);
        return Some((width, height));
    }
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        let mut index = 2usize;
        while index + 9 <= data.len() {
            if data[index] != 0xFF {
                index += 1;
                continue;
            }
            let marker = data[index + 1];
            if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
                index += 2;
                continue;
            }
            let length = usize::from(u16::from_be_bytes([data[index + 2], data[index + 3]]));
            if length < 2 {
                return None;
            }
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                let height = u32::from(u16::from_be_bytes([data[index + 5], data[index + 6]]));
                let width = u32::from(u16::from_be_bytes([data[index + 7], data[index + 8]]));
                return Some((width, height));
            }
            index += 2 + length;
        }
        return None;
    }
    if data.starts_with(b"BM") && data.len() >= 26 {
        let width = i32::from_le_bytes(data.get(18..22)?.try_into().ok()?);
        let height = i32::from_le_bytes(data.get(22..26)?.try_into().ok()?);
        return Some((width.unsigned_abs(), height.unsigned_abs()));
    }
    if data.starts_with(&[0x49, 0x49, 0x2A, 0x00]) {
        return tiff_dimensions(data, true);
    }
    if data.starts_with(&[0x4D, 0x4D, 0x00, 0x2A]) {
        return tiff_dimensions(data, false);
    }
    None
}

fn tiff_dimensions(data: &[u8], little_endian: bool) -> Option<(u32, u32)> {
    let read_u16 = |offset: usize| -> Option<u16> {
        let bytes: [u8; 2] = data.get(offset..offset + 2)?.try_into().ok()?;
        Some(if little_endian {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };
    let read_u32 = |offset: usize| -> Option<u32> {
        let bytes: [u8; 4] = data.get(offset..offset + 4)?.try_into().ok()?;
        Some(if little_endian {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    };
    let ifd = read_u32(4)? as usize;
    let count = read_u16(ifd)? as usize;
    let mut width = None;
    let mut height = None;
    for index in 0..count {
        let entry = ifd + 2 + index * 12;
        let tag = read_u16(entry)?;
        let field_type = read_u16(entry + 2)?;
        let value = match field_type {
            3 => u32::from(read_u16(entry + 8)?),
            4 => read_u32(entry + 8)?,
            _ => continue,
        };
        match tag {
            256 => width = Some(value),
            257 => height = Some(value),
            _ => {}
        }
        if width.is_some() && height.is_some() {
            return Some((width?, height?));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        AssetId, FontAsset, ImageAsset, ImageFormat, ResourceStore, apply_image_mask,
        detect_image_format, image_dimensions,
    };
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

    #[test]
    fn mask_reduces_alpha_by_gray_level() {
        let mut pixels = vec![10, 20, 30, 200];
        apply_image_mask(&mut pixels, 1, 1, &[128, 128, 128, 255], 1, 1).expect("mask");
        assert_eq!(pixels[3], 100);
    }

    #[test]
    fn mask_is_downsampled_to_image_grid() {
        let mut pixels = [10, 20, 30, 255].repeat(4);
        // 4×4 蒙版：上半黑、下半白，与 2×2 图片描述同一块区域。
        let mut mask = Vec::new();
        for y in 0..4 {
            let value = if y < 2 { 0 } else { 255 };
            for _ in 0..4 {
                mask.extend_from_slice(&[value, value, value, 255]);
            }
        }
        apply_image_mask(&mut pixels, 2, 2, &mask, 4, 4).expect("mask");
        let alphas: Vec<u8> = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .collect();
        assert_eq!(alphas, [0, 0, 255, 255]);
    }

    #[test]
    fn unusable_mask_is_rejected() {
        let mut pixels = [10, 20, 30, 255].repeat(4);
        assert!(apply_image_mask(&mut pixels, 2, 2, &[], 0, 0).is_err());
        assert!(apply_image_mask(&mut pixels, 2, 2, &[0, 0, 0, 255], 2, 2).is_err());
    }

    #[test]
    fn detects_formats_from_magic_bytes() {
        assert_eq!(
            detect_image_format(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            Some(ImageFormat::Png)
        );
        assert_eq!(
            detect_image_format(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(detect_image_format(b"BM\x00\x00"), Some(ImageFormat::Bmp));
        assert_eq!(
            detect_image_format(&[0x49, 0x49, 0x2A, 0x00]),
            Some(ImageFormat::Tiff)
        );
        assert_eq!(detect_image_format(b"not-an-image"), None);
    }

    #[test]
    fn reads_png_and_bmp_dimensions() {
        // 只有文件头也要能读出尺寸。
        let mut png = vec![0_u8; 24];
        png[..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        png[16..20].copy_from_slice(&32_u32.to_be_bytes());
        png[20..24].copy_from_slice(&16_u32.to_be_bytes());
        assert_eq!(image_dimensions(&png), Some((32, 16)));

        let mut bmp = vec![0_u8; 26];
        bmp[..2].copy_from_slice(b"BM");
        bmp[18..22].copy_from_slice(&(-40_i32).to_le_bytes());
        bmp[22..26].copy_from_slice(&25_i32.to_le_bytes());
        assert_eq!(image_dimensions(&bmp), Some((40, 25)));
        assert_eq!(image_dimensions(b"junk"), None);
    }
}
