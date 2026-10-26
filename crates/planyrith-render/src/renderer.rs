//! The frame renderer (docs/06 §3–§5, §8, §9): offscreen multisampled color
//! and reversed-Z depth targets, an optional single-sample id buffer, per
//! container GPU buffers built from `io::render_mesh` and invalidated by
//! deltas, and one draw per (occurrence, material range) with a dynamic
//! uniform offset.
//!
//! Pass order: opaque faces (or depth only in wireframe), visible edges,
//! dashed back edges, transparent faces sorted back to front per occurrence,
//! then the selection overlay (tinted faces and thick edges).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use glam::{DAffine3, DMat3, DVec3};
use planyrith_core::commands::Delta;
use planyrith_core::document::{
    Camera, ContainerRef, Document, EdgeColorMode, FaceMode, FrontOrBack, ImageRef, Projection,
    Rgba8, Style, Watermark, WatermarkPlacement,
};
use planyrith_core::geom::HalfEdgeMesh;
use planyrith_core::ids::{EdgeId, FaceId, MaterialId};
use planyrith_core::io::render_mesh::{
    DrawItem, ExtractOptions, MaterialKey, NO_FACE, RenderMesh, edge_flag_bits, extract,
};
use planyrith_core::math::Aabb;
use wgpu::util::DeviceExt;

use crate::RenderError;

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const ID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Uint;
/// Dynamic uniform offsets are multiples of this (the WebGPU minimum).
const UNIFORM_STRIDE: u64 = 256;

/// Renderer settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RendererOptions {
    /// MSAA sample count (1 or 4; docs/06 §3.1 step 8: 4× by default).
    pub msaa: u32,
    /// Physical pixels per logical pixel: edge widths scale with it.
    pub pixels_per_point: f32,
}

impl Default for RendererOptions {
    fn default() -> Self {
        RendererOptions {
            msaa: 4,
            pixels_per_point: 1.0,
        }
    }
}

/// One frame to draw.
#[derive(Debug, Clone, Copy)]
pub struct FrameRequest<'a> {
    /// The document.
    pub doc: &'a Document,
    /// Its draw list (`io::render_mesh::draw_list`).
    pub items: &'a [DrawItem],
    /// The camera (its aspect is replaced by the target's).
    pub camera: &'a Camera,
    /// The style.
    pub style: &'a Style,
    /// Target width in pixels.
    pub width: u32,
    /// Target height in pixels.
    pub height: u32,
    /// Draw hidden geometry (dashed edges, faint faces).
    pub show_hidden: bool,
    /// Also render the id buffer for [`Renderer::pick`].
    pub ids: bool,
    /// Clear to a transparent background (headless `--transparent`).
    pub transparent_background: bool,
    /// Draw the drawing axes (View › Axes, docs/06 §3.1 step 7).
    pub axes: bool,
}

/// What lies under a pixel of the id buffer (docs/06 §8 hover picking).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickHit {
    /// Index of the draw item.
    pub item: usize,
    /// The element.
    pub kind: PickKind,
}

/// The element of a [`PickHit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    /// A face.
    Face(FaceId),
    /// An edge.
    Edge(EdgeId),
}

/// One face vertex (36 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct FaceVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    face: u32,
}

/// One edge instance (72 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct EdgeInstance {
    a: [f32; 3],
    b: [f32; 3],
    n0: [f32; 3],
    n1: [f32; 3],
    flags: u32,
    index: u32,
    material_color: [f32; 4],
}

/// The per-draw uniform block (`Draw` in `scene.wgsl`, 224 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct DrawUniform {
    clip_from_local: [[f32; 4]; 4],
    normal_x: [f32; 4],
    normal_y: [f32; 4],
    normal_z: [f32; 4],
    color: [f32; 4],
    tint: [f32; 4],
    light: [f32; 4],
    fade: [f32; 4],
    viewport: [f32; 4],
    local_eye: [f32; 4],
    ids: [u32; 4],
    /// Fog start, end, depth near, depth far (mm).
    fog: [f32; 4],
    /// Fog color; alpha 0 off, 1 perspective, 2 parallel.
    fog_color: [f32; 4],
}

const TEXTURED: u32 = 1;
const UNLIT: u32 = 2;
const HIDE_SOFT: u32 = 4;
const DASHED: u32 = 8;
const BY_AXIS: u32 = 16;
const BY_MATERIAL: u32 = 32;
const EDGE_HAS_FACE0: u32 = 256;
const EDGE_HAS_FACE1: u32 = 512;
// `scene.wgsl` reads the extraction's flag bits directly.
const _: () = assert!(
    edge_flag_bits::SOFT as u32 == 2 && edge_flag_bits::SMOOTH as u32 == 4,
    "EDGE_SOFT/EDGE_SMOOTH in scene.wgsl"
);

/// GPU buffers of one container.
struct GpuMesh {
    vertices: Option<wgpu::Buffer>,
    indices: Option<wgpu::Buffer>,
    ranges: Vec<(MaterialKey, Range<u32>)>,
    edges: Option<wgpu::Buffer>,
    edge_count: u32,
    bounds: Aabb,
    /// Front triangle indices per local face (selection overlay).
    face_triangles: Vec<Vec<u32>>,
    face_lookup: Vec<FaceId>,
    edge_lookup: Vec<EdgeId>,
    /// Edge instances by local index (selection overlay).
    edge_instances: Vec<EdgeInstance>,
    /// The two local faces of each edge.
    edge_faces: Vec<[u32; 2]>,
    show_hidden: bool,
}

/// Offscreen targets of one size.
struct Targets {
    width: u32,
    height: u32,
    color_msaa: Option<wgpu::TextureView>,
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    /// The same texture viewed as `Rgba8Unorm` (gamma-encoded values for
    /// UIs that composite in gamma space, such as egui).
    display_view: wgpu::TextureView,
    depth: wgpu::TextureView,
    ids: wgpu::Texture,
    ids_view: wgpu::TextureView,
    ids_depth: wgpu::TextureView,
}

/// Pipelines of one frame kind.
struct Pipelines {
    face_opaque: wgpu::RenderPipeline,
    face_transparent: wgpu::RenderPipeline,
    face_depth_only: wgpu::RenderPipeline,
    face_overlay: wgpu::RenderPipeline,
    edge: wgpu::RenderPipeline,
    edge_back: wgpu::RenderPipeline,
    face_id: wgpu::RenderPipeline,
    edge_id: wgpu::RenderPipeline,
    sky: wgpu::RenderPipeline,
    mark: wgpu::RenderPipeline,
}

