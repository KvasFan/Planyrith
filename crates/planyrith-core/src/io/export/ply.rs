//! Implements docs/09-file-formats.md §5 PLY export: the visible faces as
//! one triangle mesh in world coordinates (Z up, `--file-units`), with the
//! front material's color on each vertex; binary little-endian by default,
//! `--ascii` for text (docs/09 Decisions, PLY).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write;

use crate::document::{Document, FrontOrBack};
use crate::io::export::scene::collect;
use crate::io::export::{
    ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter, output_transform,
};
use crate::io::{IoError, Progress};

/// The PLY exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlyExporter;

fn write_err(e: std::io::Error) -> IoError {
    IoError::Storage {
        key: "ply".into(),
        reason: e.to_string(),
    }
}

impl Exporter for PlyExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Ply
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
        let out = output_transform(
            &ExportOptions {
                z_up: true,
                ..opts.clone()
            },
            false,
        );
        // Vertices welded on (position bits, color).
        let mut index: BTreeMap<([u64; 3], [u8; 3]), u32> = BTreeMap::new();
        let mut vertices: Vec<([f32; 3], [u8; 3])> = Vec::new();
        let mut triangles: Vec<[u32; 3]> = Vec::new();
        for occ in &scene.occurrences {
            let Some(mesh) = scene.mesh(occ.container) else {
                continue;
            };
            let to = out * occ.world;
            let flip = to.matrix3.determinant() < 0.0;
            for face in &mesh.faces {
                let id = doc
                    .materials
                    .resolve(face.material.or(occ.material_override), FrontOrBack::Front);
                let rgb = doc
                    .materials
                    .get(id)
                    .map_or([255, 255, 255], |m| [m.color.r, m.color.g, m.color.b]);
                let ids: Vec<u32> = face
                    .positions
                    .iter()
                    .map(|p| {
                        let q = to.transform_point3(*p) + glam::DVec3::ZERO;
                        let key = ([q.x.to_bits(), q.y.to_bits(), q.z.to_bits()], rgb);
                        *index.entry(key).or_insert_with(|| {
                            vertices.push(([q.x as f32, q.y as f32, q.z as f32], rgb));
                            (vertices.len() - 1) as u32
                        })
                    })
                    .collect();
                for t in &face.triangles {
                    let [a, b, c] = t.map(|i| ids[i as usize]);
                    triangles.push(if flip { [a, c, b] } else { [a, b, c] });
                }
            }
        }
        progress.report(0.5, "writing the mesh");
        let mut header = String::new();
        let _ = writeln!(header, "ply");
        let _ = writeln!(
            header,
            "format {} 1.0",
            if opts.ascii {
                "ascii"
            } else {
                "binary_little_endian"
            }
        );
        let _ = writeln!(header, "comment Planyrith {}", env!("CARGO_PKG_VERSION"));
        let _ = writeln!(header, "element vertex {}", vertices.len());
        for axis in ["x", "y", "z"] {
            let _ = writeln!(header, "property float {axis}");
        }
        for channel in ["red", "green", "blue"] {
            let _ = writeln!(header, "property uchar {channel}");
        }
        let _ = writeln!(header, "element face {}", triangles.len());
        let _ = writeln!(header, "property list uchar uint vertex_indices");
        let _ = writeln!(header, "end_header");
        let mut bytes = header.into_bytes();
        if opts.ascii {
            let mut text = String::new();
            for ([x, y, z], [r, g, b]) in &vertices {
                let _ = writeln!(text, "{x} {y} {z} {r} {g} {b}");
            }
            for [a, b, c] in &triangles {
                let _ = writeln!(text, "3 {a} {b} {c}");
            }
            bytes.extend(text.into_bytes());
        } else {
            for (p, rgb) in &vertices {
                for v in p {
                    bytes.extend(v.to_le_bytes());
                }
                bytes.extend(rgb);
            }
            for t in &triangles {
                bytes.push(3);
                for i in t {
                    bytes.extend(i.to_le_bytes());
                }
            }
        }
        dst.write_all(&bytes).map_err(write_err)?;
        Ok(ExportReport {
            bytes_written: bytes.len() as u64,
            entities: triangles.len(),
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}
