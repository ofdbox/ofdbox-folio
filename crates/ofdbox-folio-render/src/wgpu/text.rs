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
                wgpu_lib::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu_lib::ShaderStages::FRAGMENT,
                    ty: wgpu_lib::BindingType::Buffer {
                        ty: wgpu_lib::BufferBindingType::Storage { read_only: true },
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
    gradient: vec4f,
    end_color: vec4f,
    mode: f32,
    _pad_a: f32,
    _pad_b: f32,
    _pad_c: f32,
    clip_start: u32,
    clip_count: u32,
    clip_rule: u32,
    _clip_pad: u32,
    ellipse: vec4f,
    radial_end: vec4f,
};
struct Params { size: vec2f, _pad: vec2f };
struct VOut {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
    @location(1) color: vec4f,
    @location(2) page: vec2f,
    @location(3) end_color: vec4f,
    @location(4) gradient: vec4f,
    @location(5) mode: f32,
    @location(6) @interpolate(flat) clip_start: u32,
    @location(7) @interpolate(flat) clip_count: u32,
    @location(8) @interpolate(flat) clip_rule: u32,
    @location(9) ellipse: vec4f,
    @location(10) radial_end: vec4f,
};

@group(0) @binding(0) var<storage, read> instances: array<Instance>;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;
@group(0) @binding(3) var<uniform> params: Params;
@group(0) @binding(4) var<storage, read> clip_edges: array<vec4f>;

fn inside_range(p: vec2f, start: u32, count: u32, rule: u32) -> bool {
    var winding: i32 = 0;
    for (var ei = 0u; ei < count; ei = ei + 1u) {
        let e = clip_edges[start + ei];
        let ay = e.y;
        let by = e.w;
        if ((ay <= p.y && by > p.y) || (by <= p.y && ay > p.y)) {
            let t = (p.y - ay) / (by - ay);
            let x = e.x + t * (e.z - e.x);
            if (x > p.x) {
                if (by > ay) { winding = winding + 1; } else { winding = winding - 1; }
            }
        }
    }
    if (rule != 0u) { return (winding & 1) != 0; }
    return winding != 0;
}

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
    out.page = page;
    out.end_color = instance.end_color;
    out.gradient = instance.gradient;
    out.mode = instance.mode;
    out.clip_start = instance.clip_start;
    out.clip_count = instance.clip_count;
    out.clip_rule = instance.clip_rule;
    out.ellipse = instance.ellipse;
    out.radial_end = instance.radial_end;
    return out;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4f {
    var t = 0.0;
    if (in.mode > 1.5) {
        let c0 = in.gradient.xy;
        let r0 = max(in.gradient.z, 0.0001);
        let c1 = in.radial_end.xy;
        let r1 = max(in.radial_end.z, 0.0001);
        let d = in.page - c0;
        let local = vec2f(
            in.ellipse.x * d.x + in.ellipse.z * d.y,
            in.ellipse.y * d.x + in.ellipse.w * d.y,
        );
        let cd = c1 - c0;
        let dr = r1 - r0;
        let a = dot(cd, cd) - dr * dr;
        let b = 2.0 * (dot(d, cd) - r0 * dr);
        let c = dot(d, d) - r0 * r0;
        if (abs(a) < 0.00001) {
            if (abs(b) < 0.00001) {
                t = length(local) / r0;
            } else {
                t = -c / b;
            }
        } else {
            let disc = max(b * b - 4.0 * a * c, 0.0);
            let root = sqrt(disc);
            let t1 = (-b - root) / (2.0 * a);
            let t2 = (-b + root) / (2.0 * a);
            t = select(t2, t1, t1 >= 0.0);
        }
        t = clamp(t, 0.0, 1.0);
    } else if (in.mode > 0.5) {
        if (in.clip_count > 0u && !inside_range(in.page, in.clip_start, in.clip_count, in.clip_rule)) {
        discard;
    }
    let axis = in.gradient.zw - in.gradient.xy;
        t = clamp(dot(in.page - in.gradient.xy, axis) / max(dot(axis, axis), 0.0001), 0.0, 1.0);
    }
    let color = mix(in.color, in.end_color, t);
    let alpha = textureSample(atlas, atlas_sampler, in.uv).r * color.a;
    return vec4f(color.rgb, alpha);
}
"#;