/// One recorded draw.
enum Draw {
    Faces {
        pipeline: Stage,
        uniform: u32,
        container: ContainerRef,
        range: Range<u32>,
        texture: Option<ImageRef>,
    },
    /// Faces drawn from a transient index buffer (selection overlay).
    Overlay {
        uniform: u32,
        container: ContainerRef,
        buffer: wgpu::Buffer,
        count: u32,
    },
    Edges {
        pipeline: Stage,
        uniform: u32,
        container: ContainerRef,
    },
    /// Edges drawn from a transient instance buffer (selected edges, axes).
    EdgeList {
        uniform: u32,
        buffer: wgpu::Buffer,
        count: u32,
    },
    /// The sky gradient behind everything.
    Sky { uniform: u32 },
    /// A watermark image on the screen.
    Mark { uniform: u32, image: ImageRef },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Opaque,
    Transparent,
    DepthOnly,
    Edge,
    BackEdge,
}

/// The renderer: device, pipelines, caches and targets.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    options: RendererOptions,
    draw_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    pipelines: Pipelines,
    sampler: wgpu::Sampler,
    white: wgpu::BindGroup,
    textures: BTreeMap<ImageRef, wgpu::BindGroup>,
    meshes: BTreeMap<ContainerRef, GpuMesh>,
    uniforms: wgpu::Buffer,
    uniform_capacity: u64,
    uniform_group: wgpu::BindGroup,
    targets: Option<Targets>,
    /// Draw items of the last frame (id buffer → item).
    last_items: Vec<(ContainerRef, usize)>,
    ids_valid: bool,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("options", &self.options)
            .field("meshes", &self.meshes.len())
            .finish_non_exhaustive()
    }
}

