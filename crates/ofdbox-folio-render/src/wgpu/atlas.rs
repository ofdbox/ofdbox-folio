//! Glyph rasterization and atlas packing groundwork for the wgpu backend.
//!
//! The atlas is built on the CPU with a 4×4 coverage supersample and packed into
//! a shelf allocator with free-rect reuse and per-glyph LRU eviction. The GPU
//! text pipeline consumes the resulting R8 tiles.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

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
    /// Atlas array layer.
    pub layer: u32,
}

/// Key identifying a rasterized glyph: font, glyph id, pixel size, bold.
type GlyphKey = (usize, u16, u32, bool);

/// Integer texel rectangle inside one atlas layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AtlasRect {
    /// Left texel.
    pub x: u32,
    /// Top texel.
    pub y: u32,
    /// Width in texels.
    pub width: u32,
    /// Height in texels.
    pub height: u32,
}

impl AtlasRect {
    /// Number of texels covered by the rectangle.
    fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Whether the two rectangles share at least one texel.
    fn overlaps(self, other: Self) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

/// Merge two free rectangles that share a full edge, if possible.
fn merge_free_rects(a: AtlasRect, b: AtlasRect) -> Option<AtlasRect> {
    if a.y == b.y && a.height == b.height {
        if a.x + a.width == b.x {
            return Some(AtlasRect {
                width: a.width + b.width,
                ..a
            });
        }
        if b.x + b.width == a.x {
            return Some(AtlasRect {
                width: a.width + b.width,
                ..b
            });
        }
    }
    if a.x == b.x && a.width == b.width {
        if a.y + a.height == b.y {
            return Some(AtlasRect {
                height: a.height + b.height,
                ..a
            });
        }
        if b.y + b.height == a.y {
            return Some(AtlasRect {
                height: a.height + b.height,
                ..b
            });
        }
    }
    None
}

/// A packed glyph together with the texel rectangle it owns.
#[derive(Clone, Copy)]
struct PlacedGlyph {
    atlas: AtlasGlyph,
    rect: AtlasRect,
}

/// Shelf allocator for a single square R8 atlas layer with free-rect reuse.
///
/// New glyphs are packed at the shelf frontier or, when an evicted glyph left a
/// suitable hole, into the best-fitting free rectangle. Live glyphs never move,
/// so UVs already emitted for a frame stay valid.
pub(crate) struct GlyphPacker {
    size: u32,
    layer: u32,
    next_x: u32,
    next_y: u32,
    row_height: u32,
    free_rects: Vec<AtlasRect>,
    placed: HashMap<GlyphKey, PlacedGlyph>,
    used_texels: u64,
}

impl GlyphPacker {
    /// Create an empty atlas layer of `size × size` texels.
    pub(crate) fn new(size: u32, layer: u32) -> Self {
        Self {
            size,
            layer,
            next_x: 0,
            next_y: 0,
            row_height: 0,
            free_rects: Vec::new(),
            placed: HashMap::new(),
            used_texels: 0,
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
        self.placed
            .get(&(font, glyph, pixel_size, bold))
            .map(|placed| placed.atlas)
    }

    /// The texel rectangle owned by an already packed glyph.
    fn rect(&self, key: GlyphKey) -> Option<AtlasRect> {
        self.placed.get(&key).map(|placed| placed.rect)
    }

    /// Texels currently owned by live glyphs.
    fn used_texels(&self) -> u64 {
        self.used_texels
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
        let key = (font, glyph, pixel_size, bold);
        if let Some(placed) = self.placed.get(&key) {
            return Some(placed.atlas);
        }
        let width = bitmap.width.max(1);
        let height = bitmap.height.max(1);
        let rect = self.allocate(width, height)?;
        let scale = 1.0 / self.size as f32;
        let packed = AtlasGlyph {
            u0: rect.x as f32 * scale,
            v0: rect.y as f32 * scale,
            u1: (rect.x + rect.width) as f32 * scale,
            v1: (rect.y + rect.height) as f32 * scale,
            left: bitmap.left,
            top: bitmap.top,
            width: bitmap.width as f32,
            height: bitmap.height as f32,
            layer: self.layer,
        };
        self.placed.insert(
            key,
            PlacedGlyph {
                atlas: packed,
                rect,
            },
        );
        self.used_texels += rect.area();
        Some(packed)
    }

    /// Drop a packed glyph and return its rectangle to the free list.
    fn remove(&mut self, key: GlyphKey) -> Option<AtlasGlyph> {
        let placed = self.placed.remove(&key)?;
        self.used_texels = self.used_texels.saturating_sub(placed.rect.area());
        self.release(placed.rect);
        Some(placed.atlas)
    }

    /// Allocate a `width × height` rectangle, preferring the best-fitting hole.
    fn allocate(&mut self, width: u32, height: u32) -> Option<AtlasRect> {
        if width > self.size || height > self.size {
            return None;
        }
        let mut best: Option<(usize, u64)> = None;
        for (index, free) in self.free_rects.iter().enumerate() {
            if free.width >= width && free.height >= height {
                let waste = free.area() - u64::from(width) * u64::from(height);
                if best.is_none_or(|(_, best_waste)| waste < best_waste) {
                    best = Some((index, waste));
                }
            }
        }
        if let Some((index, _)) = best {
            let free = self.free_rects.swap_remove(index);
            let leftover_width = free.width - width;
            let leftover_height = free.height - height;
            if leftover_width > 0 {
                self.free_rects.push(AtlasRect {
                    x: free.x + width,
                    y: free.y,
                    width: leftover_width,
                    height: free.height,
                });
            }
            if leftover_height > 0 {
                self.free_rects.push(AtlasRect {
                    x: free.x,
                    y: free.y + height,
                    width,
                    height: leftover_height,
                });
            }
            return Some(AtlasRect {
                x: free.x,
                y: free.y,
                width,
                height,
            });
        }
        if self.next_x + width > self.size {
            if self.row_height > 0 && self.next_x < self.size {
                // Release the unused tail of the finished row so later glyphs can reuse it.
                self.release(AtlasRect {
                    x: self.next_x,
                    y: self.next_y,
                    width: self.size - self.next_x,
                    height: self.row_height,
                });
            }
            self.next_x = 0;
            self.next_y += self.row_height;
            self.row_height = 0;
        }
        if self.next_y + height > self.size {
            return None;
        }
        let rect = AtlasRect {
            x: self.next_x,
            y: self.next_y,
            width,
            height,
        };
        self.next_x += width;
        self.row_height = self.row_height.max(height);
        Some(rect)
    }

    /// Return a rectangle to the free list, merging it with adjacent holes.
    fn release(&mut self, rect: AtlasRect) {
        let mut merged = rect;
        loop {
            let partner = self
                .free_rects
                .iter()
                .position(|free| merge_free_rects(merged, *free).is_some());
            let Some(index) = partner else {
                break;
            };
            let free = self.free_rects.swap_remove(index);
            merged = merge_free_rects(merged, free).expect("mergeable free rectangle");
        }
        self.free_rects.push(merged);
    }
}

/// Device-independent glyph atlas bookkeeping: packing, hole reuse and LRU eviction.
pub(crate) struct GlyphAtlas {
    size: u32,
    trim_threshold: f32,
    packers: Vec<GlyphPacker>,
    /// LRU sequence number and owning layer for every packed glyph.
    usage: HashMap<GlyphKey, (u64, u32)>,
    /// Glyphs touched during the active frame; eviction never reclaims them.
    frame_used: HashSet<GlyphKey>,
    clock: u64,
    frame_active: bool,
}

impl GlyphAtlas {
    /// Create an empty atlas layout with `layers` layers.
    pub(crate) fn new(size: u32, layers: u32, trim_threshold: f32) -> Self {
        let layers = layers.clamp(1, 16);
        Self {
            size,
            trim_threshold,
            packers: (0..layers)
                .map(|layer| GlyphPacker::new(size, layer))
                .collect(),
            usage: HashMap::new(),
            frame_used: HashSet::new(),
            clock: 0,
            frame_active: false,
        }
    }

