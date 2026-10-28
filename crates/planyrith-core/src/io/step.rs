//! Implements docs/09-file-formats.md §5 STEP, through the optional B-rep bridge.

use crate::commands::params::Target;
use crate::commands::{CommandError, Transaction};
use crate::document::{
    ComponentDefinition, ComponentInstance, Document, Entities, EntityRecord, Group,
};
use crate::geom::boolean::{Piece, stitch};
use crate::geom::{FaceSides, HalfEdgeMesh};
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter};
use crate::io::import::{
    Confidence, ImportAs, ImportFormat, ImportOptions, ImportReport, Importer,
};
use crate::io::{IoError, Progress};
use crate::tolerance::Tolerances;
use glam::{DAffine3, DVec3};
use std::io::{Read, Write};

fn error(e: impl std::fmt::Display) -> IoError {
    IoError::corrupt("step", e.to_string())
}

/// Deterministic STEP serialization of planar solids in millimeters.
pub fn write_solids(meshes: &[HalfEdgeMesh]) -> Result<String, IoError> {
    use truck_stepio::out::{CompleteStepDisplay, StepHeaderDescriptor, StepModels};
    let solids = meshes
        .iter()
        .map(crate::geom::brep::to_solid)
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let compressed = solids.iter().map(|s| s.compress()).collect::<Vec<_>>();
    let models: StepModels<'_, _, _, _> = compressed.iter().collect();
    let header = StepHeaderDescriptor {
        file_name: "Planyrith".into(),
        time_stamp: "1970-01-01T00:00:00".into(),
        authors: Vec::new(),
        organization: Vec::new(),
        organization_system: "Planyrith".into(),
        authorization: String::new(),
    };
    Ok(CompleteStepDisplay::new(models, header).to_string())
}

/// Reads closed STEP shells in entity-number order. Planar line-edge shells
/// preserve every boundary vertex; curved shells are tessellated.
pub fn read_solids(source: &str, tol: &Tolerances) -> Result<Vec<HalfEdgeMesh>, IoError> {
    if source.len() > 64 * 1024 * 1024 {
        return Err(IoError::LimitExceeded("STEP input exceeds 64 MiB".into()));
    }
    std::panic::catch_unwind(|| read_solids_inner(source, tol))
        .map_err(|_| error("invalid STEP topology"))?
}

fn read_solids_inner(source: &str, tol: &Tolerances) -> Result<Vec<HalfEdgeMesh>, IoError> {
    if source.contains("MAPPED_ITEM")
        || source.contains("REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION")
    {
        return Err(IoError::UnsupportedFormat(
            "STEP assembly placements are not supported; export placed solid bodies".into(),
        ));
    }
    use truck_stepio::r#in::alias::{Curve3D, ElementarySurface, Surface};
    use truck_stepio::r#in::{Table, ruststep};
    let exchange = ruststep::parser::parse(source).map_err(error)?;
    let mut result = Vec::new();
    for data in &exchange.data {
        let table = Table::from_data_section(data);
        let mut shells = table.shell.iter().collect::<Vec<_>>();
        shells.sort_by_key(|(id, _)| **id);
        for (_, shell) in shells {
            let shell = table.to_compressed_shell(shell).map_err(error)?;
            if shell.vertices.is_empty() || shell.faces.is_empty() {
                return Err(error("empty or unsupported shell"));
            }
            let planar=shell.faces.iter().all(|f| matches!(&f.surface,Surface::ElementarySurface(s) if matches!(s.as_ref(),ElementarySurface::Plane(_)))) && shell.edges.iter().all(|e| matches!(e.curve,Curve3D::Line(_)));
            let mesh = if planar {
                let mut pieces = Vec::new();
                for face in &shell.faces {
                    let mut loops = Vec::new();
                    for boundary in &face.boundaries {
                        let mut ring = Vec::new();
                        for e in boundary {
                            let edge = shell
                                .edges
                                .get(e.index)
                                .ok_or_else(|| error("edge index out of range"))?;
                            let p = shell
                                .vertices
                                .get(if e.orientation {
                                    edge.vertices.0
                                } else {
                                    edge.vertices.1
                                })
                                .ok_or_else(|| error("vertex index out of range"))?;
                            ring.push(DVec3::new(p.x, p.y, p.z));
                        }
                        if !face.orientation {
                            ring.reverse();
                        }
                        loops.push(ring);
                    }
                    pieces.push(Piece {
                        loops,
                        sides: FaceSides::default(),
                    });
                }
                stitch(&pieces, tol).map_err(error)?
            } else {
                use truck_meshalgo::prelude::*;
                let polygon = shell.robust_triangulation(tol.planarity).to_polygon();
                let positions = polygon
                    .positions()
                    .iter()
                    .map(|p| DVec3::new(p.x, p.y, p.z))
                    .collect::<Vec<_>>();
                let triangles = polygon
                    .face_iter()
                    .flat_map(|f| {
                        (1..f.len().saturating_sub(1))
                            .map(move |i| [f[0].pos as u32, f[i].pos as u32, f[i + 1].pos as u32])
                    })
                    .collect::<Vec<_>>();
                HalfEdgeMesh::from_triangles_merged(&positions, &triangles, Default::default(), tol)
                    .map_err(error)?
                    .mesh
            };
            if !mesh.is_solid() {
                return Err(error("STEP shell is open, incomplete or non-manifold"));
            }
            result.push(mesh);
        }
    }
    if result.is_empty() {
        return Err(error("no closed shells"));
    }
    let mut outer = Vec::new();
    let mut voids = Vec::new();
    for mesh in result {
        if mesh.volume().map_err(error)? < 0.0 {
            voids.push(mesh);
        } else {
            outer.push(mesh);
        }
    }
    for void in voids {
        let sample = void
            .vertices()
            .next()
            .ok_or_else(|| error("empty void shell"))?
            .1
            .position;
        let mut parents = Vec::new();
        for (i, mesh) in outer.iter().enumerate() {
            if crate::geom::boolean::classify_point(mesh, sample, tol).map_err(error)?
                == crate::geom::boolean::Classification::Inside
            {
                parents.push((mesh.volume().map_err(error)?, i));
            }
        }
        parents.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (_, parent) = parents
            .first()
            .copied()
            .ok_or_else(|| error("void shell has no containing outer shell"))?;
        let (indexed, _) = void.to_indexed();
        outer[parent]
            .insert_disjoint(&indexed, tol, &mut crate::geom::NoDeltas)
            .map_err(error)?
            .ok_or_else(|| error("void touches its outer shell"))?;
    }
    Ok(outer)
}

