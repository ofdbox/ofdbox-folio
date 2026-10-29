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
    let rgba = match info.pixel_format {
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
                    (255 - ((c + k).min(255))) as u8,
                    (255 - ((m + k).min(255))) as u8,
                    (255 - ((y + k).min(255))) as u8,
                    255,
                ]
            })
            .collect(),
        jpeg_decoder::PixelFormat::L16 => {
            return Err(RenderError::Image(
                "16-bit grayscale JPEG is not supported".to_owned(),
            ));
        }
    };
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

#[cfg(test)]
mod tests {
    use super::{DecodedImage, decode_image, detect_format};
    use ofdbox_folio_ir::ImageFormat;

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
