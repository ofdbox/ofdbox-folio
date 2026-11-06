//! Image decoding.
//!
//! Fixed IR only stores encoded bytes; the renderer decodes PNG, JPEG, BMP,
//! TIFF and JBIG2 to RGBA8 on demand and applies transparency masks.

use std::io::Cursor;

use ofdbox_folio_ir::ImageFormat;

use crate::error::RenderError;

/// A decoded RGBA8 image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Non-premultiplied RGBA8 pixels.
    pub rgba: Vec<u8>,
}

impl DecodedImage {
    /// Multiplies the grayscale mask into the alpha channel.
    ///
    /// The mask and the image describe the same physical region, but producers often
    /// store them at different resolutions; when the sizes differ,
    /// [`ofdbox_folio_ir::apply_image_mask`] resamples to the image pixel grid by
    /// area averaging.
    pub fn apply_mask(&mut self, mask: &DecodedImage) -> Result<(), RenderError> {
        ofdbox_folio_ir::apply_image_mask(
            &mut self.rgba,
            self.width,
            self.height,
            &mask.rgba,
            mask.width,
            mask.height,
        )
        .map_err(|error| RenderError::Image(format!("image mask unavailable: {error}")))
    }
}

/// Recognizes the image format from the file header.
pub fn detect_format(data: &[u8]) -> Option<ImageFormat> {
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

/// Decodes an image and converts it to RGBA8.
pub fn decode_image(
    format: ImageFormat,
    data: &[u8],
    max_pixels: u64,
) -> Result<DecodedImage, RenderError> {
    let image = match format {
        ImageFormat::Png => decode_png(data, max_pixels)?,
        ImageFormat::Jpeg => decode_jpeg(data, max_pixels)?,
        ImageFormat::Bmp => decode_bmp(data)?,
        ImageFormat::Jbig2 => decode_jbig2(data)?,
        ImageFormat::Tiff => decode_tiff(data, max_pixels)?,
        ImageFormat::Ccitt | ImageFormat::Unknown => {
            return Err(RenderError::Unsupported(format!(
                "image format: {format:?}"
            )));
        }
        _ => {
            return Err(RenderError::Unsupported(format!(
                "image format: {format:?}"
            )));
        }
    };
    let pixels = u64::from(image.width) * u64::from(image.height);
    if pixels > max_pixels {
        return Err(RenderError::Image(
            "image pixel count exceeds the limit".to_owned(),
        ));
    }
    Ok(image)
}

/// Decode an image and downscale it so its longest edge is at most `max_dim`.
///
/// JPEG sources are decoded at a reduced IDCT size when the target is smaller than the
/// full image, which keeps scanned pages cheap to display; other formats are decoded at
/// full resolution and resized afterwards.
pub fn decode_image_tier(
    format: ImageFormat,
    data: &[u8],
    max_pixels: u64,
    max_dim: u32,
) -> Result<DecodedImage, RenderError> {
    let image = if format == ImageFormat::Jpeg && max_dim > 0 {
        decode_jpeg_tier(data, max_pixels, max_dim)?
    } else {
        decode_image(format, data, max_pixels)?
    };
    Ok(resize_rgba(image, max_dim))
}

/// Decodes a JPEG at a reduced resolution when the IDCT size allows it.
fn decode_jpeg_tier(
    data: &[u8],
    max_pixels: u64,
    max_dim: u32,
) -> Result<DecodedImage, RenderError> {
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(data));
    decoder
        .read_info()
        .map_err(|error| RenderError::Image(error.to_string()))?;
    let header = decoder
        .info()
        .ok_or_else(|| RenderError::Image("missing JPEG image info".to_owned()))?;
    let header_width = u32::from(header.width);
    let header_height = u32::from(header.height);
    let pixel_count = u64::from(header_width) * u64::from(header_height);
    if pixel_count > max_pixels {
        return Err(RenderError::Image(
            "image pixel count exceeds the limit".to_owned(),
        ));
    }
    let longest = header_width.max(header_height);
    if max_dim < longest {
        let ratio = f64::from(max_dim) / f64::from(longest);
        let requested_width = (f64::from(header_width) * ratio).round().max(1.0) as u16;
        let requested_height = (f64::from(header_height) * ratio).round().max(1.0) as u16;
        // `scale` only supports IDCT sizes the codec can produce; fall back to the full
        // resolution instead of dropping the image when it refuses.
        let _ = decoder.scale(requested_width, requested_height);
    }
    let pixels = decoder
        .decode()
        .map_err(|error| RenderError::Image(error.to_string()))?;
    let info = decoder
        .info()
        .ok_or_else(|| RenderError::Image("missing JPEG image info".to_owned()))?;
    let pixel_format = info.pixel_format;
    let width = u32::from(info.width);
    let height = u32::from(info.height);
    let rgba = jpeg_to_rgba(pixels, pixel_format);
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

fn jpeg_to_rgba(pixels: Vec<u8>, pixel_format: jpeg_decoder::PixelFormat) -> Vec<u8> {
    match pixel_format {
        jpeg_decoder::PixelFormat::L8 => pixels
            .iter()
            .flat_map(|value| [*value, *value, *value, 255])
            .collect(),
        jpeg_decoder::PixelFormat::RGB24 => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        jpeg_decoder::PixelFormat::CMYK32 => pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|pixel| {
                let c = u16::from(pixel[0]);
                let m = u16::from(pixel[1]);
                let y = u16::from(pixel[2]);
                let k = u16::from(pixel[3]);
                [
                    (255 - (c + k).min(255)) as u8,
                    (255 - (m + k).min(255)) as u8,
                    (255 - (y + k).min(255)) as u8,
                    255,
                ]
            })
            .collect(),
        jpeg_decoder::PixelFormat::L16 => Vec::new(),
    }
}

