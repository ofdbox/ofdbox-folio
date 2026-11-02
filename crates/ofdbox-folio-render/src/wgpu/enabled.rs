//! wgpu compute backend; W0 supports solid-color fill paths.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

#[cfg(all(feature = "wgpu30", not(feature = "wgpu27")))]
use wgpu as wgpu_lib;
#[cfg(feature = "wgpu27")]
use wgpu27 as wgpu_lib;

use wgpu_lib::util::DeviceExt;

use owned_ttf_parser::{AsFaceRef, OwnedFace};

use ofdbox_folio_ir::{
    FillRule, ImageAsset, ImageElement, Matrix, Paint, Path, PathSegment, Point, Rgba, Scene, Size,
};

use crate::RenderError;
use crate::image::decode_image;
use crate::prepared::{PreparedClip, PreparedCommand, PreparedScene, scene_fingerprint};

use super::atlas::{GlyphInstance, GpuAtlas};
use super::image::ImagePipeline;
use super::text::TextPipeline;

#[derive(Clone, Copy)]
struct DrawPath {
    start: u32,
    count: u32,
    rule: u32,
    kind: u32,
    color: [f32; 4],
    width: f32,
    clip_start: u32,
    clip_count: u32,
    clip_rule: u32,
    end_color: [f32; 4],
    gradient: [f32; 4],
    /// Pixel-space bounds `(min_x, min_y, max_x, max_y)`.
    bbox: [f32; 4],
    /// Source element order within the scene.
    order: u32,
}

/// A wgpu device context.
pub struct WgpuContext {
    device: wgpu_lib::Device,
    queue: wgpu_lib::Queue,
}

impl WgpuContext {
    /// Creates the default adapter and device.
    pub async fn new() -> Result<Self, RenderError> {
        #[cfg(feature = "wgpu30")]
        let descriptor = wgpu_lib::InstanceDescriptor::new_without_display_handle();
        #[cfg(feature = "wgpu27")]
        let descriptor = wgpu_lib::InstanceDescriptor::from_env_or_default();
        #[cfg(feature = "wgpu30")]
        let instance = wgpu_lib::Instance::new(descriptor);
        #[cfg(feature = "wgpu27")]
        let instance = wgpu_lib::Instance::new(&descriptor);
        let adapter = instance
            .request_adapter(&wgpu_lib::RequestAdapterOptions::default())
            .await
            .map_err(|error| RenderError::Backend(format!("wgpu adapter: {error}")))?;
        let (device, queue) = adapter
            .request_device(&wgpu_lib::DeviceDescriptor {
                label: Some("ofdbox-wgpu"),
                ..Default::default()
            })
            .await
            .map_err(|error| RenderError::Backend(format!("wgpu device: {error}")))?;
        Ok(Self { device, queue })
    }

    /// Creates the context in a blocking manner.
    pub fn new_blocking() -> Result<Self, RenderError> {
        pollster::block_on(Self::new())
    }

    /// Reuses a device and queue already created externally.
    ///
    /// When the display core is attached to a UI framework (such as
    /// dioxus-native/anyrender), the framework's wgpu device must be reused to
    /// avoid creating a second GPU context in the same process or being unable to
    /// share textures.
    pub fn from_device_queue(device: wgpu_lib::Device, queue: wgpu_lib::Queue) -> Self {
        Self { device, queue }
    }

    /// The device reference.
    pub fn device(&self) -> &wgpu_lib::Device {
        &self.device
    }

    /// The queue reference.
    pub fn queue(&self) -> &wgpu_lib::Queue {
        &self.queue
    }
}

/// Resource limits for a wgpu backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WgpuOptions {
    /// Byte budget for decoded image textures.
    pub max_image_cache_bytes: usize,
    /// Maximum number of cached tile geometries before the cache is cleared.
    pub max_tile_cache_entries: usize,
    /// Maximum number of prepared scenes retained across tiles.
    pub max_prepared_scenes: usize,
}

impl Default for WgpuOptions {
    fn default() -> Self {
        Self {
            max_image_cache_bytes: 64 * 1024 * 1024,
            max_tile_cache_entries: 64,
            max_prepared_scenes: 8,
        }
    }
}

/// Current wgpu capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WgpuCapabilities {
    /// Solid-color paths.
    pub solid_paths: bool,
    /// Strokes.
    pub strokes: bool,
    /// Linear gradients.
    pub linear_gradients: bool,
    /// Clip paths.
    pub clips: bool,
    /// Text glyphs drawn from a glyph atlas.
    pub text_outlines: bool,
    /// Images.
    pub images: bool,
    /// Blend modes.
    pub blend_modes: bool,
}

/// A wgpu render target.
pub struct WgpuTarget {
    texture: wgpu_lib::Texture,
    view: wgpu_lib::TextureView,
    width: u32,
    height: u32,
    dpi: f64,
}

impl WgpuTarget {
    /// Creates a target from a device pixel size.
    ///
    /// Used when the display core rasterizes tiles: the tile size is fixed in
    /// device pixels and is no longer derived from the page's millimeter size.
    pub fn from_pixels(
        context: &WgpuContext,
        width: u32,
        height: u32,
        dpi: f64,
    ) -> Result<Self, RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::InvalidPage);
        }
        let texture = context.device.create_texture(&wgpu_lib::TextureDescriptor {
            label: Some("ofdbox-wgpu-tile"),
            size: wgpu_lib::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu_lib::TextureDimension::D2,
            format: wgpu_lib::TextureFormat::Rgba8Unorm,
            usage: wgpu_lib::TextureUsages::STORAGE_BINDING
                | wgpu_lib::TextureUsages::RENDER_ATTACHMENT
                | wgpu_lib::TextureUsages::COPY_SRC
                | wgpu_lib::TextureUsages::COPY_DST
                | wgpu_lib::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu_lib::TextureViewDescriptor::default());
        Ok(Self {
            texture,
            view,
            width,
            height,
            dpi,
        })
    }

    /// Creates an offscreen target from the Scene size.
    pub fn from_size(context: &WgpuContext, size: Size, dpi: f64) -> Result<Self, RenderError> {
        Self::new(context, size, dpi)
    }

    /// Creates an offscreen target from the Scene size.
    pub fn new(context: &WgpuContext, size: Size, dpi: f64) -> Result<Self, RenderError> {
        let width = (size.width / 25.4 * dpi).ceil().max(1.0) as u32;
        let height = (size.height / 25.4 * dpi).ceil().max(1.0) as u32;
        let texture = context.device.create_texture(&wgpu_lib::TextureDescriptor {
            label: Some("ofdbox-wgpu-target"),
            size: wgpu_lib::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu_lib::TextureDimension::D2,
            format: wgpu_lib::TextureFormat::Rgba8Unorm,
            usage: wgpu_lib::TextureUsages::STORAGE_BINDING
                | wgpu_lib::TextureUsages::RENDER_ATTACHMENT
                | wgpu_lib::TextureUsages::COPY_SRC
                | wgpu_lib::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu_lib::TextureViewDescriptor::default());
        Ok(Self {
            texture,
            view,
            width,
            height,
            dpi,
        })
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }
    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The target texture (can be handed directly to a UI framework for compositing).
    pub fn texture(&self) -> &wgpu_lib::Texture {
        &self.texture
    }

    /// Reads back the RGBA8 pixels.
    pub fn read_rgba(&self, context: &WgpuContext) -> Result<Vec<u8>, RenderError> {
        let row = self.width * 4;
        let padded = row.div_ceil(wgpu_lib::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu_lib::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = context.device.create_buffer(&wgpu_lib::BufferDescriptor {
            label: Some("ofdbox-wgpu-readback"),
            size: u64::from(padded) * u64::from(self.height),
            usage: wgpu_lib::BufferUsages::COPY_DST | wgpu_lib::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = context.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu_lib::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu_lib::Origin3d::ZERO,
                aspect: wgpu_lib::TextureAspect::All,
            },
            wgpu_lib::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu_lib::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu_lib::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        context.queue.submit([encoder.finish()]);

        let (sender, receiver) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu_lib::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        context
            .device
            .poll(wgpu_lib::PollType::wait_indefinitely())
            .map_err(|error| RenderError::Backend(format!("wgpu poll: {error}")))?;
        receiver
            .recv()
            .map_err(|_| RenderError::Backend("wgpu readback channel".to_owned()))?
            .map_err(|error| RenderError::Backend(format!("wgpu readback: {error}")))?;
        #[cfg(feature = "wgpu30")]
        let mapped = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|error| RenderError::Backend(format!("wgpu map range: {error}")))?;
        #[cfg(feature = "wgpu27")]
        let mapped = buffer.slice(..).get_mapped_range();
        let mut output = Vec::with_capacity((self.width * self.height * 4) as usize);
        for row in mapped.chunks(padded as usize) {
            output.extend_from_slice(&row[..(self.width * 4) as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(output)
    }
}

/// Key for the flattened-geometry cache.
type TileCacheKey = (usize, u64, i32, i32, u32, u32);

/// Flattened edges plus draw paths for one tile.
type CachedGeometry = Arc<(Vec<[f32; 4]>, Vec<DrawPath>)>;

/// A parsed font face, keeping its font alive while the pointer key is used.
struct CachedFace {
    _font: Arc<ofdbox_folio_ir::FontAsset>,
    face: OwnedFace,
}

/// A cached glyph outline, keeping its font alive while the pointer key is used.
struct CachedGlyph {
    _font: Arc<ofdbox_folio_ir::FontAsset>,
    path: Arc<Path>,
}

/// A decoded image uploaded to a GPU texture.
struct CachedImage {
    _image: Arc<ImageAsset>,
    _texture: wgpu_lib::Texture,
    view: wgpu_lib::TextureView,
    bytes: usize,
}

/// Byte-budgeted image texture cache with insertion-order eviction.
struct ImageCache {
    entries: HashMap<usize, Arc<CachedImage>>,
    order: VecDeque<usize>,
    bytes: usize,
    max_bytes: usize,
}

impl ImageCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            max_bytes,
        }
    }

    fn get(&mut self, key: usize) -> Option<Arc<CachedImage>> {
        let image = Arc::clone(self.entries.get(&key)?);
        self.order.retain(|value| *value != key);
        self.order.push_back(key);
        Some(image)
    }

    fn insert(&mut self, key: usize, image: Arc<CachedImage>) {
        if let Some(previous) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(previous.bytes);
            self.order.retain(|value| *value != key);
        }
        self.bytes = self.bytes.saturating_add(image.bytes);
        self.order.push_back(key);
        self.entries.insert(key, image);
        while self.bytes > self.max_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }
}

/// One image quad in page-pixel space.
struct ImageDraw {
    origin: [f32; 2],
    x_axis: [f32; 2],
    y_axis: [f32; 2],
    alpha: f32,
    image: Arc<ImageAsset>,
    clip_start: u32,
    clip_count: u32,
    clip_rule: u32,
}

/// A prepared text or image draw for one decoration element.
enum DecorDraw {
    Text {
        range: std::ops::Range<u32>,
        scissor: Scissor,
    },
    Image {
        index: usize,
        scissor: Scissor,
    },
}

/// One source-ordered run of path draws or decoration draws.
enum DrawRun {
    Paths(Vec<usize>),
    Decor(Vec<usize>),
}

/// Pixel-aligned scissor rectangle used to clip text and image quads.
#[derive(Clone, Copy)]
struct Scissor {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Scissor {
    fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    fn from_clip(clip: [f32; 4], width: u32, height: u32) -> Self {
        let max_x = width as f32;
        let max_y = height as f32;
        let x0 = clip[0].floor().clamp(0.0, max_x) as u32;
        let y0 = clip[1].floor().clamp(0.0, max_y) as u32;
        let x1 = clip[2].ceil().clamp(0.0, max_x) as u32;
        let y1 = clip[3].ceil().clamp(0.0, max_y) as u32;
        Self {
            x: x0,
            y: y0,
            width: x1.saturating_sub(x0),
            height: y1.saturating_sub(y0),
        }
    }
}

/// The wgpu compute backend.
pub struct WgpuBackend {
    context: Arc<WgpuContext>,
    pipeline: wgpu_lib::ComputePipeline,
    layout: wgpu_lib::BindGroupLayout,
    /// Font outlines keyed by `(font asset pointer, glyph id)`.
    glyph_cache: Mutex<HashMap<(usize, u16), CachedGlyph>>,
    /// Parsed font faces keyed by `(font asset pointer, face index)`.
    font_faces: Mutex<HashMap<(usize, u32), Arc<CachedFace>>>,
    /// Already-flattened scene geometry per scene / dpi / region.
    path_cache: Mutex<HashMap<TileCacheKey, CachedGeometry>>,
    /// Backend-neutral prepared scenes, shared by all tiles of one page.
    prepared_cache: Mutex<HashMap<(usize, u64), Arc<PreparedScene>>>,
    atlas: Mutex<GpuAtlas>,
    text_pipeline: TextPipeline,
    image_pipeline: ImagePipeline,
    /// Decoded image textures keyed by `ImageAsset` pointer.
    image_cache: Mutex<ImageCache>,
    image_sampler: wgpu_lib::Sampler,
    options: WgpuOptions,
}

impl WgpuBackend {
    /// Creates a backend; an initialized context must be supplied explicitly.
    pub fn new(context: WgpuContext) -> Result<Self, RenderError> {
        Self::new_with_options(context, WgpuOptions::default())
    }