/// sRGB 8-bit to linear.
fn linear(c: Rgba8) -> [f32; 4] {
    let f = |v: u8| {
        let s = f32::from(v) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    [f(c.r), f(c.g), f(c.b), f32::from(c.a) / 255.0]
}

fn f32x4(v: DVec3, w: f64) -> [f32; 4] {
    [v.x as f32, v.y as f32, v.z as f32, w as f32]
}

impl Renderer {
    /// A renderer on `device`.
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, options: RendererOptions) -> Self {
        let options = RendererOptions {
            msaa: if options.msaa >= 4 { 4 } else { 1 },
            ..options
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene.wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("shaders/scene.wgsl"))),
        });
        let uniform_size = NonZeroU64::new(std::mem::size_of::<DrawUniform>() as u64);
        let draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("draw"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: uniform_size,
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let face_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("faces"),
            bind_group_layouts: &[Some(&draw_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let edge_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("edges"),
            bind_group_layouts: &[Some(&draw_layout)],
            immediate_size: 0,
        });
        let pipelines = build_pipelines(&device, &shader, &face_layout, &edge_layout, options.msaa);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let white = texture_group(
            &device,
            &queue,
            &texture_layout,
            &sampler,
            1,
            1,
            &[255, 255, 255, 255],
        );
        let uniform_capacity = 64 * UNIFORM_STRIDE;
        let uniforms = uniform_buffer(&device, uniform_capacity);
        let uniform_group = uniform_group(&device, &draw_layout, &uniforms);
        Renderer {
            device,
            queue,
            options,
            draw_layout,
            texture_layout,
            pipelines,
            sampler,
            white,
            textures: BTreeMap::new(),
            meshes: BTreeMap::new(),
            uniforms,
            uniform_capacity,
            uniform_group,
            targets: None,
            last_items: Vec::new(),
            ids_valid: false,
        }
    }

    /// The options in use.
    pub fn options(&self) -> RendererOptions {
        self.options
    }

    /// Changes how many physical pixels a logical pixel has.
    pub fn set_pixels_per_point(&mut self, pixels_per_point: f32) {
        self.options.pixels_per_point = pixels_per_point.max(0.25);
    }

    /// Drops the GPU buffers of the containers `deltas` touch (docs/01 §5);
    /// document-wide changes drop everything.
    pub fn apply_deltas(&mut self, deltas: &[Delta]) {
        for delta in deltas {
            match delta.container() {
                Some(c) => {
                    self.meshes.remove(&c);
                }
                None => {
                    self.meshes.clear();
                    self.textures.clear();
                }
            }
        }
    }

    /// Drops one container's buffers (it changed, or a preview drew another
    /// version of it).
    pub fn invalidate(&mut self, container: ContainerRef) {
        self.meshes.remove(&container);
    }

    /// Drops every cached buffer and texture (another document).
    pub fn clear_cache(&mut self) {
        self.meshes.clear();
        self.textures.clear();
        self.last_items.clear();
        self.ids_valid = false;
    }

    /// The resolved color of the last frame (an `Rgba8UnormSrgb` view:
    /// sampling it gives linear values).
    pub fn color_view(&self) -> Option<&wgpu::TextureView> {
        self.targets.as_ref().map(|t| &t.color_view)
    }

    /// The same pixels viewed as `Rgba8Unorm`: sampling gives the
    /// gamma-encoded values, which is what egui expects of a texture.
    pub fn display_view(&self) -> Option<&wgpu::TextureView> {
        self.targets.as_ref().map(|t| &t.display_view)
    }

    /// The device.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    fn ensure_targets(&mut self, width: u32, height: u32) {
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.width == width && t.height == height)
        {
            return;
        }
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = |label: &str, format, samples, usage| {
            let unorm = [wgpu::TextureFormat::Rgba8Unorm];
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: if format == COLOR_FORMAT && samples == 1 {
                    &unorm
                } else {
                    &[]
                },
            })
        };
        let msaa = self.options.msaa;
        let color = texture(
            "color",
            COLOR_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let color_msaa = (msaa > 1).then(|| {
            texture(
                "color msaa",
                COLOR_FORMAT,
                msaa,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            )
            .create_view(&wgpu::TextureViewDescriptor::default())
        });
        let depth = texture(
            "depth",
            DEPTH_FORMAT,
            msaa,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        )
        .create_view(&wgpu::TextureViewDescriptor::default());
        let ids = texture(
            "ids",
            ID_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let ids_depth = texture(
            "ids depth",
            DEPTH_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        )
        .create_view(&wgpu::TextureViewDescriptor::default());
        self.targets = Some(Targets {
            width,
            height,
            color_msaa,
            color_view: color.create_view(&wgpu::TextureViewDescriptor::default()),
            display_view: color.create_view(&wgpu::TextureViewDescriptor {
                label: Some("color (gamma)"),
                format: Some(wgpu::TextureFormat::Rgba8Unorm),
                ..Default::default()
            }),
            color,
            depth,
            ids_view: ids.create_view(&wgpu::TextureViewDescriptor::default()),
            ids,
            ids_depth,
        });
    }

    fn ensure_mesh(
        &mut self,
        doc: &Document,
        container: ContainerRef,
        show_hidden: bool,
    ) -> Result<(), RenderError> {
        if self
            .meshes
            .get(&container)
            .is_some_and(|m| m.show_hidden == show_hidden)
        {
            return Ok(());
        }
        let mesh = extract(
            doc,
            container,
            ExtractOptions {
                include_hidden: show_hidden,
                smooth_normals: true,
            },
        )?;
        let entities = doc.container(container).ok_or_else(|| {
            RenderError::Extract(planyrith_core::GeometryError::NotFound(format!(
                "container {container:?}"
            )))
        })?;
        let gpu = upload(&self.device, doc, entities.mesh(), &mesh, show_hidden);
        self.meshes.insert(container, gpu);
        Ok(())
    }

    fn ensure_texture(&mut self, doc: &Document, image: ImageRef) -> bool {
        if self.textures.contains_key(&image) {
            return true;
        }
        let Some(data) = doc.images.get(image) else {
            return false;
        };
        let Ok(decoded) = image::load_from_memory(&data.bytes) else {
            tracing::warn!("texture {image:?} could not be decoded");
            return false;
        };
        let rgba = decoded.to_rgba8();
        let max = self.device.limits().max_texture_dimension_2d;
        let rgba = if rgba.width() > max || rgba.height() > max {
            image::imageops::resize(
                &rgba,
                rgba.width().min(max),
                rgba.height().min(max),
                image::imageops::FilterType::Triangle,
            )
        } else {
            rgba
        };
        let group = texture_group(
            &self.device,
            &self.queue,
            &self.texture_layout,
            &self.sampler,
            rgba.width(),
            rgba.height(),
            rgba.as_raw(),
        );
        self.textures.insert(image, group);
        true
    }

    /// Draws a frame into the offscreen targets.
    pub fn render(&mut self, req: &FrameRequest<'_>) -> Result<(), RenderError> {
        let max = self.device.limits().max_texture_dimension_2d;
        if req.width == 0 || req.height == 0 || req.width > max || req.height > max {
            return Err(RenderError::Size {
                width: req.width,
                height: req.height,
                max,
            });
        }
        self.ensure_targets(req.width, req.height);
        let doc = req.doc;
        let style = req.style;
        for item in req.items {
            self.ensure_mesh(doc, item.container, req.show_hidden)?;
        }
        let textured_mode = style.faces.mode == FaceMode::ShadedTextured;
        if textured_mode {
            let mut wanted = Vec::new();
            for item in req.items {
                if let Some(mesh) = self.meshes.get(&item.container) {
                    for (key, _) in &mesh.ranges {
                        if let (_, Some(image)) =
                            face_color(doc, style, *key, item.material_override)
                        {
                            wanted.push(image);
                        }
                    }
                }
            }
            for image in wanted {
                self.ensure_texture(doc, image);
            }
        }

        let camera = Camera {
            aspect: f64::from(req.width) / f64::from(req.height),
            ..*req.camera
        };
        let mut bounds = Aabb::EMPTY;
        for item in req.items {
            if let Some(mesh) = self.meshes.get(&item.container)
                && !mesh.bounds.is_empty()
            {
                bounds = bounds.union(&mesh.bounds.transformed(&item.world));
            }
        }
        // The axes reach a few times past the model and the eye.
        let axis_length = 4.0
            * (camera.eye.length()
                + if bounds.is_empty() {
                    0.0
                } else {
                    bounds.max.distance(bounds.min) + bounds.center().length()
                }
                + 1000.0);
        if req.axes {
            let reach = DVec3::splat(axis_length);
            bounds = bounds.union(&Aabb::new(-reach, reach));
        }
        let depth = camera.fit_depth(&bounds);

        let mode = style.faces.mode;
        let background = linear(style.background.color);
        let selection = linear(style.modeling.selection_color);
        let ppp = self.options.pixels_per_point;
        let edge_width = ppp;
        let profile_width = style.edges.profiles.map_or(0.0, |w| f32::from(w) * ppp);
        let fade_amount = style.modeling.inactive_fade.clamp(0.0, 1.0);

        let mut uniforms: Vec<DrawUniform> = Vec::new();
        let mut opaque: Vec<Draw> = Vec::new();
        let mut transparent: Vec<(f64, Draw)> = Vec::new();
        let mut edges: Vec<Draw> = Vec::new();
        let mut overlay: Vec<Draw> = Vec::new();
        let mut id_draws: Vec<(u32, ContainerRef, Range<u32>, bool)> = Vec::new();
        self.last_items = req
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| (item.container, i))
            .collect();

        for (index, item) in req.items.iter().enumerate() {
            let Some(mesh) = self.meshes.get(&item.container) else {
                continue;
            };
            // Components that always face the camera turn about their blue
            // axis (docs/04 §3.3).
            let world = match item.container {
                ContainerRef::Definition(d)
                    if doc
                        .definition(d)
                        .is_some_and(|def| def.behavior.always_face_camera) =>
                {
                    face_camera(&item.world, &camera)
                }
                _ => item.world,
            };
            let clip = camera.clip_from_local(&world, depth);
            let base = make_uniform(&clip.as_mat4().to_cols_array_2d(), &world, &camera);
            let fade = if item.faded {
                [background[0], background[1], background[2], fade_amount]
            } else {
                [0.0; 4]
            };
            let item_id = index as u32 + 1;
            let distance = item
                .world
                .transform_point3(mesh.bounds.center())
                .distance(camera.eye);

            // Faces.
            for (key, range) in &mesh.ranges {
                let (color, texture) = face_color(doc, style, *key, item.material_override);
                let texture = texture.filter(|t| textured_mode && self.textures.contains_key(t));
                let mut flags = 0;
                if texture.is_some() {
                    flags |= TEXTURED;
                }
                let mut color = color;
                match mode {
                    FaceMode::HiddenLine => {
                        color = [background[0], background[1], background[2], 1.0];
                        flags |= UNLIT;
                    }
                    FaceMode::XRay => color[3] *= 0.5,
                    _ => {}
                }
                let u = DrawUniform {
                    color,
                    fade,
                    ids: [item_id, flags, 0, 0],
                    ..base
                };
                let slot = push(&mut uniforms, u);
                let stage = if mode == FaceMode::Wireframe {
                    Stage::DepthOnly
                } else if color[3] < 0.999 {
                    Stage::Transparent
                } else {
                    Stage::Opaque
                };
                let draw = Draw::Faces {
                    pipeline: stage,
                    uniform: slot,
                    container: item.container,
                    range: range.clone(),
                    texture,
                };
                if stage == Stage::Transparent {
                    transparent.push((distance, draw));
                } else {
                    opaque.push(draw);
                }
                id_draws.push((slot, item.container, range.clone(), false));
            }

            // Edges.
            if mesh.edge_count > 0 && (style.edges.show || mode == FaceMode::Wireframe) {
                let mut flags = 0;
                if !req.show_hidden {
                    flags |= HIDE_SOFT;
                }
                match style.edges.color {
                    EdgeColorMode::All => {}
                    EdgeColorMode::ByMaterial => flags |= BY_MATERIAL,
                    EdgeColorMode::ByAxis => flags |= BY_AXIS,
                }
                let (color, tint) = if item.selected {
                    ([selection[0], selection[1], selection[2], 1.0], [0.0; 4])
                } else {
                    ([0.0, 0.0, 0.0, 1.0], [0.0; 4])
                };
                let width = if item.selected {
                    edge_width * 2.0
                } else {
                    edge_width
                };
                let u = DrawUniform {
                    color,
                    tint,
                    fade,
                    viewport: [req.width as f32, req.height as f32, width, profile_width],
                    ids: [item_id, flags, 0, 0],
                    ..base
                };
                let slot = push(&mut uniforms, u);
                edges.push(Draw::Edges {
                    pipeline: Stage::Edge,
                    uniform: slot,
                    container: item.container,
                });
                id_draws.push((slot, item.container, 0..mesh.edge_count, true));
                if style.edges.back_edges || mode == FaceMode::Wireframe || mode == FaceMode::XRay {
                    let u = DrawUniform {
                        ids: [item_id, flags | DASHED, 0, 0],
                        color: [color[0], color[1], color[2], 0.6],
                        ..u
                    };
                    let slot = push(&mut uniforms, u);
                    edges.push(Draw::Edges {
                        pipeline: Stage::BackEdge,
                        uniform: slot,
                        container: item.container,
                    });
                }
            }

            // Selection overlay: tinted faces, thick selected edges.
            let tint_faces: Vec<FaceId> = if item.selected {
                Vec::new()
            } else {
                item.selected_faces.clone()
            };
            if !tint_faces.is_empty() {
                let mut indices = Vec::new();
                for f in &tint_faces {
                    if let Some(local) = mesh.face_lookup.iter().position(|x| x == f) {
                        indices.extend_from_slice(&mesh.face_triangles[local]);
                    }
                }
                if !indices.is_empty() {
                    let u = DrawUniform {
                        color: [selection[0], selection[1], selection[2], 0.35],
                        ids: [item_id, UNLIT, 0, 0],
                        ..base
                    };
                    let slot = push(&mut uniforms, u);
                    let buffer =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("selected faces"),
                                contents: bytemuck::cast_slice(&indices),
                                usage: wgpu::BufferUsages::INDEX,
                            });
                    overlay.push(Draw::Overlay {
                        uniform: slot,
                        container: item.container,
                        buffer,
                        count: indices.len() as u32,
                    });
                }
            }
            let mut selected_edges: Vec<EdgeInstance> = Vec::new();
            if !item.selected {
                for e in &item.selected_edges {
                    if let Some(local) = mesh.edge_lookup.iter().position(|x| x == e) {
                        selected_edges.push(mesh.edge_instances[local]);
                    }
                }
                // A selected face shows its boundary thick too (docs/06 §9).
                for f in &tint_faces {
                    if let Some(local) = mesh.face_lookup.iter().position(|x| x == f) {
                        let local = local as u32;
                        selected_edges.extend(
                            mesh.edge_instances
                                .iter()
                                .filter(|inst| edge_touches(inst, local, mesh))
                                .copied(),
                        );
                    }
                }
            }
            if !selected_edges.is_empty() {
                let u = DrawUniform {
                    color: [selection[0], selection[1], selection[2], 1.0],
                    viewport: [req.width as f32, req.height as f32, edge_width * 3.0, 0.0],
                    ids: [item_id, 0, 0, 0],
                    ..base
                };
                let slot = push(&mut uniforms, u);
                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("selected edges"),
                        contents: bytemuck::cast_slice(&selected_edges),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                overlay.push(Draw::EdgeList {
                    uniform: slot,
                    buffer,
                    count: selected_edges.len() as u32,
                });
            }
        }
        // The drawing axes: solid on the positive side, dashed on the
        // negative side, colored by axis, hidden behind faces.
        if req.axes {
            let clip = camera.clip_from_local(&DAffine3::IDENTITY, depth);
            let base = make_uniform(
                &clip.as_mat4().to_cols_array_2d(),
                &DAffine3::IDENTITY,
                &camera,
            );
            // Segments are cut to the view (four times the screen, in front
            // of the eye) here, in f64: a segment reaching far off screen
            // becomes a quad too long and thin for every rasterizer to clip
            // exactly (software adapters drop it).
            let clip_f64 = camera.clip_from_local(&DAffine3::IDENTITY, depth);
            let clip_to_view = |a: DVec3, b: DVec3| -> Option<(DVec3, DVec3)> {
                let (ca, cb) = (clip_f64 * a.extend(1.0), clip_f64 * b.extend(1.0));
                let guard = 4.0;
                let tests = |c: glam::DVec4| {
                    [
                        c.w - 1e-3,
                        guard * c.w - c.x,
                        guard * c.w + c.x,
                        guard * c.w - c.y,
                        guard * c.w + c.y,
                    ]
                };
                let (fa, fb) = (tests(ca), tests(cb));
                let (mut t0, mut t1) = (0.0f64, 1.0f64);
                for (va, vb) in fa.iter().zip(fb.iter()) {
                    match (*va >= 0.0, *vb >= 0.0) {
                        (true, true) => {}
                        (false, false) => return None,
                        (true, false) => t1 = t1.min(va / (va - vb)),
                        (false, true) => t0 = t0.max(va / (va - vb)),
                    }
                }
                (t0 < t1).then(|| (a + (b - a) * t0, a + (b - a) * t1))
            };
            let segments = |sign: f64| -> Vec<EdgeInstance> {
                [DVec3::X, DVec3::Y, DVec3::Z]
                    .iter()
                    .filter_map(|d| clip_to_view(DVec3::ZERO, *d * sign * axis_length))
                    .map(|(a, b)| EdgeInstance {
                        a: a.as_vec3().to_array(),
                        b: b.as_vec3().to_array(),
                        n0: [0.0; 3],
                        n1: [0.0; 3],
                        flags: 0,
                        index: 0,
                        material_color: [0.0; 4],
                    })
                    .collect()
            };
            for (sign, flags) in [(1.0, BY_AXIS), (-1.0, BY_AXIS | DASHED)] {
                let u = DrawUniform {
                    color: [0.0, 0.0, 0.0, 1.0],
                    viewport: [req.width as f32, req.height as f32, 1.5 * ppp, 0.0],
                    ids: [0, flags, 0, 0],
                    ..base
                };
                let slot = push(&mut uniforms, u);
                let instances = segments(sign);
                if instances.is_empty() {
                    continue;
                }
                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("axes"),
                        contents: bytemuck::cast_slice(&instances),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                edges.push(Draw::EdgeList {
                    uniform: slot,
                    buffer,
                    count: instances.len() as u32,
                });
            }
        }
        // The sky (style background) behind everything.
        let mut sky = None;
        if let Some(sky_color) = style.background.sky
            && !req.transparent_background
        {
            let tan_half = match camera.projection {
                Projection::Perspective { fov_y_deg } | Projection::TwoPoint { fov_y_deg } => {
                    (fov_y_deg.to_radians() * 0.5).tan()
                }
                // Parallel views have one ray direction; the gradient spreads
                // as in a 35° perspective.
                Projection::Parallel { .. } => (35.0f64.to_radians() * 0.5).tan(),
            };
            let forward = camera.forward();
            let right = camera.right() * tan_half * camera.aspect;
            let up = camera.screen_up() * tan_half;
            let ground = style.background.ground.map_or([0.0; 4], |(c, opacity)| {
                let c = linear(c);
                [c[0], c[1], c[2], opacity.clamp(0.0, 1.0)]
            });
            let u = DrawUniform {
                color: linear(sky_color),
                fade: background,
                tint: ground,
                light: [forward.x as f32, forward.y as f32, forward.z as f32, 0.0],
                normal_x: [right.x as f32, right.y as f32, right.z as f32, 0.0],
                normal_y: [up.x as f32, up.y as f32, up.z as f32, 0.0],
                ..DrawUniform::zeroed()
            };
            sky = Some(Draw::Sky {
                uniform: push(&mut uniforms, u),
            });
        }
        // Watermarks, behind the model or over it (style).
        let mut behind = Vec::new();
        let mut over = Vec::new();
        if !req.transparent_background {
            for w in &style.watermarks {
                if !self.ensure_texture(doc, w.image) {
                    continue;
                }
                let Some(data) = doc.images.get(w.image) else {
                    continue;
                };
                let (ndc, uv) = watermark_rect(
                    w,
                    f64::from(data.width) / f64::from(data.height.max(1)),
                    f64::from(req.width),
                    f64::from(req.height),
                );
                let u = DrawUniform {
                    viewport: ndc,
                    tint: uv,
                    color: [1.0, 1.0, 1.0, w.opacity.clamp(0.0, 1.0)],
                    ..DrawUniform::zeroed()
                };
                let draw = Draw::Mark {
                    uniform: push(&mut uniforms, u),
                    image: w.image,
                };
                if w.behind_model {
                    behind.push(draw);
                } else {
                    over.push(draw);
                }
            }
        }
        // Back to front (docs/06 §3.1 step 6: per draw item).
        transparent.sort_by(|a, b| b.0.total_cmp(&a.0));

        // Fog (style distances, document switch) on every face and edge.
        if let (true, Some(fog)) = (doc.rendering.fog_enabled, style.fog) {
            let color = if fog.use_background_color {
                background
            } else {
                linear(fog.color)
            };
            let mode = if matches!(camera.projection, Projection::Parallel { .. }) {
                2.0
            } else {
                1.0
            };
            for u in &mut uniforms {
                u.fog = [
                    fog.start as f32,
                    fog.end as f32,
                    depth.near as f32,
                    depth.far as f32,
                ];
                u.fog_color = [color[0], color[1], color[2], mode];
            }
        }

        self.upload_uniforms(&uniforms);
        let Some(targets) = self.targets.as_ref() else {
            return Ok(());
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            let (view, resolve) = match &targets.color_msaa {
                Some(msaa) => (msaa, Some(&targets.color_view)),
                None => (&targets.color_view, None),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: resolve,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(background[0]),
                            g: f64::from(background[1]),
                            b: f64::from(background[2]),
                            a: if req.transparent_background { 0.0 } else { 1.0 },
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for draw in sky
                .iter()
                .chain(behind.iter())
                .chain(opaque.iter())
                .chain(edges.iter())
                .chain(transparent.iter().map(|(_, d)| d))
                .chain(overlay.iter())
                .chain(over.iter())
            {
                self.encode(&mut pass, draw);
            }
        }
        self.ids_valid = req.ids;
        if req.ids {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ids"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.ids_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.ids_depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for (slot, container, range, is_edges) in &id_draws {
                let Some(mesh) = self.meshes.get(container) else {
                    continue;
                };
                let offset = [*slot * UNIFORM_STRIDE as u32];
                if *is_edges {
                    let Some(buffer) = &mesh.edges else { continue };
                    pass.set_pipeline(&self.pipelines.edge_id);
                    pass.set_bind_group(0, &self.uniform_group, &offset);
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw(0..6, range.clone());
                } else {
                    let (Some(vb), Some(ib)) = (&mesh.vertices, &mesh.indices) else {
                        continue;
                    };
                    pass.set_pipeline(&self.pipelines.face_id);
                    pass.set_bind_group(0, &self.uniform_group, &offset);
                    pass.set_bind_group(1, &self.white, &[]);
                    pass.set_vertex_buffer(0, vb.slice(..));
                    pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(range.clone(), 0, 0..1);
                }
            }
        }
        self.queue.submit([encoder.finish()]);
        Ok(())
    }

    fn encode(&self, pass: &mut wgpu::RenderPass<'_>, draw: &Draw) {
        match draw {
            Draw::Faces {
                pipeline,
                uniform,
                container,
                range,
                texture,
            } => {
                let Some(mesh) = self.meshes.get(container) else {
                    return;
                };
                let (Some(vb), Some(ib)) = (&mesh.vertices, &mesh.indices) else {
                    return;
                };
                pass.set_pipeline(match pipeline {
                    Stage::Transparent => &self.pipelines.face_transparent,
                    Stage::DepthOnly => &self.pipelines.face_depth_only,
                    _ => &self.pipelines.face_opaque,
                });
                pass.set_bind_group(0, &self.uniform_group, &[uniform * UNIFORM_STRIDE as u32]);
                let group = texture
                    .and_then(|t| self.textures.get(&t))
                    .unwrap_or(&self.white);
                pass.set_bind_group(1, group, &[]);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(range.clone(), 0, 0..1);
            }
            Draw::Overlay {
                uniform,
                container,
                buffer,
                count,
            } => {
                // The overlay indexes the vertices of the item's container.
                let Some(vb) = self.meshes.get(container).and_then(|m| m.vertices.as_ref()) else {
                    return;
                };
                pass.set_pipeline(&self.pipelines.face_overlay);
                pass.set_bind_group(0, &self.uniform_group, &[uniform * UNIFORM_STRIDE as u32]);
                pass.set_bind_group(1, &self.white, &[]);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..*count, 0, 0..1);
            }
            Draw::Edges {
                pipeline,
                uniform,
                container,
            } => {
                let Some(mesh) = self.meshes.get(container) else {
                    return;
                };
                let Some(buffer) = &mesh.edges else { return };
                pass.set_pipeline(if *pipeline == Stage::BackEdge {
                    &self.pipelines.edge_back
                } else {
                    &self.pipelines.edge
                });
                pass.set_bind_group(0, &self.uniform_group, &[uniform * UNIFORM_STRIDE as u32]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..6, 0..mesh.edge_count);
            }
            Draw::EdgeList {
                uniform,
                buffer,
                count,
            } => {
                pass.set_pipeline(&self.pipelines.edge);
                pass.set_bind_group(0, &self.uniform_group, &[uniform * UNIFORM_STRIDE as u32]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..6, 0..*count);
            }
            Draw::Sky { uniform } => {
                pass.set_pipeline(&self.pipelines.sky);
                pass.set_bind_group(0, &self.uniform_group, &[uniform * UNIFORM_STRIDE as u32]);
                pass.draw(0..3, 0..1);
            }
            Draw::Mark { uniform, image } => {
                let Some(group) = self.textures.get(image) else {
                    return;
                };
                pass.set_pipeline(&self.pipelines.mark);
                pass.set_bind_group(0, &self.uniform_group, &[uniform * UNIFORM_STRIDE as u32]);
                pass.set_bind_group(1, group, &[]);
                pass.draw(0..6, 0..1);
            }
        }
    }

    fn upload_uniforms(&mut self, uniforms: &[DrawUniform]) {
        let needed = (uniforms.len().max(1) as u64) * UNIFORM_STRIDE;
        if needed > self.uniform_capacity {
            self.uniform_capacity = needed.next_power_of_two();
            self.uniforms = uniform_buffer(&self.device, self.uniform_capacity);
            self.uniform_group = uniform_group(&self.device, &self.draw_layout, &self.uniforms);
        }
        let mut bytes = vec![0u8; needed as usize];
        for (i, u) in uniforms.iter().enumerate() {
            let at = i * UNIFORM_STRIDE as usize;
            bytes[at..at + std::mem::size_of::<DrawUniform>()]
                .copy_from_slice(bytemuck::bytes_of(u));
        }
        self.queue.write_buffer(&self.uniforms, 0, &bytes);
    }

    /// Reads the last frame back as an sRGB image (headless output,
    /// docs/06 §11). Blocks until the GPU is done.
    pub fn read_color(&self) -> Result<image::RgbaImage, RenderError> {
        let targets = self
            .targets
            .as_ref()
            .ok_or_else(|| RenderError::Readback("nothing was rendered".into()))?;
        let bytes = self.read_texture(&targets.color, targets.width, targets.height, 4)?;
        image::RgbaImage::from_raw(targets.width, targets.height, bytes)
            .ok_or_else(|| RenderError::Readback("image size mismatch".into()))
    }

    /// The element under pixel `(x, y)` of the last frame's id buffer, or
    /// the nearest one within `radius` pixels (docs/06 §8: edges and faces
    /// near the cursor). Needs a frame rendered with `ids`. Blocks until the
    /// GPU is done.
    pub fn pick(&self, x: u32, y: u32, radius: u32) -> Result<Option<PickHit>, RenderError> {
        let Some(targets) = self.targets.as_ref() else {
            return Ok(None);
        };
        if !self.ids_valid || x >= targets.width || y >= targets.height {
            return Ok(None);
        }
        let x0 = x.saturating_sub(radius);
        let y0 = y.saturating_sub(radius);
        let x1 = (x + radius + 1).min(targets.width);
        let y1 = (y + radius + 1).min(targets.height);
        let ids = self.read_region(&targets.ids, x0, y0, x1 - x0, y1 - y0, 8)?;
        let w = (x1 - x0) as usize;
        let mut best: Option<(u64, bool, [u32; 2])> = None;
        for (i, px) in ids.as_chunks::<8>().0.iter().enumerate() {
            let item = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
            let element = u32::from_le_bytes([px[4], px[5], px[6], px[7]]);
            if item == 0 || element == 0 {
                continue;
            }
            let (px_x, px_y) = (x0 as i64 + (i % w) as i64, y0 as i64 + (i / w) as i64);
            let d = ((px_x - x as i64).pow(2) + (px_y - y as i64).pow(2)) as u64;
            let is_edge = element & 0x8000_0000 != 0;
            // Edges within the radius beat faces (docs/06 §8), then distance.
            let better = match best {
                None => true,
                Some((bd, be, _)) => (is_edge && !be) || (is_edge == be && d < bd),
            };
            if better {
                best = Some((d, is_edge, [item, element]));
            }
        }
        let Some((_, is_edge, [item, element])) = best else {
            return Ok(None);
        };
        let index = item as usize - 1;
        let Some(&(container, _)) = self.last_items.get(index) else {
            return Ok(None);
        };
        let Some(mesh) = self.meshes.get(&container) else {
            return Ok(None);
        };
        let local = (element & 0x7FFF_FFFF) as usize - 1;
        let kind = if is_edge {
            mesh.edge_lookup.get(local).map(|e| PickKind::Edge(*e))
        } else {
            mesh.face_lookup.get(local).map(|f| PickKind::Face(*f))
        };
        Ok(kind.map(|kind| PickHit { item: index, kind }))
    }

    fn read_texture(
        &self,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
    ) -> Result<Vec<u8>, RenderError> {
        self.read_region(texture, 0, 0, width, height, bytes_per_pixel)
    }

    fn read_region(
        &self,
        texture: &wgpu::Texture,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
    ) -> Result<Vec<u8>, RenderError> {
        let unpadded = width * bytes_per_pixel;
        let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|e| RenderError::Readback(e.to_string()))?
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        let mapped = buffer
            .get_mapped_range(..)
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        let mut out = Vec::with_capacity((unpadded * height) as usize);
        for row in mapped.chunks_exact(padded as usize) {
            out.extend_from_slice(&row[..unpadded as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(out)
    }
}

/// `true` when an edge instance borders local face `face`.
fn edge_touches(inst: &EdgeInstance, face: u32, mesh: &GpuMesh) -> bool {
    let local = inst.index as usize;
    mesh.edge_faces
        .get(local)
        .is_some_and(|[a, b]| *a == face || *b == face)
}

fn push(uniforms: &mut Vec<DrawUniform>, u: DrawUniform) -> u32 {
    uniforms.push(u);
    (uniforms.len() - 1) as u32
}

/// The uniform fields that depend on the occurrence and camera only.
fn make_uniform(clip: &[[f32; 4]; 4], world: &DAffine3, camera: &Camera) -> DrawUniform {
    // Inverse transpose of the linear part (normals under non-uniform scale).
    let linear = world.matrix3;
    let normal = if linear.determinant().abs() > f64::MIN_POSITIVE {
        linear.inverse().transpose()
    } else {
        DMat3::IDENTITY
    };
    let inverse = world.inverse();
    let local_eye = match camera.projection {
        Projection::Parallel { .. } => f32x4(inverse.transform_vector3(-camera.forward()), 0.0),
        _ => f32x4(inverse.transform_point3(camera.eye), 1.0),
    };
    let light = (-camera.forward() * 0.8 + camera.screen_up() * 0.45 - camera.right() * 0.4)
        .normalize_or_zero();
    DrawUniform {
        clip_from_local: *clip,
        normal_x: f32x4(normal.x_axis, 0.0),
        normal_y: f32x4(normal.y_axis, 0.0),
        normal_z: f32x4(normal.z_axis, 0.0),
        color: [1.0; 4],
        tint: [0.0; 4],
        light: f32x4(light, 0.35),
        fade: [0.0; 4],
        viewport: [0.0; 4],
        local_eye,
        ids: [0; 4],
        fog: [0.0; 4],
        fog_color: [0.0; 4],
    }
}

/// `world` turned about its blue axis (through its origin) so that its
/// front — the definition's −green direction — faces the eye.
fn face_camera(world: &DAffine3, camera: &Camera) -> DAffine3 {
    let Some(axis) = world.matrix3.z_axis.try_normalize() else {
        return *world;
    };
    let flat = |v: DVec3| (v - axis * v.dot(axis)).try_normalize();
    let front = flat(-world.matrix3.y_axis);
    let toward = flat(camera.eye - world.translation);
    match (front, toward) {
        (Some(f), Some(t)) => {
            let angle = f.cross(t).dot(axis).atan2(f.dot(t));
            let turn = DMat3::from_axis_angle(axis, angle);
            DAffine3 {
                matrix3: turn * world.matrix3,
                translation: world.translation,
            }
        }
        _ => *world,
    }
}

/// A watermark's screen rectangle (NDC x0, y0, x1, y1) and texture
/// coordinates (u0, v0, u1, v1, v0 at the top) for an image of `aspect`
/// (width / height) in a `width` × `height` pixel target. Sizes are the
/// style's `scale` times the target height.
fn watermark_rect(w: &Watermark, aspect: f64, width: f64, height: f64) -> ([f32; 4], [f32; 4]) {
    let h = f64::from(w.scale) * height;
    let wd = h * aspect;
    let to_ndc = |x0: f64, y0: f64, x1: f64, y1: f64| {
        [
            (2.0 * x0 / width - 1.0) as f32,
            (1.0 - 2.0 * y1 / height) as f32,
            (2.0 * x1 / width - 1.0) as f32,
            (1.0 - 2.0 * y0 / height) as f32,
        ]
    };
    match w.placement {
        WatermarkPlacement::Stretched => ([-1.0, -1.0, 1.0, 1.0], [0.0, 0.0, 1.0, 1.0]),
        WatermarkPlacement::Tiled => (
            [-1.0, -1.0, 1.0, 1.0],
            [
                0.0,
                0.0,
                (width / wd.max(1.0)) as f32,
                (height / h.max(1.0)) as f32,
            ],
        ),
        WatermarkPlacement::Centered => {
            let (x0, y0) = ((width - wd) * 0.5, (height - h) * 0.5);
            (to_ndc(x0, y0, x0 + wd, y0 + h), [0.0, 0.0, 1.0, 1.0])
        }
        WatermarkPlacement::Anchored(ax, ay) => {
            let margin = 0.02 * height;
            let x0 = margin + f64::from(ax.clamp(0.0, 1.0)) * (width - wd - 2.0 * margin);
            let y0 = margin + f64::from(ay.clamp(0.0, 1.0)) * (height - h - 2.0 * margin);
            (to_ndc(x0, y0, x0 + wd, y0 + h), [0.0, 0.0, 1.0, 1.0])
        }
    }
}

/// The linear color and texture of a material range (docs/06 §4): the
/// face's material, else the instance override, else the default for the
/// side; Monochrome uses the style's defaults.
fn face_color(
    doc: &Document,
    style: &Style,
    key: MaterialKey,
    material_override: Option<MaterialId>,
) -> ([f32; 4], Option<ImageRef>) {
    if style.faces.mode == FaceMode::Monochrome {
        let c = match key.side {
            FrontOrBack::Front => style.faces.front_default,
            FrontOrBack::Back => style.faces.back_default,
        };
        return (linear(c), None);
    }
    let id = key.material.or(material_override);
    let resolved = doc.materials.resolve(id, key.side);
    match doc.materials.get(resolved) {
        Some(m) => {
            let mut c = linear(m.color);
            c[3] = m.opacity.clamp(0.0, 1.0);
            (c, m.texture.as_ref().map(|t| t.image))
        }
        None => (linear(Rgba8::WHITE), None),
    }
}

/// Uploads one container's buffers (`source` is the container's mesh).
fn upload(
    device: &wgpu::Device,
    doc: &Document,
    source: &HalfEdgeMesh,
    mesh: &RenderMesh,
    show_hidden: bool,
) -> GpuMesh {
    let vertices: Vec<FaceVertex> = (0..mesh.positions.len())
        .map(|i| FaceVertex {
            position: mesh.positions[i],
            normal: mesh.normals[i],
            uv: mesh.uvs[i],
            face: mesh.face_ids[i],
        })
        .collect();
    let vertex_buffer = (!vertices.is_empty()).then(|| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("faces"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        })
    });
    let index_buffer = (!mesh.indices.is_empty()).then(|| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("face indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        })
    });
    // Front triangles per face for the selection overlay.
    let mut face_triangles = vec![Vec::new(); mesh.face_lookup.len()];
    for (key, range) in &mesh.indices_by_material {
        if key.side != FrontOrBack::Front {
            continue;
        }
        for t in mesh.indices[range.start as usize..range.end as usize]
            .as_chunks::<3>()
            .0
        {
            let face = mesh.face_ids[t[0] as usize] as usize;
            if let Some(list) = face_triangles.get_mut(face) {
                list.extend_from_slice(t);
            }
        }
    }
    let normal = |f: u32| {
        if f == NO_FACE {
            [0.0; 3]
        } else {
            mesh.face_normals[f as usize]
        }
    };
    let edge_instances: Vec<EdgeInstance> = (0..mesh.edge_lookup.len())
        .map(|i| {
            let [f0, f1] = mesh.edges.faces[i];
            let mut flags = u32::from(mesh.edges.flags[i]);
            if f0 != NO_FACE {
                flags |= EDGE_HAS_FACE0;
            }
            if f1 != NO_FACE {
                flags |= EDGE_HAS_FACE1;
            }
            let material_color = [f0, f1]
                .into_iter()
                .find(|f| *f != NO_FACE)
                .and_then(|f| {
                    let face = source
                        .face(mesh.face_lookup[f as usize])
                        .map(|face| face.sides.material_front)?;
                    doc.materials
                        .get(doc.materials.resolve(face, FrontOrBack::Front))
                        .map(|m| linear(m.color))
                })
                .unwrap_or([0.0, 0.0, 0.0, 1.0]);
            EdgeInstance {
                a: mesh.edges.positions[2 * i],
                b: mesh.edges.positions[2 * i + 1],
                n0: normal(f0),
                n1: normal(f1),
                flags,
                index: i as u32,
                material_color,
            }
        })
        .collect();
    let edge_buffer = (!edge_instances.is_empty()).then(|| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("edges"),
            contents: bytemuck::cast_slice(&edge_instances),
            usage: wgpu::BufferUsages::VERTEX,
        })
    });
    GpuMesh {
        vertices: vertex_buffer,
        indices: index_buffer,
        ranges: mesh.indices_by_material.clone(),
        edges: edge_buffer,
        edge_count: edge_instances.len() as u32,
        bounds: mesh.bounds,
        face_triangles,
        face_lookup: mesh.face_lookup.clone(),
        edge_lookup: mesh.edge_lookup.clone(),
        edge_instances,
        edge_faces: mesh.edges.faces.clone(),
        show_hidden,
    }
}