fn resize_rgba(image: DecodedImage, max_dim: u32) -> DecodedImage {
    let longest = image.width.max(image.height);
    if max_dim == 0 || max_dim >= longest || image.width == 0 || image.height == 0 {
        return image;
    }
    let scale = f64::from(max_dim) / f64::from(longest);
    let width = ((f64::from(image.width) * scale).round() as u32).max(1);
    let height = ((f64::from(image.height) * scale).round() as u32).max(1);
    if scale <= 0.5 {
        return resize_rgba_box(image, width, height);
    }
    let mut rgba = vec![0_u8; width as usize * height as usize * 4];
    for y in 0..height {
        let source_y = (f64::from(y) + 0.5) / f64::from(height) * f64::from(image.height) - 0.5;
        let y0 = source_y.floor().max(0.0) as u32;
        let y1 = (y0 + 1).min(image.height - 1);
        let fy = (source_y - f64::from(y0)).clamp(0.0, 1.0);
        for x in 0..width {
            let source_x = (f64::from(x) + 0.5) / f64::from(width) * f64::from(image.width) - 0.5;
            let x0 = source_x.floor().max(0.0) as u32;
            let x1 = (x0 + 1).min(image.width - 1);
            let fx = (source_x - f64::from(x0)).clamp(0.0, 1.0);
            let sample = |sx: u32, sy: u32, channel: usize| -> f64 {
                f64::from(image.rgba[((sy * image.width + sx) * 4) as usize + channel])
            };
            let target = ((y * width + x) * 4) as usize;
            for channel in 0..4 {
                let top = sample(x0, y0, channel) * (1.0 - fx) + sample(x1, y0, channel) * fx;
                let bottom = sample(x0, y1, channel) * (1.0 - fx) + sample(x1, y1, channel) * fx;
                rgba[target + channel] = (top * (1.0 - fy) + bottom * fy).round() as u8;
            }
        }
    }
    DecodedImage {
        width,
        height,
        rgba,
    }
}

fn decode_png(data: &[u8], max_pixels: u64) -> Result<DecodedImage, RenderError> {
    let mut decoder = png::Decoder::new(Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|error| RenderError::Image(error.to_string()))?;
    let header = reader.info();
    let pixels = u64::from(header.width) * u64::from(header.height);
    if pixels > max_pixels {
        return Err(RenderError::Image(
            "image pixel count exceeds the limit".to_owned(),
        ));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| RenderError::Image("PNG output size unknown".to_owned()))?;
    let mut buffer = vec![0; size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| RenderError::Image(error.to_string()))?;
    let pixels = &buffer[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        png::ColorType::Grayscale => pixels
            .iter()
            .flat_map(|value| [*value, *value, *value, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => pixels
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        png::ColorType::Indexed => {
            return Err(RenderError::Image(
                "PNG palette was not expanded".to_owned(),
            ));
        }
    };
    Ok(DecodedImage {
        width: info.width,
        height: info.height,
        rgba,
    })
}

fn decode_jpeg(data: &[u8], max_pixels: u64) -> Result<DecodedImage, RenderError> {
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(data));
    decoder
        .read_info()
        .map_err(|error| RenderError::Image(error.to_string()))?;
    let header = decoder
        .info()
        .ok_or_else(|| RenderError::Image("missing JPEG image info".to_owned()))?;
    let pixel_count = u64::from(header.width) * u64::from(header.height);
    if pixel_count > max_pixels {
        return Err(RenderError::Image(
            "image pixel count exceeds the limit".to_owned(),
        ));
    }
    let pixels = decoder
        .decode()
        .map_err(|error| RenderError::Image(error.to_string()))?;
    let info = decoder
        .info()
        .ok_or_else(|| RenderError::Image("missing JPEG image info".to_owned()))?;
    if info.pixel_format == jpeg_decoder::PixelFormat::L16 {
        return Err(RenderError::Image(
            "16-bit grayscale JPEG is not supported".to_owned(),
        ));
    }
    let rgba = jpeg_to_rgba(pixels, info.pixel_format);
    Ok(DecodedImage {
        width: u32::from(info.width),
        height: u32::from(info.height),
        rgba,
    })
}

fn decode_bmp(data: &[u8]) -> Result<DecodedImage, RenderError> {
    let image = bmp::from_reader(&mut Cursor::new(data))
        .map_err(|error| RenderError::Image(error.to_string()))?;
    let width = image.get_width();
    let height = image.get_height();
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let pixel = image.get_pixel(x, y);
            rgba.extend_from_slice(&[pixel.r, pixel.g, pixel.b, 255]);
        }
    }
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

