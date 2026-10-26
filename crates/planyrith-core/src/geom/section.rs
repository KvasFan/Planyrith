//! Implements docs/03-geometry-kernel.md section slices for docs/05 §5.11:
//! where a plane cuts a mesh's faces. Each face meets the plane along a
//! line; the parts of that line inside the face (outer loop minus holes)
//! are the cut segments. Faces lying in the plane and edges on it add
//! nothing (they are already there).
//!
//! Owner key: `mesh`.

use glam::{DVec2, DVec3};

use crate::geom::HalfEdgeMesh;
use crate::geom::planar::face_polygon;
use crate::ids::FaceId;
use crate::math::Plane;
use crate::tolerance::Tolerances;

/// The segments where `plane` cuts face `f`, in mesh coordinates.
pub fn face_cut(
    mesh: &HalfEdgeMesh,
    f: FaceId,
    plane: &Plane,
    tol: &Tolerances,
) -> Vec<(DVec3, DVec3)> {
    let Some(face) = mesh.face(f) else {
        return Vec::new();
    };
    let n = face.plane.normal;
    let dir = n.cross(plane.normal);
    if dir.length() <= 1e-9 {
        // Parallel planes: nothing, or the face itself (no new lines).
        return Vec::new();
    }
    let frame = face.plane.frame();
    let Ok((outer, holes)) = face_polygon(mesh, f, &frame) else {
        return Vec::new();
    };
    // The cut line in the face's 2D frame: points q with
    // plane(from_2d(q)) = 0, i.e. a·q + c = 0.
    let origin = frame.from_2d(DVec2::ZERO);
    let ex = frame.from_2d(DVec2::X) - origin;
    let ey = frame.from_2d(DVec2::Y) - origin;
    let a = DVec2::new(plane.normal.dot(ex), plane.normal.dot(ey));
    let c = plane.signed_distance(origin);
    if a.length() <= 1e-12 {
        return Vec::new();
    }
    let side = |q: DVec2| a.dot(q) + c;
    // Along the line: q = p0 + t·u.
    let u = DVec2::new(-a.y, a.x).normalize();
    let p0 = -a * c / a.length_squared();
    let eps = tol.length;
    let mut ts: Vec<f64> = Vec::new();
    for ring in std::iter::once(&outer).chain(holes.iter()) {
        let m = ring.len();
        for k in 0..m {
            let (q0, q1) = (ring[k], ring[(k + 1) % m]);
            let (s0, s1) = (side(q0), side(q1));
            // Half-open rule: a vertex on the line counts once, for the
            // edge that leaves the line's positive side.
            let (in0, in1) = (s0 > eps * 0.5, s1 > eps * 0.5);
            if in0 == in1 {
                continue;
            }
            let t = s0 / (s0 - s1);
            let q = q0 + (q1 - q0) * t;
            ts.push((q - p0).dot(u));
        }
    }
    ts.sort_by(f64::total_cmp);
    let mut out = Vec::new();
    for pair in ts.as_chunks::<2>().0 {
        let (t0, t1) = (pair[0], pair[1]);
        if t1 - t0 <= eps {
            continue;
        }
        let a3 = frame.from_2d(p0 + u * t0);
        let b3 = frame.from_2d(p0 + u * t1);
        // Snap exactly onto the plane (the frame round trip drifts).
        let snap = |p: DVec3| p - plane.normal * plane.signed_distance(p);
        out.push((snap(a3), snap(b3)));
    }
    out
}

/// Every cut segment of the mesh's faces, in face order.
pub fn slice(mesh: &HalfEdgeMesh, plane: &Plane, tol: &Tolerances) -> Vec<(DVec3, DVec3)> {
    let faces: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
    faces
        .into_iter()
        .flat_map(|f| face_cut(mesh, f, plane, tol))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::NoDeltas;

    /// A closed box from triangles.
    fn cube(size: f64) -> HalfEdgeMesh {
        let p = |i: u8| {
            DVec3::new(
                f64::from(i & 1) * size,
                f64::from((i >> 1) & 1) * size,
                f64::from((i >> 2) & 1) * size,
            )
        };
        let quads: [[u8; 4]; 6] = [
            [0, 2, 3, 1],
            [4, 5, 7, 6],
            [0, 1, 5, 4],
            [2, 6, 7, 3],
            [0, 4, 6, 2],
            [1, 3, 7, 5],
        ];
        let mut positions = Vec::new();
        let mut tris = Vec::new();
        for q in quads {
            for t in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
                let base = positions.len() as u32;
                positions.extend(t.map(p));
                tris.push([base, base + 1, base + 2]);
            }
        }
        let tol = Tolerances::default();
        HalfEdgeMesh::from_triangles_keyed(
            &positions,
            &tris,
            &vec![0; tris.len()],
            crate::geom::mesh::TriangleMergeOptions::default(),
            &tol,
        )
        .unwrap()
        .mesh
    }

    #[test]
    fn a_horizontal_cut_of_a_cube_is_a_square() {
        let mesh = cube(1000.0);
        let tol = Tolerances::default();
        let plane = Plane::from_point_normal(DVec3::new(0.0, 0.0, 400.0), DVec3::Z);
        let segments = slice(&mesh, &plane, &tol);
        assert_eq!(segments.len(), 4, "{segments:?}");
        let total: f64 = segments.iter().map(|(a, b)| (*b - *a).length()).sum();
        assert!((total - 4000.0).abs() < 1e-6);
        assert!(
            segments
                .iter()
                .all(|(a, b)| (a.z - 400.0).abs() < 1e-9 && (b.z - 400.0).abs() < 1e-9)
        );
        // Through a vertex and an edge (the cube's top face plane): nothing new.
        let top = Plane::from_point_normal(DVec3::new(0.0, 0.0, 1000.0), DVec3::Z);
        assert!(slice(&mesh, &top, &tol).is_empty());
        // The cut segments make one face when drawn.
        let mut cap = HalfEdgeMesh::new();
        for (a, b) in &segments {
            cap.add_edge(*a, *b, &tol, &mut NoDeltas).unwrap();
        }
        assert_eq!(cap.faces().count(), 1);
    }

    #[test]
    fn a_face_with_a_hole_cuts_in_two_pieces() {
        let tol = Tolerances::default();
        let mut mesh = HalfEdgeMesh::new();
        let sq = |s: f64, o: f64| {
            [
                DVec3::new(o, o, 0.0),
                DVec3::new(o + s, o, 0.0),
                DVec3::new(o + s, o + s, 0.0),
                DVec3::new(o, o + s, 0.0),
            ]
        };
        for ring in [sq(1000.0, 0.0), sq(400.0, 300.0)] {
            for k in 0..4 {
                mesh.add_edge(ring[k], ring[(k + 1) % 4], &tol, &mut NoDeltas)
                    .unwrap();
            }
        }
        // Erasing the inner face is not needed: a vertical plane through the
        // middle crosses the outer face (with the hole) and the inner face.
        let plane = Plane::from_point_normal(DVec3::new(500.0, 0.0, 0.0), DVec3::X);
        let mut lengths: Vec<f64> = slice(&mesh, &plane, &tol)
            .iter()
            .map(|(a, b)| (*b - *a).length())
            .collect();
        lengths.sort_by(f64::total_cmp);
        assert_eq!(lengths.len(), 3, "{lengths:?}");
        assert!((lengths[0] - 300.0).abs() < 1e-6 && (lengths[1] - 300.0).abs() < 1e-6);
        assert!((lengths[2] - 400.0).abs() < 1e-6);
    }
}
