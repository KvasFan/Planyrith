//! Implements docs/03-geometry-kernel.md §4.11 (sweep): extrudes a profile face
//! along a path of edges with minimum-rotation frames and mitered corners.
//!
//! Owner key: `later` (milestone 0.8).

use glam::{DAffine3, DVec3};

use crate::geom::{DeltaSink, GeometryError, HalfEdgeMesh};
use crate::ids::{EdgeId, FaceId};
use crate::tolerance::Tolerances;

/// Options for [`sweep_profile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepOptions {
    /// Delete the profile face (it becomes interior) (default `true`).
    pub delete_profile: bool,
    /// Miter the profile at path corners (default `true`).
    pub miter_corners: bool,
}

impl Default for SweepOptions {
    fn default() -> Self {
        SweepOptions {
            delete_profile: true,
            miter_corners: true,
        }
    }
}

/// Result of [`sweep_profile`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SweepResult {
    /// Faces created (side quads and end caps).
    pub faces: Vec<FaceId>,
    /// `true` when the path was closed and a ring was produced.
    pub closed: bool,
}

/// Minimum-rotation frames along a polyline (one per vertex), Frenet-free to
/// avoid twisting (docs/03 §4.11). For closed paths the last frame is
/// reconciled with the first.
pub fn path_frames(points: &[DVec3], closed: bool) -> Vec<DAffine3> {
    todo!("docs/03 §4.11 minimum rotation frames")
}

/// Sweeps `profile` along the connected edge chain `path` (docs/03 §4.11).
/// The profile must be perpendicular to the first path segment within
/// `tol.angle` (`NotApplicable` otherwise, `E1201`).
pub fn sweep_profile(
    mesh: &mut HalfEdgeMesh,
    profile: FaceId,
    path: &[EdgeId],
    opts: SweepOptions,
    tol: &Tolerances,
    sink: &mut dyn DeltaSink,
) -> Result<SweepResult, GeometryError> {
    todo!("docs/03 §4.11 sweep_profile")
}
