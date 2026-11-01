//! Glyph rasterization and atlas packing groundwork for the wgpu backend.
//!
//! The atlas is built on the CPU with a 4×4 coverage supersample and packed into
//! a shelf allocator. The GPU text pipeline consumes the resulting R8 tiles.

#![allow(dead_code)]

use std::collections::HashMap;

use ofdbox_folio_ir::{Matrix, Path};

#[cfg(feature = "wgpu30")]
use wgpu as wgpu_lib;
#[cfg(feature = "wgpu27")]
use wgpu27 as wgpu_lib;

use super::enabled::flatten_path;

/// One rasterized glyph.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GlyphBitmap {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Left edge relative to the baseline, in pixels.
    pub left: f32,
    /// Top edge relative to the baseline, in pixels.
    pub top: f32,
    /// R8 coverage, row-major.
    pub coverage: Vec<u8>,
}

/// A glyph placed in an atlas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AtlasGlyph {
    /// Normalized left UV.
    pub u0: f32,
    /// Normalized top UV.
    pub v0: f32,
    /// Normalized right UV.
    pub u1: f32,
    /// Normalized bottom UV.
    pub v1: f32,
    /// Left edge in pixels.
    pub left: f32,
    /// Top edge in pixels.
    pub top: f32,
    /// Width in pixels.
    pub width: f32,
    /// Height in pixels.
    pub height: f32,
}

/// Shelf allocator for a single square R8 atlas.
pub(crate) struct GlyphPacker {
    size: u32,
    next_x: u32,
    next_y: u32,
    row_height: u32,
    placed: HashMap<(usize, u16, u32, bool), AtlasGlyph>,
}

impl GlyphPacker {
    /// Create an empty atlas of `size × size` texels.
    pub(crate) fn new(size: u32) -> Self {
        Self {
            size,
            next_x: 0,
            next_y: 0,
            row_height: 0,
            placed: HashMap::new(),
        }
    }

    /// Look up an already packed glyph.
    pub(crate) fn get(
        &self,
        font: usize,
        glyph: u16,
        pixel_size: u32,
        bold: bool,
    ) -> Option<AtlasGlyph> {
        self.placed.get(&(font, glyph, pixel_size, bold)).copied()
    }

    /// Pack a new glyph and return its atlas rectangle.
    pub(crate) fn insert(
        &mut self,
        font: usize,
        glyph: u16,
        pixel_size: u32,
        bold: bool,
        bitmap: &GlyphBitmap,
    ) -> Option<AtlasGlyph> {
        let width = bitmap.width.max(1);
        let height = bitmap.height.max(1);
        if width > self.size || height > self.size {
            return None;
        }
        if self.next_x + width > self.size {
            self.next_x = 0;
            self.next_y += self.row_height;
            self.row_height = 0;
        }
        if self.next_y + height > self.size {
            return None;
        }
        let x = self.next_x;
        let y = self.next_y;
        self.next_x += width;
        self.row_height = self.row_height.max(height);
        let scale = 1.0 / self.size as f32;
        let packed = AtlasGlyph {
            u0: x as f32 * scale,
            v0: y as f32 * scale,
            u1: (x + width) as f32 * scale,
            v1: (y + height) as f32 * scale,
            left: bitmap.left,
            top: bitmap.top,
            width: bitmap.width as f32,
            height: bitmap.height as f32,
        };
        self.placed.insert((font, glyph, pixel_size, bold), packed);
        Some(packed)
    }
}

