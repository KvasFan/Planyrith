//! Implements docs/09-file-formats.md §5 glTF 2.0 / GLB export: one mesh
//! per container (shared by every occurrence, so components stay shared),
//! one primitive per material, one node per occurrence with its world
//! transform (positions in meters, so f32 keeps them precise); materials as base color (linear), opacity (blended) and base
//! color texture, double-sided. Meters, Y-up (the format's own); `.gltf`
//! writes its buffer as `<name>.bin` beside it, `.glb` holds everything.

use std::collections::BTreeMap;
use std::io::Write;

use serde_json::{Value, json};

use crate::document::{ContainerRef, Document, FrontOrBack};
use crate::ids::MaterialId;
use crate::io::export::scene::{ExportedMesh, collect};
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter};
use crate::io::import::source_stem;
use crate::io::{IoError, Progress};

/// The glTF exporter; `binary` writes GLB.
#[derive(Debug, Default, Clone, Copy)]
pub struct GltfExporter {
    /// GLB instead of glTF + bin.
    pub binary: bool,
}

/// 8-bit sRGB channel to linear.
fn linear(srgb: u8) -> f64 {
    let s = f64::from(srgb) / 255.0;
    if s <= 0.040_45 {
        s / 12.92
    } else {
        libm::pow((s + 0.055) / 1.055, 2.4)
    }
}

/// Rounds for stable, short JSON numbers.
fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6 + 0.0
}

/// The binary buffer and its views and accessors.
#[derive(Default)]
struct Buffer {
    bytes: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
}

impl Buffer {
    fn align(&mut self) {
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
    }

    fn view(&mut self, data: &[u8], target: Option<u32>) -> usize {
        self.align();
        let offset = self.bytes.len();
        self.bytes.extend_from_slice(data);
        let mut view = json!({ "buffer": 0, "byteOffset": offset, "byteLength": data.len() });
        if let Some(t) = target {
            view["target"] = json!(t);
        }
        self.views.push(view);
        self.views.len() - 1
    }

    /// A float accessor of `n`-component vectors.
    fn floats(&mut self, values: &[f32], n: usize, kind: &str, bounds: bool) -> usize {
        let data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let view = self.view(&data, Some(34962));
        let mut accessor = json!({
            "bufferView": view, "componentType": 5126,
            "count": values.len() / n, "type": kind,
        });
        if bounds && !values.is_empty() {
            let mut lo = vec![f32::MAX; n];
            let mut hi = vec![f32::MIN; n];
            for chunk in values.chunks_exact(n) {
                for (k, v) in chunk.iter().enumerate() {
                    lo[k] = lo[k].min(*v);
                    hi[k] = hi[k].max(*v);
                }
            }
            accessor["min"] = json!(lo);
            accessor["max"] = json!(hi);
        }
        self.accessors.push(accessor);
        self.accessors.len() - 1
    }

    fn indices(&mut self, values: &[u32]) -> usize {
        let data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let view = self.view(&data, Some(34963));
        self.accessors.push(json!({
            "bufferView": view, "componentType": 5125,
            "count": values.len(), "type": "SCALAR",
        }));
        self.accessors.len() - 1
    }
}

impl Exporter for GltfExporter {
    fn format(&self) -> ExportFormat {
        if self.binary {
            ExportFormat::Glb
        } else {
            ExportFormat::Gltf
        }
    }

