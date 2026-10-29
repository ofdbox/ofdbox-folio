//! Resource identifiers, fonts, images, and the document-level resource table.
//!
//! Resource IDs are allocated within the document; `0` means unassigned. Fonts and
//! images are deduplicated by content, and page elements reference resources through
//! shared handles without copying bytes.

use std::sync::Arc;

/// A font or image resource identifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetId(u64);

impl AssetId {
    /// The unassigned identifier.
    pub const UNASSIGNED: Self = Self(0);

    /// Construct from a raw value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// An attachment identifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttachmentId(u64);

impl AttachmentId {
    /// The unassigned identifier.
    pub const UNASSIGNED: Self = Self(0);

    /// Construct from a raw value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A template layer identifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TemplateId(u64);

impl TemplateId {
    /// The unassigned identifier.
    pub const UNASSIGNED: Self = Self(0);

    /// Construct from a raw value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A heading or bookmark anchor identifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnchorId(u64);

impl AnchorId {
    /// The unassigned identifier.
    pub const UNASSIGNED: Self = Self(0);

    /// Construct from a raw value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// An embedded font resource.
#[derive(Debug, PartialEq)]
pub struct FontAsset {
    id: AssetId,
    /// Font file bytes.
    pub data: Arc<[u8]>,
    /// Face index within the font collection.
    pub face_index: u32,
    /// Font name, corresponding to OFD `FontName`.
    pub font_name: Option<String>,
    /// Font family name.
    pub family: Option<String>,
    /// PostScript name.
    pub postscript_name: Option<String>,
}

impl FontAsset {
    /// Create a font resource; the ID is allocated by the resource table.
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

    /// Set the font name.
    pub fn with_font_name(mut self, name: impl Into<String>) -> Self {
        self.font_name = Some(name.into());
        self
    }

    /// Set the font family name.
    pub fn with_family(mut self, family: impl Into<String>) -> Self {
        self.family = Some(family.into());
        self
    }

    /// Set the PostScript name.
    pub fn with_postscript_name(mut self, name: impl Into<String>) -> Self {
        self.postscript_name = Some(name.into());
        self
    }

    /// Resource identifier.
    pub fn id(&self) -> AssetId {
        self.id
    }

    /// Font file bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Face index within the font collection.
    pub fn face_index(&self) -> u32 {
        self.face_index
    }
}

/// Image encoding format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageFormat {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// BMP.
    Bmp,
    /// TIFF.
    Tiff,
    /// JBIG2.
    Jbig2,
    /// CCITT Group 3/4.
    Ccitt,
    /// Unknown format.
    #[default]
    Unknown,
}

/// An image resource.
#[derive(Debug, PartialEq)]
pub struct ImageAsset {
    id: AssetId,
    /// Encoding format.
    pub format: ImageFormat,
    /// Original encoded bytes.
    pub encoded: Arc<[u8]>,
    /// Pixel width.
    pub pixel_width: u32,
    /// Pixel height.
    pub pixel_height: u32,
    /// Optional DPI.
    pub dpi: Option<(f64, f64)>,
    /// Optional transparency mask.
    pub mask: Option<Arc<ImageAsset>>,
}

impl ImageAsset {
    /// Create an image resource; the ID is allocated by the resource table.
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

    /// Set the DPI.
    pub fn with_dpi(mut self, x: f64, y: f64) -> Self {
        self.dpi = Some((x, y));
        self
    }

    /// Set the transparency mask.
    pub fn with_mask(mut self, mask: Arc<ImageAsset>) -> Self {
        self.mask = Some(mask);
        self
    }

    /// Resource identifier.
    pub fn id(&self) -> AssetId {
        self.id
    }
}

/// The document-level font and image resource table.
///
/// Identical content is stored only once and assigned a stable ID. IDs start at 1;
/// `0` is reserved for the unassigned state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResourceStore {
    next_id: u64,
    fonts: Vec<Arc<FontAsset>>,
    images: Vec<Arc<ImageAsset>>,
}

impl ResourceStore {
    /// Create an empty resource table.
    pub fn new() -> Self {
        Self::default()
    }

    fn allocate(&mut self) -> AssetId {
        self.next_id += 1;
        AssetId(self.next_id)
    }

    /// Add a font; identical bytes and face index reuse the existing resource.
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

    /// Add a font with metadata; identical bytes, face index, and names reuse the existing resource.
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

    /// Add an image; identical encoding, dimensions, and mask reuse the existing resource.
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

    /// Look up a font by ID.
    pub fn font(&self, id: AssetId) -> Option<&Arc<FontAsset>> {
        self.fonts.iter().find(|font| font.id == id)
    }

    /// Look up an image by ID.
    pub fn image(&self, id: AssetId) -> Option<&Arc<ImageAsset>> {
        self.images.iter().find(|image| image.id == id)
    }

    /// Iterate over all fonts.
    pub fn fonts(&self) -> impl Iterator<Item = &Arc<FontAsset>> {
        self.fonts.iter()
    }

    /// Iterate over all images.
    pub fn images(&self) -> impl Iterator<Item = &Arc<ImageAsset>> {
        self.images.iter()
    }

    /// The number of fonts.
    pub fn font_count(&self) -> usize {
        self.fonts.len()
    }

    /// The number of images.
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

/// Multiply a grayscale mask into the alpha channel of RGBA8 pixels.
///
/// The mask and the image describe the same physical area, but producers often store
/// them at different resolutions (for example a 25×25 image with a 75×75 1-bit mask).
/// When the sizes differ, the mask is area-averaged and resampled onto the image pixel
/// grid, so callers do not need to handle scaling themselves.
///
/// The returned error explains why the mask is unusable; callers should degrade to
/// using no mask or report an error, rather than using half a mask.
pub fn apply_image_mask(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    mask: &[u8],
    mask_width: u32,
    mask_height: u32,
) -> Result<(), &'static str> {
    if mask_width == 0 || mask_height == 0 {
        return Err("mask dimensions are 0");
    }
    if mask.len() < mask_width as usize * mask_height as usize * 4 {
        return Err("mask pixel data is incomplete");
    }
    if rgba.len() < width as usize * height as usize * 4 {
        return Err("image pixel data is incomplete");
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

/// The gray level of a mask pixel.
fn mask_factor(mask_pixel: &[u8]) -> u32 {
    (u32::from(mask_pixel[0]) + u32::from(mask_pixel[1]) + u32::from(mask_pixel[2])) / 3
}

/// Multiply alpha by a 0–255 gray factor.
fn apply_factor(alpha: u8, factor: u32) -> u8 {
    ((u32::from(alpha) * factor) / 255) as u8
}

/// The half-open interval of source pixel grid cells covered by destination pixel `index`.
fn sample_range(index: u32, dst_len: u32, src_len: u32) -> (u32, u32) {
    let start = (u64::from(index) * u64::from(src_len) / u64::from(dst_len)) as u32;
    let start = start.min(src_len - 1);
    let end = ((u64::from(index) + 1) * u64::from(src_len) / u64::from(dst_len)) as u32;
    (start, end.max(start + 1).min(src_len))
}

/// Detect the image encoding format from the file header.
///
/// Only the magic number is inspected and no full decoding is performed; returns
/// `None` if it cannot be recognized.
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

/// Read the pixel dimensions from the encoded bytes.
///
/// Supports PNG, JPEG, BMP, and TIFF; only the file header is read and pixels are
/// not decoded.
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
        // 4×4 mask: black on the top half, white on the bottom half, covering the same area as the 2×2 image.
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
        // Dimensions must be readable from the file header alone.
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