/// Rasterize a glyph path (in font units) at `pixel_size` into R8 coverage.
pub(crate) fn rasterize_glyph(
    path: &Path,
    units_per_em: u16,
    pixel_size: f64,
) -> Option<GlyphBitmap> {
    if units_per_em == 0 || !(pixel_size.is_finite() && pixel_size > 0.0) {
        return None;
    }
    let scale = pixel_size / f64::from(units_per_em);
    let mut edges = Vec::new();
    flatten_path(
        path,
        Matrix::scale(scale, -scale),
        1.0,
        (0.0, 0.0),
        true,
        &mut edges,
    );
    if edges.is_empty() {
        return None;
    }
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for edge in &edges {
        min_x = min_x.min(f64::from(edge[0]).min(f64::from(edge[2])));
        min_y = min_y.min(f64::from(edge[1]).min(f64::from(edge[3])));
        max_x = max_x.max(f64::from(edge[0]).max(f64::from(edge[2])));
        max_y = max_y.max(f64::from(edge[1]).max(f64::from(edge[3])));
    }
    let left = min_x.floor();
    let top = min_y.floor();
    let width = (max_x.ceil() - left).max(1.0);
    let height = (max_y.ceil() - top).max(1.0);
    if width > 4096.0 || height > 4096.0 {
        return None;
    }
    let width = width as u32;
    let height = height as u32;
    const S: u32 = 4;
    let high_width = width * S;
    let high_height = height * S;
    let mut high = vec![0_u8; (high_width * high_height) as usize];
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    for hy in 0..high_height {
        let y = top + (f64::from(hy) + 0.5) / f64::from(S);
        crossings.clear();
        for edge in &edges {
            let ax = f64::from(edge[0]);
            let ay = f64::from(edge[1]);
            let bx = f64::from(edge[2]);
            let by = f64::from(edge[3]);
            if (ay <= y && by > y) || (by <= y && ay > y) {
                let t = (y - ay) / (by - ay);
                crossings.push((ax + t * (bx - ax), if by > ay { 1 } else { -1 }));
            }
        }
        if crossings.is_empty() {
            continue;
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut winding = 0_i32;
        let mut previous = crossings[0].0;
        let mut index = 0_usize;
        while index < crossings.len() {
            let x = crossings[index].0;
            if winding != 0 {
                fill_span(&mut high, high_width, hy, previous, x, left);
            }
            while index < crossings.len() && crossings[index].0 == x {
                winding += crossings[index].1;
                index += 1;
            }
            previous = x;
        }
    }
    let mut coverage = vec![0_u8; (width * height) as usize];
    for row in 0..height {
        for column in 0..width {
            let mut sum = 0_u32;
            for sy in 0..S {
                for sx in 0..S {
                    let hx = column * S + sx;
                    let hy = row * S + sy;
                    sum += u32::from(high[(hy * high_width + hx) as usize]);
                }
            }
            if sum > 0 {
                coverage[(row * width + column) as usize] = (sum / (S * S)) as u8;
            }
        }
    }
    Some(GlyphBitmap {
        width,
        height,
        left: left as f32,
        top: top as f32,
        coverage,
    })
}

/// Rasterize a synthetic-bold glyph by dilating an already rasterized coverage
/// bitmap. The glyph is upsampled 4x, dilated with a separable max filter and
/// area-averaged back down.
fn rasterize_glyph_bold(
    path: &Path,
    units_per_em: u16,
    pixel_size: f64,
    radius_px: f32,
) -> Option<GlyphBitmap> {
    let base = rasterize_glyph(path, units_per_em, pixel_size)?;
    if !(radius_px.is_finite() && radius_px > 0.0) {
        return Some(base);
    }
    const SCALE: u32 = 4;
    let pad = radius_px.ceil().max(1.0) as u32;
    let out_width = base.width + pad * 2;
    let out_height = base.height + pad * 2;
    let high_width = out_width * SCALE;
    let high_height = out_height * SCALE;
    let mut high = vec![0_u8; (high_width * high_height) as usize];
    for y in 0..base.height {
        for x in 0..base.width {
            let value = base.coverage[(y * base.width + x) as usize];
            for sy in 0..SCALE {
                for sx in 0..SCALE {
                    let hx = (x + pad) * SCALE + sx;
                    let hy = (y + pad) * SCALE + sy;
                    high[(hy * high_width + hx) as usize] = value;
                }
            }
        }
    }
    let radius = (radius_px * SCALE as f32).round().max(1.0) as usize;
    let dilated = dilate(&high, high_width as usize, high_height as usize, radius);
    let mut coverage = vec![0_u8; (out_width * out_height) as usize];
    for y in 0..out_height {
        for x in 0..out_width {
            let mut sum = 0_u32;
            for sy in 0..SCALE {
                for sx in 0..SCALE {
                    let hx = x * SCALE + sx;
                    let hy = y * SCALE + sy;
                    sum += u32::from(dilated[(hy * high_width + hx) as usize]);
                }
            }
            coverage[(y * out_width + x) as usize] = (sum / (SCALE * SCALE)) as u8;
        }
    }
    Some(GlyphBitmap {
        width: out_width,
        height: out_height,
        left: base.left - pad as f32,
        top: base.top - pad as f32,
        coverage,
    })
}

fn dilate(source: &[u8], width: usize, height: usize, radius: usize) -> Vec<u8> {
    let mut horizontal = vec![0_u8; source.len()];
    for y in 0..height {
        for x in 0..width {
            let mut value = 0_u8;
            let start = x.saturating_sub(radius);
            let end = (x + radius + 1).min(width);
            for sample in start..end {
                value = value.max(source[y * width + sample]);
            }
            horizontal[y * width + x] = value;
        }
    }
    let mut vertical = vec![0_u8; source.len()];
    for y in 0..height {
        for x in 0..width {
            let mut value = 0_u8;
            let start = y.saturating_sub(radius);
            let end = (y + radius + 1).min(height);
            for sample in start..end {
                value = value.max(horizontal[sample * width + x]);
            }
            vertical[y * width + x] = value;
        }
    }
    vertical
}

fn fill_span(high: &mut [u8], high_width: u32, row: u32, start: f64, end: f64, left: f64) {
    const S: f64 = 4.0;
    let start = ((start - left) * S - 0.5).ceil().max(0.0) as u32;
    let end = (((end - left) * S - 0.5).floor()).min(f64::from(high_width - 1));
    if end < 0.0 {
        return;
    }
    let end = end as u32;
    if start > end {
        return;
    }
    let base = (row * high_width) as usize;
    for column in start..=end {
        high[base + column as usize] = 255;
    }
}

fn inside_edges(edges: &[[f32; 4]], x: f64, y: f64) -> bool {
    let mut winding = 0_i32;
    for edge in edges {
        let ax = f64::from(edge[0]);
        let ay = f64::from(edge[1]);
        let bx = f64::from(edge[2]);
        let by = f64::from(edge[3]);
        if (ay <= y && by > y) || (by <= y && ay > y) {
            let t = (y - ay) / (by - ay);
            let crossing = ax + t * (bx - ax);
            if crossing < x {
                winding += if by > ay { 1 } else { -1 };
            }
        }
    }
    winding != 0
}

/// GPU-side R8 glyph atlas.
pub(crate) struct GpuAtlas {
    texture: wgpu_lib::Texture,
    view: wgpu_lib::TextureView,
    sampler: wgpu_lib::Sampler,
    packer: GlyphPacker,
}

impl GpuAtlas {
    /// Create an atlas texture and sampler.
    pub(crate) fn new(device: &wgpu_lib::Device, size: u32) -> Self {
        let texture = device.create_texture(&wgpu_lib::TextureDescriptor {
            label: Some("ofdbox-glyph-atlas"),
            size: wgpu_lib::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu_lib::TextureDimension::D2,
            format: wgpu_lib::TextureFormat::R8Unorm,
            usage: wgpu_lib::TextureUsages::TEXTURE_BINDING | wgpu_lib::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu_lib::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu_lib::SamplerDescriptor {
            label: Some("ofdbox-glyph-sampler"),
            mag_filter: wgpu_lib::FilterMode::Linear,
            min_filter: wgpu_lib::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            texture,
            view,
            sampler,
            packer: GlyphPacker::new(size),
        }
    }

    /// The atlas texture view.
    pub(crate) fn view(&self) -> &wgpu_lib::TextureView {
        &self.view
    }

    /// The atlas sampler.
    pub(crate) fn sampler(&self) -> &wgpu_lib::Sampler {
        &self.sampler
    }

    /// Drop packed glyphs so the atlas can be reused after a scene change.
    ///
    /// The texture is not zeroed: old texels are simply no longer referenced and
    /// are overwritten as new glyphs are packed.
    pub(crate) fn clear(&mut self) {
        self.packer = GlyphPacker::new(self.packer.size);
    }

    /// Rasterize, pack and upload a glyph if it is not already cached.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn glyph(
        &mut self,
        queue: &wgpu_lib::Queue,
        font: usize,
        glyph: u16,
        pixel_size: u32,
        bold: bool,
        radius_px: f32,
        path: &Path,
        units_per_em: u16,
    ) -> Option<AtlasGlyph> {
        if let Some(packed) = self.packer.get(font, glyph, pixel_size, bold) {
            return Some(packed);
        }
        let bitmap = if bold {
            rasterize_glyph_bold(path, units_per_em, f64::from(pixel_size), radius_px)?
        } else {
            rasterize_glyph(path, units_per_em, f64::from(pixel_size))?
        };
        let packed = self.packer.insert(font, glyph, pixel_size, bold, &bitmap)?;
        let width = bitmap.width.max(1);
        let height = bitmap.height.max(1);
        let padded = wgpu_lib::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let mut data = vec![0_u8; padded * height as usize];
        for row in 0..height as usize {
            let source = &bitmap.coverage[row * width as usize..(row + 1) * width as usize];
            data[row * padded..row * padded + width as usize].copy_from_slice(source);
        }
        queue.write_texture(
            wgpu_lib::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu_lib::Origin3d {
                    x: (packed.u0 * self.packer.size as f32).round() as u32,
                    y: (packed.v0 * self.packer.size as f32).round() as u32,
                    z: 0,
                },
                aspect: wgpu_lib::TextureAspect::All,
            },
            &data,
            wgpu_lib::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded as u32),
                rows_per_image: Some(height),
            },
            wgpu_lib::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        Some(packed)
    }
}