    /// Creates a backend with explicit resource limits.
    pub fn new_with_options(
        context: WgpuContext,
        options: WgpuOptions,
    ) -> Result<Self, RenderError> {
        Self::from_shared_with_options(Arc::new(context), options)
    }

    /// Creates a backend from a shared context.
    pub fn from_shared(context: Arc<WgpuContext>) -> Result<Self, RenderError> {
        Self::from_shared_with_options(context, WgpuOptions::default())
    }

    /// Creates a backend from a shared context with explicit resource limits.
    pub fn from_shared_with_options(
        context: Arc<WgpuContext>,
        options: WgpuOptions,
    ) -> Result<Self, RenderError> {
        let module = context
            .device
            .create_shader_module(wgpu_lib::ShaderModuleDescriptor {
                label: Some("ofdbox-wgpu-shader"),
                source: wgpu_lib::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
            });
        let layout =
            context
                .device
                .create_bind_group_layout(&wgpu_lib::BindGroupLayoutDescriptor {
                    label: Some("ofdbox-wgpu-bind-layout"),
                    entries: &[
                        storage_entry(0, true),
                        storage_entry(1, true),
                        storage_entry(2, true),
                        wgpu_lib::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu_lib::ShaderStages::COMPUTE,
                            ty: wgpu_lib::BindingType::StorageTexture {
                                access: wgpu_lib::StorageTextureAccess::WriteOnly,
                                format: wgpu_lib::TextureFormat::Rgba8Unorm,
                                view_dimension: wgpu_lib::TextureViewDimension::D2,
                            },
                            count: None,
                        },
                    ],
                });
        #[cfg(feature = "wgpu30")]
        let pipeline_layout =
            context
                .device
                .create_pipeline_layout(&wgpu_lib::PipelineLayoutDescriptor {
                    label: Some("ofdbox-wgpu-pipeline-layout"),
                    bind_group_layouts: &[Some(&layout)],
                    immediate_size: 0,
                });
        #[cfg(feature = "wgpu27")]
        let pipeline_layout =
            context
                .device
                .create_pipeline_layout(&wgpu_lib::PipelineLayoutDescriptor {
                    label: Some("ofdbox-wgpu-pipeline-layout"),
                    bind_group_layouts: &[&layout],
                    push_constant_ranges: &[],
                });
        let pipeline =
            context
                .device
                .create_compute_pipeline(&wgpu_lib::ComputePipelineDescriptor {
                    label: Some("ofdbox-wgpu-pipeline"),
                    layout: Some(&pipeline_layout),
                    module: &module,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    cache: None,
                });
        let atlas = Mutex::new(GpuAtlas::new(&context.device, 4096));
        let text_pipeline = TextPipeline::new(&context.device);
        let image_pipeline = ImagePipeline::new(&context.device);
        let image_sampler = context.device.create_sampler(&wgpu_lib::SamplerDescriptor {
            label: Some("ofdbox-image-sampler"),
            address_mode_u: wgpu_lib::AddressMode::ClampToEdge,
            address_mode_v: wgpu_lib::AddressMode::ClampToEdge,
            mag_filter: wgpu_lib::FilterMode::Linear,
            min_filter: wgpu_lib::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            context,
            pipeline,
            layout,
            glyph_cache: Mutex::new(HashMap::new()),
            font_faces: Mutex::new(HashMap::new()),
            path_cache: Mutex::new(HashMap::new()),
            prepared_cache: Mutex::new(HashMap::new()),
            atlas,
            text_pipeline,
            image_pipeline,
            image_cache: Mutex::new(ImageCache::new(options.max_image_cache_bytes)),
            image_sampler,
            options,
        })
    }

    /// Drops cached glyph outlines and flattened tile geometry.
    ///
    /// Call this after mutating a scene that was previously rendered: the tile
    /// cache is keyed by the scene allocation address, so reusing it for different
    /// content requires an explicit invalidation.
    pub fn clear_caches(&self) {
        self.glyph_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.font_faces
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.path_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.prepared_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.atlas
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.image_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }

    /// Number of cached glyph outlines and flattened tile geometries.
    pub fn cache_stats(&self) -> (usize, usize) {
        let glyphs = self
            .glyph_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len();
        let tiles = self
            .path_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len();
        (glyphs, tiles)
    }

    /// Renders into an offscreen texture.
    pub fn render_to_target(&self, scene: &Scene, target: &WgpuTarget) -> Result<(), RenderError> {
        self.render_to_target_with_offset(scene, target, (0.0, 0.0))
    }

    /// Renders one Fixed IR page into an offscreen texture.
    pub fn render_page(
        &self,
        page: &ofdbox_folio_ir::Page,
        target: &WgpuTarget,
    ) -> Result<(), RenderError> {
        self.render_to_target(&page.content, target)
    }

    /// Renders one region of a Fixed IR page into an offscreen tile.
    pub fn render_page_region(
        &self,
        page: &ofdbox_folio_ir::Page,
        target: &WgpuTarget,
        origin_px: (f64, f64),
    ) -> Result<(), RenderError> {
        self.render_region(&page.content, target, origin_px)
    }

    /// Renders the given region of a scene into an offscreen texture.
    ///
    /// `origin_px` is the position of the region's top-left corner in whole-page
    /// device pixel coordinates, used for per-tile partial refreshes.
    pub fn render_region(
        &self,
        scene: &Scene,
        target: &WgpuTarget,
        origin_px: (f64, f64),
    ) -> Result<(), RenderError> {
        if !(origin_px.0.is_finite() && origin_px.1.is_finite()) {
            return Err(RenderError::InvalidPage);
        }
        self.render_to_target_with_offset(scene, target, origin_px)
    }

