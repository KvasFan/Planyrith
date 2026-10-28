//! Implements docs/03-geometry-kernel.md §9 (B-rep bridge), compiled only with
//! the `brep` feature. Shared planar boundaries are preserved for STEP;
//! ambiguous mesh booleans can retry through the B-rep backend.
//!
//! - `to_solid(&HalfEdgeMesh) -> truck::Solid` — planar faces become planar
//!   surfaces, loops become wires of line/arc edges using curve descriptors.
//! - `from_solid(&truck::Solid, tess_res) -> HalfEdgeMesh` — curved faces are
//!   tessellated with curve descriptors retained for cylinders/arcs.
//! - `boolean(...)` — numerical fallback for `geom::boolean` (`NonRobust` cases).
//!
//! Round trip of a planar solid must be exact within `tolerances.length`.
//! Owner key: `later`.

/// `true` when the B-rep backend is linked; frontends use it to decide whether a
/// `NonRobust` boolean can be retried.
pub const KERNEL_AVAILABLE: bool = true;

use crate::geom::boolean::{Piece, stitch};
use crate::geom::{FaceSides, GeometryError, HalfEdgeMesh};
use crate::tolerance::Tolerances;
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet};
use truck_modeling::{Curve, Line, Point3, Solid, Surface};
use truck_topology::compress::{
    CompressedEdge, CompressedEdgeIndex, CompressedFace, CompressedShell, CompressedSolid,
};

fn error(e: impl std::fmt::Display) -> GeometryError {
    GeometryError::NonRobust(format!("B-rep: {e}"))
}
fn point(p: DVec3) -> Point3 {
    Point3::new(p.x, p.y, p.z)
}

/// B-rep boolean fallback (§4.13). The backend's optional non-Rust interchange
/// path is disabled by the local manifest patch (docs/02 Decisions).
pub fn boolean(
    op: crate::geom::boolean::BooleanOp,
    a: &HalfEdgeMesh,
    ta: &glam::DAffine3,
    b: &HalfEdgeMesh,
    tb: &glam::DAffine3,
    tol: &Tolerances,
) -> Result<crate::geom::boolean::BooleanResult, GeometryError> {
    use crate::geom::boolean::{BooleanOp, BooleanResult};
    let convert =
        |mesh: &HalfEdgeMesh, t: &glam::DAffine3| -> Result<HalfEdgeMesh, GeometryError> {
            let (indexed, _) = mesh.to_indexed();
            HalfEdgeMesh::from_indexed(&crate::geom::mesh::transform_indexed(&indexed, t), tol)
        };
    let (source_a, source_b) = (convert(a, ta)?, convert(b, tb)?);
    let (a, b) = (to_solid(&source_a)?, to_solid(&source_b)?);
    let solids = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let subtract = |a: &Solid, b: &Solid| {
            let mut b = b.clone();
            b.not();
            truck_shapeops::and(a, &b, tol.length)
        };
        let result = match op {
            BooleanOp::Union | BooleanOp::OuterShell => {
                vec![truck_shapeops::or(&a, &b, tol.length)]
            }
            BooleanOp::Intersect => vec![truck_shapeops::and(&a, &b, tol.length)],
            BooleanOp::Subtract => vec![subtract(&a, &b)],
            BooleanOp::Trim => vec![subtract(&a, &b), Some(b.clone())],
            BooleanOp::Split => vec![
                subtract(&a, &b),
                truck_shapeops::and(&a, &b, tol.length),
                subtract(&b, &a),
            ],
        };
        result
            .into_iter()
            .map(|solid| solid.ok_or_else(|| error("boolean could not resolve the boundary")))
            .collect::<Result<Vec<_>, _>>()
    }))
    .map_err(|_| error("boolean could not resolve the boundary"))??;
    let mut meshes = Vec::new();
    for solid in solids {
        if solid.boundaries().is_empty() {
            continue;
        }
        let mut mesh = from_solid(&solid, tol.planarity, tol)?;
        // Backend topology may enumerate faces through internal identities.
        // Normalize the geometric boundary order before allocating mesh ids.
        let mut pieces = Vec::new();
        let compare = |a: &DVec3, b: &DVec3| {
            a.x.total_cmp(&b.x)
                .then(a.y.total_cmp(&b.y))
                .then(a.z.total_cmp(&b.z))
        };
        for (f, face) in mesh.faces() {
            let mut loops = Vec::new();
            for lp in mesh.face_loops(f)? {
                let mut ring = mesh.loop_positions(lp)?;
                if let Some((index, _)) = ring
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| compare(a, b))
                {
                    ring.rotate_left(index);
                }
                loops.push(ring);
            }
            let mut sides = face.sides.clone();
            let p = loops[0][0];
            'sources: for source in [&source_a, &source_b] {
                for (f, original) in source.faces() {
                    if tol.is_coplanar(&face.plane, &original.plane) {
                        let frame = original.plane.frame();
                        let (outer, holes) = crate::geom::planar::face_polygon(source, f, &frame)?;
                        if crate::geom::planar::inside_or_on(frame.to_2d(p), &outer, &holes, tol) {
                            sides = original.sides.clone();
                            if face.plane.normal.dot(original.plane.normal) < 0.0 {
                                std::mem::swap(&mut sides.material_front, &mut sides.material_back);
                                std::mem::swap(&mut sides.uv_front, &mut sides.uv_back);
                            }
                            break 'sources;
                        }
                    }
                }
            }
            pieces.push(Piece { loops, sides });
        }
        pieces.sort_by(|a, b| {
            a.loops
                .iter()
                .flatten()
                .zip(b.loops.iter().flatten())
                .map(|(a, b)| compare(a, b))
                .find(|order| !order.is_eq())
                .unwrap_or_else(|| {
                    a.loops
                        .iter()
                        .flatten()
                        .count()
                        .cmp(&b.loops.iter().flatten().count())
                })
        });
        mesh = stitch(&pieces, tol)?;
        if op == BooleanOp::OuterShell {
            crate::geom::boolean::remove_voids(&mut mesh, tol)?;
        }
        if !mesh.is_solid() || !mesh.validate(tol).is_empty() {
            return Err(error("boolean result is not a valid closed solid"));
        }
        meshes.push(mesh);
    }
    Ok(BooleanResult {
        solids: meshes,
        warnings: Vec::new(),
        fallback_used: true,
    })
}

