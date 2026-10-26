//! Implements docs/09-file-formats.md §5 STL export: every visible face
//! triangulated in world coordinates, binary by default (`--ascii` for
//! text), in `--units`, Y-up unless `--z-up` (docs/09 Decisions).

use std::io::Write;

use crate::document::Document;
use crate::io::export::scene::collect;
use crate::io::export::{
    ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter, output_transform,
};
use crate::io::{IoError, Progress};

/// The STL exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct StlExporter;

fn write_err(e: std::io::Error) -> IoError {
    IoError::Storage {
        key: "stl".into(),
        reason: e.to_string(),
    }
}

impl Exporter for StlExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Stl
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
        let out = output_transform(opts, true);
        let mut triangles: Vec<[glam::DVec3; 4]> = Vec::new();
        for occ in &scene.occurrences {
            let Some(mesh) = scene.mesh(occ.container) else {
                continue;
            };
            let to = out * occ.world;
            let flip = to.matrix3.determinant() < 0.0;
            for face in &mesh.faces {
                // `+ 0.0` turns −0 into 0 (`-0` in ASCII files).
                let normal = (to.matrix3 * face.normal).normalize_or_zero() + 0.0;
                for t in &face.triangles {
                    let p = t.map(|i| to.transform_point3(face.positions[i as usize]) + 0.0);
                    let [a, b, c] = if flip { [p[0], p[2], p[1]] } else { p };
                    triangles.push([normal, a, b, c]);
                }
            }
        }
        progress.report(0.5, "writing triangles");
        let mut written = 0u64;
        if opts.ascii {
            let mut put = |text: String| -> Result<(), IoError> {
                dst.write_all(text.as_bytes()).map_err(write_err)?;
                written += text.len() as u64;
                Ok(())
            };
            put("solid planyrith\n".into())?;
            for [n, a, b, c] in &triangles {
                let mut facet = format!(
                    "facet normal {} {} {}\n outer loop\n",
                    n.x as f32, n.y as f32, n.z as f32
                );
                for v in [a, b, c] {
                    facet.push_str(&format!(
                        "  vertex {} {} {}\n",
                        v.x as f32, v.y as f32, v.z as f32
                    ));
                }
                facet.push_str(" endloop\nendfacet\n");
                put(facet)?;
            }
            put("endsolid planyrith\n".into())?;
        } else {
            let mut header = [0u8; 80];
            header[..9].copy_from_slice(b"planyrith");
            dst.write_all(&header).map_err(write_err)?;
            dst.write_all(&(triangles.len() as u32).to_le_bytes())
                .map_err(write_err)?;
            written += 84;
            for t in &triangles {
                let mut record = [0u8; 50];
                for (k, v) in t.iter().enumerate() {
                    for (j, x) in [v.x, v.y, v.z].iter().enumerate() {
                        let at = k * 12 + j * 4;
                        record[at..at + 4].copy_from_slice(&(*x as f32).to_le_bytes());
                    }
                }
                dst.write_all(&record).map_err(write_err)?;
                written += 50;
            }
        }
        Ok(ExportReport {
            bytes_written: written,
            entities: triangles.len(),
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}
