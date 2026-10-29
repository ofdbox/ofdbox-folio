//! Image mask compositing: combine a base image and a grayscale mask into a PNG with
//! alpha.
//!
//! DOCX has no general alpha-mask concept, so transparency can only be baked into the
//! image itself. Only a PNG base with a PNG mask is handled here (the mask is multiplied
//! into alpha by luminance); other combinations return `None` and the caller downgrades
//! and records a diagnostic.

use std::io::Cursor;

use ofdbox_folio_ir::ImageAsset;

/// Composite a PNG with alpha; unsupported combinations return `None`.
pub(crate) fn composite_masked_png(image: &ImageAsset) -> Option<Vec<u8>> {
    let mask = image.mask.as_deref()?;
    let (width, height, mut pixels) = decode_rgba(image)?;
    let (mask_width, mask_height, mask_pixels) = decode_rgba(mask)?;
    if mask_width == 0 || mask_height == 0 {
        return None;
    }
    // The mask is multiplied into alpha by luminance; nearest-neighbour sampling when
    // the sizes differ.
    for y in 0..height {
        let mask_y = (y as u64 * u64::from(mask_height) / u64::from(height.max(1)))
            .min(u64::from(mask_height) - 1) as u32;
        for x in 0..width {
            let mask_x = (x as u64 * u64::from(mask_width) / u64::from(width.max(1)))
                .min(u64::from(mask_width) - 1) as u32;
            let index = ((mask_y * mask_width + mask_x) * 4) as usize;
            let factor = luminance(&mask_pixels[index..index + 4]);
            let target = ((y * width + x) * 4) as usize;
            pixels[target + 3] = scale(pixels[target + 3], factor);
        }
    }
    encode_png(width, height, &pixels)
}

/// Decode into RGBA8; only PNG is accepted.
fn decode_rgba(asset: &ImageAsset) -> Option<(u32, u32, Vec<u8>)> {
    if asset.format != ofdbox_folio_ir::ImageFormat::Png {
        return None;
    }
    let mut decoder = png::Decoder::new(Cursor::new(asset.encoded.as_ref()));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size().unwrap_or(0).max(1)];
    let info = reader.next_frame(&mut buffer).ok()?;
    buffer.truncate(info.buffer_size());
    let pixels = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        png::ColorType::Grayscale => buffer
            .iter()
            .flat_map(|value| [*value, *value, *value, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buffer
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        _ => return None,
    };
    Some((info.width, info.height, pixels))
}

fn luminance(pixel: &[u8]) -> u32 {
    (u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2])) / 3
}

fn scale(alpha: u8, factor: u32) -> u8 {
    ((u32::from(alpha) * factor) / 255) as u8
}

fn encode_png(width: u32, height: u32, pixels: &[u8]) -> Option<Vec<u8>> {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(pixels).ok()?;
    }
    Some(png)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ofdbox_folio_ir::{ImageAsset, ImageFormat};

    use super::*;

    fn png(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        let mut pixels = Vec::new();
        for _ in 0..(width * height) {
            pixels.extend(pixel);
        }
        encode_png(width, height, &pixels).expect("encode")
    }

    #[test]
    fn composites_alpha_from_gray_mask() {
        let base = ImageAsset::new(
            ImageFormat::Png,
            Arc::from(png(2, 2, [200, 100, 50, 255]).as_slice()),
            2,
            2,
        );
        // A mask that is half black and half white.
        let mut mask_pixels = Vec::new();
        for _ in 0..2 {
            mask_pixels.extend([0, 0, 0, 255]);
            mask_pixels.extend([255, 255, 255, 255]);
        }
        let mask_bytes = encode_png(2, 2, &mask_pixels).expect("mask");
        let mask = Arc::new(ImageAsset::new(
            ImageFormat::Png,
            Arc::from(mask_bytes.as_slice()),
            2,
            2,
        ));
        let image = base.with_mask(mask);

        let bytes = composite_masked_png(&image).expect("composite");
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::ALPHA);
        let mut reader = decoder.read_info().expect("decode");
        let mut buffer = vec![0; reader.output_buffer_size().unwrap_or(0).max(1)];
        let info = reader.next_frame(&mut buffer).expect("frame");
        assert_eq!(info.color_type, png::ColorType::Rgba);
        // The left column is fully transparent under the black mask, the right column
        // stays opaque.
        assert_eq!(buffer[3], 0);
        assert_eq!(buffer[7], 255);
    }

    #[test]
    fn non_png_base_is_not_composited() {
        let base = ImageAsset::new(ImageFormat::Jpeg, Arc::from(&b"jpeg"[..]), 2, 2);
        let mask = Arc::new(ImageAsset::new(
            ImageFormat::Png,
            Arc::from(png(2, 2, [255, 255, 255, 255]).as_slice()),
            2,
            2,
        ));
        let image = base.with_mask(mask);
        assert!(composite_masked_png(&image).is_none());
    }
}
