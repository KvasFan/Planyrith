//! Implements docs/03-geometry-kernel.md §4.3 (face finding), §4.4 (split by
//! chord, via `HalfEdgeMesh::split_face_by_chord`) and §4.5 (holes): planar
//! graph embedding, minimal cycle enumeration, loop orientation.
//!
//! Owner key: `later` (needs `mesh` first); the pure 2D helpers below are
//! independent and may be implemented by anyone.

use glam::{DVec2, DVec3};

use crate::geom::{DeltaSink, GeometryError, HalfEdgeMesh};
use crate::ids::{FaceId, HalfEdgeId, VertexId};
use crate::math::Plane;
use crate::tolerance::Tolerances;

/// Result of [`find_faces`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FaceFindingResult {
    /// Faces created from newly closed coplanar loops.
    pub new_faces: Vec<FaceId>,
    /// Faces split by a chord: `(old, new_a, new_b)` (docs/03 §4.4).
    pub split_faces: Vec<(FaceId, FaceId, FaceId)>,
    /// Hole situations: `(outer face, inner face)` (docs/03 §4.5).
    pub holes: Vec<(FaceId, FaceId)>,
}

/// Orientation of a closed 2D polygon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopOrientation {
    /// Positive signed area.
    CounterClockwise,
    /// Negative signed area.
    Clockwise,
    /// `|area| <= tol.length_sq`.
    Degenerate,
}

/// Face finding on the touched vertices after an edge was added or moved
/// (docs/03 §4.3 steps 1–4). Complexity target O(k log n) for k local edges.
pub fn find_faces(
    mesh: &mut HalfEdgeMesh,
    touched: &[VertexId],
    tol: &Tolerances,
    sink: &mut dyn DeltaSink,
) -> Result<FaceFindingResult, GeometryError> {
    todo!("docs/03 §4.3 face finding")
}

/// Candidate planes spanned by consecutive edge pairs around each touched
/// vertex, deduplicated with `tol.is_coplanar` (docs/03 §4.3 step 1).
pub fn candidate_planes(mesh: &HalfEdgeMesh, touched: &[VertexId], tol: &Tolerances) -> Vec<Plane> {
    todo!("docs/03 §4.3 candidate planes")
}

/// Minimal cycles of the coplanar subgraph connected to `seeds`, walking
/// "turn left-most" in the plane frame; the unbounded outer cycle is excluded
/// unless it is the only one (docs/03 §4.3 step 2).
pub fn minimal_cycles(
    mesh: &HalfEdgeMesh,
    plane: &Plane,
    seeds: &[VertexId],
    tol: &Tolerances,
) -> Result<Vec<Vec<HalfEdgeId>>, GeometryError> {
    todo!("docs/03 §4.3 minimal cycles")
}

/// Outgoing half-edges of `v` sorted by angle in `plane`'s 2D frame
/// (docs/03 §4.3 "angularly sorted star"). Edges not in the plane are skipped.
pub fn sort_star_by_angle(
    mesh: &HalfEdgeMesh,
    v: VertexId,
    plane: &Plane,
    tol: &Tolerances,
) -> Result<Vec<HalfEdgeId>, GeometryError> {
    todo!("docs/03 §4.3 angular star sort")
}

/// Default front orientation for a new face: `+Z` when the plane is not
/// vertical, else `+Y`, else `+X` (docs/03 §4.3 step 3).
pub fn default_face_normal(plane: &Plane, tol: &Tolerances) -> DVec3 {
    let n = plane.normal;
    if n.z.abs() > tol.angle {
        if n.z > 0.0 { n } else { -n }
    } else if n.y.abs() > tol.angle {
        if n.y > 0.0 { n } else { -n }
    } else if n.x >= 0.0 {
        n
    } else {
        -n
    }
}

/// Shoelace signed area of a closed 2D polygon (positive = counter-clockwise).
pub fn signed_area_2d(points: &[DVec2]) -> f64 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        sum += a.x * b.y - b.x * a.y;
    }
    sum * 0.5
}

/// Orientation from the signed area with the degeneracy threshold `length²`
/// (docs/03 §2: faces with area < length² are degenerate).
pub fn loop_orientation(points: &[DVec2], tol: &Tolerances) -> LoopOrientation {
    let area = signed_area_2d(points);
    if area.abs() <= tol.length_sq {
        LoopOrientation::Degenerate
    } else if area > 0.0 {
        LoopOrientation::CounterClockwise
    } else {
        LoopOrientation::Clockwise
    }
}

/// Point-in-polygon test (even-odd rule) in 2D; points on the boundary within
/// `tol.length` count as inside (docs/03 §4.2 step 3).
pub fn point_in_polygon_2d(p: DVec2, polygon: &[DVec2], tol: &Tolerances) -> bool {
    todo!("docs/03 §4.2 point in polygon")
}

/// `true` when polygon `inner` lies entirely inside `outer` (used to assign
/// holes after a split, docs/03 §4.4, and to detect hole situations, §4.5).
pub fn polygon_contains_polygon_2d(outer: &[DVec2], inner: &[DVec2], tol: &Tolerances) -> bool {
    todo!("docs/03 §4.4 polygon containment")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_area_and_orientation() {
        let ccw = [
            DVec2::new(0.0, 0.0),
            DVec2::new(10.0, 0.0),
            DVec2::new(10.0, 10.0),
            DVec2::new(0.0, 10.0),
        ];
        assert_eq!(signed_area_2d(&ccw), 100.0);
        let tol = Tolerances::default();
        assert_eq!(
            loop_orientation(&ccw, &tol),
            LoopOrientation::CounterClockwise
        );
        let mut cw = ccw;
        cw.reverse();
        assert_eq!(loop_orientation(&cw, &tol), LoopOrientation::Clockwise);
        assert_eq!(
            loop_orientation(&ccw[..2], &tol),
            LoopOrientation::Degenerate
        );
    }

    #[test]
    fn default_normal_prefers_up_then_y_then_x() {
        let tol = Tolerances::default();
        assert_eq!(
            default_face_normal(&Plane::new(-DVec3::Z, 0.0), &tol),
            DVec3::Z
        );
        assert_eq!(
            default_face_normal(&Plane::new(-DVec3::Y, 0.0), &tol),
            DVec3::Y
        );
        assert_eq!(
            default_face_normal(&Plane::new(-DVec3::X, 0.0), &tol),
            DVec3::X
        );
    }
}