    fn prepared_scene(&self, scene: &Scene) -> Arc<PreparedScene> {
        let key = (scene as *const Scene as usize, scene_fingerprint(scene));
        let mut cache = self
            .prepared_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(prepared) = cache.get(&key) {
            return Arc::clone(prepared);
        }
        let prepared = Arc::new(PreparedScene::from_scene(scene));
        if cache.len() > self.options.max_prepared_scenes {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&prepared));
        prepared
    }

    fn render_to_target_with_offset(
        &self,
        scene: &Scene,
        target: &WgpuTarget,
        origin_px: (f64, f64),
    ) -> Result<(), RenderError> {
        let prepared = self.prepared_scene(scene);
        let key = (
            scene as *const Scene as usize,
            target.dpi.to_bits(),
            origin_px.0.round() as i32,
            origin_px.1.round() as i32,
            target.width,
            target.height,
        );
        let cached = {
            let mut cache = self
                .path_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(cached) = cache.get(&key) {
                Arc::clone(cached)
            } else {
                let collected = collect_paths(
                    &prepared,
                    target.dpi,
                    origin_px,
                    target.width,
                    target.height,
                );
                let cached = Arc::new(collected);
                if cache.len() > self.options.max_tile_cache_entries {
                    cache.clear();
                }
                cache.insert(key, Arc::clone(&cached));
                cached
            }
        };
        let (edges, paths) = cached.as_ref();
        let decorations = collect_decorations(
            &prepared,
            target.dpi,
            origin_px,
            target.width,
            target.height,
        );
        let mut decor_draws = Vec::with_capacity(decorations.len());
        let mut decor_orders = Vec::with_capacity(decorations.len());
        let mut image_draws = Vec::new();
        let mut text_bind_group = None;
        let mut clip_edges: Vec<[f32; 4]> = Vec::new();
        let clip_buffer;
        {
            let mut atlas = self.atlas.lock().unwrap_or_else(|error| error.into_inner());
            let mut instances = Vec::new();
            for decoration in decorations {
                match decoration {
                    Decoration::Text {
                        text,
                        world,
                        opacity,
                        clip,
                        clip_path,
                        _order,
                    } => {
                        decor_orders.push(_order);
                        let scissor = clip
                            .map_or(Scissor::full(target.width, target.height), |clip| {
                                Scissor::from_clip(clip, target.width, target.height)
                            });
                        if scissor.width == 0 || scissor.height == 0 {
                            decor_draws.push(DecorDraw::Text {
                                range: 0..0,
                                scissor,
                            });
                            continue;
                        }
                        let start = u32::try_from(instances.len())
                            .map_err(|_| RenderError::Backend("too many text glyphs".into()))?;
                        emit_text_instances(
                            &text,
                            world,
                            opacity,
                            clip,
                            target.dpi / 25.4,
                            origin_px,
                            Viewport {
                                min_x: -1.0,
                                min_y: -1.0,
                                max_x: target.width as f32 + 1.0,
                                max_y: target.height as f32 + 1.0,
                            },
                            &self.glyph_cache,
                            &self.font_faces,
                            &mut atlas,
                            &self.context.queue,
                            &mut instances,
                        );
                        let end = u32::try_from(instances.len())
                            .map_err(|_| RenderError::Backend("too many text glyphs".into()))?;
                        if let Some(clip) = &clip_path {
                            let clip_start = u32::try_from(clip_edges.len()).unwrap_or(u32::MAX);
                            flatten_path(
                                &clip.path,
                                clip.transform,
                                target.dpi / 25.4,
                                origin_px,
                                true,
                                &mut clip_edges,
                            );
                            let clip_count =
                                u32::try_from(clip_edges.len()).unwrap_or(u32::MAX) - clip_start;
                            let clip_rule = u32::from(clip.rule == FillRule::EvenOdd);
                            for instance in &mut instances[start as usize..end as usize] {
                                instance.clip_start = clip_start;
                                instance.clip_count = clip_count;
                                instance.clip_rule = clip_rule;
                            }
                        }
                        decor_draws.push(DecorDraw::Text {
                            range: start..end,
                            scissor,
                        });
                    }
                    Decoration::Image {
                        mut draw,
                        clip,
                        clip_path,
                        _order,
                    } => {
                        decor_orders.push(_order);
                        let scissor = clip
                            .map_or(Scissor::full(target.width, target.height), |clip| {
                                Scissor::from_clip(clip, target.width, target.height)
                            });
                        if scissor.width == 0 || scissor.height == 0 {
                            decor_draws.push(DecorDraw::Image {
                                index: usize::MAX,
                                scissor,
                            });
                            continue;
                        }
                        if let Some(clip) = clip_path {
                            let start = u32::try_from(clip_edges.len()).unwrap_or(u32::MAX);
                            flatten_path(
                                &clip.path,
                                clip.transform,
                                target.dpi / 25.4,
                                origin_px,
                                true,
                                &mut clip_edges,
                            );
                            draw.clip_start = start;
                            draw.clip_count =
                                u32::try_from(clip_edges.len()).unwrap_or(u32::MAX) - start;
                            draw.clip_rule = u32::from(clip.rule == FillRule::EvenOdd);
                        }
                        let index = image_draws.len();
                        image_draws.push(draw);
                        decor_draws.push(DecorDraw::Image { index, scissor });
                    }
                }
            }
            let mut clip_data = Vec::with_capacity(clip_edges.len().max(1) * 16);
            if clip_edges.is_empty() {
                clip_data.extend_from_slice(&[0_u8; 16]);
            } else {
                for edge in &clip_edges {
                    for value in edge {
                        clip_data.extend_from_slice(&value.to_ne_bytes());
                    }
                }
            }
            clip_buffer =
                self.context
                    .device
                    .create_buffer_init(&wgpu_lib::util::BufferInitDescriptor {
                        label: Some("ofdbox-clip-edges"),
                        contents: &clip_data,
                        usage: wgpu_lib::BufferUsages::STORAGE,
                    });
            if !instances.is_empty() {
                let mut instance_data = Vec::with_capacity(instances.len() * 160);
                for instance in &instances {
                    instance.write_bytes(&mut instance_data);
                }
                let instance_buffer =
                    self.context
                        .device
                        .create_buffer_init(&wgpu_lib::util::BufferInitDescriptor {
                            label: Some("ofdbox-text-instances"),
                            contents: &instance_data,
                            usage: wgpu_lib::BufferUsages::STORAGE,
                        });
                let mut params = Vec::with_capacity(16);
                for value in [target.width as f32, target.height as f32, 0.0, 0.0] {
                    params.extend_from_slice(&value.to_ne_bytes());
                }
                let params_buffer =
                    self.context
                        .device
                        .create_buffer_init(&wgpu_lib::util::BufferInitDescriptor {
                            label: Some("ofdbox-text-params"),
                            contents: &params,
                            usage: wgpu_lib::BufferUsages::UNIFORM,
                        });
                let bind_group =
                    self.context
                        .device
                        .create_bind_group(&wgpu_lib::BindGroupDescriptor {
                            label: Some("ofdbox-text-bind-group"),
                            layout: self.text_pipeline.layout(),
                            entries: &[
                                wgpu_lib::BindGroupEntry {
                                    binding: 0,
                                    resource: instance_buffer.as_entire_binding(),
                                },
                                wgpu_lib::BindGroupEntry {
                                    binding: 1,
                                    resource: wgpu_lib::BindingResource::TextureView(atlas.view()),
                                },
                                wgpu_lib::BindGroupEntry {
                                    binding: 2,
                                    resource: wgpu_lib::BindingResource::Sampler(atlas.sampler()),
                                },
                                wgpu_lib::BindGroupEntry {
                                    binding: 3,
                                    resource: params_buffer.as_entire_binding(),
                                },
                                wgpu_lib::BindGroupEntry {
                                    binding: 4,
                                    resource: clip_buffer.as_entire_binding(),
                                },
                            ],
                        });
                text_bind_group = Some(bind_group);
            }
        }
        let mut image_batches = Vec::with_capacity(image_draws.len());
        for draw in &image_draws {
            let Some(cached) = self.cached_image(&draw.image) else {
                image_batches.push(None);
                continue;
            };
            let mut params = Vec::with_capacity(48);
            for value in [
                target.width as f32,
                target.height as f32,
                draw.origin[0],
                draw.origin[1],
                draw.x_axis[0],
                draw.x_axis[1],
                draw.y_axis[0],
                draw.y_axis[1],
                draw.alpha,
            ] {
                params.extend_from_slice(&value.to_ne_bytes());
            }
            for value in [draw.clip_start, draw.clip_count, draw.clip_rule] {
                params.extend_from_slice(&value.to_ne_bytes());
            }
            let params_buffer =
                self.context
                    .device
                    .create_buffer_init(&wgpu_lib::util::BufferInitDescriptor {
                        label: Some("ofdbox-image-params"),
                        contents: &params,
                        usage: wgpu_lib::BufferUsages::UNIFORM,
                    });
            let bind_group =
                self.context
                    .device
                    .create_bind_group(&wgpu_lib::BindGroupDescriptor {
                        label: Some("ofdbox-image-bind-group"),
                        layout: self.image_pipeline.layout(),
                        entries: &[
                            wgpu_lib::BindGroupEntry {
                                binding: 0,
                                resource: params_buffer.as_entire_binding(),
                            },
                            wgpu_lib::BindGroupEntry {
                                binding: 1,
                                resource: wgpu_lib::BindingResource::TextureView(&cached.view),
                            },
                            wgpu_lib::BindGroupEntry {
                                binding: 2,
                                resource: wgpu_lib::BindingResource::Sampler(&self.image_sampler),
                            },
                            wgpu_lib::BindGroupEntry {
                                binding: 3,
                                resource: clip_buffer.as_entire_binding(),
                            },
                        ],
                    });
            image_batches.push(Some(bind_group));
        }
        let mut path_groups: Vec<(u32, Vec<usize>)> = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            if let Some(last) = path_groups.last_mut()
                && last.0 == path.order
            {
                last.1.push(index);
            } else {
                path_groups.push((path.order, vec![index]));
            }
        }
        let mut runs = Vec::new();
        let (mut path_index, mut decor_index) = (0, 0);
        while path_index < path_groups.len() || decor_index < decor_orders.len() {
            let path_order = path_groups.get(path_index).map(|group| group.0);
            let decoration_order = decor_orders.get(decor_index).copied();
            let take_path = match (path_order, decoration_order) {
                (Some(path), Some(decoration)) => path < decoration,
                (Some(_), None) => true,
                _ => false,
            };
            if take_path {
                let group = &path_groups[path_index].1;
                if let Some(DrawRun::Paths(indices)) = runs.last_mut() {
                    indices.extend_from_slice(group);
                } else {
                    runs.push(DrawRun::Paths(group.clone()));
                }
                path_index += 1;
            } else {
                if let Some(DrawRun::Decor(indices)) = runs.last_mut() {
                    indices.push(decor_index);
                } else {
                    runs.push(DrawRun::Decor(vec![decor_index]));
                }
                decor_index += 1;
            }
        }
        let mut encoder = self
            .context
            .device
            .create_command_encoder(&Default::default());
        let clear_attachment = Some(wgpu_lib::RenderPassColorAttachment {
            view: &target.view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu_lib::Operations {
                load: wgpu_lib::LoadOp::Clear(wgpu_lib::Color {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                }),
                store: wgpu_lib::StoreOp::Store,
            },
        });
        let clear_descriptor = wgpu_lib::RenderPassDescriptor {
            label: Some("ofdbox-clear-pass"),
            color_attachments: &[clear_attachment],
            ..Default::default()
        };
        {
            let _pass = encoder.begin_render_pass(&clear_descriptor);
        }
        let has_paths = runs.iter().any(|run| matches!(run, DrawRun::Paths(_)));
        let layer = has_paths.then(|| {
            let texture = self
                .context
                .device
                .create_texture(&wgpu_lib::TextureDescriptor {
                    label: Some("ofdbox-path-layer"),
                    size: wgpu_lib::Extent3d {
                        width: target.width,
                        height: target.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu_lib::TextureDimension::D2,
                    format: wgpu_lib::TextureFormat::Rgba8Unorm,
                    usage: wgpu_lib::TextureUsages::STORAGE_BINDING
                        | wgpu_lib::TextureUsages::TEXTURE_BINDING
                        | wgpu_lib::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
            let view = texture.create_view(&wgpu_lib::TextureViewDescriptor::default());
            (texture, view)
        });
        let edge_buffer = (!edges.is_empty()).then(|| {
            let mut edge_data = Vec::with_capacity(edges.len() * 16);
            for edge in edges {
                for value in edge {
                    edge_data.extend_from_slice(&value.to_ne_bytes());
                }
            }
            self.context
                .device
                .create_buffer_init(&wgpu_lib::util::BufferInitDescriptor {
                    label: Some("ofdbox-edge-buffer"),
                    contents: &edge_data,
                    usage: wgpu_lib::BufferUsages::STORAGE,
                })
        });
        for run in &runs {
            match run {
                DrawRun::Paths(indices) => {
                    let Some((_layer_texture, layer_view)) = &layer else {
                        continue;
                    };
                    let mut path_data = Vec::with_capacity(indices.len() * 96);
                    for index in indices {
                        let path = &paths[*index];
                        for value in [path.start, path.count, path.rule, path.kind] {
                            path_data.extend_from_slice(&value.to_ne_bytes());
                        }
                        for value in path.color {
                            path_data.extend_from_slice(&value.to_ne_bytes());
                        }
                        path_data.extend_from_slice(&path.width.to_ne_bytes());
                        for value in [path.clip_start, path.clip_count, path.clip_rule] {
                            path_data.extend_from_slice(&value.to_ne_bytes());
                        }
                        for value in path.end_color {
                            path_data.extend_from_slice(&value.to_ne_bytes());
                        }
                        for value in path.gradient {
                            path_data.extend_from_slice(&value.to_ne_bytes());
                        }
                        for value in path.bbox {
                            path_data.extend_from_slice(&value.to_ne_bytes());
                        }
                    }
                    let Some(edge_buffer) = &edge_buffer else {
                        continue;
                    };
                    let path_buffer = self.context.device.create_buffer_init(
                        &wgpu_lib::util::BufferInitDescriptor {
                            label: Some("ofdbox-path-buffer"),
                            contents: &path_data,
                            usage: wgpu_lib::BufferUsages::STORAGE,
                        },
                    );
                    let params = [
                        target.width,
                        target.height,
                        u32::try_from(indices.len())
                            .map_err(|_| RenderError::Backend("too many paths".into()))?,
                        1.0_f32.to_bits(),
                    ];
                    let mut params_bytes = Vec::with_capacity(16);
                    for value in params {
                        params_bytes.extend_from_slice(&value.to_ne_bytes());
                    }
                    let params_buffer = self.context.device.create_buffer_init(
                        &wgpu_lib::util::BufferInitDescriptor {
                            label: Some("ofdbox-layer-params"),
                            contents: &params_bytes,
                            usage: wgpu_lib::BufferUsages::STORAGE,
                        },
                    );
                    let bind_group =
                        self.context
                            .device
                            .create_bind_group(&wgpu_lib::BindGroupDescriptor {
                                label: Some("ofdbox-layer-bind-group"),
                                layout: &self.layout,
                                entries: &[
                                    wgpu_lib::BindGroupEntry {
                                        binding: 0,
                                        resource: edge_buffer.as_entire_binding(),
                                    },
                                    wgpu_lib::BindGroupEntry {
                                        binding: 1,
                                        resource: path_buffer.as_entire_binding(),
                                    },
                                    wgpu_lib::BindGroupEntry {
                                        binding: 2,
                                        resource: params_buffer.as_entire_binding(),
                                    },
                                    wgpu_lib::BindGroupEntry {
                                        binding: 3,
                                        resource: wgpu_lib::BindingResource::TextureView(
                                            layer_view,
                                        ),
                                    },
                                ],
                            });
                    {
                        let mut pass =
                            encoder.begin_compute_pass(&wgpu_lib::ComputePassDescriptor::default());
                        pass.set_pipeline(&self.pipeline);
                        pass.set_bind_group(0, &bind_group, &[]);
                        pass.dispatch_workgroups(
                            target.width.div_ceil(8),
                            target.height.div_ceil(8),
                            1,
                        );
                    }
                    let mut composite_params = Vec::with_capacity(48);
                    for value in [
                        target.width as f32,
                        target.height as f32,
                        0.0,
                        0.0,
                        target.width as f32,
                        0.0,
                        0.0,
                        target.height as f32,
                        1.0,
                        0.0,
                        0.0,
                        0.0,
                    ] {
                        composite_params.extend_from_slice(&value.to_ne_bytes());
                    }
                    let composite_buffer = self.context.device.create_buffer_init(
                        &wgpu_lib::util::BufferInitDescriptor {
                            label: Some("ofdbox-layer-composite-params"),
                            contents: &composite_params,
                            usage: wgpu_lib::BufferUsages::UNIFORM,
                        },
                    );
                    let composite_group =
                        self.context
                            .device
                            .create_bind_group(&wgpu_lib::BindGroupDescriptor {
                                label: Some("ofdbox-layer-composite"),
                                layout: self.image_pipeline.layout(),
                                entries: &[
                                    wgpu_lib::BindGroupEntry {
                                        binding: 0,
                                        resource: composite_buffer.as_entire_binding(),
                                    },
                                    wgpu_lib::BindGroupEntry {
                                        binding: 1,
                                        resource: wgpu_lib::BindingResource::TextureView(
                                            layer_view,
                                        ),
                                    },
                                    wgpu_lib::BindGroupEntry {
                                        binding: 2,
                                        resource: wgpu_lib::BindingResource::Sampler(
                                            &self.image_sampler,
                                        ),
                                    },
                                    wgpu_lib::BindGroupEntry {
                                        binding: 3,
                                        resource: clip_buffer.as_entire_binding(),
                                    },
                                ],
                            });
                    let attachment = Some(wgpu_lib::RenderPassColorAttachment {
                        view: &target.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu_lib::Operations {
                            load: wgpu_lib::LoadOp::Load,
                            store: wgpu_lib::StoreOp::Store,
                        },
                    });
                    let descriptor = wgpu_lib::RenderPassDescriptor {
                        label: Some("ofdbox-layer-composite-pass"),
                        color_attachments: &[attachment],
                        ..Default::default()
                    };
                    let mut pass = encoder.begin_render_pass(&descriptor);
                    pass.set_pipeline(self.image_pipeline.pipeline());
                    pass.set_bind_group(0, &composite_group, &[]);
                    pass.set_scissor_rect(0, 0, target.width, target.height);
                    pass.draw(0..6, 0..1);
                }
                DrawRun::Decor(indices) => {
                    let attachment = Some(wgpu_lib::RenderPassColorAttachment {
                        view: &target.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu_lib::Operations {
                            load: wgpu_lib::LoadOp::Load,
                            store: wgpu_lib::StoreOp::Store,
                        },
                    });
                    let descriptor = wgpu_lib::RenderPassDescriptor {
                        label: Some("ofdbox-decoration-pass"),
                        color_attachments: &[attachment],
                        ..Default::default()
                    };
                    let mut pass = encoder.begin_render_pass(&descriptor);
                    for index in indices {
                        match &decor_draws[*index] {
                            DecorDraw::Text { range, scissor } => {
                                if range.is_empty() {
                                    continue;
                                }
                                let Some(bind_group) = &text_bind_group else {
                                    continue;
                                };
                                pass.set_scissor_rect(
                                    scissor.x,
                                    scissor.y,
                                    scissor.width,
                                    scissor.height,
                                );
                                pass.set_pipeline(self.text_pipeline.pipeline());
                                pass.set_bind_group(0, bind_group, &[]);
                                pass.draw(0..6, range.clone());
                            }
                            DecorDraw::Image { index, scissor } => {
                                let Some(Some(bind_group)) = image_batches.get(*index) else {
                                    continue;
                                };
                                pass.set_scissor_rect(
                                    scissor.x,
                                    scissor.y,
                                    scissor.width,
                                    scissor.height,
                                );
                                pass.set_pipeline(self.image_pipeline.pipeline());
                                pass.set_bind_group(0, bind_group, &[]);
                                pass.draw(0..6, 0..1);
                            }
                        }
                    }
                }
            }
        }
        self.context.queue.submit([encoder.finish()]);
        self.context
            .device
            .poll(wgpu_lib::PollType::wait_indefinitely())
            .map_err(|error| RenderError::Backend(format!("wgpu poll: {error}")))?;
        Ok(())
    }
}

impl WgpuBackend {
    /// Decodes and uploads an image once, then reuses the GPU texture.
    fn cached_image(&self, image: &Arc<ImageAsset>) -> Option<Arc<CachedImage>> {
        let key = Arc::as_ptr(image) as usize;
        if let Some(cached) = self
            .image_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(key)
        {
            return Some(cached);
        }
        let mut decoded = decode_image(image.format, &image.encoded, 64_000_000).ok()?;
        if let Some(mask) = &image.mask
            && let Ok(mask) = decode_image(mask.format, &mask.encoded, 64_000_000)
        {
            let _ = decoded.apply_mask(&mask);
        }
        if decoded.width == 0 || decoded.height == 0 {
            return None;
        }
        let width = decoded.width;
        let height = decoded.height;
        let texture = self
            .context
            .device
            .create_texture(&wgpu_lib::TextureDescriptor {
                label: Some("ofdbox-image-texture"),
                size: wgpu_lib::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu_lib::TextureDimension::D2,
                format: wgpu_lib::TextureFormat::Rgba8Unorm,
                usage: wgpu_lib::TextureUsages::TEXTURE_BINDING | wgpu_lib::TextureUsages::COPY_DST,
                view_formats: &[],
            });
        let padded = (width * 4).div_ceil(wgpu_lib::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu_lib::COPY_BYTES_PER_ROW_ALIGNMENT;
        let mut data = vec![0_u8; (padded * height) as usize];
        for row in 0..height as usize {
            let source = &decoded.rgba[row * width as usize * 4..(row + 1) * width as usize * 4];
            let start = row * padded as usize;
            data[start..start + source.len()].copy_from_slice(source);
        }
        self.context.queue.write_texture(
            wgpu_lib::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu_lib::Origin3d::ZERO,
                aspect: wgpu_lib::TextureAspect::All,
            },
            &data,
            wgpu_lib::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
            wgpu_lib::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu_lib::TextureViewDescriptor::default());
        let cached = Arc::new(CachedImage {
            _image: Arc::clone(image),
            _texture: texture,
            view,
            bytes: width as usize * height as usize * 4,
        });
        self.image_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key, Arc::clone(&cached));
        Some(cached)
    }

    /// Returns the supported capabilities.
    pub fn capabilities(&self) -> WgpuCapabilities {
        WgpuCapabilities {
            solid_paths: true,
            strokes: true,
            linear_gradients: true,
            clips: true,
            text_outlines: true,
            images: true,
            blend_modes: false,
        }
    }

    /// Returns the shared wgpu context.
    pub fn context(&self) -> &WgpuContext {
        &self.context
    }
}

