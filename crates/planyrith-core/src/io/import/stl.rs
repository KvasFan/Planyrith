//! Implements docs/09-file-formats.md §5 STL import: ASCII and binary
//! triangle lists (binary when the size matches its triangle count), one
//! mesh, unitless (`--units`), Y-up unless `--z-up` (docs/09 §6). Triangles
//! merge into faces through the shared mesh path (`build`).

use std::io::Read;

use glam::DVec3;

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::io::import::build::{ImportedMesh, ImportedScene, build_scene};
use crate::io::import::{Confidence, ImportFormat, ImportOptions, ImportReport, Importer};
use crate::io::{IoError, Progress};

/// The STL importer.
#[derive(Debug, Default, Clone, Copy)]
pub struct StlImporter;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "stl".into(),
        reason: reason.into(),
    }
}

/// `true` when `bytes` are a binary STL (its triangle count matches its
/// size); ASCII files start with `solid` and do not.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.len() >= 84 && {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        n.checked_mul(50).and_then(|b| b.checked_add(84)) == Some(bytes.len())
    }
}

/// The triangles of an STL file, in file order.
pub fn read_triangles(bytes: &[u8]) -> Result<Vec<[DVec3; 3]>, IoError> {
    if is_binary(bytes) {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        let f = |at: usize| {
            f64::from(f32::from_le_bytes([
                bytes[at],
                bytes[at + 1],
                bytes[at + 2],
                bytes[at + 3],
            ]))
        };
        return Ok((0..n)
            .map(|i| {
                let base = 84 + i * 50 + 12;
                let v = |k: usize| {
                    DVec3::new(f(base + k * 12), f(base + k * 12 + 4), f(base + k * 12 + 8))
                };
                [v(0), v(1), v(2)]
            })
            .collect());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| corrupt("neither binary nor ASCII STL"))?;
    let mut out = Vec::new();
    let mut corners: Vec<DVec3> = Vec::with_capacity(3);
    for (line_no, line) in text.lines().enumerate() {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("vertex") => {
                let mut c = [0.0f64; 3];
                for v in &mut c {
                    *v = words
                        .next()
                        .and_then(|w| w.parse::<f64>().ok())
                        .filter(|x| x.is_finite())
                        .ok_or_else(|| corrupt(format!("line {}: bad vertex", line_no + 1)))?;
                }
                corners.push(DVec3::from(c));
            }
            Some("endloop") => {
                if corners.len() == 3 {
                    out.push([corners[0], corners[1], corners[2]]);
                } else if !corners.is_empty() {
                    return Err(corrupt(format!(
                        "line {}: a facet has {} vertices",
                        line_no + 1,
                        corners.len()
                    )));
                }
                corners.clear();
            }
            _ => {}
        }
    }
    Ok(out)
}

impl Importer for StlImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Stl
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        if bytes.starts_with(b"solid") && bytes.windows(5).any(|w| w == b"facet") {
            Confidence::CERTAIN
        } else if name.to_ascii_lowercase().ends_with(".stl") {
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
        progress.report(0.1, "reading triangles");
        let triangles = read_triangles(&bytes)?;
        let mut mesh = ImportedMesh {
            name: "STL".into(),
            ..ImportedMesh::default()
        };
        for t in &triangles {
            let base = mesh.positions.len() as u32;
            mesh.positions.extend_from_slice(t);
            mesh.triangles.push([base, base + 1, base + 2]);
            mesh.materials.push(None);
        }
        progress.report(0.5, "building faces");
        build_scene(
            tx,
            target,
            ImportedScene {
                name: "STL".into(),
                meshes: vec![mesh],
                y_up: true,
                ..ImportedScene::default()
            },
            opts,
        )
    }
}
