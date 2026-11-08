//! A progressive path tracer for final images (docs/06 §11.1), on the CPU
//! so it runs anywhere the CLI does. The visible model (smooth normals,
//! texture coordinates and materials as the viewport extracts them) and a
//! ground plane under it are traced with the sun (next-event estimation,
//! a slightly soft disc), light from the sky, diffuse and microfacet
//! bounces after each material's roughness and metalness, and see-through
//! materials. Each pixel's random numbers depend only on the pixel and the
//! sample, so the image is the same whatever the thread count.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use glam::{DVec2, DVec3};
use planyrith_core::document::{
    Camera, Document, EditContext, FaceMode, FrontOrBack, ImageRef, Material, Rgba8, Selection,
    Style,
};
use planyrith_core::ids::MaterialId;
use planyrith_core::io::render_mesh::{ExtractOptions, MaterialKey, draw_list, extract};

/// What to trace.
#[derive(Debug, Clone, PartialEq)]
pub struct PathTraceOptions {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Paths per pixel.
    pub samples: u32,
    /// Bounces after the first hit.
    pub bounces: u32,
    /// Camera (the document's when `None`).
    pub camera: Option<Camera>,
    /// Style for the background, sky, ground and exposure (the active one
    /// when `None`).
    pub style: Option<Style>,
    /// Worker threads (0: as many as the machine has).
    pub threads: usize,
}

impl Default for PathTraceOptions {
    fn default() -> Self {
        PathTraceOptions {
            width: 1280,
            height: 720,
            samples: 32,
            bounces: 4,
            camera: None,
            style: None,
            threads: 0,
        }
    }
}

/// A surface's look.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Surface {
    pub(crate) albedo: DVec3,
    pub(crate) roughness: f64,
    pub(crate) metallic: f64,
    pub(crate) opacity: f64,
    /// Index into the scene's textures; the albedo then tints it.
    pub(crate) texture: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Triangle {
    pub(crate) p: [DVec3; 3],
    pub(crate) n: [DVec3; 3],
    pub(crate) uv: [DVec2; 3],
    pub(crate) surface: u32,
}

/// A decoded texture, linear RGB.
pub(crate) struct Texture {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) texels: Vec<DVec3>,
}

impl Texture {
    fn sample(&self, uv: DVec2) -> DVec3 {
        let u = uv.x - uv.x.floor();
        let v = 1.0 - (uv.y - uv.y.floor());
        let x = ((u * self.width as f64) as usize).min(self.width - 1);
        let y = ((v * self.height as f64) as usize).min(self.height - 1);
        self.texels[y * self.width + x]
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Node {
    pub(crate) lo: DVec3,
    pub(crate) hi: DVec3,
    /// Leaf: first triangle and count; inner: left child and 0.
    pub(crate) first: u32,
    pub(crate) count: u32,
    pub(crate) right: u32,
}

/// The traced scene.
pub(crate) struct Scene {
    pub(crate) triangles: Vec<Triangle>,
    pub(crate) nodes: Vec<Node>,
    pub(crate) surfaces: Vec<Surface>,
    pub(crate) textures: Vec<Texture>,
    pub(crate) ground: Option<(f64, Surface)>,
}

fn linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        libm::pow((c + 0.055) / 1.055, 2.4)
    }
}

fn rgb(c: Rgba8) -> DVec3 {
    DVec3::new(linear(c.r), linear(c.g), linear(c.b))
}

/// The bounding box of triangles `tris`.
fn bounds(tris: &[Triangle]) -> (DVec3, DVec3) {
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for t in tris {
        for p in t.p {
            lo = lo.min(p);
            hi = hi.max(p);
        }
    }
    (lo, hi)
}