/// STEP exporter (AP geometry in millimeters).
#[derive(Debug, Default, Clone, Copy)]
pub struct StepExporter;
impl Exporter for StepExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Step
    }
    fn export(
        &self,
        doc: &Document,
        scope: &ExportScope,
        _opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<ExportReport, IoError> {
        let scene = crate::io::export::scene::collect(doc, scope);
        let mut solids = Vec::new();
        for occurrence in &scene.occurrences {
            progress.check_cancelled()?;
            let mesh = doc
                .container(occurrence.container)
                .ok_or_else(|| error("container vanished"))?
                .mesh();
            if mesh.is_empty() {
                continue;
            }
            let exported = scene
                .mesh(occurrence.container)
                .ok_or_else(|| error("mesh vanished"))?;
            if exported.faces.len() != mesh.face_count() {
                return Err(IoError::UnsupportedFormat("STEP export needs whole visible solids; include hidden faces or select the whole solid".into()));
            }
            let (indexed, _) = mesh.to_indexed();
            let indexed = crate::geom::mesh::transform_indexed(&indexed, &occurrence.world);
            solids.push(HalfEdgeMesh::from_indexed(&indexed, doc.tolerances()).map_err(error)?);
        }
        if solids.is_empty() {
            return Err(error("no solids to export"));
        }
        let text = write_solids(&solids)?;
        dst.write_all(text.as_bytes())
            .map_err(|e| IoError::storage("step", e.to_string()))?;
        Ok(ExportReport {
            bytes_written: text.len() as u64,
            entities: solids.len(),
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}

/// STEP importer with exact planar boundaries.
#[derive(Debug, Default, Clone, Copy)]
pub struct StepImporter;
impl Importer for StepImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Step
    }
    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        if bytes.starts_with(b"ISO-10303-21;") {
            Confidence::CERTAIN
        } else if name.ends_with(".step") || name.ends_with(".stp") {
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
        let mut source = String::new();
        src.take(64 * 1024 * 1024 + 1)
            .read_to_string(&mut source)
            .map_err(error)?;
        let solids = read_solids(&source, tx.tolerances())?;
        let mut report = ImportReport::default();
        let unit = opts.units.map(|u| u.millimeters()).unwrap_or_else(|| {
            if source.contains(".MILLI.,.METRE.") || source.contains(".MILLI., .METRE.") {
                1.0
            } else {
                1000.0
            }
        });
        for (i, mesh) in solids.into_iter().enumerate() {
            progress.check_cancelled()?;
            let axes = if opts.swap_yz == Some(true) {
                glam::DMat3::from_cols(DVec3::X, -DVec3::Z, DVec3::Y)
            } else {
                glam::DMat3::IDENTITY
            };
            let transform = target.local
                * DAffine3::from_translation(opts.at)
                * DAffine3::from_mat3(axes * (opts.scale * unit));
            let (indexed, _) = mesh.to_indexed();
            let mesh = HalfEdgeMesh::from_indexed(
                &crate::geom::mesh::transform_indexed(&indexed, &transform),
                tx.tolerances(),
            )
            .map_err(error)?;
            report.faces += mesh.face_count();
            report.edges += mesh.edge_count();
            let name = format!(
                "{} {}",
                crate::io::import::source_stem(&opts.source, "STEP"),
                i + 1
            );
            let entities = Entities::from_mesh(mesh, || tx.allocate_pid());
            let uuid = crate::commands::common::definition_uuid(tx.document(), &name, &entities);
            let mut def = ComponentDefinition::new(uuid, name.clone());
            def.is_internal_group = matches!(opts.import_as, ImportAs::Group);
            if let ImportAs::Component(name) = &opts.import_as {
                def.name = name.clone();
            }
            if matches!(opts.import_as, ImportAs::Image | ImportAs::Texture) {
                return Err(IoError::UnsupportedFormat(
                    "STEP imports as a group or component".into(),
                ));
            }
            def.entities = entities;
            let definition = tx.insert_definition(def).map_err(error)?;
            let record = if matches!(opts.import_as, ImportAs::Component(_)) {
                EntityRecord::Instance(ComponentInstance {
                    definition,
                    transform: DAffine3::IDENTITY,
                    name: None,
                    material_override: None,
                    glued_to: None,
                    dynamic_attrs: Default::default(),
                })
            } else {
                EntityRecord::Group(Group {
                    definition,
                    transform: DAffine3::IDENTITY,
                    name,
                })
            };
            let (_, pid) = tx
                .insert_entity(target.container, record)
                .map_err(|e: CommandError| error(e))?;
            report.created.push(pid);
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn planar_fixtures_preserve_all_vertices_edges_faces_and_voids() {
        fn signature(mesh: &HalfEdgeMesh) -> Vec<String> {
            let coordinate =
                |p: DVec3| format!("{:.6},{:.6},{:.6}", p.x + 0.0, p.y + 0.0, p.z + 0.0);
            let mut faces = mesh
                .faces()
                .map(|(_, face)| {
                    let mut loops = std::iter::once(face.outer)
                        .chain(face.holes.iter().copied())
                        .map(|l| {
                            let mut ring = mesh
                                .loop_vertices(l)
                                .unwrap()
                                .into_iter()
                                .map(|v| coordinate(mesh.position(v).unwrap()))
                                .collect::<Vec<_>>();
                            let index = ring
                                .iter()
                                .enumerate()
                                .min_by_key(|(_, p)| (*p).clone())
                                .unwrap()
                                .0;
                            ring.rotate_left(index);
                            ring.join(";")
                        })
                        .collect::<Vec<_>>();
                    loops.sort();
                    loops.join("|")
                })
                .collect::<Vec<_>>();
            faces.sort();
            faces
        }
        let fixtures = [
            (
                include_str!("../../tests/fixtures/step/box.step"),
                include_str!("../../tests/fixtures/step/box.mesh.json"),
            ),
            (
                include_str!("../../tests/fixtures/step/through-hole.step"),
                include_str!("../../tests/fixtures/step/through-hole.mesh.json"),
            ),
            (
                include_str!("../../tests/fixtures/step/hollow.step"),
                include_str!("../../tests/fixtures/step/hollow.mesh.json"),
            ),
        ];
        let tol = Tolerances::default();
        for (source, golden) in fixtures {
            let indexed = serde_json::from_str::<crate::geom::mesh::IndexedMesh>(golden).unwrap();
            let expected = HalfEdgeMesh::from_indexed(&indexed, &tol).unwrap();
            let solids = read_solids(source, &tol).unwrap();
            assert_eq!(solids.len(), 1);
            assert_eq!(signature(&solids[0]), signature(&expected));
            assert!((solids[0].volume().unwrap() - expected.volume().unwrap()).abs() < tol.length);
            let again = read_solids(&write_solids(&solids).unwrap(), &tol).unwrap();
            assert_eq!(signature(&again[0]), signature(&expected));
        }
    }
    #[test]
    fn planar_step_round_trip_is_exact_and_repeatable() {
        let tol = Tolerances::default();
        let mesh = crate::geom::boolean::tests::cube(
            DVec3::new(12.345, 23.456, 34.567),
            DVec3::new(100.125, 200.25, 300.5),
        );
        let source = write_solids(std::slice::from_ref(&mesh)).unwrap();
        assert_eq!(source, write_solids(std::slice::from_ref(&mesh)).unwrap());
        let read = read_solids(&source, &tol).unwrap();
        assert_eq!(read.len(), 1);
        let result = &read[0];
        assert_eq!(result.face_count(), mesh.face_count());
        assert_eq!(result.edge_count(), mesh.edge_count());
        assert_eq!(result.vertex_count(), mesh.vertex_count());
        for (_, v) in mesh.vertices() {
            assert!(
                result
                    .vertices()
                    .any(|(_, w)| tol.same_point(v.position, w.position))
            );
        }
        assert!((result.volume().unwrap() - mesh.volume().unwrap()).abs() < tol.length);
        assert!(source.contains(".MILLI."));
    }
    #[test]
    fn malformed_step_is_an_error() {
        for source in [
            "",
            "ISO-10303-21; END-ISO-10303-21;",
            "ISO-10303-21; HEADER; ENDSEC; DATA; #1=BAD(); ENDSEC; END-ISO-10303-21;",
        ] {
            assert!(read_solids(source, &Tolerances::default()).is_err());
        }
    }
}
