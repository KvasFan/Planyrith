//! Implements docs/09-file-formats.md §5 USD export: the visible model as
//! a text stage (`.usda`) or packed for augmented-reality viewers
//! (`.usdz`: the stage stored uncompressed, 64-byte aligned). One mesh per
//! occurrence and material, world coordinates baked in, Z up, the file
//! unit as `metersPerUnit`; materials as preview surfaces with color,
//! opacity, roughness and metalness (docs/09 Decisions, USD).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, System, ZipWriter};

use crate::document::selection::occurrence_name;
use crate::document::{Document, FrontOrBack, Material};
use crate::ids::MaterialId;
use crate::io::export::scene::collect;
use crate::io::export::{
    ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter, output_transform,
};
use crate::io::{IoError, Progress};

/// The USD exporter: text stage or USDZ package.
#[derive(Debug, Default, Clone, Copy)]
pub struct UsdExporter {
    /// Write a `.usdz` package instead of `.usda` text.
    pub packed: bool,
}

fn storage_err(e: &dyn std::fmt::Display) -> IoError {
    IoError::Storage {
        key: "usd".into(),
        reason: e.to_string(),
    }
}

/// A valid prim name: letters, digits and `_`, not starting with a digit.
fn prim_name(text: &str) -> String {
    let mut out: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn number(v: f64) -> String {
    format!("{}", v + 0.0)
}

/// sRGB channel (0–255) to linear (USD colors are linear).
fn linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        libm::pow((c + 0.055) / 1.055, 2.4)
    }
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// One mesh prim: welded points, triangles and their normals.
struct Part {
    name: String,
    material: MaterialId,
    points: Vec<glam::DVec3>,
    triangles: Vec<[u32; 3]>,
    normals: Vec<glam::DVec3>,
}

fn parts(doc: &Document, scope: &ExportScope, opts: &ExportOptions) -> Vec<Part> {
    let scene = collect(doc, scope);
    let out = output_transform(
        &ExportOptions {
            z_up: true,
            ..opts.clone()
        },
        false,
    );
    let mut parts: Vec<Part> = Vec::new();
    let mut taken: BTreeMap<String, usize> = BTreeMap::new();
    for occ in &scene.occurrences {
        let Some(mesh) = scene.mesh(occ.container) else {
            continue;
        };
        let to = out * occ.world;
        let flip = to.matrix3.determinant() < 0.0;
        let normal_matrix = to.matrix3.inverse().transpose();
        let base = prim_name(&occurrence_name(doc, &occ.path));
        let mut by_material: BTreeMap<MaterialId, usize> = BTreeMap::new();
        let mut welds: Vec<BTreeMap<[u64; 3], u32>> = Vec::new();
        let first = parts.len();
        for face in &mesh.faces {
            let material = doc
                .materials
                .resolve(face.material.or(occ.material_override), FrontOrBack::Front);
            let slot = *by_material.entry(material).or_insert_with(|| {
                let n = taken.entry(base.clone()).or_insert(0);
                *n += 1;
                parts.push(Part {
                    name: format!("{base}_{n}"),
                    material,
                    points: Vec::new(),
                    triangles: Vec::new(),
                    normals: Vec::new(),
                });
                welds.push(BTreeMap::new());
                parts.len() - 1 - first
            });
            let part = &mut parts[first + slot];
            let weld = &mut welds[slot];
            let n = (normal_matrix * face.normal).normalize_or_zero() + glam::DVec3::ZERO;
            let ids: Vec<u32> = face
                .positions
                .iter()
                .map(|p| {
                    let q = to.transform_point3(*p) + glam::DVec3::ZERO;
                    *weld
                        .entry([q.x.to_bits(), q.y.to_bits(), q.z.to_bits()])
                        .or_insert_with(|| {
                            part.points.push(q);
                            (part.points.len() - 1) as u32
                        })
                })
                .collect();
            for t in &face.triangles {
                let [a, b, c] = t.map(|i| ids[i as usize]);
                part.triangles
                    .push(if flip { [a, c, b] } else { [a, b, c] });
                part.normals.push(n);
            }
        }
    }
    parts.retain(|p| !p.triangles.is_empty());
    parts
}