/// Builds the hierarchy over `tris` (reordered in place): median splits on
/// the longest axis of the centroids, four triangles per leaf.
fn build(tris: &mut [Triangle], offset: u32, nodes: &mut Vec<Node>) -> u32 {
    let (lo, hi) = bounds(tris);
    let index = nodes.len() as u32;
    nodes.push(Node {
        lo,
        hi,
        first: offset,
        count: tris.len() as u32,
        right: 0,
    });
    if tris.len() <= 4 {
        return index;
    }
    let centroid = |t: &Triangle| (t.p[0] + t.p[1] + t.p[2]) / 3.0;
    let (mut clo, mut chi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for t in tris.iter() {
        clo = clo.min(centroid(t));
        chi = chi.max(centroid(t));
    }
    let extent = chi - clo;
    let axis = if extent.x >= extent.y && extent.x >= extent.z {
        0
    } else if extent.y >= extent.z {
        1
    } else {
        2
    };
    tris.sort_by(|a, b| centroid(a)[axis].total_cmp(&centroid(b)[axis]));
    let mid = tris.len() / 2;
    let (left, right) = tris.split_at_mut(mid);
    build(left, offset, nodes);
    let r = build(right, offset + mid as u32, nodes);
    let node = &mut nodes[index as usize];
    node.count = 0;
    node.first = index + 1;
    node.right = r;
    index
}

fn hit_box(lo: DVec3, hi: DVec3, origin: DVec3, inv: DVec3, limit: f64) -> bool {
    let t1 = (lo - origin) * inv;
    let t2 = (hi - origin) * inv;
    let near = t1.min(t2).max_element();
    let far = t1.max(t2).min_element();
    far >= near.max(0.0) && near < limit
}

/// Möller–Trumbore: distance and barycentrics. Only a triangle's front
/// counts: a face's back is its own triangle, wound the other way, as in
/// the viewport.
fn hit_triangle(t: &Triangle, origin: DVec3, dir: DVec3) -> Option<(f64, f64, f64)> {
    let e1 = t.p[1] - t.p[0];
    let e2 = t.p[2] - t.p[0];
    let pv = dir.cross(e2);
    let det = e1.dot(pv);
    if det < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let tv = origin - t.p[0];
    let u = tv.dot(pv) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let qv = tv.cross(e1);
    let v = dir.dot(qv) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let d = e2.dot(qv) * inv;
    (d > 1e-6).then_some((d, u, v))
}

/// A ray's hit: distance, triangle (or `u32::MAX` for the ground) and
/// barycentrics.
#[derive(Debug, Clone, Copy)]
struct Hit {
    t: f64,
    triangle: u32,
    u: f64,
    v: f64,
}

impl Scene {
    fn intersect(&self, origin: DVec3, dir: DVec3, limit: f64) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        let mut limit = limit;
        if let Some((z, _)) = self.ground
            && dir.z.abs() > 1e-12
        {
            let t = (z - origin.z) / dir.z;
            if t > 1e-6 && t < limit {
                limit = t;
                best = Some(Hit {
                    t,
                    triangle: u32::MAX,
                    u: 0.0,
                    v: 0.0,
                });
            }
        }
        if self.nodes.is_empty() {
            return best;
        }
        let inv = DVec3::ONE / dir;
        let mut stack = [0u32; 64];
        let mut depth = 1;
        while depth > 0 {
            depth -= 1;
            let node = self.nodes[stack[depth] as usize];
            if !hit_box(node.lo, node.hi, origin, inv, limit) {
                continue;
            }
            if node.count > 0 {
                for i in node.first..node.first + node.count {
                    if let Some((t, u, v)) = hit_triangle(&self.triangles[i as usize], origin, dir)
                        && t < limit
                    {
                        limit = t;
                        best = Some(Hit {
                            t,
                            triangle: i,
                            u,
                            v,
                        });
                    }
                }
            } else if depth + 2 <= stack.len() {
                stack[depth] = node.first;
                stack[depth + 1] = node.right;
                depth += 2;
            }
        }
        best
    }
}