fn decode_tiff(data: &[u8], max_pixels: u64) -> Result<DecodedImage, RenderError> {
    let mut decoder = tiff::decoder::Decoder::new(Cursor::new(data))
        .map_err(|error| RenderError::Image(error.to_string()))?;
    let (width, height) = decoder
        .dimensions()
        .map_err(|error| RenderError::Image(error.to_string()))?;
    if u64::from(width) * u64::from(height) > max_pixels {
        return Err(RenderError::Image(
            "image pixel count exceeds the limit".to_owned(),
        ));
    }
    let color = decoder
        .colortype()
        .map_err(|error| RenderError::Image(error.to_string()))?;
    let bytes = match decoder
        .read_image()
        .map_err(|error| RenderError::Image(error.to_string()))?
    {
        tiff::decoder::DecodingResult::U8(bytes) => bytes,
        _ => {
            return Err(RenderError::Image(
                "unsupported TIFF pixel depth".to_owned(),
            ));
        }
    };
    let rgba = match color {
        tiff::ColorType::Gray(8) => bytes
            .iter()
            .flat_map(|value| [*value, *value, *value, 255])
            .collect(),
        tiff::ColorType::GrayA(8) => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        tiff::ColorType::RGB(8) => bytes
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        tiff::ColorType::RGBA(8) => bytes,
        tiff::ColorType::CMYK(8) => bytes
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|pixel| {
                let c = u16::from(pixel[0]);
                let m = u16::from(pixel[1]);
                let y = u16::from(pixel[2]);
                let k = u16::from(pixel[3]);
                [
                    (255 - (c + k).min(255)) as u8,
                    (255 - (m + k).min(255)) as u8,
                    (255 - (y + k).min(255)) as u8,
                    255,
                ]
            })
            .collect(),
        _ => {
            return Err(RenderError::Image(format!(
                "unsupported TIFF color type: {color:?}"
            )));
        }
    };
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

fn decode_jbig2(data: &[u8]) -> Result<DecodedImage, RenderError> {
    let pages = justbig2::decode(data)
        .or_else(|_| justbig2::decode_embedded(data))
        .map_err(|error| RenderError::Image(error.to_string()))?;
    let page = pages
        .into_iter()
        .next()
        .ok_or_else(|| RenderError::Image("JBIG2 has no page".to_owned()))?;
    if page.width == 0 || page.height == 0 {
        return Err(RenderError::Image("invalid JBIG2 page size".to_owned()));
    }
    let mut rgba = Vec::with_capacity(page.width as usize * page.height as usize * 4);
    for y in 0..page.height {
        for x in 0..page.width {
            let color = if page.get_pixel(x, y) == 0 { 255 } else { 0 };
            rgba.extend_from_slice(&[color, color, color, 255]);
        }
    }
    Ok(DecodedImage {
        width: page.width,
        height: page.height,
        rgba,
    })
}

