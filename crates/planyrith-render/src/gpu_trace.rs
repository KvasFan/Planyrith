//! Implements docs/06 §11.3: the path tracer of §11.1 on the graphics card.
//! The scene, light and camera are prepared as for the CPU tracer
//! (`PathTracer::new`), packed into storage buffers relative to the camera
//! origin (f32) and traced by a compute shader that adds paths to an
//! accumulation buffer, pass by pass. The image is read back and finished
//! (exposure, filmic curve, sRGB) as the CPU tracer finishes its own.
//!
//! The card draws its own random numbers, so its image is not the CPU's bit
//! for bit; it converges to the same picture (tested on a test scene). It
//! needs compute shaders: not on WebGL2, where the CPU tracer remains.

use bytemuck::{Pod, Zeroable};
use glam::DVec3;
use planyrith_core::document::Document;
use wgpu::util::DeviceExt;

use crate::RenderError;
use crate::pathtrace::{PathTraceOptions, PathTracer, encode, filmic};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    o0: [f32; 4],
    ox: [f32; 4],
    oy: [f32; 4],
    d0: [f32; 4],
    dx: [f32; 4],
    dy: [f32; 4],
    sun: [f32; 4],
    sky: [f32; 4],
    horizon: [f32; 4],
    below: [f32; 4],
    background: [f32; 4],
    ground: [f32; 4],
    ground_albedo: [f32; 4],
    width: u32,
    height: u32,
    first: u32,
    count: u32,
    bounces: u32,
    nodes: u32,
    pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Tri {
    p: [[f32; 4]; 3],
    n: [[f32; 4]; 3],
    uv01: [f32; 4],
    uv2: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Node {
    lo: [f32; 3],
    first: u32,
    hi: [f32; 3],
    count: u32,
    right: u32,
    pad: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Surf {
    albedo: [f32; 3],
    roughness: f32,
    metallic: f32,
    opacity: f32,
    tex: u32,
    tex_w: u32,
    tex_h: u32,
    pad: [u32; 3],
}

fn v4(v: DVec3, w: f32) -> [f32; 4] {
    [v.x as f32, v.y as f32, v.z as f32, w]
}

/// Most paths per pixel one dispatch adds (long dispatches can trip a
/// driver's watchdog).
const PER_DISPATCH: u32 = 4;

/// A path-traced image refined on the graphics card.
pub struct GpuPathTracer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    params_buffer: wgpu::Buffer,
    accum: wgpu::Buffer,
    params: Params,
    exposure: f64,
    samples: u32,
}

/// A device for compute work: the first adapter with compute shaders, its
/// full limits.
pub fn compute_device() -> Result<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo), RenderError> {
    pollster::block_on(async {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = wgpu::util::initialize_adapter_from_env_or_default(&instance, None)
            .await
            .map_err(|e| RenderError::NoAdapter(e.to_string()))?;
        if !adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
        {
            return Err(RenderError::NoAdapter(
                "the graphics adapter has no compute shaders".into(),
            ));
        }
        let info = adapter.get_info();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("planyrith path tracer"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                experimental_features: wgpu::ExperimentalFeatures::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::default(),
            })
            .await
            .map_err(|e| RenderError::Device(e.to_string()))?;
        Ok((device, queue, info))
    })
}