/// One instanced text quad.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct GlyphInstance {
    /// Page-pixel position of the quad's top-left corner.
    pub origin: [f32; 2],
    /// Page-pixel x axis per atlas texel.
    pub x_axis: [f32; 2],
    /// Page-pixel y axis per atlas texel.
    pub y_axis: [f32; 2],
    /// Atlas UV of the top-left corner.
    pub uv0: [f32; 2],
    /// Atlas UV of the bottom-right corner.
    pub uv1: [f32; 2],
    /// Padding to the vec4-aligned color.
    pub _pad: [f32; 2],
    /// Straight-alpha text color.
    pub color: [f32; 4],
    /// Gradient start/end in page pixels.
    pub gradient: [f32; 4],
    /// Gradient end color.
    pub end_color: [f32; 4],
    /// 0 = solid, 1 = linear, 2 = radial.
    pub mode: f32,
    pub _pad_a: f32,
    pub _pad_b: f32,
    pub _pad_c: f32,
    /// Clip edge range and fill rule; clip_count == 0 means unclipped.
    pub clip_start: u32,
    pub clip_count: u32,
    pub clip_rule: u32,
    pub _clip_pad: u32,
    /// 2x2 gradient-space transform for elliptical radial gradients.
    pub ellipse: [f32; 4],
    /// End center/radius for two-point radial gradients.
    pub radial_end: [f32; 4],
}

