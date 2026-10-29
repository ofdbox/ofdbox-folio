//! wgpu compute backend; W0 supports solid-color fill paths.

use std::borrow::Cow;
use std::sync::Arc;

use wgpu::util::DeviceExt;

use ofdbox_folio_ir::{
    Element, FillRule, Group, Matrix, Paint, Path, PathSegment, Point, Rgba, Scene, Size,
};

use crate::RenderError;

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
}

/// A wgpu device context.
pub struct WgpuContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl WgpuContext {
    /// Creates the default adapter and device.
    pub async fn new() -> Result<Self, RenderError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .map_err(|error| RenderError::Backend(format!("wgpu adapter: {error}")))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
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
    pub fn from_device_queue(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self { device, queue }
    }

    /// The device reference.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The queue reference.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
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
    /// Text outlines.
    pub text_outlines: bool,
    /// Images.
    pub images: bool,
    /// Blend modes.
    pub blend_modes: bool,
}

/// A wgpu render target.
pub struct WgpuTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
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
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ofdbox-wgpu-tile"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Self {
            texture,
            view,
            width,
            height,
            dpi,
        })
    }

    /// Creates an offscreen target from the Scene size.
    pub fn new(context: &WgpuContext, size: Size, dpi: f64) -> Result<Self, RenderError> {
        let width = (size.width / 25.4 * dpi).ceil().max(1.0) as u32;
        let height = (size.height / 25.4 * dpi).ceil().max(1.0) as u32;
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ofdbox-wgpu-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
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
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Reads back the RGBA8 pixels.
    pub fn read_rgba(&self, context: &WgpuContext) -> Result<Vec<u8>, RenderError> {
        let row = self.width * 4;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ofdbox-wgpu-readback"),
            size: u64::from(padded) * u64::from(self.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = context.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        context.queue.submit([encoder.finish()]);

        let (sender, receiver) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        context
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| RenderError::Backend(format!("wgpu poll: {error}")))?;
        receiver
            .recv()
            .map_err(|_| RenderError::Backend("wgpu readback channel".to_owned()))?
            .map_err(|error| RenderError::Backend(format!("wgpu readback: {error}")))?;
        let mapped = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|error| RenderError::Backend(format!("wgpu map range: {error}")))?;
        let mut output = Vec::with_capacity((self.width * self.height * 4) as usize);
        for row in mapped.chunks(padded as usize) {
            output.extend_from_slice(&row[..(self.width * 4) as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(output)
    }
}

/// The wgpu compute backend.
pub struct WgpuBackend {
    context: Arc<WgpuContext>,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl WgpuBackend {
    /// Creates a backend; an initialized context must be supplied explicitly.
    pub fn new(context: WgpuContext) -> Result<Self, RenderError> {
        Self::from_shared(Arc::new(context))
    }

    /// Creates a backend from a shared context.
    pub fn from_shared(context: Arc<WgpuContext>) -> Result<Self, RenderError> {
        let module = context
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("ofdbox-wgpu-shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
            });
        let layout = context
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("ofdbox-wgpu-bind-layout"),
                entries: &[
                    storage_entry(0, true),
                    storage_entry(1, true),
                    storage_entry(2, true),
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                ],
            });
        let pipeline_layout =
            context
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("ofdbox-wgpu-pipeline-layout"),
                    bind_group_layouts: &[Some(&layout)],
                    immediate_size: 0,
                });
        let pipeline = context
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("ofdbox-wgpu-pipeline"),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
        Ok(Self {
            context,
            pipeline,
            layout,
        })
    }

    /// Renders into an offscreen texture.
    pub fn render_to_target(&self, scene: &Scene, target: &WgpuTarget) -> Result<(), RenderError> {
        self.render_to_target_with_offset(scene, target, (0.0, 0.0))
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

    fn render_to_target_with_offset(
        &self,
        scene: &Scene,
        target: &WgpuTarget,
        origin_px: (f64, f64),
    ) -> Result<(), RenderError> {
        let (edges, paths) = collect_paths(scene, target.dpi, origin_px);
        if paths.is_empty() {
            return Ok(());
        }
        let mut edge_data = Vec::with_capacity(edges.len() * 16);
        for edge in &edges {
            for value in edge {
                edge_data.extend_from_slice(&value.to_ne_bytes());
            }
        }
        let mut path_data = Vec::with_capacity(paths.len() * 80);
        for path in &paths {
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
        }
        let edge_buffer =
            self.context
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("ofdbox-edge-buffer"),
                    contents: &edge_data,
                    usage: wgpu::BufferUsages::STORAGE,
                });
        let path_buffer =
            self.context
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("ofdbox-path-buffer"),
                    contents: &path_data,
                    usage: wgpu::BufferUsages::STORAGE,
                });
        let params = [
            target.width,
            target.height,
            u32::try_from(paths.len())
                .map_err(|_| RenderError::Backend("too many paths".into()))?,
            ((target.dpi / 25.4) as f32).to_bits(),
        ];
        let mut params_bytes = Vec::with_capacity(16);
        for value in params {
            params_bytes.extend_from_slice(&value.to_ne_bytes());
        }
        let params_buffer =
            self.context
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("ofdbox-params"),
                    contents: &params_bytes,
                    usage: wgpu::BufferUsages::STORAGE,
                });
        let bind_group = self
            .context
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ofdbox-wgpu-bind-group"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: edge_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: path_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: params_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&target.view),
                    },
                ],
            });
        let mut encoder = self
            .context
            .device
            .create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(target.width.div_ceil(8), target.height.div_ceil(8), 1);
        }
        self.context.queue.submit([encoder.finish()]);
        self.context
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| RenderError::Backend(format!("wgpu poll: {error}")))?;
        Ok(())
    }
}

