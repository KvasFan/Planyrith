//! Implements docs/03-geometry-kernel.md §4.10 (offset loop): offsets a face
//! boundary in its plane with miter joins and self-intersection clipping.
//!
//! Owner key: `later`.

use glam::DVec2;

use crate::geom::{DeltaSink, GeometryError, HalfEdgeMesh};
use crate::ids::{EdgeId, FaceId};
use crate::tolerance::Tolerances;
use crate::units::Length;

/// Result of [`offset_face_boundary`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OffsetResult {
    /// Edges created (via `add_edge`, so faces form automatically).
    pub edges: Vec<EdgeId>,
    /// The face bounded by the offset loop, when one formed.
    pub face: Option<FaceId>,
    /// Faces split as a side effect.
    pub faces_split: Vec<FaceId>,
}

/// Offsets every loop of `f` by `d` (positive = outward for the outer loop,
/// inward for holes) and adds the result as edges (docs/03 §4.10).
pub fn offset_face_boundary(
    mesh: &mut HalfEdgeMesh,
    f: FaceId,
    d: Length,
    tol: &Tolerances,
    sink: &mut dyn DeltaSink,
) -> Result<OffsetResult, GeometryError> {
    todo!("docs/03 §4.10 offset_face_boundary")
}

/// Pure 2D polygon offset: miter joins at convex corners, trimming at concave
/// corners, self-intersections clipped (docs/03 §4.10).
pub fn offset_polygon_2d(
    polygon: &[DVec2],
    d: f64,
    tol: &Tolerances,
) -> Result<Vec<DVec2>, GeometryError> {
    todo!("docs/03 §4.10 2D polygon offset")
}
