//! Implements docs/09-file-formats.md §6 for every mesh exporter: the
//! document's visible geometry gathered once — each container's faces
//! triangulated (container coordinates, f64) with their material and
//! texture coordinates, and every occurrence of a container with its world
//! transform — in deterministic order (containers by first occurrence,
//! faces and edges in container order).

use std::collections::BTreeMap;

use glam::{DAffine3, DVec2, DVec3};

use crate::document::{ContainerRef, Document, EditContext, FrontOrBack, InstancePath};
use crate::geom::EdgeFlags;
use crate::geom::tessellate::tessellate_face;
use crate::ids::{EntityId, MaterialId};
use crate::io::export::ExportScope;
use crate::io::render_mesh::{draw_list, uv_of};

/// A triangulated face (container coordinates).
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedFace {
    /// Corner positions.
    pub positions: Vec<DVec3>,
    /// Triangles, counter-clockwise seen from the front.
    pub triangles: Vec<[u32; 3]>,
    /// Front normal.
    pub normal: DVec3,
    /// Front material (`None`: the default).
    pub material: Option<MaterialId>,
    /// Texture coordinates of the front, per position (repeats of the
    /// material's texture tile).
    pub uvs: Vec<DVec2>,
}

/// One container's geometry.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExportedMesh {
    /// Faces.
    pub faces: Vec<ExportedFace>,
    /// Edges that are not soft or hidden.
    pub edges: Vec<[DVec3; 2]>,
}

/// One occurrence of a container.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedOccurrence {
    /// Its instance path.
    pub path: InstancePath,
    /// The container.
    pub container: ContainerRef,
    /// Container-to-world transform.
    pub world: DAffine3,
    /// Override for default-material faces.
    pub material_override: Option<MaterialId>,
}

/// The document's visible geometry.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExportedScene {
    /// Meshes by container, in order of first occurrence.
    pub meshes: Vec<(ContainerRef, ExportedMesh)>,
    /// Occurrences in draw order.
    pub occurrences: Vec<ExportedOccurrence>,
}

impl ExportedScene {
    /// The mesh of a container.
    pub fn mesh(&self, container: ContainerRef) -> Option<&ExportedMesh> {
        self.meshes
            .iter()
            .find(|(c, _)| *c == container)
            .map(|(_, m)| m)
    }
}

/// Gathers what `scope` exports.
pub fn collect(doc: &Document, scope: &ExportScope) -> ExportedScene {
    let selection = scope.selection.clone().unwrap_or_default();
    let items = draw_list(doc, &selection, &EditContext::model(), scope.include_hidden);
    let tol = doc.tolerances();
    let mut out = ExportedScene::default();
    let mut seen: BTreeMap<ContainerRef, ()> = BTreeMap::new();
    for item in &items {
        if item.hidden && !scope.include_hidden {
            continue;
        }
        if scope.selection.is_some() && !item.selected && item.selected_faces.is_empty() {
            continue;
        }
        out.occurrences.push(ExportedOccurrence {
            path: item.path.clone(),
            container: item.container,
            world: item.world,
            material_override: item.material_override,
        });
        if seen.insert(item.container, ()).is_some() {
            continue;
        }
        let Some(entities) = doc.container(item.container) else {
            continue;
        };
        let mesh = entities.mesh();
        let mut exported = ExportedMesh::default();
        for &id in entities.order() {
            match id {
                EntityId::Face(f) => {
                    if entities.is_hidden(id) && !scope.include_hidden {
                        continue;
                    }
                    let (Some(face), Ok(tess)) = (mesh.face(f), tessellate_face(mesh, f, tol))
                    else {
                        continue;
                    };
                    let frame = face.plane.frame();
                    let material = face.sides.material_front;
                    let tile = doc
                        .materials
                        .get(doc.materials.resolve(material, FrontOrBack::Front))
                        .and_then(|m| m.texture.as_ref())
                        .map(|t| t.size_mm);
                    let uvs = tess
                        .positions
                        .iter()
                        .zip(&tess.vertices)
                        .map(|(p, v)| {
                            let uv = uv_of(
                                face.sides.uv_front.as_ref(),
                                frame.to_2d(*p),
                                *v,
                                *p,
                                tile.or(Some(DVec2::splat(1000.0))),
                            );
                            DVec2::new(f64::from(uv[0]), f64::from(uv[1]))
                        })
                        .collect();
                    exported.faces.push(ExportedFace {
                        positions: tess.positions.clone(),
                        triangles: tess.triangles.clone(),
                        normal: face.plane.normal,
                        material,
                        uvs,
                    });
                }
                EntityId::Edge(e) => {
                    let Some(edge) = mesh.edge(e) else { continue };
                    if edge.flags.contains(EdgeFlags::SOFT)
                        || edge.flags.contains(EdgeFlags::HIDDEN)
                        || (entities.is_hidden(id) && !scope.include_hidden)
                    {
                        continue;
                    }
                    if let Ok(seg) = mesh.edge_segment(e) {
                        exported.edges.push([seg.a, seg.b]);
                    }
                }
                _ => {}
            }
        }
        out.meshes.push((item.container, exported));
    }
    out
}