fn uniform_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("draw uniforms"),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn uniform_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("draw uniforms"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: NonZeroU64::new(std::mem::size_of::<DrawUniform>() as u64),
            }),
        }],
    })
}

fn texture_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> wgpu::BindGroup {
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("material texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        rgba,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("material texture"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

fn build_pipelines(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    face_layout: &wgpu::PipelineLayout,
    edge_layout: &wgpu::PipelineLayout,
    msaa: u32,
) -> Pipelines {
    let face_buffers = [Some(wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<FaceVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Uint32],
    })];
    let edge_buffers = [Some(wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<EdgeInstance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &wgpu::vertex_attr_array![
            0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Float32x3,
            4 => Uint32, 5 => Uint32, 6 => Float32x4
        ],
    })];
    let depth = |write: bool, compare: wgpu::CompareFunction| {
        Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(compare),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        })
    };
    let pipeline = |label: &str,
                    layout: &wgpu::PipelineLayout,
                    vs: &str,
                    fs: &str,
                    buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                    cull: Option<wgpu::Face>,
                    target: wgpu::ColorTargetState,
                    depth_stencil: Option<wgpu::DepthStencilState>,
                    samples: u32| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some(vs),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers,
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: cull,
                ..Default::default()
            },
            depth_stencil,
            multisample: wgpu::MultisampleState {
                count: samples,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some(fs),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(target)],
            }),
            multiview_mask: None,
            cache: None,
        })
    };
    let color =
        |blend: Option<wgpu::BlendState>, write: wgpu::ColorWrites| wgpu::ColorTargetState {
            format: COLOR_FORMAT,
            blend,
            write_mask: write,
        };
    let id_target = wgpu::ColorTargetState {
        format: ID_FORMAT,
        blend: None,
        write_mask: wgpu::ColorWrites::ALL,
    };
    use wgpu::CompareFunction::{Greater, GreaterEqual, Less};
    let alpha = Some(wgpu::BlendState::ALPHA_BLENDING);
    let all = wgpu::ColorWrites::ALL;
    Pipelines {
        face_opaque: pipeline(
            "faces",
            face_layout,
            "vs_face",
            "fs_face",
            &face_buffers,
            Some(wgpu::Face::Back),
            color(None, all),
            depth(true, Greater),
            msaa,
        ),
        face_transparent: pipeline(
            "transparent faces",
            face_layout,
            "vs_face",
            "fs_face",
            &face_buffers,
            Some(wgpu::Face::Back),
            color(alpha, all),
            depth(false, Greater),
            msaa,
        ),
        face_depth_only: pipeline(
            "face depth",
            face_layout,
            "vs_face",
            "fs_face",
            &face_buffers,
            Some(wgpu::Face::Back),
            color(None, wgpu::ColorWrites::empty()),
            depth(true, Greater),
            msaa,
        ),
        face_overlay: pipeline(
            "selected faces",
            face_layout,
            "vs_face",
            "fs_face",
            &face_buffers,
            Some(wgpu::Face::Back),
            color(alpha, all),
            depth(false, GreaterEqual),
            msaa,
        ),
        edge: pipeline(
            "edges",
            edge_layout,
            "vs_edge",
            "fs_edge",
            &edge_buffers,
            None,
            color(alpha, all),
            depth(false, GreaterEqual),
            msaa,
        ),
        edge_back: pipeline(
            "back edges",
            edge_layout,
            "vs_edge",
            "fs_edge",
            &edge_buffers,
            None,
            color(alpha, all),
            depth(false, Less),
            msaa,
        ),
        face_id: pipeline(
            "face ids",
            face_layout,
            "vs_face",
            "fs_face_id",
            &face_buffers,
            Some(wgpu::Face::Back),
            id_target.clone(),
            depth(true, Greater),
            1,
        ),
        edge_id: pipeline(
            "edge ids",
            edge_layout,
            "vs_edge",
            "fs_edge_id",
            &edge_buffers,
            None,
            id_target,
            depth(false, GreaterEqual),
            1,
        ),
        sky: pipeline(
            "sky",
            edge_layout,
            "vs_sky",
            "fs_sky",
            &[],
            None,
            color(None, all),
            depth(false, wgpu::CompareFunction::Always),
            msaa,
        ),
        mark: pipeline(
            "watermarks",
            face_layout,
            "vs_mark",
            "fs_mark",
            &[],
            None,
            color(alpha, all),
            depth(false, wgpu::CompareFunction::Always),
            msaa,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn billboards_turn_their_front_to_the_eye() {
        let camera = Camera {
            eye: DVec3::new(5000.0, 0.0, 1700.0),
            target: DVec3::ZERO,
            ..Camera::default()
        };
        let world = DAffine3::from_translation(DVec3::new(100.0, 0.0, 0.0));
        let turned = face_camera(&world, &camera);
        // The front (−green) now points at the eye, level.
        let front = -turned.matrix3.y_axis;
        assert!(front.distance(DVec3::X) < 1e-9, "{front}");
        assert!(turned.matrix3.z_axis.distance(DVec3::Z) < 1e-9);
        assert_eq!(turned.translation, world.translation);
    }

    #[test]
    fn watermarks_anchor_inside_a_margin() {
        let w = Watermark {
            image: ImageRef::from_bytes(b"x"),
            placement: WatermarkPlacement::Anchored(1.0, 1.0),
            opacity: 1.0,
            scale: 0.1,
            behind_model: false,
        };
        // A 2:1 image, 10 % of a 1000 px high target: 200 × 100 px.
        let (ndc, uv) = watermark_rect(&w, 2.0, 2000.0, 1000.0);
        let px = |x: f32| (f64::from(x) + 1.0) * 0.5 * 2000.0;
        assert!((px(ndc[2]) - (2000.0 - 20.0)).abs() < 1e-3, "{ndc:?}");
        assert!((px(ndc[2]) - px(ndc[0]) - 200.0).abs() < 1e-3);
        assert_eq!(uv, [0.0, 0.0, 1.0, 1.0]);
    }
}