/// Area-averaged downscale: reads every source pixel exactly once.
fn resize_rgba_box(image: DecodedImage, width: u32, height: u32) -> DecodedImage {
    let mut rgba = vec![0_u8; width as usize * height as usize * 4];
    let source_width = image.width as usize;
    for y in 0..height as usize {
        let y0 = y * image.height as usize / height as usize;
        let y1 = ((y + 1) * image.height as usize / height as usize).max(y0 + 1);
        for x in 0..width as usize {
            let x0 = x * source_width / width as usize;
            let x1 = ((x + 1) * source_width / width as usize).max(x0 + 1);
            let mut sums = [0_u32; 4];
            let mut count = 0_u32;
            for sy in y0..y1.min(image.height as usize) {
                for sx in x0..x1.min(source_width) {
                    let index = (sy * source_width + sx) * 4;
                    for channel in 0..4 {
                        sums[channel] += u32::from(image.rgba[index + channel]);
                    }
                    count += 1;
                }
            }
            let count = count.max(1);
            let target = (y * width as usize + x) * 4;
            for channel in 0..4 {
                rgba[target + channel] = ((sums[channel] + count / 2) / count) as u8;
            }
        }
    }
    DecodedImage {
        width,
        height,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodedImage, decode_image, detect_format, resize_rgba};
    use ofdbox_folio_ir::ImageFormat;

    #[test]
    fn resize_tier_preserves_aspect_ratio() {
        let image = DecodedImage {
            width: 4,
            height: 2,
            rgba: vec![128; 4 * 2 * 4],
        };
        let resized = resize_rgba(image, 2);
        assert_eq!((resized.width, resized.height), (2, 1));
        assert_eq!(resized.rgba.len(), 8);
    }

    #[test]
    fn applies_grayscale_mask() {
        let mut image = DecodedImage {
            width: 1,
            height: 1,
            rgba: vec![10, 20, 30, 200],
        };
        let mask = DecodedImage {
            width: 1,
            height: 1,
            rgba: vec![128, 128, 128, 255],
        };
        image.apply_mask(&mask).expect("mask");
        assert_eq!(image.rgba[3], 100);
    }

    #[test]
    fn downsamples_mask_to_image_grid() {
        let mut image = DecodedImage {
            width: 2,
            height: 2,
            rgba: [10, 20, 30, 255].repeat(4),
        };
        // A 4×4 mask: black on top, white on the bottom, describing the same region as the 2×2 image.
        let mut mask = DecodedImage {
            width: 4,
            height: 4,
            rgba: Vec::with_capacity(4 * 4 * 4),
        };
        for y in 0..4 {
            let value = if y < 2 { 0 } else { 255 };
            for _ in 0..4 {
                mask.rgba.extend_from_slice(&[value, value, value, 255]);
            }
        }
        image.apply_mask(&mask).expect("mask");
        let alphas: Vec<u8> = image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .collect();
        assert_eq!(alphas, [0, 0, 255, 255]);
    }

    #[test]
    fn upsamples_mask_to_image_grid() {
        let mut image = DecodedImage {
            width: 4,
            height: 4,
            rgba: [10, 20, 30, 255].repeat(16),
        };
        let mask = DecodedImage {
            width: 2,
            height: 2,
            rgba: vec![
                0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255,
            ],
        };
        image.apply_mask(&mask).expect("mask");
        let alphas: Vec<u8> = image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .collect();
        assert_eq!(
            alphas,
            [
                0, 0, 255, 255, 0, 0, 255, 255, 255, 255, 0, 0, 255, 255, 0, 0
            ]
        );
    }

    #[test]
    fn empty_mask_is_rejected() {
        let mut image = DecodedImage {
            width: 1,
            height: 1,
            rgba: vec![10, 20, 30, 255],
        };
        let mask = DecodedImage {
            width: 0,
            height: 0,
            rgba: Vec::new(),
        };
        assert!(image.apply_mask(&mask).is_err());
    }

    #[test]
    fn truncated_mask_is_rejected() {
        let mut image = DecodedImage {
            width: 2,
            height: 2,
            rgba: [10, 20, 30, 255].repeat(4),
        };
        let mask = DecodedImage {
            width: 2,
            height: 2,
            rgba: vec![0, 0, 0, 255],
        };
        assert!(image.apply_mask(&mask).is_err());
    }

    #[test]
    fn unsupported_format_is_explicit() {
        assert!(decode_image(ImageFormat::Unknown, &[], 10).is_err());
    }

    #[test]
    fn detects_formats_by_magic() {
        assert_eq!(
            detect_format(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            Some(ImageFormat::Png)
        );
        assert_eq!(
            detect_format(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(detect_format(b"BM\x00\x00"), Some(ImageFormat::Bmp));
        assert_eq!(
            detect_format(&[0x49, 0x49, 0x2A, 0x00]),
            Some(ImageFormat::Tiff)
        );
        assert_eq!(detect_format(b"not-an-image"), None);
    }
}