    /// Look up an already packed glyph and mark it as recently used.
    pub(crate) fn get(
        &mut self,
        font: usize,
        glyph: u16,
        pixel_size: u32,
        bold: bool,
    ) -> Option<AtlasGlyph> {
        let key = (font, glyph, pixel_size, bold);
        let layer = self.usage.get(&key)?.1;
        let atlas = self.packers[layer as usize].get(font, glyph, pixel_size, bold)?;
        self.touch(key, layer);
        Some(atlas)
    }

    /// Pack a new glyph, evicting least-recently-used glyphs until it fits.
    ///
    /// Returns `None` only when the glyph cannot fit even after every evictable
    /// glyph has been reclaimed (for example when it is larger than one layer).
    pub(crate) fn pack(
        &mut self,
        key: GlyphKey,
        bitmap: &GlyphBitmap,
    ) -> Option<(AtlasGlyph, AtlasRect)> {
        let width = bitmap.width.max(1);
        let height = bitmap.height.max(1);
        if width > self.size || height > self.size {
            return None;
        }
        loop {
            for layer in 0..self.packers.len() {
                let inserted = self.packers[layer].insert(key.0, key.1, key.2, key.3, bitmap);
                if let Some(atlas) = inserted {
                    let rect = self.packers[layer]
                        .rect(key)
                        .expect("a packed glyph owns a rectangle");
                    self.touch(key, layer as u32);
                    return Some((atlas, rect));
                }
            }
            if !self.evict_one_lru() {
                return None;
            }
        }
    }

