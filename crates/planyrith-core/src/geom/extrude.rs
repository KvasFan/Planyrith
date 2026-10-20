//! Implements docs/03-geometry-kernel.md §4.9 (extrude face, the Push/Pull
//! core): offset loops along the normal, side quads, cap, coplanar merge and
//! the push-through rule.
//!
//! Owner key: `later` (first consumer: the `push-pull` command, docs/05 §4.3).

use crate::geom::{DeltaSink, GeometryError, HalfEdgeMesh};
use crate::ids::FaceId;
use crate::tolerance::Tolerances;
use crate::units::Length;

/// Options for [`extrude_face`] (docs/03 §4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtrudeOptions {
    /// Keep the original face (Push/Pull modifier) instead of deleting it.
    pub create_new_start_face: bool,
    /// Merge new side faces with coplanar neighbors (step 3).
    pub merge_coplanar: bool,
    /// Cut a through-hole when the cap lands inside a coplanar face (step 4).
    pub push_through: bool,
}

impl Default for ExtrudeOptions {
    fn default() -> Self {
        ExtrudeOptions {
            create_new_start_face: false,
            merge_coplanar: true,
            push_through: true,
        }
    }
}

/// Result of [`extrude_face`]; mirrors the `push-pull` output of docs/08 §7.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtrudeResult {
    /// The new cap face (`None` when a through-hole consumed it).
    pub cap: Option<FaceId>,
    /// Side faces created (after merging).
    pub side_faces: Vec<FaceId>,
    /// Faces removed (the start face unless kept; the hole interior).
    pub removed_faces: Vec<FaceId>,
    /// Side faces merged into existing coplanar neighbors (`W2001`).
    pub merged_faces: Vec<FaceId>,
    /// `true` when the push-through rule created a hole.
    pub through_hole: bool,
}

/// Extrudes face `f` by `distance` along its normal (negative = push)
/// (docs/03 §4.9 steps 1–5).
pub fn extrude_face(
    mesh: &mut HalfEdgeMesh,
    f: FaceId,
    distance: Length,
    opts: ExtrudeOptions,
    tol: &Tolerances,
    sink: &mut dyn DeltaSink,
) -> Result<ExtrudeResult, GeometryError> {
    todo!("docs/03 §4.9 extrude_face")
}
