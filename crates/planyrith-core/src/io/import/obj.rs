//! Implements docs/09-file-formats.md §5 OBJ import: objects and groups
//! (`o`/`g`) become groups, `usemtl` materials come from the MTL files the
//! OBJ names (read through the engine's storage next to it, with their
//! diffuse textures), texture coordinates map the textures. Polygons are
//! ear-clipped in their plane; points and lines are skipped (`W2004`).
//! Unitless (`--units`), Y-up unless `--z-up` (docs/09 §6).

use std::collections::BTreeMap;
use std::io::{BufReader, Read};
use std::path::Path;

use glam::{DVec2, DVec3};

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::diagnostics::{Warning, WarningCode};
use crate::document::Rgba8;
use crate::geom::tessellate::tessellate_polygon_2d;
use crate::io::import::build::{ImportedMaterial, ImportedMesh, ImportedScene, build_scene};
use crate::io::import::{
    Confidence, ImportFormat, ImportOptions, ImportReport, Importer, sibling, source_stem,
};
use crate::io::{IoError, Progress, StorageKey};
use crate::math::Plane;

/// The OBJ importer.
#[derive(Debug, Default, Clone, Copy)]
pub struct ObjImporter;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "obj".into(),
        reason: reason.into(),
    }
}

/// `0..1` color channels to 8 bits.
fn channel(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Triangles of one polygon (corner indices into `corners`), counter-
/// clockwise around its Newell normal.
fn triangulate(corners: &[DVec3]) -> Vec<[usize; 3]> {
    match corners.len() {
        0..=2 => Vec::new(),
        3 => vec![[0, 1, 2]],
        n => {
            let mut normal = DVec3::ZERO;
            for i in 0..n {
                let (a, b) = (corners[i], corners[(i + 1) % n]);
                normal += DVec3::new(
                    (a.y - b.y) * (a.z + b.z),
                    (a.z - b.z) * (a.x + b.x),
                    (a.x - b.x) * (a.y + b.y),
                );
            }
            let Some(normal) = normal.try_normalize() else {
                return Vec::new();
            };
            let frame = Plane::from_point_normal(corners[0], normal).frame();
            let flat: Vec<DVec2> = corners.iter().map(|c| frame.to_2d(*c)).collect();
            let Ok(tris) = tessellate_polygon_2d(&flat, &[]) else {
                return Vec::new();
            };
            tris.into_iter()
                .map(|t| {
                    let [a, b, c] = t.map(|i| i as usize);
                    let turn = (corners[b] - corners[a]).cross(corners[c] - corners[a]);
                    if turn.dot(normal) < 0.0 {
                        [a, c, b]
                    } else {
                        [a, b, c]
                    }
                })
                .collect()
        }
    }
}

impl Importer for ObjImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Obj
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        let head = &bytes[..bytes.len().min(4096)];
        let text = String::from_utf8_lossy(head);
        if text
            .lines()
            .any(|l| l.starts_with("v ") || l.starts_with("mtllib "))
        {
            Confidence(80)
        } else if name.to_ascii_lowercase().ends_with(".obj") {
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
        let mut warnings = Vec::new();
        let skipped_lines = bytes
            .split(|b| *b == b'\n')
            .filter(|l| l.starts_with(b"l ") || l.starts_with(b"p "))
            .count();
        if skipped_lines > 0 {
            warnings.push(Warning::new(
                WarningCode::W2004,
                format!("{skipped_lines} point or line element(s) skipped"),
            ));
        }
        progress.report(0.1, "reading OBJ");
        let storage = tx.storage();
        let missing = std::cell::RefCell::new(Vec::<String>::new());
        let load = tobj::LoadOptions {
            triangulate: false,
            single_index: false,
            ignore_points: true,
            ignore_lines: true,
        };
        let (models, materials) = tobj::load_obj_buf(
            &mut BufReader::new(bytes.as_slice()),
            &load,
            |mtl: &Path| {
                let key = sibling(&opts.source, &mtl.to_string_lossy());
                match storage.map(|s| s.read(&StorageKey::new(&key))) {
                    Some(Ok(text)) => tobj::load_mtl_buf(&mut BufReader::new(text.as_slice())),
                    _ => {
                        missing.borrow_mut().push(key);
                        Err(tobj::LoadError::OpenFileFailed)
                    }
                }
            },
        )
        .map_err(|e| corrupt(e.to_string()))?;
        for key in missing.into_inner() {
            warnings.push(Warning::new(
                WarningCode::W2004,
                format!("material library `{key}` not found; default materials used"),
            ));
        }
        let materials = materials.unwrap_or_default();

        // Materials, with their diffuse textures.
        let mut imported_materials = Vec::with_capacity(materials.len());
        for m in &materials {
            let [r, g, b] = m.diffuse.unwrap_or([0.8, 0.8, 0.8]).map(channel);
            let texture = m.diffuse_texture.as_ref().and_then(|t| {
                // Options (`-s 1 1 1 file.png`) come first; the file is last.
                let file = t.split_whitespace().last().unwrap_or(t);
                let key = sibling(&opts.source, file);
                match storage.map(|s| s.read(&StorageKey::new(&key))) {
                    Some(Ok(bytes)) => Some(bytes),
                    _ => {
                        warnings.push(Warning::new(
                            WarningCode::W2004,
                            format!("texture `{key}` of `{}` not found", m.name),
                        ));
                        None
                    }
                }
            });
            imported_materials.push(ImportedMaterial {
                name: m.name.clone(),
                color: Rgba8::rgb(r, g, b),
                opacity: m.dissolve.unwrap_or(1.0),
                texture,
            });
        }

        // Models of the same name (split by `usemtl`) form one mesh.
        let mut meshes: Vec<ImportedMesh> = Vec::new();
        let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
        let mut textured: Vec<bool> = Vec::new();
        for model in &models {
            let name = if model.name.is_empty() || model.name == "unnamed_object" {
                source_stem(&opts.source, "OBJ")
            } else {
                model.name.clone()
            };
            let index = *by_name.entry(name.clone()).or_insert_with(|| {
                meshes.push(ImportedMesh {
                    name,
                    ..ImportedMesh::default()
                });
                textured.push(false);
                meshes.len() - 1
            });
            textured[index] |= !model.mesh.texcoord_indices.is_empty();
            let out = &mut meshes[index];
            let m = &model.mesh;
            let position = |i: u32| {
                let i = i as usize * 3;
                m.positions
                    .get(i..i + 3)
                    .map(|p| DVec3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2])))
            };
            let uv = |i: u32| {
                let i = i as usize * 2;
                m.texcoords
                    .get(i..i + 2)
                    .map(|t| DVec2::new(f64::from(t[0]), f64::from(t[1])))
            };
            let has_uv = !m.texcoord_indices.is_empty();
            let normal = |i: u32| {
                let i = i as usize * 3;
                m.normals
                    .get(i..i + 3)
                    .map(|n| DVec3::new(f64::from(n[0]), f64::from(n[1]), f64::from(n[2])))
                    .filter(|n| n.is_finite())
            };
            let has_normals = !m.normal_indices.is_empty();
            let material = m.material_id.filter(|&i| i < imported_materials.len());
            let arities: Vec<usize> = if m.face_arities.is_empty() {
                vec![3; m.indices.len() / 3]
            } else {
                m.face_arities.iter().map(|&a| a as usize).collect()
            };
            let mut at = 0usize;
            for arity in arities {
                let range = at..at + arity;
                at += arity;
                let Some(corners) = m.indices.get(range.clone()).map(|ix| {
                    ix.iter()
                        .map(|&i| position(i))
                        .collect::<Option<Vec<DVec3>>>()
                }) else {
                    return Err(corrupt("a face indexes past the vertex list"));
                };
                let corners =
                    corners.ok_or_else(|| corrupt("a face indexes past the vertex list"))?;
                if corners.iter().any(|c| !c.is_finite()) {
                    return Err(corrupt("a vertex is not a finite number"));
                }
                let normals: Vec<DVec3> = if has_normals {
                    m.normal_indices
                        .get(range.clone())
                        .map(|ix| {
                            ix.iter()
                                .map(|&i| normal(i).unwrap_or(DVec3::ZERO))
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                let uvs: Vec<DVec2> = if has_uv {
                    m.texcoord_indices
                        .get(range)
                        .map(|ix| ix.iter().map(|&i| uv(i).unwrap_or(DVec2::ZERO)).collect())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                for tri in triangulate(&corners) {
                    let base = out.positions.len() as u32;
                    for k in tri {
                        out.positions.push(corners[k]);
                        out.uvs.push(uvs.get(k).copied().unwrap_or(DVec2::ZERO));
                        out.normals
                            .push(normals.get(k).copied().unwrap_or(DVec3::ZERO));
                    }
                    out.triangles.push([base, base + 1, base + 2]);
                    out.materials.push(material);
                }
            }
        }
        // A mesh without texture coordinates anywhere carries none.
        for (mesh, textured) in meshes.iter_mut().zip(&textured) {
            if !textured {
                mesh.uvs.clear();
            }
            if mesh.normals.iter().all(|n| n.length_squared() < 0.5) {
                mesh.normals.clear();
            }
        }
        meshes.retain(|m| !m.triangles.is_empty());
        progress.report(0.5, "building faces");
        let mut report = build_scene(
            tx,
            target,
            ImportedScene {
                name: source_stem(&opts.source, "OBJ"),
                materials: imported_materials,
                meshes,
                nodes: Vec::new(),
                y_up: true,
                units: None,
            },
            opts,
        )?;
        report.skipped += skipped_lines;
        warnings.append(&mut report.warnings);
        report.warnings = warnings;
        Ok(report)
    }
}