impl std::fmt::Debug for WgpuBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WgpuBackend").finish_non_exhaustive()
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu_lib::BindGroupLayoutEntry {
    wgpu_lib::BindGroupLayoutEntry {
        binding,
        visibility: wgpu_lib::ShaderStages::COMPUTE,
        ty: wgpu_lib::BindingType::Buffer {
            ty: wgpu_lib::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// A clip path retained for per-fragment testing.
#[derive(Clone)]
struct ClipDraw {
    path: Arc<Path>,
    transform: Matrix,
    rule: FillRule,
}

fn collect_paths(
    prepared: &PreparedScene,
    dpi: f64,
    origin_px: (f64, f64),
    width: u32,
    height: u32,
) -> (Vec<[f32; 4]>, Vec<DrawPath>) {
    let scale = dpi / 25.4;
    let mut edges = Vec::new();
    let mut paths = Vec::new();
    let mut order = 0_u32;
    let viewport = Viewport {
        min_x: -1.0,
        min_y: -1.0,
        max_x: width as f32 + 1.0,
        max_y: height as f32 + 1.0,
    };
    let mut frames = vec![PathFrame {
        clip: None,
        opacity: 1.0,
    }];
    let mut skip_depth = 0_usize;
    for command in &prepared.commands {
        match command {
            PreparedCommand::PushLayer { clip, opacity } => {
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                let parent = frames.last().expect("root path frame");
                let combined_opacity = parent.opacity * opacity.clamp(0.0, 1.0);
                if combined_opacity <= 0.0 {
                    skip_depth = 1;
                    continue;
                }
                let mut frame = PathFrame {
                    clip: parent.clip,
                    opacity: combined_opacity,
                };
                if let Some(clip) = clip {
                    let start = u32::try_from(edges.len()).unwrap_or(u32::MAX);
                    flatten_path(
                        &clip.path,
                        clip.transform,
                        scale,
                        origin_px,
                        true,
                        &mut edges,
                    );
                    let count = u32::try_from(edges.len()).unwrap_or(u32::MAX) - start;
                    if count == 0
                        || !viewport
                            .intersects(edge_bounds(&edges[start as usize..]).unwrap_or([0.0; 4]))
                    {
                        skip_depth = 1;
                        continue;
                    }
                    frame.clip = Some((start, count, u32::from(clip.rule == FillRule::EvenOdd)));
                }
                frames.push(frame);
            }
            PreparedCommand::PopLayer => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                } else if frames.len() > 1 {
                    frames.pop();
                }
            }
            PreparedCommand::FillPath {
                path,
                transform,
                paint,
                rule,
            } => {
                if skip_depth > 0 {
                    order += 1;
                    continue;
                }
                let frame = frames.last().expect("root path frame");
                let start = u32::try_from(edges.len()).unwrap_or(u32::MAX);
                flatten_path(path, *transform, scale, origin_px, true, &mut edges);
                let count = u32::try_from(edges.len()).unwrap_or(u32::MAX) - start;
                if count == 0 {
                    order += 1;
                    continue;
                }
                let Some(bounds) = edge_bounds(&edges[start as usize..]) else {
                    order += 1;
                    continue;
                };
                if viewport.intersects(bounds) {
                    let (kind, color, end_color, gradient) = match paint {
                        Paint::Color(color) => (
                            0,
                            scaled_color(*color, frame.opacity),
                            scaled_color(*color, frame.opacity),
                            [0.0; 4],
                        ),
                        Paint::LinearGradient(linear) => {
                            let start_color =
                                linear.stops.first().map_or(Rgba::BLACK, |stop| stop.color);
                            let end_color =
                                linear.stops.last().map_or(start_color, |stop| stop.color);
                            let a = transform.apply(linear.start);
                            let b = transform.apply(linear.end);
                            (
                                2,
                                scaled_color(start_color, frame.opacity),
                                scaled_color(end_color, frame.opacity),
                                [
                                    (a.x * scale - origin_px.0) as f32,
                                    (a.y * scale - origin_px.1) as f32,
                                    (b.x * scale - origin_px.0) as f32,
                                    (b.y * scale - origin_px.1) as f32,
                                ],
                            )
                        }
                        Paint::RadialGradient(radial) => {
                            let color = radial.stops.first().map_or(Rgba::BLACK, |stop| stop.color);
                            (
                                0,
                                scaled_color(color, frame.opacity),
                                scaled_color(color, frame.opacity),
                                [0.0; 4],
                            )
                        }
                        _ => (0, [0.0; 4], [0.0; 4], [0.0; 4]),
                    };
                    paths.push(DrawPath {
                        start,
                        count,
                        rule: u32::from(*rule == FillRule::EvenOdd),
                        kind,
                        color,
                        width: 0.0,
                        clip_start: frame.clip.map_or(0, |value| value.0),
                        clip_count: frame.clip.map_or(0, |value| value.1),
                        clip_rule: frame.clip.map_or(0, |value| value.2),
                        end_color,
                        gradient,
                        bbox: bounds,
                        order,
                    });
                }
                order += 1;
            }
            PreparedCommand::StrokePath {
                path,
                transform,
                stroke,
            } => {
                if skip_depth > 0 {
                    order += 1;
                    continue;
                }
                let frame = frames.last().expect("root path frame");
                let start = u32::try_from(edges.len()).unwrap_or(u32::MAX);
                flatten_path(path, *transform, scale, origin_px, false, &mut edges);
                let count = u32::try_from(edges.len()).unwrap_or(u32::MAX) - start;
                if count == 0 {
                    order += 1;
                    continue;
                }
                let Some(bounds) = edge_bounds(&edges[start as usize..]) else {
                    order += 1;
                    continue;
                };
                if let Paint::Color(color) = &stroke.paint {
                    let width = (stroke.style.width * scale) as f32;
                    let stroke_bounds = expand_bounds(bounds, width * 0.5);
                    if viewport.intersects(stroke_bounds) {
                        paths.push(DrawPath {
                            start,
                            count,
                            rule: 0,
                            kind: 1,
                            color: scaled_color(*color, frame.opacity),
                            width,
                            clip_start: frame.clip.map_or(0, |value| value.0),
                            clip_count: frame.clip.map_or(0, |value| value.1),
                            clip_rule: frame.clip.map_or(0, |value| value.2),
                            end_color: scaled_color(*color, frame.opacity),
                            gradient: [0.0; 4],
                            bbox: stroke_bounds,
                            order,
                        });
                    }
                }
                order += 1;
            }
            PreparedCommand::DrawText { .. } | PreparedCommand::DrawImage { .. } => {
                order += 1;
            }
        }
    }
    (edges, paths)
}

#[derive(Clone, Copy)]
struct PathFrame {
    clip: Option<(u32, u32, u32)>,
    opacity: f32,
}

/// One ordered text or image draw from the prepared scene.
enum Decoration {
    Text {
        text: Box<ofdbox_folio_ir::TextElement>,
        world: Matrix,
        opacity: f32,
        clip: Option<[f32; 4]>,
        clip_path: Option<ClipDraw>,
        _order: u32,
    },
    Image {
        draw: ImageDraw,
        clip: Option<[f32; 4]>,
        clip_path: Option<ClipDraw>,
        _order: u32,
    },
}

#[derive(Clone)]
struct DecorationFrame {
    clip: Option<[f32; 4]>,
    clip_path: Option<ClipDraw>,
    opacity: f32,
}

/// Collect text and image draws from a prepared scene.
///
/// Paths are still collected separately and drawn first; this ordering is used to
/// preserve the relative stacking of images and text.
#[allow(clippy::too_many_arguments)]
fn collect_decorations(
    prepared: &PreparedScene,
    dpi: f64,
    origin_px: (f64, f64),
    width: u32,
    height: u32,
) -> Vec<Decoration> {
    let viewport = Viewport {
        min_x: -1.0,
        min_y: -1.0,
        max_x: width as f32 + 1.0,
        max_y: height as f32 + 1.0,
    };
    let scale = dpi / 25.4;
    let mut decorations = Vec::new();
    let mut frames = vec![DecorationFrame {
        clip: None,
        clip_path: None,
        opacity: 1.0,
    }];
    let mut skip_depth = 0_usize;
    let mut order = 0_u32;
    for command in &prepared.commands {
        match command {
            PreparedCommand::PushLayer { clip, opacity } => {
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                let parent = frames.last().expect("root decoration frame");
                let combined_opacity = parent.opacity * opacity.clamp(0.0, 1.0);
                if combined_opacity <= 0.0 {
                    skip_depth = 1;
                    continue;
                }
                let mut frame = DecorationFrame {
                    clip: parent.clip,
                    clip_path: parent.clip_path.clone(),
                    opacity: combined_opacity,
                };
                if let Some(clip) = clip {
                    let Some((intersected, clip_path)) =
                        prepared_clip(clip, scale, origin_px, viewport, frame.clip)
                    else {
                        skip_depth = 1;
                        continue;
                    };
                    frame.clip = Some(intersected);
                    frame.clip_path = Some(clip_path);
                }
                frames.push(frame);
            }
            PreparedCommand::PopLayer => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                } else if frames.len() > 1 {
                    frames.pop();
                }
            }
            PreparedCommand::DrawText {
                font,
                font_size,
                transform,
                run,
                fill,
                stroke,
            } => {
                if skip_depth > 0 {
                    order += 1;
                    continue;
                }
                let frame = frames.last().expect("root decoration frame");
                let element = ofdbox_folio_ir::TextElement {
                    font: Arc::clone(font),
                    font_size: *font_size,
                    transform: *transform,
                    run: Arc::clone(run),
                    fill: fill.clone(),
                    stroke: stroke.clone(),
                };
                decorations.push(Decoration::Text {
                    text: Box::new(element),
                    world: *transform,
                    opacity: frame.opacity,
                    clip: frame.clip,
                    clip_path: frame.clip_path.clone(),
                    _order: order,
                });
                order += 1;
            }
            PreparedCommand::DrawImage {
                image,
                rect,
                transform,
                alpha,
            } => {
                if skip_depth > 0 {
                    order += 1;
                    continue;
                }
                let frame = frames.last().expect("root decoration frame");
                let element = ofdbox_folio_ir::ImageElement {
                    image: Arc::clone(image),
                    rect: *rect,
                    transform: *transform,
                    alpha: *alpha,
                };
                if let Some(draw) = image_draw(
                    &element,
                    Matrix::IDENTITY,
                    frame.opacity,
                    frame.clip,
                    scale,
                    origin_px,
                    viewport,
                ) {
                    decorations.push(Decoration::Image {
                        draw,
                        clip: frame.clip,
                        clip_path: frame.clip_path.clone(),
                        _order: order,
                    });
                }
                order += 1;
            }
            PreparedCommand::FillPath { .. } | PreparedCommand::StrokePath { .. } => {
                if skip_depth == 0 {
                    order += 1;
                }
            }
        }
    }
    decorations
}