/// Collects the visible model as triangles in world coordinates.
fn scene(doc: &Document, style: &Style) -> Scene {
    let items = draw_list(doc, &Selection::new(), &EditContext::model(), false);
    let mut meshes = BTreeMap::new();
    let mut surfaces: Vec<Surface> = Vec::new();
    let mut surface_of: BTreeMap<(Option<MaterialId>, FrontOrBack), u32> = BTreeMap::new();
    let mut textures: Vec<Texture> = Vec::new();
    let mut texture_of: BTreeMap<ImageRef, Option<usize>> = BTreeMap::new();
    let mut triangles = Vec::new();
    let monochrome = style.faces.mode == FaceMode::Monochrome;
    for item in items.iter().filter(|i| !i.hidden) {
        let mesh = meshes
            .entry(item.container)
            .or_insert_with(|| extract(doc, item.container, ExtractOptions::default()).ok());
        let Some(mesh) = mesh.as_ref() else { continue };
        let normal_matrix = item.world.matrix3.inverse().transpose();
        let flip = item.world.matrix3.determinant() < 0.0;
        for (key, range) in &mesh.indices_by_material {
            let MaterialKey { material, side } = *key;
            let id = material.or(item.material_override);
            let surface = *surface_of.entry((id, side)).or_insert_with(|| {
                let resolved = doc.materials.resolve(id, side);
                let m = doc.materials.get(resolved);
                let mut s = Surface {
                    albedo: DVec3::ONE,
                    roughness: f64::from(Material::DEFAULT_ROUGHNESS),
                    metallic: 0.0,
                    opacity: 1.0,
                    texture: None,
                };
                if let Some(m) = m {
                    s.albedo = rgb(m.color);
                    s.roughness = f64::from(m.roughness.unwrap_or(Material::DEFAULT_ROUGHNESS));
                    s.metallic = f64::from(m.metallic.unwrap_or(0.0));
                    s.opacity = f64::from(m.opacity.clamp(0.0, 1.0));
                    if let Some(t) = &m.texture
                        && style.faces.mode == FaceMode::ShadedTextured
                    {
                        s.texture = *texture_of.entry(t.image).or_insert_with(|| {
                            let data = doc.images.get(t.image)?;
                            let img = image::load_from_memory(&data.bytes).ok()?.to_rgb8();
                            textures.push(Texture {
                                width: img.width().max(1) as usize,
                                height: img.height().max(1) as usize,
                                texels: img
                                    .pixels()
                                    .map(|p| DVec3::new(linear(p[0]), linear(p[1]), linear(p[2])))
                                    .collect(),
                            });
                            Some(textures.len() - 1)
                        });
                        if s.texture.is_some() && t.colorize.is_none() {
                            s.albedo = DVec3::ONE;
                        }
                    }
                }
                if monochrome {
                    s.albedo = rgb(match side {
                        FrontOrBack::Front => style.faces.front_default,
                        FrontOrBack::Back => style.faces.back_default,
                    });
                    s.texture = None;
                }
                surfaces.push(s);
                (surfaces.len() - 1) as u32
            });
            for tri in mesh.indices[range.start as usize..range.end as usize]
                .as_chunks::<3>()
                .0
            {
                let corner = |i: u32| {
                    let i = i as usize;
                    let p = mesh.positions[i];
                    let n = mesh.normals[i];
                    let uv = mesh.uvs.get(i).copied().unwrap_or([0.0, 0.0]);
                    (
                        item.world.transform_point3(DVec3::new(
                            f64::from(p[0]),
                            f64::from(p[1]),
                            f64::from(p[2]),
                        )),
                        (normal_matrix
                            * DVec3::new(f64::from(n[0]), f64::from(n[1]), f64::from(n[2])))
                        .normalize_or_zero(),
                        DVec2::new(f64::from(uv[0]), f64::from(uv[1])),
                    )
                };
                let mut c = [corner(tri[0]), corner(tri[1]), corner(tri[2])];
                if flip {
                    c.swap(1, 2);
                }
                triangles.push(Triangle {
                    p: c.map(|x| x.0),
                    n: c.map(|x| x.1),
                    uv: c.map(|x| x.2),
                    surface,
                });
            }
        }
    }
    let (lo, _) = bounds(&triangles);
    let ground = (!triangles.is_empty()).then(|| {
        // The viewport's ground: at the model's lowest point.
        let z = lo.z;
        let color = style
            .background
            .ground
            .map_or(Rgba8::rgb(0xD6, 0xD2, 0xCA), |(c, _)| c);
        (
            z - 0.01,
            Surface {
                albedo: rgb(color),
                roughness: 0.9,
                metallic: 0.0,
                opacity: 1.0,
                texture: None,
            },
        )
    });
    let mut nodes = Vec::new();
    if !triangles.is_empty() {
        build(&mut triangles, 0, &mut nodes);
    }
    Scene {
        triangles,
        nodes,
        surfaces,
        textures,
        ground,
    }
}

/// A small, fast generator (PCG32) seeded per pixel and sample.
struct Rng(u64);

impl Rng {
    fn new(pixel: u64, sample: u64) -> Self {
        let mut r =
            Rng(pixel.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ sample.wrapping_mul(0xD1B5_4A32_D192_ED03));
        r.next();
        r
    }

    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let x = (((self.0 >> 18) ^ self.0) >> 27) as u32;
        let rot = (self.0 >> 59) as u32;
        f64::from(x.rotate_right(rot)) / 4_294_967_296.0
    }
}