    fn export(
        &self,
        doc: &Document,
        scope: &ExportScope,
        opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<ExportReport, IoError> {
        let scene = collect(doc, scope);
        let mut report = ExportReport::default();
        let mut buffer = Buffer::default();

        // Materials (and their textures) in order of first use.
        let mut materials: Vec<Value> = Vec::new();
        let mut textures: Vec<Value> = Vec::new();
        let mut images: Vec<Value> = Vec::new();
        let mut material_index: BTreeMap<MaterialId, usize> = BTreeMap::new();
        let mut textured: BTreeMap<MaterialId, bool> = BTreeMap::new();
        let mut material = |id: MaterialId, buffer: &mut Buffer| -> (usize, bool) {
            if let Some(&i) = material_index.get(&id) {
                return (i, textured.get(&id).copied().unwrap_or(false));
            }
            let m = doc.materials.get(id);
            let (color, opacity, name) = m.map_or(
                (crate::document::Rgba8::WHITE, 1.0, String::from("Material")),
                |m| (m.color, m.opacity, m.name.clone()),
            );
            let mut pbr = json!({
                "baseColorFactor": [
                    round6(linear(color.r)), round6(linear(color.g)), round6(linear(color.b)),
                    round6(f64::from(opacity)),
                ],
                "metallicFactor": 0.0,
                "roughnessFactor": 1.0,
            });
            let mut has_texture = false;
            if let Some(t) = m.and_then(|m| m.texture.as_ref())
                && let Some(image) = doc.images.get(t.image)
            {
                let view = buffer.view(&image.bytes, None);
                let mime = match image.format {
                    crate::document::ImageFormat::Png => "image/png",
                    crate::document::ImageFormat::Jpeg => "image/jpeg",
                    crate::document::ImageFormat::WebP => "image/webp",
                };
                images.push(json!({ "bufferView": view, "mimeType": mime }));
                textures.push(json!({ "source": images.len() - 1 }));
                pbr["baseColorTexture"] = json!({ "index": textures.len() - 1 });
                has_texture = true;
            }
            let mut value = json!({
                "name": name, "pbrMetallicRoughness": pbr, "doubleSided": true,
            });
            if opacity < 1.0 {
                value["alphaMode"] = json!("BLEND");
            }
            materials.push(value);
            let i = materials.len() - 1;
            material_index.insert(id, i);
            textured.insert(id, has_texture);
            (i, has_texture)
        };

        // Meshes by (container, override), nodes by occurrence.
        let mut meshes: Vec<Value> = Vec::new();
        let mut mesh_index: BTreeMap<(ContainerRef, Option<MaterialId>), usize> = BTreeMap::new();
        let mut nodes: Vec<Value> = Vec::new();
        let out = crate::io::export::output_transform(
            &ExportOptions {
                units: crate::units::LengthUnit::Meter,
                z_up: false,
                ..opts.clone()
            },
            true,
        );
        for occ in &scene.occurrences {
            let Some(mesh) = scene.mesh(occ.container) else {
                continue;
            };
            if mesh.faces.is_empty() {
                continue;
            }
            let key = (occ.container, occ.material_override);
            let index = match mesh_index.get(&key) {
                Some(&i) => i,
                None => {
                    let name = match occ.container {
                        ContainerRef::Model => "Model".to_string(),
                        ContainerRef::Definition(d) => doc
                            .definitions
                            .get(d)
                            .map_or_else(|| "Mesh".into(), |def| def.name.clone()),
                    };
                    let primitives =
                        primitives(doc, mesh, occ.material_override, &mut buffer, &mut material);
                    report.entities += mesh.faces.iter().map(|f| f.triangles.len()).sum::<usize>();
                    meshes.push(json!({ "name": name, "primitives": primitives }));
                    mesh_index.insert(key, meshes.len() - 1);
                    meshes.len() - 1
                }
            };
            // Meshes are in meters; the node keeps rotation and position.
            let m = out * occ.world * glam::DAffine3::from_scale(glam::DVec3::splat(1000.0));
            let matrix: Vec<f64> = [
                m.matrix3.x_axis.extend(0.0),
                m.matrix3.y_axis.extend(0.0),
                m.matrix3.z_axis.extend(0.0),
                m.translation.extend(1.0),
            ]
            .iter()
            .flat_map(|c| c.to_array())
            .map(round6)
            .collect();
            let name = meshes[index]["name"].clone();
            nodes.push(json!({ "name": name, "mesh": index, "matrix": matrix }));
        }
        progress.report(0.7, "writing glTF");
        buffer.align();
        let stem = source_stem(&opts.destination, "model");
        let mut buffer_json = json!({ "byteLength": buffer.bytes.len() });
        if !self.binary {
            buffer_json["uri"] = json!(format!("{stem}.bin"));
        }
        let mut root = json!({
            "asset": { "version": "2.0", "generator": "Planyrith" },
            "scene": 0,
            "scenes": [{ "name": stem, "nodes": (0..nodes.len()).collect::<Vec<_>>() }],
            "nodes": nodes,
            "meshes": meshes,
            "materials": materials,
            "accessors": buffer.accessors,
            "bufferViews": buffer.views,
        });
        if !buffer.bytes.is_empty() {
            root["buffers"] = json!([buffer_json]);
        }
        if !images.is_empty() {
            root["images"] = json!(images);
            root["textures"] = json!(textures);
        }
        let mut text = if opts.pretty {
            serde_json::to_vec_pretty(&root)
        } else {
            serde_json::to_vec(&root)
        }
        .map_err(|e| IoError::Corrupt {
            part: "gltf".into(),
            reason: e.to_string(),
        })?;
        let write = |dst: &mut dyn Write, b: &[u8]| {
            dst.write_all(b).map_err(|e| IoError::Storage {
                key: "gltf".into(),
                reason: e.to_string(),
            })
        };
        if self.binary {
            while !text.len().is_multiple_of(4) {
                text.push(b' ');
            }
            let has_bin = !buffer.bytes.is_empty();
            let total = 12 + 8 + text.len() + if has_bin { 8 + buffer.bytes.len() } else { 0 };
            let mut head = Vec::with_capacity(28);
            head.extend_from_slice(b"glTF");
            head.extend_from_slice(&2u32.to_le_bytes());
            head.extend_from_slice(&(total as u32).to_le_bytes());
            head.extend_from_slice(&(text.len() as u32).to_le_bytes());
            head.extend_from_slice(b"JSON");
            write(dst, &head)?;
            write(dst, &text)?;
            if has_bin {
                let mut bin_head = Vec::with_capacity(8);
                bin_head.extend_from_slice(&(buffer.bytes.len() as u32).to_le_bytes());
                bin_head.extend_from_slice(b"BIN\0");
                write(dst, &bin_head)?;
                write(dst, &buffer.bytes)?;
            }
            report.bytes_written = total as u64;
        } else {
            write(dst, &text)?;
            report.bytes_written = text.len() as u64;
            if !buffer.bytes.is_empty() {
                report
                    .side_files
                    .push((format!("{stem}.bin"), std::mem::take(&mut buffer.bytes)));
            }
        }
        Ok(report)
    }
}

/// A mesh's primitives, one per material, in order of first use.
fn primitives(
    doc: &Document,
    mesh: &ExportedMesh,
    material_override: Option<MaterialId>,
    buffer: &mut Buffer,
    material: &mut impl FnMut(MaterialId, &mut Buffer) -> (usize, bool),
) -> Vec<Value> {
    // Faces grouped by material.
    let mut groups: Vec<(MaterialId, Vec<usize>)> = Vec::new();
    for (i, face) in mesh.faces.iter().enumerate() {
        let id = doc
            .materials
            .resolve(face.material.or(material_override), FrontOrBack::Front);
        match groups.iter_mut().find(|(m, _)| *m == id) {
            Some((_, faces)) => faces.push(i),
            None => groups.push((id, vec![i])),
        }
    }
    let mut out = Vec::new();
    for (id, faces) in groups {
        let (index, textured) = material(id, buffer);
        let (mut positions, mut normals, mut uvs, mut indices) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for &f in &faces {
            let face = &mesh.faces[f];
            let base = (positions.len() / 3) as u32;
            for (k, p) in face.positions.iter().enumerate() {
                let p = *p / 1000.0;
                positions.extend([p.x as f32, p.y as f32, p.z as f32]);
                let n = face.normal;
                normals.extend([n.x as f32, n.y as f32, n.z as f32]);
                let uv = face.uvs.get(k).copied().unwrap_or_default();
                // glTF's v runs down the image.
                uvs.extend([uv.x as f32, (1.0 - uv.y) as f32]);
            }
            indices.extend(face.triangles.iter().flatten().map(|i| base + i));
        }
        let mut attributes = json!({
            "POSITION": buffer.floats(&positions, 3, "VEC3", true),
            "NORMAL": buffer.floats(&normals, 3, "VEC3", false),
        });
        if textured {
            attributes["TEXCOORD_0"] = json!(buffer.floats(&uvs, 2, "VEC2", false));
        }
        out.push(json!({
            "attributes": attributes,
            "indices": buffer.indices(&indices),
            "material": index,
            "mode": 4,
        }));
    }
    out
}