    /// Mark a glyph as used by the current frame and refresh its LRU position.
    fn touch(&mut self, key: GlyphKey, layer: u32) {
        self.clock = self.clock.wrapping_add(1);
        self.usage.insert(key, (self.clock, layer));
        if self.frame_active {
            self.frame_used.insert(key);
        }
    }

    /// Evict the least-recently-used glyph that the current frame has not used.
    fn evict_one_lru(&mut self) -> bool {
        let victim = self
            .usage
            .iter()
            .filter(|(key, _)| !self.frame_used.contains(*key))
            .min_by_key(|(_, (sequence, _))| *sequence)
            .map(|(key, (_, layer))| (*key, *layer));
        let Some((key, layer)) = victim else {
            return false;
        };
        self.packers[layer as usize].remove(key);
        self.usage.remove(&key);
        true
    }

    /// Start a frame, trimming stale glyphs down to the occupancy threshold.
    pub(crate) fn begin_frame(&mut self) {
        self.frame_active = true;
        self.frame_used.clear();
        let limit = self.trim_threshold.clamp(0.5, 1.0);
        while self.occupancy() > limit && self.evict_one_lru() {}
    }

    /// Finish a frame and drop its eviction protection.
    pub(crate) fn end_frame(&mut self) {
        self.frame_active = false;
        self.frame_used.clear();
    }

    /// Drop every packed glyph.
    pub(crate) fn clear(&mut self) {
        let layers = self.packers.len() as u32;
        self.packers = (0..layers)
            .map(|layer| GlyphPacker::new(self.size, layer))
            .collect();
        self.usage.clear();
        self.frame_used.clear();
        self.clock = 0;
        self.frame_active = false;
    }