impl WgpuBackend {
    /// Returns the supported capabilities.
    pub fn capabilities(&self) -> WgpuCapabilities {
        WgpuCapabilities {
            solid_paths: true,
            strokes: true,
            linear_gradients: true,
            clips: true,
            text_outlines: true,
            images: false,
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

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn collect_paths(
    scene: &Scene,
    dpi: f64,
    origin_px: (f64, f64),
) -> (Vec<[f32; 4]>, Vec<DrawPath>) {
    let scale = dpi / 25.4;
    let mut edges = Vec::new();
    let mut paths = Vec::new();
    collect_group(
        &scene.root,
        Matrix::IDENTITY,
        scale,
        origin_px,
        None,
        1.0,
        &mut edges,
        &mut paths,
    );
    (edges, paths)
}

fn collect_group(
    group: &Group,
    parent: Matrix,
    scale: f64,
    origin_px: (f64, f64),
    inherited_clip: Option<(u32, u32, u32)>,
    inherited_opacity: f32,
    edges: &mut Vec<[f32; 4]>,
    paths: &mut Vec<DrawPath>,
) {
    let transform = group.transform.then(parent);
    let opacity = inherited_opacity * group.opacity.clamp(0.0, 1.0);
    let clip = if let Some(clip) = &group.clip {
        let start = u32::try_from(edges.len()).unwrap_or(u32::MAX);
        flatten_path(&clip.path, transform, scale, origin_px, edges);
        let count = u32::try_from(edges.len()).unwrap_or(u32::MAX) - start;
        Some((start, count, u32::from(clip.rule == FillRule::EvenOdd)))
    } else {
        inherited_clip
    };
    for element in &group.children {
        match element {
            Element::Group(child) => collect_group(
                child, transform, scale, origin_px, clip, opacity, edges, paths,
            ),
            Element::Path(path) => {
                let start = u32::try_from(edges.len()).unwrap_or(u32::MAX);
                flatten_path(&path.path, transform, scale, origin_px, edges);
                let count = u32::try_from(edges.len()).unwrap_or(u32::MAX) - start;
                if count == 0 {
                    continue;
                }
                if let Some(fill) = &path.fill {
                    let (kind, color, end_color, gradient) = match fill {
                        Paint::Color(color) => (
                            0,
                            scaled_color(*color, opacity),
                            scaled_color(*color, opacity),
                            [0.0, 0.0, 0.0, 0.0],
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
                                scaled_color(start_color, opacity),
                                scaled_color(end_color, opacity),
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
                                scaled_color(color, opacity),
                                scaled_color(color, opacity),
                                [0.0; 4],
                            )
                        }
                        _ => (0, [0.0; 4], [0.0; 4], [0.0; 4]),
                    };
                    paths.push(DrawPath {
                        start,
                        count,
                        rule: u32::from(path.rule == FillRule::EvenOdd),
                        kind,
                        color,
                        width: 0.0,
                        clip_start: clip.map_or(0, |value| value.0),
                        clip_count: clip.map_or(0, |value| value.1),
                        clip_rule: clip.map_or(0, |value| value.2),
                        end_color,
                        gradient,
                    });
                }
                if let Some(stroke) = &path.stroke
                    && let Paint::Color(color) = &stroke.paint
                {
                    paths.push(DrawPath {
                        start,
                        count,
                        rule: 0,
                        kind: 1,
                        color: scaled_color(*color, opacity),
                        width: (stroke.style.width * scale) as f32,
                        clip_start: clip.map_or(0, |value| value.0),
                        clip_count: clip.map_or(0, |value| value.1),
                        clip_rule: clip.map_or(0, |value| value.2),
                        end_color: scaled_color(*color, opacity),
                        gradient: [0.0; 4],
                    });
                }
            }
            Element::Text(text) => {
                let Some(Paint::Color(color)) = text.fill.as_ref() else {
                    continue;
                };
                let Ok(face) = ttf_parser::Face::parse(text.font.data(), text.font.face_index())
                else {
                    continue;
                };
                let units = f64::from(face.units_per_em());
                if units == 0.0 {
                    continue;
                }
                let glyph_scale = text.font_size / units;
                for glyph in &text.run.glyphs {
                    let Ok(glyph_id) = u16::try_from(glyph.id) else {
                        continue;
                    };
                    let mut builder = GlyphPathBuilder::default();
                    if face
                        .outline_glyph(ttf_parser::GlyphId(glyph_id), &mut builder)
                        .is_none()
                        || builder.path.is_empty()
                    {
                        continue;
                    }
                    let glyph_transform = Matrix::scale(glyph_scale, -glyph_scale)
                        .then(Matrix::translate(glyph.x, glyph.y))
                        .then(text.transform)
                        .then(transform);
                    let start = u32::try_from(edges.len()).unwrap_or(u32::MAX);
                    flatten_path(&builder.path, glyph_transform, scale, origin_px, edges);
                    let count = u32::try_from(edges.len()).unwrap_or(u32::MAX) - start;
                    if count == 0 {
                        continue;
                    }
                    paths.push(DrawPath {
                        start,
                        count,
                        rule: 0,
                        kind: 0,
                        color: scaled_color(*color, opacity),
                        width: 0.0,
                        clip_start: clip.map_or(0, |value| value.0),
                        clip_count: clip.map_or(0, |value| value.1),
                        clip_rule: clip.map_or(0, |value| value.2),
                        end_color: scaled_color(*color, opacity),
                        gradient: [0.0; 4],
                    });
                }
            }
            Element::Image(_) => {}
            _ => {}
        }
    }
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

fn flatten_path(
    path: &Path,
    transform: Matrix,
    scale: f64,
    origin_px: (f64, f64),
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
struct PathInfo { start: u32, count: u32, rule: u32, kind: u32, color: vec4f, width: f32, clip_start: u32, clip_count: u32, clip_rule: u32, end_color: vec4f, gradient: vec4f };
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
    var result = vec4f(1.0, 1.0, 1.0, 1.0);
    for (var pi = 0u; pi < params.count; pi = pi + 1u) {
        let info = paths[pi];
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
            result = vec4f(color.rgb * a + result.rgb * (1.0 - a), 1.0);
        }
    }
    textureStore(output, vec2i(i32(id.x), i32(id.y)), result);
}
"#;

#[cfg(test)]
mod tests {
    use super::{WgpuBackend, WgpuContext, WgpuTarget};
    use ofdbox_folio_ir::{
        Element, FillRule, Group, Matrix, Paint, Path, PathElement, Point, Rgba, Scene, Size,
    };

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
