//! Implements docs/09-file-formats.md §5 glTF 2.0 / GLB import: the default
//! scene's nodes with meshes become groups (a mesh several nodes use, a
//! component), primitives' materials become materials (base color, opacity
//! when blended, base color texture with `KHR_texture_transform`).
//! Buffers and images come from the GLB, data URIs or files next to the
//! glTF (through the storage). glTF is Y-up in meters (docs/09 §6).

use std::collections::BTreeMap;
use std::io::Read;

use glam::{DAffine3, DMat4, DVec2, DVec3};

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::diagnostics::{Warning, WarningCode};
use crate::document::Rgba8;
use crate::io::import::build::{
    ImportedMaterial, ImportedMesh, ImportedNode, ImportedScene, build_scene,
};
use crate::io::import::{
    Confidence, ImportFormat, ImportOptions, ImportReport, Importer, sibling, source_stem,
};
use crate::io::{IoError, Progress, Storage, StorageKey};
use crate::units::LengthUnit;

/// The glTF / GLB importer.
#[derive(Debug, Default, Clone, Copy)]
pub struct GltfImporter;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "gltf".into(),
        reason: reason.into(),
    }
}

/// Decodes standard base64 (padding optional, whitespace ignored).
pub(crate) fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// `%XX` escapes of a URI path.
fn percent_decode(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = uri
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The bytes a URI names: a base64 data URI or a file next to `source`.
fn read_uri(uri: &str, source: &str, storage: Option<&dyn Storage>) -> Result<Vec<u8>, String> {
    if let Some(data) = uri.strip_prefix("data:") {
        let (_, payload) = data
            .split_once(";base64,")
            .ok_or_else(|| "data URI without base64 payload".to_string())?;
        return base64_decode(payload).ok_or_else(|| "bad base64 in a data URI".to_string());
    }
    let key = sibling(source, &percent_decode(uri));
    storage
        .ok_or_else(|| "no storage".to_string())?
        .read(&StorageKey::new(&key))
        .map_err(|e| format!("`{key}`: {e}"))
}

/// Linear color channel to an 8-bit sRGB one.
fn srgb(linear: f32) -> u8 {
    let l = linear.clamp(0.0, 1.0);
    let s = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * libm::powf(l, 1.0 / 2.4) - 0.055
    };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

/// A primitive's triangles as index triples (lists, strips and fans).
fn triangles(mode: gltf::mesh::Mode, indices: &[u32]) -> Option<Vec<[u32; 3]>> {
    use gltf::mesh::Mode;
    let n = indices.len();
    Some(match mode {
        Mode::Triangles => indices.as_chunks::<3>().0.to_vec(),
        Mode::TriangleStrip => (2..n)
            .map(|i| {
                if i % 2 == 0 {
                    [indices[i - 2], indices[i - 1], indices[i]]
                } else {
                    [indices[i - 1], indices[i - 2], indices[i]]
                }
            })
            .collect(),
        Mode::TriangleFan => (2..n)
            .map(|i| [indices[0], indices[i - 1], indices[i]])
            .collect(),
        _ => return None,
    })
}

impl Importer for GltfImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Gltf
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        let lower = name.to_ascii_lowercase();
        if bytes.starts_with(b"glTF") {
            Confidence::CERTAIN
        } else if lower.ends_with(".gltf") || lower.ends_with(".glb") {
            Confidence::EXTENSION
        } else {
            Confidence::NONE
        }
    }

    fn import(
        &self,
        src: &mut dyn Read,
        opts: &ImportOptions,
        tx: &mut Transaction<'_>,
        target: &Target,
        progress: &dyn Progress,
    ) -> Result<ImportReport, IoError> {
        let mut bytes = Vec::new();
        src.read_to_end(&mut bytes)
            .map_err(|e| corrupt(e.to_string()))?;
        let gltf = gltf::Gltf::from_slice(&bytes).map_err(|e| corrupt(e.to_string()))?;
        drop(bytes);
        let doc = &gltf.document;
        let storage = tx.storage();
        let mut warnings = Vec::new();
        let mut skipped = 0usize;
        progress.report(0.1, "reading buffers");

        let mut buffers: Vec<Vec<u8>> = Vec::with_capacity(doc.buffers().len());
        for buffer in doc.buffers() {
            let data = match buffer.source() {
                gltf::buffer::Source::Bin => gltf
                    .blob
                    .clone()
                    .ok_or_else(|| corrupt("the GLB has no binary chunk"))?,
                gltf::buffer::Source::Uri(uri) => read_uri(uri, &opts.source, storage)
                    .map_err(|e| corrupt(format!("buffer {}: {e}", buffer.index())))?,
            };
            if data.len() < buffer.length() {
                return Err(corrupt(format!("buffer {} is too short", buffer.index())));
            }
            buffers.push(data);
        }
        let get = |b: gltf::Buffer<'_>| buffers.get(b.index()).map(Vec::as_slice);

        // Materials; textures read once per image.
        let mut images: BTreeMap<usize, Option<Vec<u8>>> = BTreeMap::new();
        let mut materials = Vec::new();
        // Per material: its texture coordinate set and transform (offset,
        // rotation, scale).
        type UvTransform = ([f32; 2], f32, [f32; 2]);
        let mut uv_setups: Vec<Option<(u32, Option<UvTransform>)>> = Vec::new();
        for (i, m) in doc.materials().enumerate() {
            let pbr = m.pbr_metallic_roughness();
            let [r, g, b, a] = pbr.base_color_factor();
            let texture = pbr.base_color_texture().and_then(|info| {
                let image = info.texture().source();
                images
                    .entry(image.index())
                    .or_insert_with(|| {
                        let data = match image.source() {
                            gltf::image::Source::View { view, .. } => get(view.buffer())
                                .and_then(|d| d.get(view.offset()..view.offset() + view.length()))
                                .map(<[u8]>::to_vec)
                                .ok_or_else(|| "image view out of range".to_string()),
                            gltf::image::Source::Uri { uri, .. } => {
                                read_uri(uri, &opts.source, storage)
                            }
                        };
                        match data {
                            Ok(d) => Some(d),
                            Err(e) => {
                                warnings.push(Warning::new(
                                    WarningCode::W2004,
                                    format!("image {} skipped: {e}", image.index()),
                                ));
                                None
                            }
                        }
                    })
                    .clone()
            });
            uv_setups.push(
                pbr.base_color_texture()
                    .filter(|_| texture.is_some())
                    .map(|info| {
                        let transform = info.texture_transform();
                        let set = transform
                            .as_ref()
                            .and_then(|t| t.tex_coord())
                            .unwrap_or(info.tex_coord());
                        (
                            set,
                            transform.map(|t| (t.offset(), t.rotation(), t.scale())),
                        )
                    }),
            );
            let blended = m.alpha_mode() == gltf::material::AlphaMode::Blend;
            materials.push(ImportedMaterial {
                name: m
                    .name()
                    .map_or_else(|| format!("Material {}", i + 1), str::to_string),
                color: Rgba8::rgb(srgb(r), srgb(g), srgb(b)),
                opacity: if blended { a } else { 1.0 },
                texture,
            });
        }

        // Meshes: one per glTF mesh, primitives' triangles together.
        progress.report(0.3, "reading meshes");
        let mut meshes = Vec::with_capacity(doc.meshes().len());
        for mesh in doc.meshes() {
            let mut out = ImportedMesh {
                name: mesh
                    .name()
                    .map_or_else(|| format!("Mesh {}", mesh.index() + 1), str::to_string),
                ..ImportedMesh::default()
            };
            let mut any_uv = false;
            for primitive in mesh.primitives() {
                let reader = primitive.reader(get);
                let Some(positions) = reader.read_positions() else {
                    skipped += 1;
                    continue;
                };
                let positions: Vec<DVec3> = positions
                    .map(|p| DVec3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2])))
                    .collect();
                if positions.iter().any(|p| !p.is_finite()) {
                    return Err(corrupt("a vertex is not a finite number"));
                }
                let indices: Vec<u32> = match reader.read_indices() {
                    Some(ix) => ix.into_u32().collect(),
                    None => (0..positions.len() as u32).collect(),
                };
                let Some(tris) = triangles(primitive.mode(), &indices) else {
                    skipped += 1;
                    continue;
                };
                if tris
                    .iter()
                    .flatten()
                    .any(|&i| i as usize >= positions.len())
                {
                    return Err(corrupt("an index is past the vertex list"));
                }
                let material = primitive.material().index();
                let setup = material.and_then(|m| uv_setups.get(m).copied().flatten());
                let uvs: Vec<DVec2> = match &setup {
                    Some((set, transform)) => match reader.read_tex_coords(*set) {
                        Some(tc) => {
                            any_uv = true;
                            tc.into_f32()
                                .map(|[u, v]| {
                                    let (mut u, mut v) = (u, v);
                                    if let Some(([ou, ov], rotation, [su, sv])) = transform {
                                        let (s, c) = (libm::sinf(*rotation), libm::cosf(*rotation));
                                        let (x, y) = (u * su, v * sv);
                                        u = c * x + s * y + ou;
                                        v = -s * x + c * y + ov;
                                    }
                                    // glTF's v runs down the image.
                                    DVec2::new(f64::from(u), 1.0 - f64::from(v))
                                })
                                .collect()
                        }
                        None => Vec::new(),
                    },
                    None => Vec::new(),
                };
                let base = out.positions.len() as u32;
                out.positions.extend_from_slice(&positions);
                out.uvs.extend(
                    (0..positions.len()).map(|i| uvs.get(i).copied().unwrap_or(DVec2::ZERO)),
                );
                for t in tris {
                    out.triangles.push(t.map(|i| base + i));
                    out.materials
                        .push(material.filter(|&m| m < materials.len()));
                }
            }
            if !any_uv {
                out.uvs.clear();
            }
            meshes.push(out);
        }

        // Nodes of the default (or first) scene, with world transforms.
        let mut nodes = Vec::new();
        let roots: Vec<gltf::Node<'_>> = match doc.default_scene().or_else(|| doc.scenes().next()) {
            Some(scene) => scene.nodes().collect(),
            None => doc.nodes().collect(),
        };
        let mut stack: Vec<(gltf::Node<'_>, DAffine3, usize)> = roots
            .into_iter()
            .rev()
            .map(|n| (n, DAffine3::IDENTITY, 0))
            .collect();
        while let Some((node, parent, depth)) = stack.pop() {
            if depth > 64 {
                return Err(corrupt("nodes nest deeper than 64 levels"));
            }
            let m = node.transform().matrix();
            let local =
                DAffine3::from_mat4(DMat4::from_cols_array_2d(&m.map(|c| c.map(f64::from))));
            let world = parent * local;
            if let Some(mesh) = node.mesh() {
                nodes.push(ImportedNode {
                    name: node
                        .name()
                        .map_or_else(|| meshes[mesh.index()].name.clone(), str::to_string),
                    transform: world,
                    mesh: mesh.index(),
                });
            }
            if node.camera().is_some() {
                skipped += 1;
            }
            for child in node.children().collect::<Vec<_>>().into_iter().rev() {
                stack.push((child, world, depth + 1));
            }
        }
        if skipped > 0 {
            warnings.push(Warning::new(
                WarningCode::W2004,
                format!("{skipped} primitive(s) or camera(s) without triangles skipped"),
            ));
        }
        drop(buffers);
        progress.report(0.5, "building faces");
        let mut report = build_scene(
            tx,
            target,
            ImportedScene {
                name: source_stem(&opts.source, "glTF"),
                materials,
                meshes,
                nodes,
                y_up: true,
                units: Some(LengthUnit::Meter),
            },
            opts,
        )?;
        report.skipped += skipped;
        warnings.append(&mut report.warnings);
        report.warnings = warnings;
        Ok(report)
    }
}