/// The stage as text.
fn stage(doc: &Document, parts: &[Part], meters_per_unit: f64) -> String {
    let mut used: Vec<MaterialId> = Vec::new();
    for p in parts {
        if !used.contains(&p.material) {
            used.push(p.material);
        }
    }
    let mut names: BTreeMap<MaterialId, String> = BTreeMap::new();
    let mut taken: BTreeMap<String, usize> = BTreeMap::new();
    for &id in &used {
        let base = prim_name(
            doc.materials
                .get(id)
                .map_or("Material", |m| m.name.as_str()),
        );
        let n = taken.entry(base.clone()).or_insert(0);
        *n += 1;
        let name = if *n == 1 { base } else { format!("{base}_{n}") };
        names.insert(id, name);
    }
    let mut s = String::new();
    let _ = writeln!(s, "#usda 1.0");
    let _ = writeln!(s, "(");
    let _ = writeln!(s, "    defaultPrim = \"Model\"");
    let _ = writeln!(s, "    doc = \"Planyrith {}\"", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(s, "    metersPerUnit = {}", number(meters_per_unit));
    let _ = writeln!(s, "    upAxis = \"Z\"");
    let _ = writeln!(s, ")");
    let _ = writeln!(s);
    let _ = writeln!(s, "def Xform \"Model\" (");
    let _ = writeln!(s, "    kind = \"component\"");
    let _ = writeln!(s, ")");
    let _ = writeln!(s, "{{");
    for p in parts {
        let _ = writeln!(s, "    def Mesh \"{}\" (", p.name);
        let _ = writeln!(s, "        prepend apiSchemas = [\"MaterialBindingAPI\"]");
        let _ = writeln!(s, "    )");
        let _ = writeln!(s, "    {{");
        let counts = vec!["3"; p.triangles.len()].join(", ");
        let _ = writeln!(s, "        int[] faceVertexCounts = [{counts}]");
        let indices: Vec<String> = p
            .triangles
            .iter()
            .flat_map(|t| t.map(|i| i.to_string()))
            .collect();
        let _ = writeln!(
            s,
            "        int[] faceVertexIndices = [{}]",
            indices.join(", ")
        );
        let points: Vec<String> = p
            .points
            .iter()
            .map(|q| format!("({}, {}, {})", number(q.x), number(q.y), number(q.z)))
            .collect();
        let _ = writeln!(s, "        point3f[] points = [{}]", points.join(", "));
        let normals: Vec<String> = p
            .normals
            .iter()
            .flat_map(|n| {
                let v = format!(
                    "({}, {}, {})",
                    number(round4(n.x)),
                    number(round4(n.y)),
                    number(round4(n.z))
                );
                [v.clone(), v.clone(), v]
            })
            .collect();
        let _ = writeln!(s, "        normal3f[] normals = [{}] (", normals.join(", "));
        let _ = writeln!(s, "            interpolation = \"faceVarying\"");
        let _ = writeln!(s, "        )");
        let _ = writeln!(s, "        uniform bool doubleSided = 1");
        let _ = writeln!(s, "        uniform token subdivisionScheme = \"none\"");
        let _ = writeln!(
            s,
            "        rel material:binding = </Model/Materials/{}>",
            names.get(&p.material).map_or("Material", String::as_str)
        );
        let _ = writeln!(s, "    }}");
        let _ = writeln!(s);
    }
    let _ = writeln!(s, "    def Scope \"Materials\"");
    let _ = writeln!(s, "    {{");
    for &id in &used {
        let name = names.get(&id).map_or("Material", String::as_str);
        let m = doc.materials.get(id);
        let (rgb, opacity, roughness, metallic) = m.map_or(
            (
                [255, 255, 255],
                1.0,
                f64::from(Material::DEFAULT_ROUGHNESS),
                0.0,
            ),
            |m| {
                (
                    [m.color.r, m.color.g, m.color.b],
                    f64::from(m.opacity),
                    f64::from(m.roughness.unwrap_or(Material::DEFAULT_ROUGHNESS)),
                    f64::from(m.metallic.unwrap_or(0.0)),
                )
            },
        );
        let path = format!("/Model/Materials/{name}");
        let _ = writeln!(s, "        def Material \"{name}\"");
        let _ = writeln!(s, "        {{");
        let _ = writeln!(
            s,
            "            token outputs:surface.connect = <{path}/Surface.outputs:surface>"
        );
        let _ = writeln!(s, "            def Shader \"Surface\"");
        let _ = writeln!(s, "            {{");
        let _ = writeln!(
            s,
            "                uniform token info:id = \"UsdPreviewSurface\""
        );
        let _ = writeln!(
            s,
            "                color3f inputs:diffuseColor = ({}, {}, {})",
            number(round4(linear(rgb[0]))),
            number(round4(linear(rgb[1]))),
            number(round4(linear(rgb[2])))
        );
        let _ = writeln!(
            s,
            "                float inputs:metallic = {}",
            number(round4(metallic))
        );
        let _ = writeln!(
            s,
            "                float inputs:opacity = {}",
            number(round4(opacity))
        );
        let _ = writeln!(
            s,
            "                float inputs:roughness = {}",
            number(round4(roughness))
        );
        let _ = writeln!(s, "                token outputs:surface");
        let _ = writeln!(s, "            }}");
        let _ = writeln!(s, "        }}");
    }
    let _ = writeln!(s, "    }}");
    let _ = writeln!(s, "}}");
    s
}

/// A USDZ package: entries stored uncompressed, each one's data starting
/// on a 64-byte boundary, the stage first.
pub(crate) fn usdz(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, IoError> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .last_modified_time(DateTime::default())
            .unix_permissions(0o644)
            .system(System::Unix)
            .with_alignment(64);
        writer
            .start_file(*name, options)
            .map_err(|e| storage_err(&e))?;
        writer.write_all(bytes).map_err(|e| storage_err(&e))?;
    }
    Ok(writer.finish().map_err(|e| storage_err(&e))?.into_inner())
}

