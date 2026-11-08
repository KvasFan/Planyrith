//! Screen-space ambient occlusion of the realistic look (docs/06 §4.1):
//! the occlusion pass reads the opaque faces' depth (drawn by the renderer
//! into [`AoTargets::depth`]) and writes an occlusion factor; the apply
//! pass averages it over 4×4 pixels and multiplies the frame by it.

use std::borrow::Cow;

use bytemuck::{Pod, Zeroable};

/// Depth format of the occlusion's depth pass (the scene's).
pub(crate) const AO_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// The faces' depth as the four bytes of its float: WebGL2 can neither
/// read depth textures nor render to float ones.
pub(crate) const AO_PACKED_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const AO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// The occlusion pass's uniform (`ao.wgsl`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub(crate) struct AoUniform {
    /// Clip space to eye-relative world coordinates.
    pub eye_from_clip: [[f32; 4]; 4],
    /// Radius (mm), strength, target width and height (px).
    pub params: [f32; 4],
}

/// Pipelines and the uniform buffer, made once.
pub(crate) struct AoPass {
    layout: wgpu::BindGroupLayout,
    apply_layout: wgpu::BindGroupLayout,
    occlusion: wgpu::RenderPipeline,
    apply: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
}

/// Per-size textures and bind groups.
pub(crate) struct AoTargets {
    /// Depth buffer of the occlusion's depth pass.
    pub depth: wgpu::TextureView,
    /// The faces' depth, packed (`fs_ao_depth`).
    pub packed: wgpu::TextureView,
    occlusion: wgpu::TextureView,
    occlusion_group: wgpu::BindGroup,
    apply_group: wgpu::BindGroup,
}

fn fullscreen(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
    entry: &str,
    target: wgpu::ColorTargetState,
) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(entry),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(entry),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(target)],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

impl AoPass {
    /// Builds the pipelines; `color_format` is the frame's.
    pub(crate) fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ao.wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("shaders/ao.wgsl"))),
        });
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ambient occlusion"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: fragment,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: fragment,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let apply_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ambient occlusion apply"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: fragment,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let occlusion = fullscreen(
            device,
            &shader,
            &layout,
            "fs_ao",
            wgpu::ColorTargetState {
                format: AO_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            },
        );
        // frame × occlusion; the frame's alpha stays.
        let multiply = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::Src,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let apply = fullscreen(
            device,
            &shader,
            &apply_layout,
            "fs_apply",
            wgpu::ColorTargetState {
                format: color_format,
                blend: Some(multiply),
                write_mask: wgpu::ColorWrites::ALL,
            },
        );
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ambient occlusion"),
            size: std::mem::size_of::<AoUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        AoPass {
            layout,
            apply_layout,
            occlusion,
            apply,
            uniform,
        }
    }

    /// Textures and bind groups for a `width` × `height` frame.
    pub(crate) fn targets(&self, device: &wgpu::Device, width: u32, height: u32) -> AoTargets {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = |label: &str, format| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let depth = texture("ambient occlusion depth", AO_DEPTH_FORMAT);
        let packed = texture("ambient occlusion packed depth", AO_PACKED_FORMAT);
        let occlusion = texture("ambient occlusion", AO_FORMAT);
        let occlusion_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ambient occlusion"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&packed),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.uniform.as_entire_binding(),
                },
            ],
        });
        let apply_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ambient occlusion apply"),
            layout: &self.apply_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&occlusion),
            }],
        });
        AoTargets {
            depth,
            packed,
            occlusion,
            occlusion_group,
            apply_group,
        }
    }

    /// The occlusion and apply passes (after the depth pass) onto `frame`.
    pub(crate) fn encode(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        targets: &AoTargets,
        frame: &wgpu::TextureView,
        uniform: &AoUniform,
    ) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(uniform));
        let passes: [(
            &wgpu::TextureView,
            wgpu::LoadOp<wgpu::Color>,
            &wgpu::RenderPipeline,
            &wgpu::BindGroup,
        ); 2] = [
            (
                &targets.occlusion,
                wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                &self.occlusion,
                &targets.occlusion_group,
            ),
            (frame, wgpu::LoadOp::Load, &self.apply, &targets.apply_group),
        ];
        for (view, load, pipeline, group) in passes {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ambient occlusion"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}