impl GlyphInstance {
    /// Serialize for the GPU instance buffer (160 bytes).
    pub(crate) fn write_bytes(self, out: &mut Vec<u8>) {
        for value in self
            .origin
            .into_iter()
            .chain(self.x_axis)
            .chain(self.y_axis)
            .chain(self.uv0)
            .chain(self.uv1)
            .chain(self._pad)
            .chain(self.color)
            .chain(self.gradient)
            .chain(self.end_color)
            .chain([self.mode, self._pad_a, self._pad_b, self._pad_c])
        {
            out.extend_from_slice(&value.to_ne_bytes());
        }
        for value in [
            self.clip_start,
            self.clip_count,
            self.clip_rule,
            self._clip_pad,
        ] {
            out.extend_from_slice(&value.to_ne_bytes());
        }
        for value in self.ellipse {
            out.extend_from_slice(&value.to_ne_bytes());
        }
        for value in self.radial_end {
            out.extend_from_slice(&value.to_ne_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ofdbox_folio_ir::{Path, Point};

    #[test]
    fn rasterizes_a_square_glyph() {
        let mut path = Path::new();
        path.move_to(Point::new(100.0, 0.0));
        path.line_to(Point::new(900.0, 0.0));
        path.line_to(Point::new(900.0, 800.0));
        path.line_to(Point::new(100.0, 800.0));
        path.close();
        let bitmap = rasterize_glyph(&path, 1000, 10.0).expect("bitmap");
        assert_eq!(bitmap.width, 8);
        assert_eq!(bitmap.height, 8);
        assert!(bitmap.coverage.contains(&255));
    }

    #[test]
    fn packs_glyphs_without_overlap() {
        let bitmap = GlyphBitmap {
            width: 4,
            height: 4,
            left: 0.0,
            top: 0.0,
            coverage: vec![255; 16],
        };
        let mut packer = GlyphPacker::new(16);
        let first = packer.insert(1, 2, 3, false, &bitmap).expect("first");
        let second = packer.insert(4, 5, 6, false, &bitmap).expect("second");
        assert_ne!((first.u0, first.v0), (second.u0, second.v0));
        assert_eq!(packer.get(1, 2, 3, false), Some(first));
    }
}
