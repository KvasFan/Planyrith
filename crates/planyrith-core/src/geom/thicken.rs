//! Implements docs/03-geometry-kernel.md §4.16 Thicken: a surface (faces
//! of a mesh, open or closed) made into a solid of a given thickness. Each
//! vertex moves so that every face around it moves by the thickness along
//! its own normal (least squares where more than three faces meet), the
//! moved faces cap one side, the original faces the other, and quads along
//! the surface's free edges close it.

use std::collections::{BTreeMap, BTreeSet};

use glam::{DMat3, DVec3};

use crate::geom::boolean::{Piece, stitch};
use crate::geom::planar::reverse_faces;
use crate::geom::tessellate::tessellate_face;
use crate::geom::{FaceSides, GeometryError, HalfEdgeMesh, NoDeltas};
use crate::ids::{FaceId, HalfEdgeId, VertexId};
use crate::tolerance::Tolerances;

/// The move of a vertex shared by faces with `normals` that offsets each
/// of their planes by `t`: the least-squares solution of `n·d = t`, with
/// the smallest length when the normals do not fix it (one or two
/// directions).
fn offset(normals: &[DVec3], t: f64) -> DVec3 {
    // Distinct directions only (parallel faces would weigh twice).
    let mut distinct: Vec<DVec3> = Vec::new();
    for &n in normals {
        if !distinct.iter().any(|m| m.dot(n) > 1.0 - 1e-9) {
            distinct.push(n);
        }
    }
    match distinct.as_slice() {
        [] => DVec3::ZERO,
        [n] => *n * t,
        [a, b] => {
            // d = α a + β b with a·d = b·d = t.
            let c = a.dot(*b);
            let det = 1.0 - c * c;
            if det < 1e-12 {
                return *a * t;
            }
            let alpha = t * (1.0 - c) / det;
            *a * alpha + *b * alpha
        }
        many => {
            let mut m = DMat3::ZERO;
            let mut rhs = DVec3::ZERO;
            for n in many {
                m += DMat3::from_cols(*n * n.x, *n * n.y, *n * n.z);
                rhs += *n * t;
            }
            if m.determinant().abs() < 1e-9 {
                // Normals in one plane: fall back to their average.
                let mean = many.iter().copied().sum::<DVec3>().normalize_or_zero();
                return mean * t / mean.dot(many[0]).max(0.2);
            }
            m.inverse() * rhs
        }
    }
}