fn prepared_clip(
    clip: &PreparedClip,
    scale: f64,
    origin_px: (f64, f64),
    viewport: Viewport,
    parent: Option<[f32; 4]>,
) -> Option<([f32; 4], ClipDraw)> {
    let mut edges = Vec::new();
    flatten_path(
        &clip.path,
        clip.transform,
        scale,
        origin_px,
        true,
        &mut edges,
    );
    let bounds = edge_bounds(&edges)?;
    let intersected = intersect_bounds(parent.unwrap_or(bounds), bounds)?;
    if !viewport.intersects(intersected) {
        return None;
    }
    Some((
        intersected,
        ClipDraw {
            path: Arc::clone(&clip.path),
            transform: clip.transform,
            rule: clip.rule,
        },
    ))
}

/// Emit atlas-backed glyph quads for one text element.
#[allow(clippy::too_many_arguments)]
fn emit_text_instances(
    text: &ofdbox_folio_ir::TextElement,
    world: Matrix,
    opacity: f32,
    clip: Option<[f32; 4]>,
    scale: f64,
    origin_px: (f64, f64),
    viewport: Viewport,
    glyph_cache: &Mutex<HashMap<(usize, u16), CachedGlyph>>,
    font_faces: &Mutex<HashMap<(usize, u32), Arc<CachedFace>>>,
    atlas: &mut GpuAtlas,
    queue: &wgpu_lib::Queue,
    instances: &mut Vec<GlyphInstance>,
) {
    let (fill_color, end_fill_color, gradient, gradient_mode) = match text.fill.as_ref() {
        Some(Paint::Color(color)) => (*color, *color, [0.0; 4], 0.0),
        Some(Paint::LinearGradient(gradient)) => {
            let start = gradient
                .stops
                .first()
                .map_or(Rgba::BLACK, |stop| stop.color);
            let end = gradient.stops.last().map_or(start, |stop| stop.color);
            let a = world.apply(gradient.start);
            let b = world.apply(gradient.end);
            (
                start,
                end,
                [
                    (a.x * scale - origin_px.0) as f32,
                    (a.y * scale - origin_px.1) as f32,
                    (b.x * scale - origin_px.0) as f32,
                    (b.y * scale - origin_px.1) as f32,
                ],
                1.0,
            )
        }
        Some(Paint::RadialGradient(gradient)) => {
            let start = gradient
                .stops
                .first()
                .map_or(Rgba::BLACK, |stop| stop.color);
            let end = gradient.stops.last().map_or(start, |stop| stop.color);
            let center = world.apply(gradient.start_center);
            (
                start,
                end,
                [
                    (center.x * scale - origin_px.0) as f32,
                    (center.y * scale - origin_px.1) as f32,
                    (gradient.start_radius * scale) as f32,
                    0.0,
                ],
                2.0,
            )
        }
        Some(_) | None => return,
    };
    let face_key = (Arc::as_ptr(&text.font) as usize, text.font.face_index());
    let face = {
        let faces = font_faces.lock().unwrap_or_else(|error| error.into_inner());
        match faces.get(&face_key) {
            Some(face) => Arc::clone(face),
            None => {
                drop(faces);
                let Ok(parsed) =
                    OwnedFace::from_vec(text.font.data().to_vec(), text.font.face_index())
                else {
                    return;
                };
                let cached = Arc::new(CachedFace {
                    _font: Arc::clone(&text.font),
                    face: parsed,
                });
                font_faces
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(face_key, Arc::clone(&cached));
                cached
            }
        }
    };
    let face = face.face.as_face_ref();
    let units = face.units_per_em();
    if units == 0 {
        return;
    }
    let determinant = (world.a * world.d - world.b * world.c).abs();
    if !(determinant.is_finite() && determinant > 1.0e-12) {
        return;
    }
    // Rasterize at the size the glyph occupies on the page, including any scale
    // in the text transform. Rotations/shears are applied by the quad axes below.
    let em_px = text.font_size * scale * determinant.sqrt();
    if !(em_px.is_finite() && em_px > 0.5) {
        return;
    }
    let pixel_size = em_px.round().clamp(1.0, 512.0) as u32;
    let factor = scale * text.font_size / f64::from(pixel_size);
    let x_axis = (world.a * factor, world.b * factor);
    let y_axis = (world.c * factor, world.d * factor);
    let color = scaled_color(fill_color, opacity);
    let end_color = scaled_color(end_fill_color, opacity);
    let gradient_ellipse = if let Some(Paint::RadialGradient(gradient)) = text.fill.as_ref() {
        if gradient.eccentricity > 1e-6 {
            let ratio = (1.0 - gradient.eccentricity.clamp(0.0, 0.999).powi(2))
                .max(1e-4)
                .sqrt();
            let (sin, cos) = gradient.angle.to_radians().sin_cos();
            [
                cos as f32,
                sin as f32,
                (-sin * ratio) as f32,
                (cos * ratio) as f32,
            ]
        } else {
            [1.0, 0.0, 0.0, 1.0]
        }
    } else {
        [1.0, 0.0, 0.0, 1.0]
    };
    let gradient_radial_end = if let Some(Paint::RadialGradient(gradient)) = text.fill.as_ref() {
        let end = world.apply(gradient.end_center);
        [
            (end.x * scale - origin_px.0) as f32,
            (end.y * scale - origin_px.1) as f32,
            (gradient.end_radius * scale) as f32,
            0.0,
        ]
    } else {
        [0.0; 4]
    };
    let font_key = Arc::as_ptr(&text.font) as usize;
    for glyph in &text.run.glyphs {
        let Ok(glyph_id) = u16::try_from(glyph.id) else {
            continue;
        };
        let key = (font_key, glyph_id);
        let glyph_path = {
            let mut cache = glyph_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(glyph) = cache.get(&key) {
                Arc::clone(&glyph.path)
            } else {
                let mut builder = GlyphPathBuilder::default();
                if face
                    .outline_glyph(ttf_parser::GlyphId(glyph_id), &mut builder)
                    .is_none()
                    || builder.path.is_empty()
                {
                    continue;
                }
                let path = Arc::new(builder.path);
                cache.insert(
                    key,
                    CachedGlyph {
                        _font: Arc::clone(&text.font),
                        path: Arc::clone(&path),
                    },
                );
                path
            }
        };
        let (bold, bold_radius) = match text.stroke.as_ref() {
            Some(stroke) if matches!(&stroke.paint, Paint::Color(color) if *color == fill_color) => {
                let radius = stroke.style.width * scale * determinant.sqrt() * 0.5;
                (true, radius as f32)
            }
            _ => (false, 0.0),
        };
        let Some(packed) = atlas.glyph(
            queue,
            font_key,
            glyph_id,
            pixel_size,
            bold,
            bold_radius,
            &glyph_path,
            units,
        ) else {
            continue;
        };
        if packed.width <= 0.0 || packed.height <= 0.0 {
            continue;
        }
        let baseline = world.apply(Point::new(glyph.x, glyph.y));
        let origin_x = baseline.x * scale - origin_px.0
            + x_axis.0 * f64::from(packed.left)
            + y_axis.0 * f64::from(packed.top);
        let origin_y = baseline.y * scale - origin_px.1
            + x_axis.1 * f64::from(packed.left)
            + y_axis.1 * f64::from(packed.top);
        let x_advance = (
            x_axis.0 * f64::from(packed.width),
            x_axis.1 * f64::from(packed.width),
        );
        let y_advance = (
            y_axis.0 * f64::from(packed.height),
            y_axis.1 * f64::from(packed.height),
        );
        let corners = [
            (origin_x, origin_y),
            (origin_x + x_advance.0, origin_y + x_advance.1),
            (
                origin_x + x_advance.0 + y_advance.0,
                origin_y + x_advance.1 + y_advance.1,
            ),
            (origin_x + y_advance.0, origin_y + y_advance.1),
        ];
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for (x, y) in corners {
            min_x = min_x.min(x as f32);
            min_y = min_y.min(y as f32);
            max_x = max_x.max(x as f32);
            max_y = max_y.max(y as f32);
        }
        let bounds = [min_x, min_y, max_x, max_y];
        if !bounds.iter().all(|value| value.is_finite()) || !viewport.intersects(bounds) {
            continue;
        }
        if let Some(clip) = clip
            && intersect_bounds(bounds, clip).is_none()
        {
            continue;
        }
        let instance = GlyphInstance {
            origin: [origin_x as f32, origin_y as f32],
            x_axis: [x_advance.0 as f32, x_advance.1 as f32],
            y_axis: [y_advance.0 as f32, y_advance.1 as f32],
            uv0: [packed.u0, packed.v0],
            uv1: [packed.u1, packed.v1],
            _pad: [0.0; 2],
            color,
            gradient,
            end_color,
            mode: gradient_mode,
            _pad_a: 0.0,
            _pad_b: 0.0,
            _pad_c: 0.0,
            clip_start: 0,
            clip_count: 0,
            clip_rule: 0,
            _clip_pad: 0,
            ellipse: gradient_ellipse,
            radial_end: gradient_radial_end,
        };
        instances.push(instance);
    }
}

