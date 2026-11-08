//! Implements docs/09-file-formats.md §5 COLLADA export (1.4.1): one
//! geometry per container (and material override), shared by every
//! occurrence of it; one node per occurrence with its world matrix; lambert
//! effects with the material's color or texture and transparency; the unit
//! and Z up declared in the asset (docs/09 Decisions, COLLADA).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write;

use crate::document::selection::occurrence_name;
use crate::document::{ContainerRef, Document, FrontOrBack};
use crate::ids::MaterialId;
use crate::io::export::scene::collect;
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter};
use crate::io::{IoError, Progress};

/// The COLLADA exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct ColladaExporter;

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

fn number(v: f64) -> String {
    format!("{}", v + 0.0)
}

/// An XML id: letters, digits, `_` and `-`, starting with a letter.
fn xml_id(prefix: &str, name: &str) -> String {
    let body: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{prefix}-{body}")
}

/// sRGB channel (0–255) to linear, as COLLADA colors are read.
fn linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        libm::pow((c + 0.055) / 1.055, 2.4)
    }
}

fn round5(v: f64) -> f64 {
    (v * 100_000.0).round() / 100_000.0
}

impl Exporter for ColladaExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Dae
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
        let scale = 1.0 / opts.units.millimeters();
        let stem = crate::io::import::source_stem(&opts.destination, "model");
        let texture_dir = format!("{}_textures", xml_id("t", &stem).trim_start_matches("t-"));

        // Geometries by (container, override).
        let mut geometry_ids: BTreeMap<(ContainerRef, Option<MaterialId>), String> =
            BTreeMap::new();
        let mut geometries = String::new();
        let mut used: Vec<MaterialId> = Vec::new();
        let mut triangles_total = 0;
        let mut nodes = String::new();
        for (n, occ) in scene.occurrences.iter().enumerate() {
            let Some(mesh) = scene.mesh(occ.container) else {
                continue;
            };
            if mesh.faces.is_empty() {
                continue;
            }
            let key = (occ.container, occ.material_override);
            let next = geometry_ids.len();
            let id = geometry_ids
                .entry(key)
                .or_insert_with(|| format!("geometry-{next}"))
                .clone();
            if geometry_ids.len() > next {
                // Faces grouped by their resolved material.
                let mut groups: BTreeMap<MaterialId, Vec<usize>> = BTreeMap::new();
                for (i, face) in mesh.faces.iter().enumerate() {
                    let m = doc
                        .materials
                        .resolve(face.material.or(occ.material_override), FrontOrBack::Front);
                    groups.entry(m).or_default().push(i);
                }
                let mut positions = Vec::new();
                let mut normals = Vec::new();
                let mut uvs = Vec::new();
                let mut primitives = String::new();
                for (material, faces) in &groups {
                    if !used.contains(material) {
                        used.push(*material);
                    }
                    let mut indices = Vec::new();
                    let mut count = 0;
                    for &f in faces {
                        let face = &mesh.faces[f];
                        let base = positions.len() / 3;
                        for (k, p) in face.positions.iter().enumerate() {
                            positions.extend([p.x * scale, p.y * scale, p.z * scale]);
                            let uv = face.uvs.get(k).copied().unwrap_or_default();
                            uvs.extend([uv.x, uv.y]);
                        }
                        let normal = normals.len() / 3;
                        normals.extend([face.normal.x, face.normal.y, face.normal.z]);
                        for t in &face.triangles {
                            for i in t {
                                let v = base + *i as usize;
                                indices.push(format!("{v} {normal} {v}"));
                            }
                            count += 1;
                        }
                    }
                    triangles_total += count;
                    let symbol = format!(
                        "material-{}",
                        used.iter().position(|m| m == material).unwrap_or(0)
                    );
                    let _ = writeln!(
                        primitives,
                        "        <triangles material=\"{symbol}\" count=\"{count}\">\n          <input semantic=\"VERTEX\" source=\"#{id}-vertices\" offset=\"0\"/>\n          <input semantic=\"NORMAL\" source=\"#{id}-normals\" offset=\"1\"/>\n          <input semantic=\"TEXCOORD\" source=\"#{id}-uvs\" offset=\"2\" set=\"0\"/>\n          <p>{}</p>\n        </triangles>",
                        indices.join(" ")
                    );
                }
                let source = |name: &str, values: &[f64], params: &[&str]| -> String {
                    let stride = params.len();
                    let text: Vec<String> = values.iter().map(|v| number(round5(*v))).collect();
                    let params: String = params
                        .iter()
                        .map(|p| format!("<param name=\"{p}\" type=\"float\"/>"))
                        .collect();
                    format!(
                        "        <source id=\"{id}-{name}\">\n          <float_array id=\"{id}-{name}-array\" count=\"{}\">{}</float_array>\n          <technique_common>\n            <accessor source=\"#{id}-{name}-array\" count=\"{}\" stride=\"{stride}\">{params}</accessor>\n          </technique_common>\n        </source>\n",
                        values.len(),
                        text.join(" "),
                        values.len() / stride.max(1)
                    )
                };
                let name = escape(&occurrence_name(doc, &occ.path));
                let _ = write!(
                    geometries,
                    "    <geometry id=\"{id}\" name=\"{name}\">\n      <mesh>\n{}{}{}        <vertices id=\"{id}-vertices\">\n          <input semantic=\"POSITION\" source=\"#{id}-positions\"/>\n        </vertices>\n{primitives}      </mesh>\n    </geometry>\n",
                    source("positions", &positions, &["X", "Y", "Z"]),
                    source("normals", &normals, &["X", "Y", "Z"]),
                    source("uvs", &uvs, &["S", "T"]),
                );
            }
            // The node: world matrix (row-major), translation in file units.
            let w = occ.world;
            let m = w.matrix3;
            let t = w.translation * scale;
            let matrix = [
                m.x_axis.x, m.y_axis.x, m.z_axis.x, t.x, m.x_axis.y, m.y_axis.y, m.z_axis.y, t.y,
                m.x_axis.z, m.y_axis.z, m.z_axis.z, t.z, 0.0, 0.0, 0.0, 1.0,
            ]
            .map(|v| number(round5(v)));
            let symbols: String = used
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    format!(
                        "<instance_material symbol=\"material-{i}\" target=\"#material-{i}\"><bind_vertex_input semantic=\"UVSET0\" input_semantic=\"TEXCOORD\" input_set=\"0\"/></instance_material>"
                    )
                })
                .collect();
            let _ = writeln!(
                nodes,
                "      <node id=\"node-{n}\" name=\"{}\">\n        <matrix>{}</matrix>\n        <instance_geometry url=\"#{id}\"><bind_material><technique_common>{symbols}</technique_common></bind_material></instance_geometry>\n      </node>",
                escape(&occurrence_name(doc, &occ.path)),
                matrix.join(" ")
            );
        }
        progress.report(0.6, "writing materials");
        // Materials, effects and images.
        let mut images = String::new();
        let mut effects = String::new();
        let mut materials = String::new();
        let mut side_files: Vec<(String, Vec<u8>)> = Vec::new();
        for (i, id) in used.iter().enumerate() {
            let m = doc.materials.get(*id);
            let name = m.map_or("Material".to_string(), |m| m.name.clone());
            let opacity = m.map_or(1.0, |m| f64::from(m.opacity.clamp(0.0, 1.0)));
            let texture = m
                .and_then(|m| m.texture.as_ref())
                .and_then(|t| doc.images.get(t.image).map(|img| (img, t)));
            let diffuse = match texture {
                Some((image, _)) => {
                    let ext = match image.format {
                        crate::document::ImageFormat::Png => "png",
                        crate::document::ImageFormat::Jpeg => "jpg",
                        crate::document::ImageFormat::WebP => "webp",
                    };
                    let file = format!(
                        "{texture_dir}/{}.{ext}",
                        xml_id("m", &name).trim_start_matches("m-")
                    );
                    side_files.push((file.clone(), image.bytes.clone()));
                    let _ = writeln!(
                        images,
                        "    <image id=\"image-{i}\"><init_from>{}</init_from></image>",
                        escape(&file)
                    );
                    format!(
                        "<newparam sid=\"surface-{i}\"><surface type=\"2D\"><init_from>image-{i}</init_from></surface></newparam><newparam sid=\"sampler-{i}\"><sampler2D><source>surface-{i}</source></sampler2D></newparam><technique sid=\"common\"><lambert><diffuse><texture texture=\"sampler-{i}\" texcoord=\"UVSET0\"/></diffuse>"
                    )
                }
                None => {
                    let c = m.map_or([255, 255, 255], |m| [m.color.r, m.color.g, m.color.b]);
                    format!(
                        "<technique sid=\"common\"><lambert><diffuse><color>{} {} {} 1</color></diffuse>",
                        number(round5(linear(c[0]))),
                        number(round5(linear(c[1]))),
                        number(round5(linear(c[2])))
                    )
                }
            };
            let _ = writeln!(
                effects,
                "    <effect id=\"effect-{i}\"><profile_COMMON>{diffuse}<transparent opaque=\"A_ONE\"><color>1 1 1 1</color></transparent><transparency><float>{}</float></transparency></lambert></technique></profile_COMMON></effect>",
                number(round5(opacity))
            );
            let _ = writeln!(
                materials,
                "    <material id=\"material-{i}\" name=\"{}\"><instance_effect url=\"#effect-{i}\"/></material>",
                escape(&name)
            );
        }
        let unit = opts.units.millimeters() / 1000.0;
        let unit_name = match opts.units {
            crate::units::LengthUnit::Millimeter => "millimeter",
            crate::units::LengthUnit::Centimeter => "centimeter",
            crate::units::LengthUnit::Meter => "meter",
            crate::units::LengthUnit::Foot => "foot",
            _ => "inch",
        };
        let mut xml = String::new();
        let _ = writeln!(xml, "<?xml version=\"1.0\" encoding=\"utf-8\"?>");
        let _ = writeln!(
            xml,
            "<COLLADA xmlns=\"http://www.collada.org/2005/11/COLLADASchema\" version=\"1.4.1\">"
        );
        let _ = writeln!(
            xml,
            "  <asset>\n    <contributor><authoring_tool>Planyrith {}</authoring_tool></contributor>\n    <created>1970-01-01T00:00:00Z</created>\n    <modified>1970-01-01T00:00:00Z</modified>\n    <unit name=\"{unit_name}\" meter=\"{}\"/>\n    <up_axis>Z_UP</up_axis>\n  </asset>",
            env!("CARGO_PKG_VERSION"),
            number(unit)
        );
        if !images.is_empty() {
            let _ = writeln!(xml, "  <library_images>\n{images}  </library_images>");
        }
        let _ = writeln!(xml, "  <library_effects>\n{effects}  </library_effects>");
        let _ = writeln!(
            xml,
            "  <library_materials>\n{materials}  </library_materials>"
        );
        let _ = writeln!(
            xml,
            "  <library_geometries>\n{geometries}  </library_geometries>"
        );
        let _ = writeln!(
            xml,
            "  <library_visual_scenes>\n    <visual_scene id=\"scene\" name=\"{}\">\n{nodes}    </visual_scene>\n  </library_visual_scenes>",
            escape(&stem)
        );
        let _ = writeln!(
            xml,
            "  <scene><instance_visual_scene url=\"#scene\"/></scene>"
        );
        let _ = writeln!(xml, "</COLLADA>");
        dst.write_all(xml.as_bytes())
            .map_err(|e| IoError::Storage {
                key: "dae".into(),
                reason: e.to_string(),
            })?;
        Ok(ExportReport {
            bytes_written: xml.len() as u64,
            entities: triangles_total,
            warnings: Vec::new(),
            side_files,
        })
    }
}