/// Converts a closed planar mesh to shared line edges and planar surfaces.
pub fn to_solid(mesh: &HalfEdgeMesh) -> Result<Solid, GeometryError> {
    if !mesh.is_solid() {
        return Err(GeometryError::NotApplicable(
            "B-rep conversion needs a closed solid".into(),
        ));
    }
    let mut remaining = mesh.faces().map(|(f, _)| f).collect::<BTreeSet<_>>();
    let mut boundaries = Vec::new();
    while let Some(&first) = remaining.first() {
        let mut stack = vec![first];
        let mut faces = Vec::new();
        while let Some(f) = stack.pop() {
            if !remaining.remove(&f) {
                continue;
            }
            faces.push(f);
            stack.extend(mesh.face_neighbors(f)?);
        }
        faces.sort();
        let mut shell = CompressedShell {
            vertices: Vec::new(),
            edges: Vec::new(),
            faces: Vec::new(),
        };
        let mut vertices = BTreeMap::new();
        let mut edges = BTreeMap::new();
        for f in faces {
            let mut loops = Vec::new();
            for cycle in mesh.face_cycles(f)? {
                let mut boundary = Vec::new();
                for h in cycle {
                    let edge = mesh.he(h)?.edge;
                    let (a, b) = mesh.edge_endpoints(edge)?;
                    let mut vertex = |v| -> Result<usize, GeometryError> {
                        if let Some(&i) = vertices.get(&v) {
                            return Ok(i);
                        }
                        let i = shell.vertices.len();
                        shell.vertices.push(point(mesh.position(v)?));
                        vertices.insert(v, i);
                        Ok(i)
                    };
                    let (va, vb) = (vertex(a)?, vertex(b)?);
                    let index = *edges.entry(edge).or_insert_with(|| {
                        let index = shell.edges.len();
                        shell.edges.push(CompressedEdge {
                            vertices: (va, vb),
                            curve: Curve::Line(Line(shell.vertices[va], shell.vertices[vb])),
                        });
                        index
                    });
                    boundary.push(CompressedEdgeIndex {
                        index,
                        orientation: mesh.he(h)?.origin == a,
                    });
                }
                loops.push(boundary);
            }
            let frame = mesh.face_ref(f)?.plane.frame();
            shell.faces.push(CompressedFace {
                boundaries: loops,
                orientation: true,
                surface: Surface::Plane(truck_modeling::Plane::new(
                    point(frame.from_2d(glam::DVec2::ZERO)),
                    point(frame.from_2d(glam::DVec2::X)),
                    point(frame.from_2d(glam::DVec2::Y)),
                )),
            });
        }
        boundaries.push(shell);
    }
    Solid::extract(CompressedSolid { boundaries }).map_err(error)
}