fn image_draw(
    element: &ImageElement,
    parent: Matrix,
    opacity: f32,
    clip: Option<[f32; 4]>,
    scale: f64,
    origin_px: (f64, f64),
    viewport: Viewport,
) -> Option<ImageDraw> {
    let width = element
        .rect
        .width
        .is_finite()
        .then_some(element.rect.width)
        .filter(|width| *width > 0.0)
        .unwrap_or(f64::from(element.image.pixel_width));
    let height = element
        .rect
        .height
        .is_finite()
        .then_some(element.rect.height)
        .filter(|height| *height > 0.0)
        .unwrap_or(f64::from(element.image.pixel_height));
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let world = element.transform.then(parent);
    let x_axis = (world.a * width * scale, world.b * width * scale);
    let y_axis = (world.c * height * scale, world.d * height * scale);
    let origin = (world.e * scale - origin_px.0, world.f * scale - origin_px.1);
    let corners = [
        (origin.0, origin.1),
        (origin.0 + x_axis.0, origin.1 + x_axis.1),
        (
            origin.0 + x_axis.0 + y_axis.0,
            origin.1 + x_axis.1 + y_axis.1,
        ),
        (origin.0 + y_axis.0, origin.1 + y_axis.1),
    ];
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in corners {
        min_x = min_x.min(x as f32);
        min_y = min_y.min(y as f32);
        max_x = max_x.max(x as f32);
        max_y = max_y.max(y as f32);
    }
    let bounds = [min_x, min_y, max_x, max_y];
    if !bounds.iter().all(|value| value.is_finite()) || !viewport.intersects(bounds) {
        return None;
    }
    if let Some(clip) = clip
        && intersect_bounds(bounds, clip).is_none()
    {
        return None;
    }
    Some(ImageDraw {
        origin: [origin.0 as f32, origin.1 as f32],
        x_axis: [x_axis.0 as f32, x_axis.1 as f32],
        y_axis: [y_axis.0 as f32, y_axis.1 as f32],
        alpha: element.alpha.clamp(0.0, 1.0) * opacity,
        image: Arc::clone(&element.image),
        clip_start: 0,
        clip_count: 0,
        clip_rule: 0,
    })
}

fn intersect_bounds(a: [f32; 4], b: [f32; 4]) -> Option<[f32; 4]> {
    let bounds = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    (bounds[0] < bounds[2] && bounds[1] < bounds[3]).then_some(bounds)
}

/// Visible pixel rectangle with a one-pixel bleed for antialiasing.
#[derive(Clone, Copy)]
struct Viewport {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

impl Viewport {
    fn intersects(&self, bbox: [f32; 4]) -> bool {
        bbox[0] < self.max_x && bbox[2] > self.min_x && bbox[1] < self.max_y && bbox[3] > self.min_y
    }
}

fn edge_bounds(edges: &[[f32; 4]]) -> Option<[f32; 4]> {
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for edge in edges {
        for x in [edge[0], edge[2]] {
            bounds[0] = bounds[0].min(x);
            bounds[2] = bounds[2].max(x);
        }
        for y in [edge[1], edge[3]] {
            bounds[1] = bounds[1].min(y);
            bounds[3] = bounds[3].max(y);
        }
    }
    bounds[0].is_finite().then_some(bounds)
}

fn expand_bounds(bounds: [f32; 4], padding: f32) -> [f32; 4] {
    [
        bounds[0] - padding,
        bounds[1] - padding,
        bounds[2] + padding,
        bounds[3] + padding,
    ]
}

fn color_float(color: Rgba) -> [f32; 4] {
    [
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        f32::from(color.a) / 255.0,
    ]
}

fn scaled_color(color: Rgba, opacity: f32) -> [f32; 4] {
    let mut value = color_float(color);
    value[3] *= opacity.clamp(0.0, 1.0);
    value
}

pub(crate) fn flatten_path(
    path: &Path,
    transform: Matrix,
    scale: f64,
    origin_px: (f64, f64),
    close: bool,
    edges: &mut Vec<[f32; 4]>,
) {
    let mut current = Point::default();
    let mut start = Point::default();
    let push = |edges: &mut Vec<[f32; 4]>, from: Point, to: Point| {
        let from = transform.apply(from);
        let to = transform.apply(to);
        edges.push([
            (from.x * scale - origin_px.0) as f32,
            (from.y * scale - origin_px.1) as f32,
            (to.x * scale - origin_px.0) as f32,
            (to.y * scale - origin_px.1) as f32,
        ]);
    };
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(point) => {
                if close && current != start {
                    push(edges, current, start);
                }
                current = *point;
                start = *point;
            }
            PathSegment::LineTo(point) => {
                push(edges, current, *point);
                current = *point;
            }
            PathSegment::QuadTo(control, to) => {
                let from = current;
                for index in 1..=8 {
                    let t = f64::from(index) / 8.0;
                    let point = quadratic(from, *control, *to, t);
                    push(edges, current, point);
                    current = point;
                }
            }
            PathSegment::CubicTo(control1, control2, to) => {
                let from = current;
                for index in 1..=16 {
                    let t = f64::from(index) / 16.0;
                    let point = cubic(from, *control1, *control2, *to, t);
                    push(edges, current, point);
                    current = point;
                }
            }
            PathSegment::ArcTo { to, .. } => {
                push(edges, current, *to);
                current = *to;
            }
            PathSegment::Close => {
                push(edges, current, start);
                current = start;
            }
            _ => {}
        }
    }
    if close && current != start {
        push(edges, current, start);
    }
}

fn quadratic(from: Point, control: Point, to: Point, t: f64) -> Point {
    let mt = 1.0 - t;
    Point::new(
        mt * mt * from.x + 2.0 * mt * t * control.x + t * t * to.x,
        mt * mt * from.y + 2.0 * mt * t * control.y + t * t * to.y,
    )
}

fn cubic(from: Point, c1: Point, c2: Point, to: Point, t: f64) -> Point {
    let mt = 1.0 - t;
    Point::new(
        mt * mt * mt * from.x
            + 3.0 * mt * mt * t * c1.x
            + 3.0 * mt * t * t * c2.x
            + t * t * t * to.x,
        mt * mt * mt * from.y
            + 3.0 * mt * mt * t * c1.y
            + 3.0 * mt * t * t * c2.y
            + t * t * t * to.y,
    )
}

#[derive(Default)]
struct GlyphPathBuilder {
    path: Path,
}

impl ttf_parser::OutlineBuilder for GlyphPathBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to(Point::new(f64::from(x), f64::from(y)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to(Point::new(f64::from(x), f64::from(y)));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.path.quadratic_to(
            Point::new(f64::from(x1), f64::from(y1)),
            Point::new(f64::from(x), f64::from(y)),
        );
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.path.cubic_to(
            Point::new(f64::from(x1), f64::from(y1)),
            Point::new(f64::from(x2), f64::from(y2)),
            Point::new(f64::from(x), f64::from(y)),
        );
    }
    fn close(&mut self) {
        self.path.close();
    }
}

const SHADER: &str = r#"
struct Edge { a: vec2f, b: vec2f };
struct PathInfo { start: u32, count: u32, rule: u32, kind: u32, color: vec4f, width: f32, clip_start: u32, clip_count: u32, clip_rule: u32, end_color: vec4f, gradient: vec4f, bbox: vec4f };
struct Params { width: u32, height: u32, count: u32, scale: f32 };

@group(0) @binding(0) var<storage, read> edges: array<Edge>;
@group(0) @binding(1) var<storage, read> paths: array<PathInfo>;
@group(0) @binding(2) var<storage, read> params: Params;
@group(0) @binding(3) var output: texture_storage_2d<rgba8unorm, write>;

fn inside_range(p: vec2f, start: u32, count: u32, rule: u32) -> bool {
    var winding: i32 = 0;
    for (var ei = 0u; ei < count; ei = ei + 1u) {
        let e = edges[start + ei];
        let ay = e.a.y;
        let by = e.b.y;
        if ((ay <= p.y && by > p.y) || (by <= p.y && ay > p.y)) {
            let t = (p.y - ay) / (by - ay);
            let x = e.a.x + t * (e.b.x - e.a.x);
            if (x > p.x) {
                if (by > ay) { winding = winding + 1; } else { winding = winding - 1; }
            }
        }
    }
    if (rule != 0u) { return (winding & 1) != 0; }
    return winding != 0;
}

fn paint_color(info: PathInfo, p: vec2f) -> vec4f {
    if (info.kind == 2u) {
        let axis = info.gradient.zw - info.gradient.xy;
        let t = clamp(dot(p - info.gradient.xy, axis) / max(dot(axis, axis), 0.0001), 0.0, 1.0);
        return mix(info.color, info.end_color, t);
    }
    return info.color;
}

fn inside_fill(p: vec2f, info: PathInfo) -> bool {
    return inside_range(p, info.start, info.count, info.rule);
}

fn inside_clip(p: vec2f, info: PathInfo) -> bool {
    if (info.clip_count == 0u) { return true; }
    return inside_range(p, info.clip_start, info.clip_count, info.clip_rule);
}

fn stroke_coverage(p: vec2f, info: PathInfo) -> f32 {
    var coverage = 0.0;
    let half_width = max(info.width * 0.5, 0.5);
    for (var ei = 0u; ei < info.count; ei = ei + 1u) {
        let e = edges[info.start + ei];
        let ab = e.b - e.a;
        let length_sq = max(dot(ab, ab), 0.0001);
        let t = clamp(dot(p - e.a, ab) / length_sq, 0.0, 1.0);
        let nearest = e.a + ab * t;
        let distance = length(p - nearest);
        coverage = max(coverage, clamp(half_width + 0.75 - distance, 0.0, 1.0));
    }
    return coverage;
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    if (id.x >= params.width || id.y >= params.height) { return; }
    let layer = params.scale > 0.5;
    var result = select(vec4f(1.0, 1.0, 1.0, 1.0), vec4f(0.0), layer);
    for (var pi = 0u; pi < params.count; pi = pi + 1u) {
        let info = paths[pi];
        let pixel = vec2f(f32(id.x) + 0.5, f32(id.y) + 0.5);
        if (pixel.x < info.bbox.x - 1.0 || pixel.x > info.bbox.z + 1.0 || pixel.y < info.bbox.y - 1.0 || pixel.y > info.bbox.w + 1.0) {
            continue;
        }
        var coverage = 0.0;
        if (info.kind == 0u) {
            let offsets = array<vec2f, 4>(vec2f(0.25, 0.25), vec2f(0.75, 0.25), vec2f(0.25, 0.75), vec2f(0.75, 0.75));
            for (var si = 0u; si < 4u; si = si + 1u) {
                let p = vec2f(f32(id.x), f32(id.y)) + offsets[si];
                if (inside_fill(p, info) && inside_clip(p, info)) { coverage = coverage + 0.25; }
            }
        } else {
            let center = vec2f(f32(id.x) + 0.5, f32(id.y) + 0.5);
            if (inside_clip(center, info)) {
                coverage = stroke_coverage(center, info);
            }
        }
        if (coverage > 0.0) {
            let color = paint_color(info, vec2f(f32(id.x) + 0.5, f32(id.y) + 0.5));
            let a = color.a * coverage;
            if (layer) {
                result = vec4f(color.rgb * a, a) + result * (1.0 - a);
            } else {
                result = vec4f(color.rgb * a + result.rgb * (1.0 - a), 1.0);
            }
        }
    }
    if (layer && result.a > 0.0) {
        result = vec4f(result.rgb / result.a, result.a);
    }
    textureStore(output, vec2i(i32(id.x), i32(id.y)), result);
}
"#;

#[cfg(test)]
mod tests {
    use super::{WgpuBackend, WgpuContext, WgpuTarget, collect_paths};
    use crate::prepared::PreparedScene;
    use ofdbox_folio_ir::{
        Element, FillRule, Group, Matrix, Paint, Path, PathElement, Point, Rgba, Scene, Size,
    };

    fn rectangle(x: f64, y: f64) -> PathElement {
        let mut path = Path::new();
        path.move_to(Point::new(x, y));
        path.line_to(Point::new(x + 10.0, y));
        path.line_to(Point::new(x + 10.0, y + 10.0));
        path.line_to(Point::new(x, y + 10.0));
        path.close();
        PathElement {
            path: std::sync::Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: Some(Paint::Color(Rgba::from_rgb(255, 0, 0))),
            stroke: None,
            rule: FillRule::NonZero,
        }
    }

