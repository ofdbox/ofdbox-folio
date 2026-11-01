//! Glyph rasterization and atlas packing groundwork for the wgpu backend.
//!
//! The atlas is built on the CPU with a 4×4 coverage supersample and packed into
//! a shelf allocator. The GPU text pipeline consumes the resulting R8 tiles.

#![allow(dead_code)]

use std::collections::HashMap;

use ofdbox_folio_ir::{Matrix, Path};

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
    placed: HashMap<(usize, u16, u32), AtlasGlyph>,
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
    pub(crate) fn get(&self, font: usize, glyph: u16, pixel_size: u32) -> Option<AtlasGlyph> {
        self.placed.get(&(font, glyph, pixel_size)).copied()
    }

    /// Pack a new glyph and return its atlas rectangle.
    pub(crate) fn insert(
        &mut self,
        font: usize,
        glyph: u16,
        pixel_size: u32,
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
        self.placed.insert((font, glyph, pixel_size), packed);
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
    let mut coverage = vec![0_u8; (width * height) as usize];
    const OFFSETS: [(f64, f64); 16] = [
        (0.125, 0.125),
        (0.375, 0.125),
        (0.625, 0.125),
        (0.875, 0.125),
        (0.125, 0.375),
        (0.375, 0.375),
        (0.625, 0.375),
        (0.875, 0.375),
        (0.125, 0.625),
        (0.375, 0.625),
        (0.625, 0.625),
        (0.875, 0.625),
        (0.125, 0.875),
        (0.375, 0.875),
        (0.625, 0.875),
        (0.875, 0.875),
    ];
    for row in 0..height {
        for column in 0..width {
            let mut covered = 0_u32;
            for (dx, dy) in OFFSETS {
                let x = left + f64::from(column) + dx;
                let y = top + f64::from(row) + dy;
                if inside_edges(&edges, x, y) {
                    covered += 1;
                }
            }
            if covered > 0 {
                coverage[(row * width + column) as usize] = ((covered * 255 + 8) / 16) as u8;
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
        let first = packer.insert(1, 2, 3, &bitmap).expect("first");
        let second = packer.insert(4, 5, 6, &bitmap).expect("second");
        assert_ne!((first.u0, first.v0), (second.u0, second.v0));
        assert_eq!(packer.get(1, 2, 3), Some(first));
    }
}