/// An orthonormal frame around `n`.
fn frame(n: DVec3) -> (DVec3, DVec3) {
    let a = if n.x.abs() > 0.9 { DVec3::Y } else { DVec3::X };
    let t = n.cross(a).normalize();
    (t, n.cross(t))
}

/// Lighting of the scene.
pub(crate) struct Light {
    pub(crate) sun: DVec3,
    pub(crate) sun_radiance: f64,
    pub(crate) sky: DVec3,
    pub(crate) horizon: DVec3,
    pub(crate) ground: DVec3,
    pub(crate) background: Option<DVec3>,
    pub(crate) exposure: f64,
}

impl Light {
    /// Light arriving from direction `d` (not the sun's disc).
    fn environment(&self, d: DVec3) -> DVec3 {
        if d.z >= 0.0 {
            self.horizon.lerp(self.sky, libm::pow(d.z, 0.5))
        } else {
            self.ground
        }
    }
}

fn ggx(ndh: f64, a2: f64) -> f64 {
    let d = ndh * ndh * (a2 - 1.0) + 1.0;
    a2 / (PI * d * d)
}

fn smith(ndv: f64, ndl: f64, roughness: f64) -> f64 {
    let k = (roughness + 1.0) * (roughness + 1.0) / 8.0;
    (ndv / (ndv * (1.0 - k) + k)) * (ndl / (ndl * (1.0 - k) + k))
}

/// The surface's reflection of light from `l` toward `v`, times `n·l`.
fn brdf(albedo: DVec3, s: &Surface, n: DVec3, v: DVec3, l: DVec3) -> DVec3 {
    let ndl = n.dot(l);
    let ndv = n.dot(v).max(1e-4);
    if ndl <= 0.0 {
        return DVec3::ZERO;
    }
    let roughness = s.roughness.clamp(0.04, 1.0);
    let h = (v + l).normalize();
    let f0 = DVec3::splat(0.04).lerp(albedo, s.metallic);
    let vdh = v.dot(h).max(0.0);
    let fresnel = f0 + (DVec3::ONE - f0) * libm::pow(1.0 - vdh, 5.0);
    let a2 = libm::pow(roughness, 4.0);
    let specular =
        fresnel * ggx(n.dot(h).max(0.0), a2) * smith(ndv, ndl, roughness) / (4.0 * ndv * ndl);
    let diffuse = (DVec3::ONE - fresnel) * (1.0 - s.metallic) * albedo / PI;
    (diffuse + specular) * ndl
}