/// Thickens `faces` of `mesh` by `thickness` (mm) along their front
/// normals (negative: behind them), or half each way when `centered`.
/// Returns a solid mesh facing outward.
pub fn thicken(
    mesh: &HalfEdgeMesh,
    faces: &BTreeSet<FaceId>,
    thickness: f64,
    centered: bool,
    tol: &Tolerances,
) -> Result<HalfEdgeMesh, GeometryError> {
    if faces.is_empty() {
        return Err(GeometryError::NotApplicable("no faces to thicken".into()));
    }
    if thickness.abs() <= tol.length {
        return Err(GeometryError::NotApplicable(
            "the thickness must be larger than the model tolerance".into(),
        ));
    }
    // Normals of the selected faces around each vertex.
    let mut around: BTreeMap<VertexId, Vec<DVec3>> = BTreeMap::new();
    for &f in faces {
        let n = mesh.face_normal(f)?;
        for l in mesh.face_loops(f)? {
            for v in mesh.loop_vertices(l)? {
                around.entry(v).or_default().push(n);
            }
        }
    }
    let (low, high) = if centered {
        (-thickness / 2.0, thickness / 2.0)
    } else {
        (0.0, thickness)
    };
    let moved = |v: VertexId, t: f64| -> Result<DVec3, GeometryError> {
        let p = mesh.position(v)?;
        Ok(p + offset(around.get(&v).map_or(&[][..], Vec::as_slice), t))
    };
    let mut pieces = Vec::new();
    for &f in faces {
        let sides = mesh.face(f).map(|x| x.sides.clone()).unwrap_or_default();
        let loops = mesh.face_loops(f)?;
        let mut top = Vec::new();
        let mut bottom = Vec::new();
        for l in &loops {
            let vertices = mesh.loop_vertices(*l)?;
            top.push(
                vertices
                    .iter()
                    .map(|&v| moved(v, high))
                    .collect::<Result<Vec<_>, _>>()?,
            );
            bottom.push(
                vertices
                    .iter()
                    .rev()
                    .map(|&v| moved(v, low))
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        let back = FaceSides {
            material_front: sides.material_back,
            material_back: sides.material_front,
            uv_front: None,
            uv_back: None,
        };
        // A moved face that left its plane (more than three faces met at a
        // corner) goes in as triangles.
        let flat = |loops: &[Vec<DVec3>]| -> bool {
            let Some(outer) = loops.first() else {
                return false;
            };
            let n = mesh.face_normal(f).unwrap_or(DVec3::Z);
            let d = n.dot(outer[0]);
            loops
                .iter()
                .flatten()
                .all(|p| (n.dot(*p) - d).abs() <= tol.planarity)
        };
        for (loops, sides, offset_t, reversed) in
            [(top, sides.clone(), high, false), (bottom, back, low, true)]
        {
            if flat(&loops) {
                pieces.push(Piece { loops, sides });
            } else {
                let tess = tessellate_face(mesh, f, tol)?;
                for tri in &tess.triangles {
                    let mut corners: Vec<DVec3> = tri
                        .iter()
                        .map(|&i| {
                            let p = tess.positions[i as usize];
                            mesh.find_vertex_near(p, tol)
                                .map_or(Ok(p), |v| moved(v, offset_t))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if reversed {
                        corners.reverse();
                    }
                    pieces.push(Piece {
                        loops: vec![corners],
                        sides: sides.clone(),
                    });
                }
            }
        }
    }
    // Free edges of the surface: a selected face on one side only.
    for &f in faces {
        for l in mesh.face_loops(f)? {
            for h in mesh.loop_half_edges(l)? {
                let twin: HalfEdgeId = mesh.twin_of(h)?;
                let other = mesh.he(twin)?.face;
                if other.is_some_and(|g| faces.contains(&g)) {
                    continue;
                }
                let a = mesh.he(h)?.origin;
                let b = mesh.dest(h)?;
                // Runs b → a on top and a → b below, against the caps.
                pieces.push(Piece {
                    loops: vec![vec![
                        moved(a, low)?,
                        moved(b, low)?,
                        moved(b, high)?,
                        moved(a, high)?,
                    ]],
                    sides: mesh.face(f).map(|x| x.sides.clone()).unwrap_or_default(),
                });
            }
        }
    }
    let mut solid = stitch(&pieces, tol)?;
    if !solid.is_solid() {
        return Err(GeometryError::NonRobust(
            "the thickened surface does not close; thicken fewer faces at a time".into(),
        ));
    }
    if solid.volume()? < 0.0 {
        let all = solid.faces().map(|(f, _)| f).collect();
        reverse_faces(&mut solid, &all, tol, &mut NoDeltas)?;
    }
    Ok(solid)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tol() -> Tolerances {
        Tolerances::default()
    }

    fn polygon(mesh: &mut HalfEdgeMesh, points: &[DVec3]) {
        for i in 0..points.len() {
            mesh.add_edge(
                points[i],
                points[(i + 1) % points.len()],
                &tol(),
                &mut NoDeltas,
            )
            .unwrap();
        }
    }

    fn all_faces(mesh: &HalfEdgeMesh) -> BTreeSet<FaceId> {
        mesh.faces().map(|(f, _)| f).collect()
    }

    #[test]
    fn a_square_becomes_a_slab() {
        let mut mesh = HalfEdgeMesh::new();
        polygon(
            &mut mesh,
            &[
                DVec3::ZERO,
                DVec3::new(100.0, 0.0, 0.0),
                DVec3::new(100.0, 50.0, 0.0),
                DVec3::new(0.0, 50.0, 0.0),
            ],
        );
        let solid = thicken(&mesh, &all_faces(&mesh), 10.0, false, &tol()).unwrap();
        assert_eq!(solid.face_count(), 6);
        assert!((solid.volume().unwrap() - 50_000.0).abs() < 1e-6);
        let centered = thicken(&mesh, &all_faces(&mesh), 10.0, true, &tol()).unwrap();
        let z: Vec<f64> = centered.vertices().map(|(_, v)| v.position.z).collect();
        assert!(z.iter().all(|z| (z.abs() - 5.0).abs() < 1e-9), "{z:?}");
    }

    #[test]
    fn a_folded_sheet_keeps_its_thickness_on_both_leaves() {
        // Two faces meeting at a right angle along x (an open L sheet).
        let mut mesh = HalfEdgeMesh::new();
        polygon(
            &mut mesh,
            &[
                DVec3::ZERO,
                DVec3::new(100.0, 0.0, 0.0),
                DVec3::new(100.0, 50.0, 0.0),
                DVec3::new(0.0, 50.0, 0.0),
            ],
        );
        polygon(
            &mut mesh,
            &[
                DVec3::new(0.0, 50.0, 0.0),
                DVec3::new(100.0, 50.0, 0.0),
                DVec3::new(100.0, 50.0, 30.0),
                DVec3::new(0.0, 50.0, 30.0),
            ],
        );
        assert_eq!(mesh.face_count(), 2);
        let solid = thicken(&mesh, &all_faces(&mesh), 4.0, false, &tol()).unwrap();
        assert!(solid.is_solid());
        assert_eq!(solid.face_count(), 4 + 6, "two caps per leaf and six sides");
        // Leaves of 50 and 30 mm along their faces, both 4 thick, 100 long:
        // the inner corner block (4 × 4) is counted once.
        let expected = 100.0 * 4.0 * (50.0 + 30.0) + 100.0 * 4.0 * 4.0;
        let low = 100.0 * 4.0 * (50.0 + 30.0) - 100.0 * 4.0 * 4.0;
        let v = solid.volume().unwrap();
        assert!((v - expected).abs() < 1e-6 || (v - low).abs() < 1e-6, "{v}");
    }

    #[test]
    fn nothing_or_no_thickness_is_refused() {
        let mut mesh = HalfEdgeMesh::new();
        polygon(
            &mut mesh,
            &[DVec3::ZERO, DVec3::X * 10.0, DVec3::new(10.0, 10.0, 0.0)],
        );
        assert!(thicken(&mesh, &BTreeSet::new(), 1.0, false, &tol()).is_err());
        assert!(thicken(&mesh, &all_faces(&mesh), 0.0, false, &tol()).is_err());
    }
}