    #[test]
    fn culls_paths_outside_the_tile() {
        let scene = Scene {
            size: Size::new(120.0, 40.0),
            root: Group {
                children: vec![
                    Element::Path(rectangle(0.0, 0.0)),
                    Element::Path(rectangle(100.0, 0.0)),
                ],
                ..Group::default()
            },
            diagnostics: Vec::new(),
        };
        let prepared = PreparedScene::from_scene(&scene);
        let (_, paths) = collect_paths(&prepared, 96.0, (0.0, 0.0), 100, 100);
        assert_eq!(paths.len(), 1, "the right-hand path is outside the tile");
    }

    #[test]
    fn renders_text_from_atlas() {
        let Ok(font_data) = std::fs::read("C:/Windows/Fonts/arial.ttf") else {
            return;
        };
        let Ok(context) = WgpuContext::new_blocking() else {
            return;
        };
        let font = std::sync::Arc::new(ofdbox_folio_ir::FontAsset::new(
            std::sync::Arc::from(font_data.into_boxed_slice()),
            0,
        ));
        let Ok(face) = ttf_parser::Face::parse(font.data(), 0) else {
            return;
        };
        let Some(glyph_id) = face.glyph_index('A').map(|glyph| glyph.0) else {
            return;
        };
        let run = std::sync::Arc::new(ofdbox_folio_ir::GlyphRun::new(
            "A",
            vec![ofdbox_folio_ir::Glyph {
                id: u32::from(glyph_id),
                x: 5.0,
                y: 20.0,
                advance: 7.0,
            }],
            Vec::new(),
        ));
        let mut root = Group::default();
        root.children.push(Element::Path(rectangle(0.0, 0.0)));
        root.children
            .push(Element::Text(ofdbox_folio_ir::TextElement {
                font,
                font_size: 10.0,
                transform: Matrix::IDENTITY,
                run,
                fill: Some(Paint::Color(Rgba::from_rgb(0, 0, 0))),
                stroke: None,
            }));
        let scene = Scene {
            size: Size::new(30.0, 30.0),
            root,
            diagnostics: Vec::new(),
        };
        let backend = WgpuBackend::new(context).expect("backend");
        let target = WgpuTarget::new(backend.context(), scene.size, 96.0).expect("target");
        backend.render_to_target(&scene, &target).expect("render");
        let pixels = target.read_rgba(backend.context()).expect("read");
        let mut dark = 0_u32;
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_y = 0_u32;
        for y in 0..target.height() {
            for x in 0..target.width() {
                let index = ((y * target.width() + x) * 4) as usize;
                if pixels[index] < 128 {
                    dark += 1;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        assert!(dark > 5, "expected atlas glyph pixels, got {dark}");
        // A 10 mm 'A' with its baseline at y = 20 mm occupies roughly
        // x = 5..12 mm and y = 12..20 mm at 96 dpi.
        let px_per_mm = 96.0 / 25.4;
        let x_min = (5.0 * px_per_mm) as u32;
        let y_min = (12.0 * px_per_mm) as u32;
        let y_max = (20.5 * px_per_mm) as u32;
        assert!(min_x >= x_min, "glyph starts too far left: {min_x}");
        assert!(min_y >= y_min, "glyph starts too high: {min_y}");
        assert!(max_y <= y_max, "glyph baseline too low: {max_y}");
    }

    #[test]
    fn renders_image_quad() {
        let Ok(context) = WgpuContext::new_blocking() else {
            return;
        };
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("png header");
            writer
                .write_image_data(&[
                    255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
                ])
                .expect("png data");
        }
        let image = std::sync::Arc::new(ofdbox_folio_ir::ImageAsset::new(
            ofdbox_folio_ir::ImageFormat::Png,
            std::sync::Arc::from(encoded.into_boxed_slice()),
            2,
            2,
        ));
        let mut root = Group::default();
        root.children
            .push(Element::Image(ofdbox_folio_ir::ImageElement {
                image,
                rect: ofdbox_folio_ir::Rect::new(0.0, 0.0, 10.0, 10.0),
                transform: Matrix::translate(5.0, 5.0),
                alpha: 1.0,
            }));
        let scene = Scene {
            size: Size::new(20.0, 20.0),
            root,
            diagnostics: Vec::new(),
        };
        let backend = WgpuBackend::new(context).expect("backend");
        let target = WgpuTarget::new(backend.context(), scene.size, 96.0).expect("target");
        backend.render_to_target(&scene, &target).expect("render");
        let pixels = target.read_rgba(backend.context()).expect("read");
        let x = (10.0_f64 / 25.4 * 96.0) as u32;
        let y = (10.0_f64 / 25.4 * 96.0) as u32;
        let index = ((y * target.width() + x) * 4) as usize;
        assert!(pixels[index] > 200, "red channel: {}", pixels[index]);
        assert!(pixels[index + 1] < 40);
    }

    fn text_element(
        font: &std::sync::Arc<ofdbox_folio_ir::FontAsset>,
        character: char,
        x: f64,
        y: f64,
    ) -> Element {
        let face = ttf_parser::Face::parse(font.data(), font.face_index()).expect("face");
        let glyph_id = face.glyph_index(character).expect("glyph").0;
        let run = std::sync::Arc::new(ofdbox_folio_ir::GlyphRun::new(
            character.to_string(),
            vec![ofdbox_folio_ir::Glyph {
                id: u32::from(glyph_id),
                x,
                y,
                advance: 7.0,
            }],
            Vec::new(),
        ));
        Element::Text(ofdbox_folio_ir::TextElement {
            font: std::sync::Arc::clone(font),
            font_size: 10.0,
            transform: Matrix::IDENTITY,
            run,
            fill: Some(Paint::Color(Rgba::from_rgb(0, 0, 0))),
            stroke: None,
        })
    }

    #[test]
    fn preserves_text_and_image_order() {
        let Ok(font_data) = std::fs::read("C:/Windows/Fonts/arial.ttf") else {
            return;
        };
        let Ok(context) = WgpuContext::new_blocking() else {
            return;
        };
        let font = std::sync::Arc::new(ofdbox_folio_ir::FontAsset::new(
            std::sync::Arc::from(font_data.into_boxed_slice()),
            0,
        ));
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("png header");
            writer
                .write_image_data(&[255, 0, 0, 255])
                .expect("png data");
        }
        let image = std::sync::Arc::new(ofdbox_folio_ir::ImageAsset::new(
            ofdbox_folio_ir::ImageFormat::Png,
            std::sync::Arc::from(encoded.into_boxed_slice()),
            1,
            1,
        ));
        let mut root = Group::default();
        root.children.push(text_element(&font, 'A', 5.0, 12.0));
        root.children
            .push(Element::Image(ofdbox_folio_ir::ImageElement {
                image,
                rect: ofdbox_folio_ir::Rect::new(0.0, 0.0, 30.0, 30.0),
                transform: Matrix::IDENTITY,
                alpha: 1.0,
            }));
        root.children.push(text_element(&font, 'B', 5.0, 25.0));
        let scene = Scene {
            size: Size::new(30.0, 30.0),
            root,
            diagnostics: Vec::new(),
        };
        let backend = WgpuBackend::new(context).expect("backend");
        let target = WgpuTarget::new(backend.context(), scene.size, 96.0).expect("target");
        backend.render_to_target(&scene, &target).expect("render");
        let pixels = target.read_rgba(backend.context()).expect("read");
        let mut dark_top = 0;
        let mut dark_bottom = 0;
        for y in 0..target.height() {
            for x in 0..target.width() {
                let index = ((y * target.width() + x) * 4) as usize;
                if pixels[index] < 64 {
                    if y < target.height() / 3 {
                        dark_top += 1;
                    } else if y > target.height() * 2 / 3 {
                        dark_bottom += 1;
                    }
                }
            }
        }
        assert_eq!(dark_top, 0, "the image must cover the first text run");
        assert!(dark_bottom > 5, "the second text run must be on top");
    }

    #[test]
    fn clips_text_to_group_bounds() {
        let Ok(font_data) = std::fs::read("C:/Windows/Fonts/arial.ttf") else {
            return;
        };
        let Ok(context) = WgpuContext::new_blocking() else {
            return;
        };
        let font = std::sync::Arc::new(ofdbox_folio_ir::FontAsset::new(
            std::sync::Arc::from(font_data.into_boxed_slice()),
            0,
        ));
        let mut clip_path = Path::new();
        clip_path.move_to(Point::new(0.0, 0.0));
        clip_path.line_to(Point::new(7.0, 0.0));
        clip_path.line_to(Point::new(7.0, 30.0));
        clip_path.line_to(Point::new(0.0, 30.0));
        clip_path.close();
        let mut group = Group {
            clip: Some(std::sync::Arc::new(ofdbox_folio_ir::Clip {
                path: std::sync::Arc::new(clip_path),
                rule: FillRule::NonZero,
            })),
            ..Group::default()
        };
        group.children.push(text_element(&font, 'A', 5.0, 20.0));
        let mut root = Group::default();
        root.children.push(Element::Group(group));
        let scene = Scene {
            size: Size::new(30.0, 30.0),
            root,
            diagnostics: Vec::new(),
        };
        let backend = WgpuBackend::new(context).expect("backend");
        let target = WgpuTarget::new(backend.context(), scene.size, 96.0).expect("target");
        backend.render_to_target(&scene, &target).expect("render");
        let pixels = target.read_rgba(backend.context()).expect("read");
        let clip_x = (7.0_f64 / 25.4 * 96.0).ceil() as u32 + 1;
        for y in 0..target.height() {
            for x in clip_x..target.width() {
                let index = ((y * target.width() + x) * 4) as usize;
                assert!(
                    pixels[index] > 200,
                    "pixel at ({x}, {y}) escaped the text clip"
                );
            }
        }
    }

    #[test]
    fn renders_text_in_region_at_offset() {
        let Ok(font_data) = std::fs::read("C:/Windows/Fonts/arial.ttf") else {
            return;
        };
        let Ok(context) = WgpuContext::new_blocking() else {
            return;
        };
        let font = std::sync::Arc::new(ofdbox_folio_ir::FontAsset::new(
            std::sync::Arc::from(font_data.into_boxed_slice()),
            0,
        ));
        let mut root = Group::default();
        root.children.push(text_element(&font, 'A', 5.0, 20.0));
        let scene = Scene {
            size: Size::new(30.0, 30.0),
            root,
            diagnostics: Vec::new(),
        };
        let backend = WgpuBackend::new(context).expect("backend");
        let target = WgpuTarget::from_pixels(backend.context(), 64, 64, 96.0).expect("target");
        backend
            .render_region(&scene, &target, (10.0, 30.0))
            .expect("render");
        let pixels = target.read_rgba(backend.context()).expect("read");
        let dark = pixels.chunks(4).filter(|pixel| pixel[0] < 128).count();
        assert!(dark > 5, "expected glyph pixels in the tile, got {dark}");
    }

    #[test]
    fn paths_follow_scene_order() {
        let Ok(font_data) = std::fs::read("C:/Windows/Fonts/arial.ttf") else {
            return;
        };
        let Ok(context) = WgpuContext::new_blocking() else {
            return;
        };
        let font = std::sync::Arc::new(ofdbox_folio_ir::FontAsset::new(
            std::sync::Arc::from(font_data.into_boxed_slice()),
            0,
        ));
        let mut rectangle = Path::new();
        rectangle.move_to(Point::new(0.0, 0.0));
        rectangle.line_to(Point::new(30.0, 0.0));
        rectangle.line_to(Point::new(30.0, 30.0));
        rectangle.line_to(Point::new(0.0, 30.0));
        rectangle.close();
        let red = Element::Path(PathElement {
            path: std::sync::Arc::new(rectangle),
            transform: Matrix::IDENTITY,
            fill: Some(Paint::Color(Rgba::from_rgb(255, 0, 0))),
            stroke: None,
            rule: FillRule::NonZero,
        });
        let mut covered = Group::default();
        covered.children.push(text_element(&font, 'A', 5.0, 20.0));
        covered.children.push(red.clone());
        let mut covered_root = Group::default();
        covered_root.children.push(Element::Group(covered));
        let covered_scene = Scene {
            size: Size::new(30.0, 30.0),
            root: covered_root,
            diagnostics: Vec::new(),
        };
        let backend = WgpuBackend::new(context).expect("backend");
        let target = WgpuTarget::new(backend.context(), covered_scene.size, 96.0).expect("target");
        backend
            .render_to_target(&covered_scene, &target)
            .expect("render");
        let pixels = target.read_rgba(backend.context()).expect("read");
        let dark = pixels.chunks(4).filter(|pixel| pixel[0] < 64).count();
        assert_eq!(dark, 0, "the later path must cover the text");

        let mut on_top = Group::default();
        on_top.children.push(red);
        on_top.children.push(text_element(&font, 'A', 5.0, 20.0));
        let mut on_top_root = Group::default();
        on_top_root.children.push(Element::Group(on_top));
        let on_top_scene = Scene {
            size: Size::new(30.0, 30.0),
            root: on_top_root,
            diagnostics: Vec::new(),
        };
        backend
            .render_to_target(&on_top_scene, &target)
            .expect("render");
        let pixels = target.read_rgba(backend.context()).expect("read");
        let dark = pixels.chunks(4).filter(|pixel| pixel[0] < 64).count();
        assert!(dark > 5, "the later text must be visible over the path");
    }

    #[test]
    fn renders_gradient_text() {
        let Ok(font_data) = std::fs::read("C:/Windows/Fonts/arial.ttf") else {
            return;
        };
        let Ok(context) = WgpuContext::new_blocking() else {
            return;
        };
        let font = std::sync::Arc::new(ofdbox_folio_ir::FontAsset::new(
            std::sync::Arc::from(font_data.into_boxed_slice()),
            0,
        ));
        let mut element = text_element(&font, 'A', 5.0, 20.0);
        if let Element::Text(text) = &mut element {
            text.fill = Some(Paint::LinearGradient(ofdbox_folio_ir::LinearGradient {
                start: Point::new(5.0, 20.0),
                end: Point::new(12.0, 20.0),
                stops: vec![
                    ofdbox_folio_ir::GradientStop {
                        offset: 0.0,
                        color: Rgba::from_rgb(255, 0, 0),
                    },
                    ofdbox_folio_ir::GradientStop {
                        offset: 1.0,
                        color: Rgba::from_rgb(0, 0, 255),
                    },
                ],
                extend: Default::default(),
            }));
        }
        let mut root = Group::default();
        root.children.push(element);
        let scene = Scene {
            size: Size::new(30.0, 30.0),
            root,
            diagnostics: Vec::new(),
        };
        let backend = WgpuBackend::new(context).expect("backend");
        let target = WgpuTarget::new(backend.context(), scene.size, 96.0).expect("target");
        backend.render_to_target(&scene, &target).expect("render");
        let pixels = target.read_rgba(backend.context()).expect("read");
        let mut red = 0;
        let mut blue = 0;
        for pixel in pixels.chunks(4) {
            if pixel[0] > 160 && pixel[2] < 100 {
                red += 1;
            }
            if pixel[2] > 160 && pixel[0] < 100 {
                blue += 1;
            }
        }
        assert!(red > 5, "expected red gradient pixels, got {red}");
        assert!(blue > 5, "expected blue gradient pixels, got {blue}");
    }

    #[test]
    fn renders_radial_gradient_text() {
        let Ok(font_data) = std::fs::read("C:/Windows/Fonts/arial.ttf") else {
            return;
        };
        let Ok(context) = WgpuContext::new_blocking() else {
            return;
        };
        let font = std::sync::Arc::new(ofdbox_folio_ir::FontAsset::new(
            std::sync::Arc::from(font_data.into_boxed_slice()),
            0,
        ));
        let mut element = text_element(&font, 'A', 5.0, 20.0);
        if let Element::Text(text) = &mut element {
            text.fill = Some(Paint::RadialGradient(ofdbox_folio_ir::RadialGradient {
                start_center: Point::new(8.5, 16.0),
                start_radius: 2.5,
                end_center: Point::new(9.5, 17.0),
                end_radius: 3.5,
                stops: vec![
                    ofdbox_folio_ir::GradientStop {
                        offset: 0.0,
                        color: Rgba::from_rgb(255, 0, 0),
                    },
                    ofdbox_folio_ir::GradientStop {
                        offset: 1.0,
                        color: Rgba::from_rgb(0, 0, 255),
                    },
                ],
                extend: Default::default(),
                eccentricity: 0.8,
                angle: 30.0,
            }));
        }
        let mut root = Group::default();
        root.children.push(element);
        let scene = Scene {
            size: Size::new(30.0, 30.0),
            root,
            diagnostics: Vec::new(),
        };
        let backend = WgpuBackend::new(context).expect("backend");
        let target = WgpuTarget::new(backend.context(), scene.size, 96.0).expect("target");
        backend.render_to_target(&scene, &target).expect("render");
        let pixels = target.read_rgba(backend.context()).expect("read");
        let mut red = 0;
        let mut blue = 0;
        for pixel in pixels.chunks(4) {
            if pixel[0] > 160 && pixel[2] < 100 {
                red += 1;
            }
            if pixel[2] > 160 && pixel[0] < 100 {
                blue += 1;
            }
        }
        assert!(red > 5, "expected radial center pixels, got {red}");
        assert!(blue > 5, "expected radial edge pixels, got {blue}");
    }

    #[test]
    fn clips_image_to_group_bounds() {
        let Ok(context) = WgpuContext::new_blocking() else {
            return;
        };
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("png header");
            writer
                .write_image_data(&[255, 0, 0, 255])
                .expect("png data");
        }
        let image = std::sync::Arc::new(ofdbox_folio_ir::ImageAsset::new(
            ofdbox_folio_ir::ImageFormat::Png,
            std::sync::Arc::from(encoded.into_boxed_slice()),
            1,
            1,
        ));
        let mut clip_path = Path::new();
        clip_path.move_to(Point::new(0.0, 0.0));
        clip_path.line_to(Point::new(7.0, 0.0));
        clip_path.line_to(Point::new(0.0, 30.0));
        clip_path.close();
        let mut group = Group {
            clip: Some(std::sync::Arc::new(ofdbox_folio_ir::Clip {
                path: std::sync::Arc::new(clip_path),
                rule: FillRule::NonZero,
            })),
            ..Group::default()
        };
        group
            .children
            .push(Element::Image(ofdbox_folio_ir::ImageElement {
                image,
                rect: ofdbox_folio_ir::Rect::new(0.0, 0.0, 30.0, 30.0),
                transform: Matrix::IDENTITY,
                alpha: 1.0,
            }));
        let mut root = Group::default();
        root.children.push(Element::Group(group));
        let scene = Scene {
            size: Size::new(30.0, 30.0),
            root,
            diagnostics: Vec::new(),
        };
        let backend = WgpuBackend::new(context).expect("backend");
        let target = WgpuTarget::new(backend.context(), scene.size, 96.0).expect("target");
        backend.render_to_target(&scene, &target).expect("render");
        let pixels = target.read_rgba(backend.context()).expect("read");
        let inside_box = (
            (25.0_f64 / 25.4 * 96.0) as u32,
            (5.0_f64 / 25.4 * 96.0) as u32,
        );
        let index = ((inside_box.1 * target.width() + inside_box.0) * 4) as usize;
        assert!(
            pixels[index] > 200,
            "a point inside the clip bounds but outside the triangle must be clipped"
        );
        let clip_x = (7.0_f64 / 25.4 * 96.0).ceil() as u32 + 1;
        for y in 0..target.height() {
            for x in clip_x..target.width() {
                let index = ((y * target.width() + x) * 4) as usize;
                assert!(
                    pixels[index] > 200,
                    "pixel at ({x}, {y}) escaped the image clip"
                );
            }
        }
    }