/// One path from the eye.
fn trace(
    scene: &Scene,
    light: &Light,
    origin: DVec3,
    dir: DVec3,
    bounces: u32,
    rng: &mut Rng,
) -> DVec3 {
    let mut radiance = DVec3::ZERO;
    let mut throughput = DVec3::ONE;
    let (mut o, mut d) = (origin, dir);
    for bounce in 0..=bounces + 1 {
        let Some(hit) = scene.intersect(o, d, f64::INFINITY) else {
            radiance += throughput
                * match (bounce, light.background) {
                    (0, Some(bg)) => bg,
                    _ => light.environment(d),
                };
            break;
        };
        let p = o + d * hit.t;
        let (surface, mut n, albedo) = if hit.triangle == u32::MAX {
            let (_, s) = scene
                .ground
                .as_ref()
                .map_or((0.0, None), |(z, s)| (*z, Some(*s)));
            let Some(s) = s else { break };
            (s, DVec3::Z, s.albedo)
        } else {
            let t = &scene.triangles[hit.triangle as usize];
            let w = 1.0 - hit.u - hit.v;
            let s = scene.surfaces[t.surface as usize];
            let n = (t.n[0] * w + t.n[1] * hit.u + t.n[2] * hit.v).normalize_or_zero();
            let n = if n == DVec3::ZERO {
                (t.p[1] - t.p[0]).cross(t.p[2] - t.p[0]).normalize()
            } else {
                n
            };
            let albedo = match s.texture {
                Some(k) => {
                    let uv = t.uv[0] * w + t.uv[1] * hit.u + t.uv[2] * hit.v;
                    s.albedo * scene.textures[k].sample(uv)
                }
                None => s.albedo,
            };
            (s, n, albedo)
        };
        if n.dot(d) > 0.0 {
            n = -n;
        }
        // See-through materials let some paths pass straight on.
        if surface.opacity < 1.0 && rng.next() > surface.opacity {
            o = p + d * 1e-3;
            continue;
        }
        let v = -d;
        let lift = p + n * 1e-3;
        // The sun: a disc a little over half a degree wide.
        if light.sun_radiance > 0.0 {
            let (t, b) = frame(light.sun);
            let r = 0.008 * libm::sqrt(rng.next());
            let a = 2.0 * PI * rng.next();
            let l = (light.sun + t * (r * libm::cos(a)) + b * (r * libm::sin(a))).normalize();
            if n.dot(l) > 0.0 && scene.intersect(lift, l, f64::INFINITY).is_none() {
                radiance += throughput * brdf(albedo, &surface, n, v, l) * light.sun_radiance;
            }
        }
        if bounce > bounces {
            break;
        }
        // The next direction: a microfacet reflection or a diffuse bounce.
        let roughness = surface.roughness.clamp(0.04, 1.0);
        let specular_share = (0.5 * (1.0 - roughness) + 0.5 * surface.metallic).clamp(0.1, 0.9);
        let next = if rng.next() < specular_share {
            let a2 = libm::pow(roughness, 4.0);
            let (u1, u2) = (rng.next(), rng.next());
            let cos_t = libm::sqrt((1.0 - u1) / (1.0 + (a2 - 1.0) * u1));
            let sin_t = libm::sqrt((1.0 - cos_t * cos_t).max(0.0));
            let phi = 2.0 * PI * u2;
            let (t, b) = frame(n);
            let h = (t * (sin_t * libm::cos(phi)) + b * (sin_t * libm::sin(phi)) + n * cos_t)
                .normalize();
            let l = (h * (2.0 * v.dot(h)) - v).normalize();
            let ndl = n.dot(l);
            if ndl <= 0.0 {
                break;
            }
            let f0 = DVec3::splat(0.04).lerp(albedo, surface.metallic);
            let vdh = v.dot(h).max(1e-4);
            let fresnel = f0 + (DVec3::ONE - f0) * libm::pow(1.0 - vdh, 5.0);
            let weight = fresnel * smith(n.dot(v).max(1e-4), ndl, roughness) * vdh
                / (n.dot(h).max(1e-4) * n.dot(v).max(1e-4));
            throughput *= weight / specular_share;
            l
        } else {
            let (u1, u2) = (rng.next(), rng.next());
            let r = libm::sqrt(u1);
            let phi = 2.0 * PI * u2;
            let (t, b) = frame(n);
            let l =
                (t * (r * libm::cos(phi)) + b * (r * libm::sin(phi)) + n * libm::sqrt(1.0 - u1))
                    .normalize();
            let f0 = DVec3::splat(0.04).lerp(albedo, surface.metallic);
            throughput *=
                albedo * (1.0 - surface.metallic) * (DVec3::ONE - f0) / (1.0 - specular_share);
            l
        };
        // Russian roulette after two bounces.
        if bounce >= 2 {
            let keep = throughput.max_element().clamp(0.05, 0.95);
            if rng.next() > keep {
                break;
            }
            throughput /= keep;
        }
        o = lift;
        d = next;
    }
    radiance
}

/// Brightest a single path may count (scene light units).
const FIREFLY: f64 = 6.0;

