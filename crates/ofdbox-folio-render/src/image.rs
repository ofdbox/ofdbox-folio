//! 图片解码。
//!
//! Fixed IR 只保存编码字节；渲染器按需把 PNG、JPEG、BMP、TIFF、JBIG2
//! 解码为 RGBA8，并应用透明度蒙版。

use std::io::Cursor;

use ofdbox_folio_ir::ImageFormat;

use crate::error::RenderError;

/// 解码后的 RGBA8 图像。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedImage {
    /// 宽度，像素。
    pub width: u32,
    /// 高度，像素。
    pub height: u32,
    /// 非预乘 RGBA8 像素。
    pub rgba: Vec<u8>,
}

impl DecodedImage {
    /// 将灰度蒙版乘入 alpha 通道。
    ///
    /// 蒙版与图片描述同一块物理区域，但生产方常按不同分辨率存放；尺寸不等时
    /// 由 [`ofdbox_folio_ir::apply_image_mask`] 按面积平均重采样到图片像素网格。
    pub fn apply_mask(&mut self, mask: &DecodedImage) -> Result<(), RenderError> {
        ofdbox_folio_ir::apply_image_mask(
            &mut self.rgba,
            self.width,
            self.height,
            &mask.rgba,
            mask.width,
            mask.height,
        )
        .map_err(|error| RenderError::Image(format!("图像蒙版不可用: {error}")))
    }
}

/// 按文件头识别图片格式。
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

/// 解码图片并转换为 RGBA8。
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
            return Err(RenderError::Unsupported(format!("图片格式: {format:?}")));
        }
        _ => {
            return Err(RenderError::Unsupported(format!("图片格式: {format:?}")));
        }
    };
    let pixels = u64::from(image.width) * u64::from(image.height);
    if pixels > max_pixels {
        return Err(RenderError::Image("图片像素数超出限制".to_owned()));
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
        return Err(RenderError::Image("图片像素数超出限制".to_owned()));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| RenderError::Image("PNG 输出尺寸未知".to_owned()))?;
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
            return Err(RenderError::Image("PNG 调色板未展开".to_owned()));
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
        .ok_or_else(|| RenderError::Image("缺少 JPEG 图像信息".to_owned()))?;
    let pixel_count = u64::from(header.width) * u64::from(header.height);
    if pixel_count > max_pixels {
        return Err(RenderError::Image("图片像素数超出限制".to_owned()));
    }
    let pixels = decoder
        .decode()
        .map_err(|error| RenderError::Image(error.to_string()))?;
    let info = decoder
        .info()
        .ok_or_else(|| RenderError::Image("缺少 JPEG 图像信息".to_owned()))?;
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
            return Err(RenderError::Image("不支持 16 位灰度 JPEG".to_owned()));
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
        return Err(RenderError::Image("图片像素数超出限制".to_owned()));
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
            return Err(RenderError::Image("不支持的 TIFF 像素深度".to_owned()));
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
                "不支持的 TIFF 颜色类型: {color:?}"
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
        .ok_or_else(|| RenderError::Image("JBIG2 没有页面".to_owned()))?;
    if page.width == 0 || page.height == 0 {
        return Err(RenderError::Image("JBIG2 页面尺寸无效".to_owned()));
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
        // 4×4 蒙版：上半黑、下半白，与 2×2 图片描述同一块区域。
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