    /// Fraction of the atlas area currently owned by live glyphs.
    pub(crate) fn occupancy(&self) -> f32 {
        let total = u64::from(self.size) * u64::from(self.size) * self.packers.len() as u64;
        if total == 0 {
            return 0.0;
        }
        let used: u64 = self.packers.iter().map(GlyphPacker::used_texels).sum();
        (used as f64 / total as f64) as f32
    }
}

/// Bytes per row for an atlas upload, aligned to wgpu's copy requirement.
fn padded_row_bytes(width: u32) -> usize {
    let alignment = wgpu_lib::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    (width as usize).div_ceil(alignment) * alignment
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
    layout: GlyphAtlas,
    pending: Vec<PendingGlyph>,
}

struct PendingGlyph {
    layer: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl GpuAtlas {
    /// Create an atlas texture and sampler.
    pub(crate) fn new(
        device: &wgpu_lib::Device,
        size: u32,
        layers: u32,
        trim_threshold: f32,
    ) -> Self {
        let layers = layers.clamp(1, 16);
        let texture = device.create_texture(&wgpu_lib::TextureDescriptor {
            label: Some("ofdbox-glyph-atlas"),
            size: wgpu_lib::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu_lib::TextureDimension::D2,
            format: wgpu_lib::TextureFormat::R8Unorm,
            usage: wgpu_lib::TextureUsages::TEXTURE_BINDING | wgpu_lib::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu_lib::TextureViewDescriptor {
            dimension: Some(wgpu_lib::TextureViewDimension::D2Array),
            ..Default::default()
        });
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
            layout: GlyphAtlas::new(size, layers, trim_threshold),
            pending: Vec::new(),
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

    /// Drop packed glyphs so the atlas can be reused after a cache reset.
    ///
    /// The texture is not zeroed: old texels are simply no longer referenced and
    /// are overwritten as new glyphs are packed.
    pub(crate) fn clear(&mut self) {
        self.layout.clear();
        self.pending.clear();
    }

    /// Start a frame: glyphs used from now on are protected from eviction.
    pub(crate) fn begin_frame(&mut self) {
        self.layout.begin_frame();
    }

    /// End a frame: its glyphs may be evicted again by later frames.
    pub(crate) fn end_frame(&mut self) {
        self.layout.end_frame();
    }

    /// Fraction of the atlas area currently owned by live glyphs.
    pub(crate) fn occupancy(&self) -> f32 {
        self.layout.occupancy()
    }

    /// Rasterize, pack and upload a glyph if it is not already cached.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn glyph(
        &mut self,
        font: usize,
        glyph: u16,
        pixel_size: u32,
        bold: bool,
        radius_px: f32,
        path: &Path,
        units_per_em: u16,
    ) -> Option<AtlasGlyph> {
        if let Some(packed) = self.layout.get(font, glyph, pixel_size, bold) {
            return Some(packed);
        }
        let bitmap = if bold {
            rasterize_glyph_bold(path, units_per_em, f64::from(pixel_size), radius_px)?
        } else {
            rasterize_glyph(path, units_per_em, f64::from(pixel_size))?
        };
        let (packed, rect) = self.layout.pack((font, glyph, pixel_size, bold), &bitmap)?;
        let width = bitmap.width.max(1);
        let height = bitmap.height.max(1);
        let padded = padded_row_bytes(width);
        let mut data = vec![0_u8; padded * height as usize];
        for row in 0..height as usize {
            let source = &bitmap.coverage[row * width as usize..(row + 1) * width as usize];
            data[row * padded..row * padded + width as usize].copy_from_slice(source);
        }
        self.pending.push(PendingGlyph {
            layer: packed.layer,
            x: rect.x,
            y: rect.y,
            width,
            height,
            data,
        });
        Some(packed)
    }

    /// Uploads deferred glyph bitmaps with one staging buffer.
    pub(crate) fn flush(
        &mut self,
        device: &wgpu_lib::Device,
        queue: &wgpu_lib::Queue,
        encoder: &mut wgpu_lib::CommandEncoder,
    ) {
        if self.pending.is_empty() {
            return;
        }
        let mut bytes = Vec::new();
        let mut offsets = Vec::with_capacity(self.pending.len());
        for glyph in &self.pending {
            offsets.push(bytes.len());
            bytes.extend_from_slice(&glyph.data);
        }
        if bytes.is_empty() {
            self.pending.clear();
            return;
        }
        let buffer = device.create_buffer(&wgpu_lib::BufferDescriptor {
            label: Some("ofdbox-glyph-upload"),
            size: bytes.len() as u64,
            usage: wgpu_lib::BufferUsages::COPY_SRC | wgpu_lib::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, &bytes);
        for (glyph, offset) in self.pending.iter().zip(offsets) {
            encoder.copy_buffer_to_texture(
                wgpu_lib::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu_lib::TexelCopyBufferLayout {
                        offset: offset as u64,
                        bytes_per_row: Some(
                            (glyph.data.len() / glyph.height.max(1) as usize) as u32,
                        ),
                        rows_per_image: Some(glyph.height),
                    },
                },
                wgpu_lib::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu_lib::Origin3d {
                        x: glyph.x,
                        y: glyph.y,
                        z: glyph.layer,
                    },
                    aspect: wgpu_lib::TextureAspect::All,
                },
                wgpu_lib::Extent3d {
                    width: glyph.width,
                    height: glyph.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        self.pending.clear();
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
    /// Atlas array layer.
    pub atlas_layer: u32,
    pub _atlas_pad: [u32; 3],
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
        for value in [
            self.atlas_layer,
            self._atlas_pad[0],
            self._atlas_pad[1],
            self._atlas_pad[2],
        ] {
            out.extend_from_slice(&value.to_ne_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ofdbox_folio_ir::{Path, Point};

    /// A solid coverage bitmap of the given size.
    fn test_bitmap(width: u32, height: u32) -> GlyphBitmap {
        GlyphBitmap {
            width,
            height,
            left: 0.0,
            top: 0.0,
            coverage: vec![255; (width * height) as usize],
        }
    }

    /// Check that free holes stay in bounds, disjoint from each other and from
    /// every live glyph.
    fn assert_packer_consistent(packer: &GlyphPacker) {
        for (index, free) in packer.free_rects.iter().enumerate() {
            assert!(free.width > 0 && free.height > 0, "empty free rectangle");
            assert!(
                free.x + free.width <= packer.size && free.y + free.height <= packer.size,
                "free rectangle outside the layer: {free:?}"
            );
            for other in &packer.free_rects[index + 1..] {
                assert!(!free.overlaps(*other), "free rectangles overlap: {free:?}");
            }
            for placed in packer.placed.values() {
                assert!(
                    !free.overlaps(placed.rect),
                    "free rectangle overlaps a live glyph: {free:?}"
                );
            }
        }
    }

    /// Check that every live glyph of every layer stays in bounds and that no
    /// two live glyphs overlap.
    fn assert_atlas_consistent(atlas: &GlyphAtlas) {
        let mut rects = Vec::new();
        for packer in &atlas.packers {
            assert_packer_consistent(packer);
            for placed in packer.placed.values() {
                assert!(
                    placed.rect.x + placed.rect.width <= atlas.size
                        && placed.rect.y + placed.rect.height <= atlas.size,
                    "glyph outside the layer: {:?}",
                    placed.rect
                );
                rects.push(placed.rect);
            }
        }
        for (index, rect) in rects.iter().enumerate() {
            for other in &rects[index + 1..] {
                assert!(!rect.overlaps(*other), "live glyphs overlap: {rect:?}");
            }
        }
    }

    #[test]
    fn pads_rows_wider_than_the_copy_alignment() {
        assert_eq!(padded_row_bytes(1), 256);
        assert_eq!(padded_row_bytes(256), 256);
        assert_eq!(padded_row_bytes(257), 512);
        assert_eq!(padded_row_bytes(75525), 75776);
    }

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
        let bitmap = test_bitmap(4, 4);
        let mut packer = GlyphPacker::new(16, 0);
        let first = packer.insert(1, 2, 3, false, &bitmap).expect("first");
        let second = packer.insert(4, 5, 6, false, &bitmap).expect("second");
        assert_ne!((first.u0, first.v0), (second.u0, second.v0));
        assert_eq!(packer.get(1, 2, 3, false), Some(first));
        assert_packer_consistent(&packer);
    }

    #[test]
    fn free_rects_are_reused_and_split_without_overlap() {
        let mut packer = GlyphPacker::new(16, 0);
        let small = test_bitmap(8, 8);
        packer.insert(0, 1, 1, false, &small).expect("first");
        packer.insert(0, 2, 1, false, &small).expect("second");
        assert!(packer.remove((0, 1, 1, false)).is_some());

        let reused = packer.insert(0, 3, 1, false, &test_bitmap(6, 6));
        let reused = reused.expect("a freed hole must be reused");
        assert_eq!(reused.u0, 0.0);
        assert_eq!(reused.v0, 0.0);
        assert_eq!(
            packer.rect((0, 3, 1, false)),
            Some(AtlasRect {
                x: 0,
                y: 0,
                width: 6,
                height: 6,
            })
        );
        // The 8x8 hole is split into a right and a bottom remainder.
        let mut free = packer.free_rects.clone();
        free.sort_by_key(|rect| (rect.y, rect.x));
        assert_eq!(
            free,
            vec![
                AtlasRect {
                    x: 6,
                    y: 0,
                    width: 2,
                    height: 8,
                },
                AtlasRect {
                    x: 0,
                    y: 6,
                    width: 6,
                    height: 2,
                },
            ]
        );
        assert_packer_consistent(&packer);
    }

    #[test]
    fn adjacent_free_rects_are_merged() {
        let mut packer = GlyphPacker::new(32, 0);
        let small = test_bitmap(8, 8);
        for glyph in 0..4 {
            packer
                .insert(0, glyph, 8, false, &small)
                .expect("packed glyph");
        }
        for glyph in 0..4 {
            packer.remove((0, glyph, 8, false)).expect("removed glyph");
        }
        // Evicting a whole shelf row must coalesce into one full-width hole.
        assert_eq!(
            packer.free_rects,
            vec![AtlasRect {
                x: 0,
                y: 0,
                width: 32,
                height: 8,
            }]
        );
        assert_packer_consistent(&packer);
    }

    #[test]
    fn evicts_the_least_recently_used_glyph_first() {
        let mut atlas = GlyphAtlas::new(32, 1, 1.0);
        let small = test_bitmap(8, 8);
        for glyph in 0u16..4 {
            atlas
                .pack((0, glyph, 8, false), &small)
                .expect("packed glyph");
        }
        atlas.get(0, 0, 8, false).expect("cached glyph");
        atlas.get(0, 1, 8, false).expect("cached glyph");

        assert!(atlas.evict_one_lru());
        assert!(
            atlas.get(0, 2, 8, false).is_none(),
            "the least recently used glyph must be evicted"
        );
        assert!(atlas.get(0, 0, 8, false).is_some());
        assert!(atlas.get(0, 1, 8, false).is_some());
        assert!(atlas.get(0, 3, 8, false).is_some());
        assert_atlas_consistent(&atlas);
    }

    #[test]
    fn frame_used_glyphs_are_not_evicted() {
        let mut atlas = GlyphAtlas::new(32, 1, 1.0);
        let small = test_bitmap(8, 8);
        for glyph in 0u16..4 {
            atlas
                .pack((0, glyph, 8, false), &small)
                .expect("packed glyph");
        }
        atlas.begin_frame();
        for glyph in 0u16..3 {
            atlas.get(0, glyph, 8, false).expect("cached glyph");
        }
        assert!(atlas.evict_one_lru());
        assert!(
            atlas.get(0, 3, 8, false).is_none(),
            "a glyph not used by this frame is evictable"
        );
        assert!(
            !atlas.evict_one_lru(),
            "frame-protected glyphs must not be evicted"
        );
        for glyph in 0u16..3 {
            assert!(atlas.get(0, glyph, 8, false).is_some());
        }
        atlas.end_frame();
        assert!(atlas.evict_one_lru());
    }

    #[test]
    fn evicted_glyph_can_be_packed_again_in_bounds() {
        let mut atlas = GlyphAtlas::new(32, 1, 1.0);
        let small = test_bitmap(8, 8);
        let key = (0usize, 7u16, 8u32, false);
        let (first, _) = atlas.pack(key, &small).expect("packed glyph");
        assert_eq!(first.layer, 0);
        assert!(atlas.evict_one_lru());
        assert!(atlas.get(0, 7, 8, false).is_none());

        let (second, rect) = atlas.pack(key, &small).expect("repacked glyph");
        assert_eq!(
            rect,
            AtlasRect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            }
        );
        for value in [second.u0, second.v0, second.u1, second.v1] {
            assert!((0.0..=1.0).contains(&value), "UV out of bounds: {value}");
        }
        assert!(second.u1 > second.u0 && second.v1 > second.v0);
        assert_atlas_consistent(&atlas);
    }

    #[test]
    fn eviction_reclaims_space_for_a_larger_glyph() {
        let mut atlas = GlyphAtlas::new(32, 1, 1.0);
        let small = test_bitmap(8, 8);
        for glyph in 0u16..16 {
            atlas
                .pack((0, glyph, 8, false), &small)
                .expect("packed glyph");
        }
        let (_, rect) = atlas
            .pack((0, 99, 16, false), &test_bitmap(16, 16))
            .expect("eviction must free room for a larger glyph");
        assert_eq!(rect.width, 16);
        assert_eq!(rect.height, 16);
        assert!(
            atlas.usage.len() < 17,
            "evicting must reclaim space from older glyphs"
        );
        assert_atlas_consistent(&atlas);
    }

    #[test]
    fn proactive_trim_evicts_until_below_the_threshold() {
        let mut atlas = GlyphAtlas::new(32, 1, 0.5);
        let small = test_bitmap(8, 8);
        for glyph in 0u16..16 {
            atlas
                .pack((0, glyph, 8, false), &small)
                .expect("packed glyph");
        }
        assert!((atlas.occupancy() - 1.0).abs() < 1e-6);
        atlas.begin_frame();
        assert!(
            atlas.occupancy() <= 0.5 + 1e-6,
            "trim must drop occupancy below the threshold, got {}",
            atlas.occupancy()
        );
        atlas.end_frame();
        assert_atlas_consistent(&atlas);
    }
}