    #[test]
    fn synthetic_bold_thickens_glyphs() {
        let Ok(font_data) = std::fs::read("C:/Windows/Fonts/arial.ttf") else {
            return;
        };
        let Ok(context) = WgpuContext::new_blocking() else {
            return;
        };
        let font = std::sync::Arc::new(ofdbox_folio_ir::FontAsset::new(
            std::sync::Arc::from(font_data.into_boxed_slice()),
            0,
        ));
        let backend = WgpuBackend::new(context).expect("backend");
        let count = |element: Element| {
            let mut root = Group::default();
            root.children.push(element);
            let scene = Scene {
                size: Size::new(30.0, 30.0),
                root,
                diagnostics: Vec::new(),
            };
            let target = WgpuTarget::new(backend.context(), scene.size, 96.0).expect("target");
            backend.render_to_target(&scene, &target).expect("render");
            target
                .read_rgba(backend.context())
                .expect("read")
                .chunks(4)
                .filter(|pixel| pixel[0] < 128)
                .count()
        };
        let regular = count(text_element(&font, 'A', 5.0, 20.0));
        let mut bold = text_element(&font, 'A', 5.0, 20.0);
        if let Element::Text(text) = &mut bold {
            text.stroke = Some(ofdbox_folio_ir::StrokePaint {
                paint: Paint::Color(Rgba::from_rgb(0, 0, 0)),
                style: ofdbox_folio_ir::StrokeStyle {
                    width: 0.4,
                    ..Default::default()
                },
            });
        }
        let thicker = count(bold);
        assert!(
            thicker > regular,
            "synthetic bold must cover more pixels: {thicker} vs {regular}"
        );
    }

    #[test]
    fn renders_linear_gradient_path() {
        let Ok(context) = WgpuContext::new_blocking() else {
            return;
        };
        let mut path = Path::new();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(20.0, 0.0));
        path.line_to(Point::new(20.0, 20.0));
        path.line_to(Point::new(0.0, 20.0));
        path.close();
        let mut root = Group::default();
        root.children.push(Element::Path(PathElement {
            path: std::sync::Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: Some(Paint::LinearGradient(ofdbox_folio_ir::LinearGradient {
                start: Point::new(0.0, 0.0),
                end: Point::new(20.0, 0.0),
                stops: vec![
                    ofdbox_folio_ir::GradientStop {
                        offset: 0.0,
                        color: Rgba::from_rgb(255, 0, 0),
                    },
                    ofdbox_folio_ir::GradientStop {
                        offset: 1.0,
                        color: Rgba::from_rgb(0, 0, 255),
                    },
                ],
                extend: Default::default(),
            })),
            stroke: None,
            rule: FillRule::NonZero,
        }));
        let scene = Scene {
            size: Size::new(20.0, 20.0),
            root,
            diagnostics: Vec::new(),
        };
        let backend = WgpuBackend::new(context).expect("backend");
        let target = WgpuTarget::new(backend.context(), scene.size, 96.0).expect("target");
        backend.render_to_target(&scene, &target).expect("render");
        let pixels = target.read_rgba(backend.context()).expect("read");
        let red = (
            (5.0_f64 / 25.4 * 96.0) as u32,
            (10.0_f64 / 25.4 * 96.0) as u32,
        );
        let blue = (
            (15.0_f64 / 25.4 * 96.0) as u32,
            (10.0_f64 / 25.4 * 96.0) as u32,
        );
        let red_index = ((red.1 * target.width() + red.0) * 4) as usize;
        let blue_index = ((blue.1 * target.width() + blue.0) * 4) as usize;
        assert!(pixels[red_index] > 180, "red gradient side missing");
        assert!(pixels[blue_index + 2] > 180, "blue gradient side missing");
    }

    #[test]
    fn renders_solid_rectangle() {
        let Ok(context) = WgpuContext::new_blocking() else {
            return;
        };
        let mut path = Path::new();
        path.move_to(Point::new(10.0, 10.0));
        path.line_to(Point::new(50.0, 10.0));
        path.line_to(Point::new(50.0, 50.0));
        path.line_to(Point::new(10.0, 50.0));
        path.close();
        let mut root = Group::default();
        root.children.push(Element::Path(PathElement {
            path: std::sync::Arc::new(path),
            transform: Matrix::IDENTITY,
            fill: Some(Paint::Color(Rgba::from_rgb(255, 0, 0))),
            stroke: None,
            rule: FillRule::NonZero,
        }));
        let scene = Scene {
            size: Size::new(60.0, 60.0),
            root,
            diagnostics: Vec::new(),
        };
        let backend = WgpuBackend::new(context).expect("backend");
        let target = WgpuTarget::new(backend.context(), scene.size, 96.0).expect("target");
        backend.render_to_target(&scene, &target).expect("render");
        let pixels = target.read_rgba(backend.context()).expect("read");
        let x = (30.0 / 25.4 * 96.0) as u32;
        let y = (30.0 / 25.4 * 96.0) as u32;
        let index = ((y * target.width() + x) * 4) as usize;
        assert!(pixels[index] > 200);
        assert!(pixels[index + 1] < 30);
    }
}