impl GpuPathTracer {
    /// Prepares tracing `doc` on `device`.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        doc: &Document,
        options: &PathTraceOptions,
    ) -> Result<Self, RenderError> {
        let cpu = PathTracer::new(
            doc,
            &PathTraceOptions {
                // The CPU tracer's sums are not needed.
                threads: 1,
                ..options.clone()
            },
        );
        let (width, height) = (cpu.width, cpu.height);
        let frame = cpu.camera.ray_frame();
        let origin = frame.origin;
        let scene = &cpu.scene;
        let light = &cpu.light;

        // Textures as sRGB texels, one after another.
        let mut texels: Vec<u32> = Vec::new();
        let mut offsets = Vec::new();
        for t in &scene.textures {
            offsets.push(texels.len() as u32);
            texels.extend(
                t.texels
                    .iter()
                    .map(|c| u32::from_le_bytes([encode(c.x), encode(c.y), encode(c.z), 255])),
            );
        }
        if texels.is_empty() {
            texels.push(0);
        }
        let surfs: Vec<Surf> = scene
            .surfaces
            .iter()
            .map(|s| {
                let (tex, w, h) = match s.texture {
                    Some(k) => (
                        offsets[k],
                        scene.textures[k].width as u32,
                        scene.textures[k].height as u32,
                    ),
                    None => (u32::MAX, 0, 0),
                };
                Surf {
                    albedo: [s.albedo.x as f32, s.albedo.y as f32, s.albedo.z as f32],
                    roughness: s.roughness as f32,
                    metallic: s.metallic as f32,
                    opacity: s.opacity as f32,
                    tex,
                    tex_w: w,
                    tex_h: h,
                    pad: [0; 3],
                }
            })
            .chain(std::iter::once(Surf::zeroed()))
            .collect();
        let tris: Vec<Tri> = scene
            .triangles
            .iter()
            .map(|t| Tri {
                p: t.p.map(|p| v4(p - origin, 0.0)),
                n: t.n.map(|n| v4(n, 0.0)),
                uv01: [
                    t.uv[0].x as f32,
                    t.uv[0].y as f32,
                    t.uv[1].x as f32,
                    t.uv[1].y as f32,
                ],
                uv2: [
                    t.uv[2].x as f32,
                    t.uv[2].y as f32,
                    f32::from_bits(t.surface),
                    0.0,
                ],
            })
            .chain(std::iter::once(Tri::zeroed()))
            .collect();
        let nodes: Vec<Node> = scene
            .nodes
            .iter()
            .map(|n| {
                let lo = n.lo - origin;
                let hi = n.hi - origin;
                Node {
                    lo: [lo.x as f32, lo.y as f32, lo.z as f32],
                    first: n.first,
                    hi: [hi.x as f32, hi.y as f32, hi.z as f32],
                    count: n.count,
                    right: n.right,
                    pad: [0; 3],
                }
            })
            .chain(std::iter::once(Node::zeroed()))
            .collect();
        let (ground, ground_albedo) = match &scene.ground {
            Some((z, s)) => (
                [(z - origin.z) as f32, 1.0, s.roughness as f32, 0.0],
                v4(s.albedo, 0.0),
            ),
            None => ([0.0, 0.0, 1.0, 0.0], [0.0; 4]),
        };
        let params = Params {
            o0: v4(frame.origin - origin, 0.0),
            ox: v4(frame.origin_x, 0.0),
            oy: v4(frame.origin_y, 0.0),
            d0: v4(frame.direction, 0.0),
            dx: v4(frame.direction_x, 0.0),
            dy: v4(frame.direction_y, 0.0),
            sun: v4(light.sun, light.sun_radiance as f32),
            sky: v4(light.sky, 0.0),
            horizon: v4(light.horizon, 0.0),
            below: v4(light.ground, 0.0),
            background: match light.background {
                Some(c) => v4(c, 1.0),
                None => [0.0; 4],
            },
            ground,
            ground_albedo,
            width,
            height,
            first: 0,
            count: 0,
            bounces: cpu.bounces,
            nodes: scene.nodes.len() as u32,
            pad: [0; 2],
        };
        let storage = |label: &str, contents: &[u8]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage: wgpu::BufferUsages::STORAGE,
            })
        };
        let tri_buffer = storage("trace triangles", bytemuck::cast_slice(&tris));
        let node_buffer = storage("trace nodes", bytemuck::cast_slice(&nodes));
        let surf_buffer = storage("trace surfaces", bytemuck::cast_slice(&surfs));
        let texel_buffer = storage("trace texels", bytemuck::cast_slice(&texels));
        let accum = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("trace accumulation"),
            contents: &vec![0u8; width as usize * height as usize * 16],
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("trace parameters"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("path tracer"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/pathtrace.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("path tracer"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let layout = pipeline.get_bind_group_layout(0);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("path tracer"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: tri_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: node_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: surf_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: texel_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: accum.as_entire_binding(),
                },
            ],
        });
        Ok(GpuPathTracer {
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
            bind_group,
            params_buffer,
            accum,
            params,
            exposure: light.exposure,
            samples: 0,
        })
    }

    /// Paths per pixel so far.
    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// Adds `count` paths to every pixel.
    pub fn pass(&mut self, count: u32) {
        let mut left = count;
        while left > 0 {
            let n = left.min(PER_DISPATCH);
            self.params.first = self.samples;
            self.params.count = n;
            self.queue
                .write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(&self.params));
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("path tracer pass"),
                });
            {
                let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("path tracer pass"),
                    timestamp_writes: None,
                });
                cpass.set_pipeline(&self.pipeline);
                cpass.set_bind_group(0, &self.bind_group, &[]);
                cpass.dispatch_workgroups(
                    self.params.width.div_ceil(8),
                    self.params.height.div_ceil(8),
                    1,
                );
            }
            self.queue.submit([encoder.finish()]);
            self.samples += n;
            left -= n;
        }
    }

    /// The image so far (waits for the card).
    pub fn image(&self) -> Result<image::RgbaImage, RenderError> {
        let size = self.accum.size();
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("trace readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("trace readback"),
            });
        encoder.copy_buffer_to_buffer(&self.accum, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        staging.map_async(wgpu::MapMode::Read, .., move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|e| RenderError::Readback(e.to_string()))?
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        let mapped = staging
            .get_mapped_range(..)
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        let sums: &[[f32; 4]] = bytemuck::cast_slice(&mapped);
        let (width, height) = (self.params.width, self.params.height);
        let n = f64::from(self.samples.max(1));
        let mut image = image::RgbaImage::new(width, height);
        for (i, s) in sums
            .iter()
            .enumerate()
            .take(width as usize * height as usize)
        {
            let c =
                DVec3::new(f64::from(s[0]), f64::from(s[1]), f64::from(s[2])) / n * self.exposure;
            image.put_pixel(
                i as u32 % width,
                i as u32 / width,
                image::Rgba([
                    encode(filmic(c.x)),
                    encode(filmic(c.y)),
                    encode(filmic(c.z)),
                    255,
                ]),
            );
        }
        drop(mapped);
        staging.unmap();
        Ok(image)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use planyrith_core::commands::{CommandRegistry, Engine};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn the_card_converges_to_the_cpu_picture() {
        let Ok((device, queue, info)) = compute_device() else {
            eprintln!("no compute adapter: skipped");
            return;
        };
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        for (a, b, h) in [
            ([0, 0, 0], [1000, 600, 0], 400),
            ([1300, 200, 0], [1700, 600, 0], 900),
        ] {
            e.execute_erased("rectangle", json!({ "corner_a": a, "corner_b": b }))
                .unwrap();
            e.execute_erased(
                "push-pull",
                json!({ "face": { "Query": "face & last" }, "distance": h }),
            )
            .unwrap();
        }
        for (name, color, roughness, metallic) in [
            ("Brick", "#B03A2E", 0.8, 0.0),
            ("Steel", "#C0C4C8", 0.25, 1.0),
        ] {
            e.execute_erased(
                "material-create",
                json!({ "name": name, "color": color, "roughness": roughness, "metallic": metallic }),
            )
            .unwrap();
        }
        e.execute_erased(
            "material-apply",
            json!({ "targets": "face & normal:z", "material": "Brick" }),
        )
        .unwrap();
        e.execute_erased(
            "material-apply",
            json!({ "targets": "face & normal:x", "material": "Steel" }),
        )
        .unwrap();
        e.execute_erased("shadows-set", json!({ "enabled": true }))
            .unwrap();
        e.execute_erased("zoom-extents", json!({})).unwrap();
        let doc = e.document().clone();
        let options = PathTraceOptions {
            width: 96,
            height: 60,
            samples: 256,
            ..PathTraceOptions::default()
        };
        let cpu = crate::path_trace(&doc, &options);
        let mut gpu = GpuPathTracer::new(&device, &queue, &doc, &options).unwrap();
        gpu.pass(256);
        let image = gpu.image().unwrap();
        // The same picture up to noise: the mean difference per channel is
        // small and no large region disagrees.
        let diff: Vec<f64> = cpu
            .pixels()
            .zip(image.pixels())
            .map(|(a, b)| {
                (0..3)
                    .map(|k| (f64::from(a[k]) - f64::from(b[k])).abs())
                    .sum::<f64>()
                    / 3.0
            })
            .collect();
        let mean = diff.iter().sum::<f64>() / diff.len() as f64;
        eprintln!("{}: mean difference {mean:.2}", info.name);
        if let Ok(dir) = std::env::var("PLANYRITH_TRACE_OUT") {
            cpu.save(format!("{dir}/cpu.png")).unwrap();
            image.save(format!("{dir}/gpu.png")).unwrap();
        }
        assert!(mean < 6.0, "{}: mean difference {mean}", info.name);
        // The same card gives the same image twice.
        let mut again = GpuPathTracer::new(&device, &queue, &doc, &options).unwrap();
        again.pass(256);
        assert_eq!(again.image().unwrap(), image);
    }
}
