//! Implements docs/03-geometry-kernel.md §4.12 (intersect faces) and the
//! primitive intersection tests used by §4.2 (edge splitting), §6 (ray casts)
//! and §4.13 (booleans).
//!
//! Face–face intersection clips the line where the two planes meet to each
//! face with the section-cut routine of `geom::section` (outer loop minus
//! holes), then keeps the overlap of the two sets of intervals.
//!
//! Owner key: `later` (milestone 0.8).

#![warn(unused_variables, dead_code)]

use std::collections::BTreeSet;

use glam::{DAffine3, DVec3};

use crate::geom::planar::{face_polygon, inside_or_on};
use crate::geom::section::face_cut;
use crate::geom::{DeltaSink, GeometryError, HalfEdgeMesh};
use crate::ids::{EdgeId, FaceId};
use crate::math::{Plane, Ray, Segment, SegmentContact};
use crate::tolerance::Tolerances;

/// Outcome of a segment–segment test (docs/03 §4.2 step 2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SegmentIntersection {
    /// No contact within tolerance.
    None,
    /// A single crossing or touching point.
    Point {
        /// Parameter on the first segment.
        t_a: f64,
        /// Parameter on the second segment.
        t_b: f64,
        /// The point.
        point: DVec3,
    },
    /// Collinear overlap on `[t0, t1]` of the first segment.
    Overlap {
        /// Start parameter on the first segment.
        t0: f64,
        /// End parameter on the first segment.
        t1: f64,
    },
}

/// A ray hit on a face or segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
    /// Ray parameter.
    pub t: f64,
    /// Hit point.
    pub point: DVec3,
}

/// Which geometry a face intersection runs against (docs/03 §4.12 `scope`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum IntersectScope {
    /// With everything in the model.
    Model,
    /// Only within the selection.
    Selection,
    /// With the current edit context.
    Context,
}

/// Result of [`intersect_faces`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IntersectResult {
    /// Edges added (after splitting, docs/03 §4.2).
    pub edges: Vec<EdgeId>,
    /// Faces split by the new edges.
    pub faces_split: Vec<FaceId>,
}

/// Segment–segment intersection with `tol.length` contact and `tol.parametric`
/// clamping (docs/03 §8); see [`crate::math::intersect::segment_segment`].
pub fn segment_segment(a: &Segment, b: &Segment, tol: &Tolerances) -> SegmentIntersection {
    match crate::math::intersect::segment_segment(a, b, tol) {
        SegmentContact::None => SegmentIntersection::None,
        SegmentContact::Point { t_a, t_b, point } => SegmentIntersection::Point { t_a, t_b, point },
        SegmentContact::Overlap { t0, t1 } => SegmentIntersection::Overlap { t0, t1 },
    }
}

/// Segment–plane crossing: `(t, point)` or `None` when parallel or off the
/// segment; see [`crate::math::intersect::segment_plane`].
pub fn segment_plane(s: &Segment, plane: &Plane, tol: &Tolerances) -> Option<(f64, DVec3)> {
    crate::math::intersect::segment_plane(s, plane, tol)
}

/// Ray against one face (plane hit + point-in-polygon with holes), exact CPU
/// confirmation for picking (docs/06 §8). Hits on the boundary count.
pub fn ray_face(
    ray: &Ray,
    mesh: &HalfEdgeMesh,
    f: FaceId,
    tol: &Tolerances,
) -> Result<Option<RayHit>, GeometryError> {
    let face = mesh
        .face(f)
        .ok_or_else(|| GeometryError::NotFound(f.to_string()))?;
    let Some(t) = ray.intersect_plane(&face.plane, tol) else {
        return Ok(None);
    };
    let point = ray.at(t);
    let frame = face.plane.frame();
    let (outer, holes) = face_polygon(mesh, f, &frame)?;
    Ok(inside_or_on(frame.to_2d(point), &outer, &holes, tol).then_some(RayHit { t, point }))
}

/// The inverse of a container transform (`NotApplicable` when it flattens
/// space).
pub(crate) fn invert(t: &DAffine3) -> Result<DAffine3, GeometryError> {
    if t.matrix3.determinant().abs() <= 1e-12 {
        return Err(GeometryError::NotApplicable(
            "a transform flattens the geometry".into(),
        ));
    }
    Ok(t.inverse())
}

/// Face–face intersection segments: the plane–plane line clipped to both
/// polygons, with each face in its own container frame (docs/03 §4.12).
pub fn face_face(
    mesh_a: &HalfEdgeMesh,
    fa: FaceId,
    to_world_a: &DAffine3,
    mesh_b: &HalfEdgeMesh,
    fb: FaceId,
    to_world_b: &DAffine3,
    tol: &Tolerances,
) -> Result<Vec<Segment>, GeometryError> {
    let face_plane = |mesh: &HalfEdgeMesh, f: FaceId| {
        mesh.face(f)
            .map(|face| face.plane)
            .ok_or_else(|| GeometryError::NotFound(f.to_string()))
    };
    let plane_a = face_plane(mesh_a, fa)?.transformed(to_world_a);
    let plane_b = face_plane(mesh_b, fb)?.transformed(to_world_b);
    let Some((origin, dir)) = plane_a.intersect_plane(&plane_b, tol) else {
        return Ok(Vec::new());
    };
    // Where each face meets the other's plane, as parameter intervals on
    // the common line `origin + t·dir`.
    let intervals = |mesh: &HalfEdgeMesh,
                     f: FaceId,
                     other: &Plane,
                     to_world: &DAffine3|
     -> Result<Vec<(f64, f64)>, GeometryError> {
        let local = other.transformed(&invert(to_world)?);
        let mut out: Vec<(f64, f64)> = face_cut(mesh, f, &local, tol)
            .into_iter()
            .map(|(p, q)| {
                let t0 = (to_world.transform_point3(p) - origin).dot(dir);
                let t1 = (to_world.transform_point3(q) - origin).dot(dir);
                (t0.min(t1), t0.max(t1))
            })
            .collect();
        out.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.total_cmp(&y.1)));
        Ok(out)
    };
    let in_a = intervals(mesh_a, fa, &plane_b, to_world_a)?;
    let in_b = intervals(mesh_b, fb, &plane_a, to_world_b)?;
    let mut out = Vec::new();
    for &(a0, a1) in &in_a {
        for &(b0, b1) in &in_b {
            let (lo, hi) = (a0.max(b0), a1.min(b1));
            if hi - lo > tol.length {
                out.push(Segment::new(origin + dir * lo, origin + dir * hi));
            }
        }
    }
    Ok(out)
}

