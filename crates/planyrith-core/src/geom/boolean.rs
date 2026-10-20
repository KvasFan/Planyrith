//! Implements docs/03-geometry-kernel.md §4.13 (solid classification and
//! booleans): union, intersect, subtract, trim, split, outer shell on closed
//! 2-manifold containers, with the `brep` fallback hook.
//!
//! Owner key: `later` (milestone 0.8).

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::diagnostics::Warning;
use crate::geom::{GeometryError, HalfEdgeMesh};
use crate::ids::FaceId;
use crate::tolerance::Tolerances;

/// Boolean operation (docs/03 §4.13 step 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BooleanOp {
    /// Outside A ∪ outside B.
    Union,
    /// Inside ∩.
    Intersect,
    /// A − B.
    Subtract,
    /// Subtract but keep B.
    Trim,
    /// All three regions as separate solids.
    Split,
    /// Union of all, dropping internal voids.
    OuterShell,
}

/// Position of a point or face piece relative to a solid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// Strictly inside.
    Inside,
    /// Strictly outside.
    Outside,
    /// On the boundary (coplanar piece); `same_normal` tells the orientation.
    On {
        /// `true` when the piece's normal agrees with the solid's face.
        same_normal: bool,
    },
}

/// Result of [`boolean`].
#[derive(Debug, Clone)]
pub struct BooleanResult {
    /// Resulting solids (one for most ops; up to three for `Split`).
    pub solids: Vec<HalfEdgeMesh>,
    /// Non-fatal notes (e.g. `W2001` coplanar merges).
    pub warnings: Vec<Warning>,
}

/// Classifies a point against a solid by ray parity (docs/03 §4.13 step 2).
pub fn classify_point(
    solid: &HalfEdgeMesh,
    p: DVec3,
    tol: &Tolerances,
) -> Result<Classification, GeometryError> {
    todo!("docs/03 §4.13 point classification")
}

/// Classifies face `f` of `pieces` against `other` (ray parity + coplanar
/// handling by normal comparison).
pub fn classify_face(
    pieces: &HalfEdgeMesh,
    f: FaceId,
    other: &HalfEdgeMesh,
    tol: &Tolerances,
) -> Result<Classification, GeometryError> {
    todo!("docs/03 §4.13 face classification")
}

/// Half-edge boolean of two solids given in world space through their
/// container transforms (docs/03 §4.13). Returns `NonRobust` with guidance when
/// classification is ambiguous and no `brep` fallback is available.
pub fn boolean(
    op: BooleanOp,
    a: &HalfEdgeMesh,
    to_world_a: &DAffine3,
    b: &HalfEdgeMesh,
    to_world_b: &DAffine3,
    tol: &Tolerances,
) -> Result<BooleanResult, GeometryError> {
    todo!("docs/03 §4.13 boolean")
}

/// Merges coplanar adjacent faces across the whole mesh (step 4 of
/// docs/03 §4.13; also used after imports, docs/09 §6).
pub fn merge_coplanar_faces(
    mesh: &mut HalfEdgeMesh,
    tol: &Tolerances,
) -> Result<usize, GeometryError> {
    todo!("docs/03 §4.13 coplanar merge pass")
}