/// Converts a planar solid exactly, retaining loops and holes. Curved surfaces
/// are tessellated at `tess_res` millimeters.
pub fn from_solid(
    solid: &Solid,
    tess_res: f64,
    tol: &Tolerances,
) -> Result<HalfEdgeMesh, GeometryError> {
    if !tess_res.is_finite() || tess_res <= 0.0 {
        return Err(GeometryError::NotApplicable(
            "tessellation resolution must be positive".into(),
        ));
    }
    let compressed = solid.compress();
    if compressed.boundaries.iter().all(|s| {
        s.faces
            .iter()
            .all(|f| matches!(f.surface, Surface::Plane(_)))
            && s.edges.iter().all(|e| matches!(e.curve, Curve::Line(_)))
    }) {
        let mut pieces = Vec::new();
        for shell in &compressed.boundaries {
            for face in &shell.faces {
                let mut loops = Vec::new();
                for boundary in &face.boundaries {
                    let mut ring = boundary
                        .iter()
                        .map(|e| {
                            let edge = &shell.edges[e.index];
                            let p = shell.vertices[if e.orientation {
                                edge.vertices.0
                            } else {
                                edge.vertices.1
                            }];
                            DVec3::new(p.x, p.y, p.z)
                        })
                        .collect::<Vec<_>>();
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
        }
        return stitch(&pieces, tol);
    }
    use truck_meshalgo::prelude::*;
    let polygon = solid.robust_triangulation(tess_res).to_polygon();
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
    let r = HalfEdgeMesh::from_triangles_merged(&positions, &triangles, Default::default(), tol)?;
    if !r.mesh.is_solid() {
        return Err(error("tessellation did not close the solid"));
    }
    Ok(r.mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brep_fallback_returns_closed_solids_for_transversal_cubes() {
        let tol = Tolerances::default();
        let a = crate::geom::boolean::tests::cube(DVec3::ZERO, DVec3::splat(100.0));
        let b = crate::geom::boolean::tests::cube(DVec3::ZERO, DVec3::splat(100.0));
        let transform = glam::DAffine3::from_rotation_z(0.3)
            * glam::DAffine3::from_translation(DVec3::splat(40.0));
        let r = boolean(
            crate::geom::boolean::BooleanOp::Union,
            &a,
            &glam::DAffine3::IDENTITY,
            &b,
            &transform,
            &tol,
        )
        .unwrap();
        assert!(r.fallback_used);
        assert_eq!(r.solids.len(), 1);
        assert!(r.solids[0].is_solid());
    }
    #[test]
    fn planar_bridge_keeps_vertices_faces_volume_and_holes() {
        let tol = Tolerances::default();
        let a = crate::geom::boolean::tests::cube(DVec3::ZERO, DVec3::splat(100.0));
        let b = crate::geom::boolean::tests::cube(
            DVec3::new(25.0, 25.0, -50.0),
            DVec3::new(50.0, 50.0, 200.0),
        );
        let hole = crate::geom::boolean::boolean(
            crate::geom::boolean::BooleanOp::Subtract,
            &a,
            &glam::DAffine3::IDENTITY,
            &b,
            &glam::DAffine3::IDENTITY,
            &tol,
        )
        .unwrap()
        .solids
        .remove(0);
        for mesh in [a, hole] {
            let result = from_solid(&to_solid(&mesh).unwrap(), 0.01, &tol).unwrap();
            assert!(result.is_solid());
            assert_eq!(result.vertex_count(), mesh.vertex_count());
            assert_eq!(result.face_count(), mesh.face_count());
            assert!((result.volume().unwrap() - mesh.volume().unwrap()).abs() < 1e-6);
            for (_, v) in mesh.vertices() {
                assert!(
                    result
                        .vertices()
                        .any(|(_, w)| tol.same_point(v.position, w.position))
                );
            }
        }
    }
}
