//! Implements docs/03-geometry-kernel.md §4.12 (intersect faces) and the
//! primitive intersection tests used by §4.2 (edge splitting), §6 (ray casts)
//! and §4.13 (booleans).
//!
//! Owner key: `later`.

use glam::{DAffine3, DVec3};

use crate::geom::{DeltaSink, GeometryError, HalfEdgeMesh};
use crate::ids::{EdgeId, FaceId};
use crate::math::{Plane, Ray, Segment};
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
/// clamping (docs/03 §8).
pub fn segment_segment(a: &Segment, b: &Segment, tol: &Tolerances) -> SegmentIntersection {
    todo!("docs/03 §4.2 segment–segment intersection")
}

/// Segment–plane crossing: `(t, point)` or `None` when parallel or off the segment.
pub fn segment_plane(s: &Segment, plane: &Plane, tol: &Tolerances) -> Option<(f64, DVec3)> {
    todo!("docs/03 §4.12 segment–plane")
}

/// Ray against one face (plane hit + point-in-polygon with holes), exact CPU
/// confirmation for picking (docs/06 §8).
pub fn ray_face(
    ray: &Ray,
    mesh: &HalfEdgeMesh,
    f: FaceId,
    tol: &Tolerances,
) -> Result<Option<RayHit>, GeometryError> {
    todo!("docs/06 §8 ray–face")
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
    todo!("docs/03 §4.12 face–face intersection")
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
    todo!("docs/03 §4.12 intersect_faces")
}