impl Exporter for UsdExporter {
    fn format(&self) -> ExportFormat {
        if self.packed {
            ExportFormat::Usdz
        } else {
            ExportFormat::Usda
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
        let parts = parts(doc, scope, opts);
        progress.report(0.5, "writing the stage");
        let text = stage(doc, &parts, opts.units.millimeters() / 1000.0);
        let bytes = if self.packed {
            usdz(&[("model.usda", text.as_bytes())])?
        } else {
            text.into_bytes()
        };
        dst.write_all(&bytes).map_err(|e| storage_err(&e))?;
        Ok(ExportReport {
            bytes_written: bytes.len() as u64,
            entities: parts.iter().map(|p| p.triangles.len()).sum(),
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn names_and_colors() {
        assert_eq!(prim_name("Oak table #2"), "Oak_table__2");
        assert_eq!(prim_name("3D"), "_3D");
        assert!((linear(255) - 1.0).abs() < 1e-12 && linear(0) == 0.0);
    }

    #[test]
    fn the_package_is_stored_and_aligned() {
        let bytes = usdz(&[("model.usda", b"#usda 1.0\n"), ("a.png", &[1, 2, 3])]).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
        for i in 0..archive.len() {
            let file = archive.by_index(i).unwrap();
            assert_eq!(file.compression(), CompressionMethod::Stored);
            assert_eq!(file.data_start().unwrap() % 64, 0, "entry {i}");
        }
        let mut first = String::new();
        archive
            .by_index(0)
            .unwrap()
            .read_to_string(&mut first)
            .unwrap();
        assert!(first.starts_with("#usda 1.0"));
    }
}