/// Intersect Faces: adds the intersection segments of `set_a` × `set_b` as
/// edges into `mesh` (which splits faces automatically) (docs/03 §4.12).
pub fn intersect_faces(
    mesh: &mut HalfEdgeMesh,
    set_a: &[FaceId],
    set_b: &[FaceId],
    tol: &Tolerances,
    sink: &mut dyn DeltaSink,
) -> Result<IntersectResult, GeometryError> {
    let segments = pairwise_segments(mesh, set_a, set_b, tol)?;
    add_segments(mesh, &segments, tol, sink)
}

/// The intersection segments of every unordered pair of distinct faces of
/// `set_a` × `set_b` whose boxes meet, in pair order (container coordinates).
pub(crate) fn pairwise_segments(
    mesh: &HalfEdgeMesh,
    set_a: &[FaceId],
    set_b: &[FaceId],
    tol: &Tolerances,
) -> Result<Vec<Segment>, GeometryError> {
    let id = DAffine3::IDENTITY;
    let mut done: BTreeSet<(FaceId, FaceId)> = BTreeSet::new();
    let mut out = Vec::new();
    for &fa in set_a {
        let box_a = mesh.face_aabb(fa)?.expanded(tol.length);
        for &fb in set_b {
            let key = if fa < fb { (fa, fb) } else { (fb, fa) };
            if fa == fb || !done.insert(key) || !box_a.intersects(&mesh.face_aabb(fb)?) {
                continue;
            }
            out.extend(face_face(mesh, fa, &id, mesh, fb, &id, tol)?);
        }
    }
    Ok(out)
}

/// Adds segments (container coordinates) as edges with `add_edge`, so faces
/// split; one atomic step.
pub(crate) fn add_segments(
    mesh: &mut HalfEdgeMesh,
    segments: &[Segment],
    tol: &Tolerances,
    sink: &mut dyn DeltaSink,
) -> Result<IntersectResult, GeometryError> {
    mesh.transact(tol, sink, |m, d| {
        let mut result = IntersectResult::default();
        let mut seen: BTreeSet<EdgeId> = BTreeSet::new();
        for s in segments {
            if s.is_degenerate(tol) {
                continue;
            }
            let r = m.add_edge_inner(s.a, s.b, tol, d)?;
            result
                .edges
                .extend(r.edges.into_iter().filter(|e| seen.insert(*e)));
            result
                .faces_split
                .extend(r.split_faces.iter().map(|&(_, _, new)| new));
        }
        // Edges and pieces split again by a later segment are gone.
        result.edges.retain(|&e| m.edge(e).is_some());
        result.faces_split.retain(|&f| m.face(f).is_some());
        Ok(result)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::NoDeltas;

    #[test]
    fn ray_and_face_intersections_respect_holes_and_transforms() {
        let tol = Tolerances::default();
        let a = crate::geom::boolean::tests::cube(DVec3::ZERO, DVec3::splat(100.0));
        let top = a.faces().find(|(_, f)| f.plane.normal.z > 0.5).unwrap().0;
        assert!(
            ray_face(
                &Ray::new(DVec3::new(50.0, 50.0, 200.0), -DVec3::Z),
                &a,
                top,
                &tol
            )
            .unwrap()
            .is_some()
        );
        assert!(
            ray_face(
                &Ray::new(DVec3::new(150.0, 50.0, 200.0), -DVec3::Z),
                &a,
                top,
                &tol
            )
            .unwrap()
            .is_none()
        );
        let mut b = HalfEdgeMesh::new();
        let corners = [
            DVec3::new(50.0, -50.0, 50.0),
            DVec3::new(50.0, 150.0, 50.0),
            DVec3::new(50.0, 150.0, 150.0),
            DVec3::new(50.0, -50.0, 150.0),
        ];
        for i in 0..4 {
            b.add_edge(corners[i], corners[(i + 1) % 4], &tol, &mut NoDeltas)
                .unwrap();
        }
        let face = b.faces().next().unwrap().0;
        let segments = face_face(
            &a,
            top,
            &DAffine3::IDENTITY,
            &b,
            face,
            &DAffine3::IDENTITY,
            &tol,
        )
        .unwrap();
        assert_eq!(segments.len(), 1);
        assert!((segments[0].a.distance(segments[0].b) - 100.0).abs() < tol.length);
        assert!(
            face_face(
                &a,
                top,
                &DAffine3::IDENTITY,
                &b,
                face,
                &DAffine3::from_translation(DVec3::X * 200.0),
                &tol
            )
            .unwrap()
            .is_empty()
        );
    }
}
