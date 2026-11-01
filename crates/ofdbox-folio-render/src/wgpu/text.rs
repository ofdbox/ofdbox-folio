//! Instanced glyph rendering pipeline for the wgpu backend.

#![allow(dead_code)]

#[cfg(feature = "wgpu30")]
use wgpu as wgpu_lib;
#[cfg(feature = "wgpu27")]
use wgpu27 as wgpu_lib;

/// Text pipeline and its bind group layout.
pub(crate) struct TextPipeline {
    pipeline: wgpu_lib::RenderPipeline,
    layout: wgpu_lib::BindGroupLayout,
}

impl TextPipeline {
    /// Create the instanced text pipeline.
    pub(crate) fn new(device: &wgpu_lib::Device) -> Self {
        let module = device.create_shader_module(wgpu_lib::ShaderModuleDescriptor {
            label: Some("ofdbox-text-shader"),
            source: wgpu_lib::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(TEXT_SHADER)),
        });
        let layout = device.create_bind_group_layout(&wgpu_lib::BindGroupLayoutDescriptor {
            label: Some("ofdbox-text-bind-layout"),
            entries: &[
                wgpu_lib::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu_lib::ShaderStages::VERTEX,
                    ty: wgpu_lib::BindingType::Buffer {
                        ty: wgpu_lib::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu_lib::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu_lib::ShaderStages::FRAGMENT,
                    ty: wgpu_lib::BindingType::Texture {
                        sample_type: wgpu_lib::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu_lib::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu_lib::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu_lib::ShaderStages::FRAGMENT,
                    ty: wgpu_lib::BindingType::Sampler(wgpu_lib::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu_lib::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu_lib::ShaderStages::VERTEX,
                    ty: wgpu_lib::BindingType::Buffer {
                        ty: wgpu_lib::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        #[cfg(feature = "wgpu30")]
        let pipeline_layout = device.create_pipeline_layout(&wgpu_lib::PipelineLayoutDescriptor {
            label: Some("ofdbox-text-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        #[cfg(feature = "wgpu27")]
        let pipeline_layout = device.create_pipeline_layout(&wgpu_lib::PipelineLayoutDescriptor {
            label: Some("ofdbox-text-pipeline-layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu_lib::RenderPipelineDescriptor {
            label: Some("ofdbox-text-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu_lib::VertexState {
                module: &module,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu_lib::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                targets: &[Some(wgpu_lib::ColorTargetState {
                    format: wgpu_lib::TextureFormat::Rgba8Unorm,
                    blend: Some(wgpu_lib::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu_lib::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu_lib::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu_lib::MultisampleState::default(),
            #[cfg(feature = "wgpu30")]
            multiview_mask: None,
            #[cfg(feature = "wgpu27")]
            multiview: None,
            cache: None,
        });
        Self { pipeline, layout }
    }

    /// The render pipeline.
    pub(crate) fn pipeline(&self) -> &wgpu_lib::RenderPipeline {
        &self.pipeline
    }

    /// The bind group layout.
    pub(crate) fn layout(&self) -> &wgpu_lib::BindGroupLayout {
        &self.layout
    }
}

/// Vertex/fragment shader for instanced glyph quads.
pub(crate) const TEXT_SHADER: &str = r#"
struct Instance {
    origin: vec2f,
    x_axis: vec2f,
    y_axis: vec2f,
    uv0: vec2f,
    uv1: vec2f,
    color: vec4f,
};
struct Params { size: vec2f, _pad: vec2f };
struct VOut {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
    @location(1) color: vec4f,
};

@group(0) @binding(0) var<storage, read> instances: array<Instance>;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;
@group(0) @binding(3) var<uniform> params: Params;

@vertex
fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    var corners = array<vec2f, 6>(
        vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0),
        vec2f(0.0, 1.0), vec2f(1.0, 0.0), vec2f(1.0, 1.0),
    );
    let corner = corners[vi];
    let instance = instances[ii];
    let page = instance.origin + instance.x_axis * corner.x + instance.y_axis * corner.y;
    let ndc = vec2f(page.x / params.size.x * 2.0 - 1.0, 1.0 - page.y / params.size.y * 2.0);
    var out: VOut;
    out.position = vec4f(ndc, 0.0, 1.0);
    out.uv = mix(instance.uv0, instance.uv1, corner);
    out.color = instance.color;
    return out;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4f {
    let alpha = textureSample(atlas, atlas_sampler, in.uv).r * in.color.a;
    return vec4f(in.color.rgb, alpha);
}
"#;