pub(crate) fn filmic(x: f64) -> f64 {
    ((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0.0, 1.0)
}

pub(crate) fn encode(c: f64) -> u8 {
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * libm::pow(c, 1.0 / 2.4) - 0.055
    };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

/// A path-traced image refined pass by pass (docs/06 §11.1–§11.2): each
/// pass adds samples to every pixel. Pixel `p`'s sample `k` always uses the
/// same random sequence and samples add up in order, so an image traced in
/// several passes equals one traced at once, whatever the thread count.
pub struct PathTracer {
    pub(crate) scene: Scene,
    pub(crate) light: Light,
    pub(crate) camera: Camera,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bounces: u32,
    pub(crate) threads: usize,
    pub(crate) sum: Vec<DVec3>,
    pub(crate) samples: u32,
}

impl PathTracer {
    /// Prepares tracing `doc` (the scene's hierarchy is built once).
    pub fn new(doc: &Document, options: &PathTraceOptions) -> Self {
        let (width, height) = (options.width.max(1), options.height.max(1));
        let style = options
            .style
            .clone()
            .or_else(|| doc.styles.active_style().cloned())
            .unwrap_or_default();
        let camera = Camera {
            aspect: f64::from(width) / f64::from(height),
            ..options.camera.unwrap_or(doc.camera)
        };
        let scene = scene(doc, &style);
        let shadows = doc.rendering.shadows;
        let sun = shadows.sun.direction();
        let sun = if shadows.enabled && sun.z > 0.02 {
            sun
        } else {
            DVec3::new(0.35, -0.55, 0.76).normalize()
        };
        let realism = style.realism.unwrap_or_default();
        let sky = rgb(style.background.sky.unwrap_or(Rgba8::rgb(0xA8, 0xC4, 0xE8)));
        let light = Light {
            sun,
            sun_radiance: 3.2 * f64::from(shadows.light.clamp(5, 100)) / 80.0,
            sky: sky * f64::from(realism.sky_light) * 1.6,
            horizon: (sky * 0.5 + DVec3::splat(0.5)) * f64::from(realism.sky_light) * 1.6,
            ground: scene
                .ground
                .map_or(DVec3::splat(0.3), |(_, s)| s.albedo * 0.3),
            background: Some(rgb(style.background.color)),
            exposure: libm::pow(2.0, f64::from(realism.exposure.clamp(-3.0, 3.0))),
        };
        let threads = if options.threads == 0 {
            std::thread::available_parallelism().map_or(1, usize::from)
        } else {
            options.threads
        };
        PathTracer {
            scene,
            light,
            camera,
            width,
            height,
            bounces: options.bounces,
            threads,
            sum: vec![DVec3::ZERO; width as usize * height as usize],
            samples: 0,
        }
    }

    /// Samples per pixel so far.
    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// Adds `count` samples to every pixel.
    pub fn pass(&mut self, count: u32) {
        let (width, height) = (self.width, self.height);
        let first = self.samples;
        let (scene, light, camera, bounces) =
            (&self.scene, &self.light, &self.camera, self.bounces);
        let row = |y: u32, sums: &mut [DVec3]| {
            for (x, sum) in sums.iter_mut().enumerate() {
                let x = x as u32;
                let pixel = u64::from(y) * u64::from(width) + u64::from(x);
                for k in first..first + count {
                    let mut rng = Rng::new(pixel, u64::from(k));
                    let ndc = DVec2::new(
                        (f64::from(x) + rng.next()) / f64::from(width) * 2.0 - 1.0,
                        1.0 - (f64::from(y) + rng.next()) / f64::from(height) * 2.0,
                    );
                    let ray = camera.ray_through_ndc(ndc);
                    let c = trace(scene, light, ray.origin, ray.dir, bounces, &mut rng);
                    // A rare, very bright path would show as a dot.
                    *sum += c * (FIREFLY / c.max_element().max(FIREFLY));
                }
            }
        };
        let workers = self.threads.clamp(1, height as usize);
        let mut rows: Vec<(u32, &mut [DVec3])> = self
            .sum
            .chunks_mut(width as usize)
            .enumerate()
            .map(|(y, r)| (y as u32, r))
            .collect();
        if workers == 1 {
            // One worker traces here (where threads are unavailable, the
            // browser).
            for (y, r) in &mut rows {
                row(*y, r);
            }
        } else {
            // Each worker takes every `workers`-th row.
            let mut lanes: Vec<Vec<(u32, &mut [DVec3])>> =
                (0..workers).map(|_| Vec::new()).collect();
            for (i, r) in rows.into_iter().enumerate() {
                lanes[i % workers].push(r);
            }
            std::thread::scope(|s| {
                for lane in lanes {
                    let row = &row;
                    s.spawn(move || {
                        for (y, r) in lane {
                            row(y, r);
                        }
                    });
                }
            });
        }
        self.samples += count;
    }

    /// The image so far.
    pub fn image(&self) -> image::RgbaImage {
        let mut image = image::RgbaImage::new(self.width, self.height);
        let n = f64::from(self.samples.max(1));
        for (i, sum) in self.sum.iter().enumerate() {
            let c = *sum / n * self.light.exposure;
            let (x, y) = (i as u32 % self.width, i as u32 / self.width);
            image.put_pixel(
                x,
                y,
                image::Rgba([
                    encode(filmic(c.x)),
                    encode(filmic(c.y)),
                    encode(filmic(c.z)),
                    255,
                ]),
            );
        }
        image
    }
}

/// Traces `doc` into an image (docs/06 §11.1).
pub fn path_trace(doc: &Document, options: &PathTraceOptions) -> image::RgbaImage {
    let mut tracer = PathTracer::new(doc, options);
    tracer.pass(options.samples.max(1));
    tracer.image()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tri(z: f64) -> Triangle {
        Triangle {
            p: [
                DVec3::new(0.0, 0.0, z),
                DVec3::new(10.0, 0.0, z),
                DVec3::new(0.0, 10.0, z),
            ],
            n: [DVec3::Z; 3],
            uv: [DVec2::ZERO; 3],
            surface: 0,
        }
    }

    #[test]
    fn the_hierarchy_finds_the_nearest_triangle() {
        let mut triangles: Vec<Triangle> = (0..20).map(|i| tri(f64::from(i) * 5.0)).collect();
        let mut nodes = Vec::new();
        build(&mut triangles, 0, &mut nodes);
        let scene = Scene {
            triangles,
            nodes,
            surfaces: Vec::new(),
            textures: Vec::new(),
            ground: None,
        };
        let hit = scene
            .intersect(DVec3::new(1.0, 1.0, 200.0), -DVec3::Z, f64::INFINITY)
            .unwrap();
        assert!((hit.t - (200.0 - 95.0)).abs() < 1e-9);
        assert!(
            scene
                .intersect(DVec3::new(20.0, 20.0, 200.0), -DVec3::Z, f64::INFINITY)
                .is_none()
        );
    }

    #[test]
    fn the_generator_is_deterministic_and_uniform() {
        let a: Vec<f64> = {
            let mut r = Rng::new(7, 3);
            (0..4).map(|_| r.next()).collect()
        };
        let mut r = Rng::new(7, 3);
        assert_eq!(a, (0..4).map(|_| r.next()).collect::<Vec<_>>());
        let mut r = Rng::new(1, 1);
        let mean = (0..10_000).map(|_| r.next()).sum::<f64>() / 10_000.0;
        assert!((mean - 0.5).abs() < 0.02, "{mean}");
    }

    #[test]
    fn a_white_floor_under_the_sun_reflects_its_energy() {
        // Lambert: a white face square to a sun of radiance E returns E/π·π.
        let s = Surface {
            albedo: DVec3::ONE,
            roughness: 1.0,
            metallic: 0.0,
            opacity: 1.0,
            texture: None,
        };
        let f = brdf(DVec3::ONE, &s, DVec3::Z, DVec3::Z, DVec3::Z);
        assert!(f.x > 0.2 && f.x < 0.4, "{f}");
    }

    #[test]
    fn the_image_does_not_depend_on_the_thread_count() {
        use planyrith_core::{CommandRegistry, Engine};
        use std::sync::Arc;
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        for (name, params) in [
            (
                "rectangle",
                serde_json::json!({ "corner_a": [0, 0, 0], "corner_b": [600, 400, 0] }),
            ),
            (
                "push-pull",
                serde_json::json!({ "face": { "Query": "face & last" }, "distance": 300 }),
            ),
            (
                "camera-standard",
                serde_json::json!({ "view": "iso", "extents": true }),
            ),
        ] {
            e.execute_erased(name, params).unwrap();
        }
        let options = |threads| PathTraceOptions {
            width: 48,
            height: 32,
            samples: 3,
            bounces: 2,
            threads,
            ..PathTraceOptions::default()
        };
        let one = path_trace(e.document(), &options(1));
        let three = path_trace(e.document(), &options(3));
        assert_eq!(one, three);
        let corner = one.get_pixel(0, 0).0;
        let middle = one.get_pixel(24, 16).0;
        assert_ne!(corner, middle, "the box is in the middle of the view");
    }

    #[test]
    fn passes_add_up_to_the_same_image() {
        let mut doc = Document::new();
        doc.camera = Camera::default();
        let options = PathTraceOptions {
            width: 24,
            height: 16,
            samples: 4,
            threads: 3,
            ..PathTraceOptions::default()
        };
        let once = path_trace(&doc, &options);
        let mut tracer = PathTracer::new(&doc, &options);
        tracer.pass(1);
        tracer.pass(3);
        assert_eq!(tracer.samples(), 4);
        assert_eq!(tracer.image(), once);
    }
}
