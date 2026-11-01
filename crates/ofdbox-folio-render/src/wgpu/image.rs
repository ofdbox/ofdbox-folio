//! Textured image pipeline for the wgpu backend.

#![allow(dead_code)]

#[cfg(feature = "wgpu30")]
use wgpu as wgpu_lib;
#[cfg(feature = "wgpu27")]
use wgpu27 as wgpu_lib;

/// Pipeline that draws one RGBA image as an affine-mapped quad.
pub(crate) struct ImagePipeline {
    pipeline: wgpu_lib::RenderPipeline,
    layout: wgpu_lib::BindGroupLayout,
}

impl ImagePipeline {
    /// Create the image pipeline.
    pub(crate) fn new(device: &wgpu_lib::Device) -> Self {
        let module = device.create_shader_module(wgpu_lib::ShaderModuleDescriptor {
            label: Some("ofdbox-image-shader"),
            source: wgpu_lib::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(IMAGE_SHADER)),
        });
        let layout = device.create_bind_group_layout(&wgpu_lib::BindGroupLayoutDescriptor {
            label: Some("ofdbox-image-bind-layout"),
            entries: &[
                wgpu_lib::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu_lib::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu_lib::BindingType::Buffer {
                        ty: wgpu_lib::BufferBindingType::Uniform,
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
            ],
        });
        #[cfg(feature = "wgpu30")]
        let pipeline_layout = device.create_pipeline_layout(&wgpu_lib::PipelineLayoutDescriptor {
            label: Some("ofdbox-image-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        #[cfg(feature = "wgpu27")]
        let pipeline_layout = device.create_pipeline_layout(&wgpu_lib::PipelineLayoutDescriptor {
            label: Some("ofdbox-image-pipeline-layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu_lib::RenderPipelineDescriptor {
            label: Some("ofdbox-image-pipeline"),
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

const IMAGE_SHADER: &str = r#"
struct Params {
    size: vec2f,
    origin: vec2f,
    x_axis: vec2f,
    y_axis: vec2f,
    alpha: f32,
};
struct VOut {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var image: texture_2d<f32>;
@group(0) @binding(2) var image_sampler: sampler;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    var corners = array<vec2f, 6>(
        vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0),
        vec2f(0.0, 1.0), vec2f(1.0, 0.0), vec2f(1.0, 1.0),
    );
    let corner = corners[vi];
    let page = params.origin + params.x_axis * corner.x + params.y_axis * corner.y;
    let ndc = vec2f(page.x / params.size.x * 2.0 - 1.0, 1.0 - page.y / params.size.y * 2.0);
    var out: VOut;
    out.position = vec4f(ndc, 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4f {
    let texel = textureSample(image, image_sampler, in.uv);
    return vec4f(texel.rgb, texel.a * params.alpha);
}
"#;
