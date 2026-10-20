//! Implements the `geom/` entry of docs/01-architecture.md §2 and
//! docs/03-geometry-kernel.md §3–§5, §9: the half-edge mesh, face finding,
//! tessellation, intersections, booleans, offset, sweep, extrude and the
//! optional B-rep bridge.
//!
//! Every operation takes `&mut HalfEdgeMesh`, the document [`Tolerances`] and a
//! `&mut dyn DeltaSink` that records inverse operations for undo (docs/03 §4,
//! docs/01 §3.1 rule 1). Operations return `Result<_, GeometryError>` and never
//! leave the mesh invalid (apply-or-rollback inside the function).
//!
//! Mesh deltas identify vertices by **position**, not by `VertexId`: slot-map
//! keys cannot be re-created at a chosen index, while positions are a stable
//! identity thanks to invariant 3 of docs/03 §3.1 (no two vertices within
//! `tolerances.length`). Replaying `VertexAdded { position }` through
//! `add_vertex` therefore restores the mesh exactly (docs/04 Decisions).

pub mod boolean;
#[cfg(feature = "brep")]
pub mod brep;
pub mod extrude;
pub mod intersect;
pub mod mesh;
pub mod offset;
pub mod planar;
pub mod sweep;
pub mod tessellate;

pub use mesh::{
    AddEdgeResult, CurveDescriptor, Edge, EdgeFlags, EraseEdgeResult, EraseFaceResult, Face,
    HalfEdge, HalfEdgeMesh, IndexedMesh, Loop, TransformOptions, TransformResult, Vertex,
};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::ids::{CurveId, EdgeId, FaceId, MaterialId, VertexId};
use crate::math::Plane;
use crate::tolerance::Tolerances;

/// Geometry failures (docs/01 §7, codes from docs/08 §9).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GeometryError {
    /// Zero-length edge, zero-area face, collinear points… (`E1301`).
    #[error("degenerate geometry: {0}")]
    Degenerate(String),
    /// A robust answer could not be computed, e.g. a boolean with coplanar
    /// overlaps beyond tolerance (`E1302`). The message carries guidance.
    #[error("non-robust geometry: {0}")]
    NonRobust(String),
    /// A coordinate exceeds the supported model extent of docs/03 §1 (`E1002`).
    #[error("coordinate out of the supported range (±1e9 mm)")]
    OutOfRange,
    /// A vertex/edge/face/loop/curve id is not in this mesh (`E1101`).
    #[error("mesh element not found: {0}")]
    NotFound(String),
    /// The operation does not apply to this input, e.g. extruding a face whose
    /// loop is not planar, or a boolean on a non-solid (`E1201`).
    #[error("operation not applicable: {0}")]
    NotApplicable(String),
    /// A mesh invariant of docs/03 §3.1 would be violated (`E1302`).
    #[error("mesh invariant violated: {0}")]
    Invariant(String),
}

impl GeometryError {
    /// Stable machine-readable code (docs/08 §9).
    pub const fn code(&self) -> &'static str {
        match self {
            GeometryError::Degenerate(_) => "E1301",
            GeometryError::NonRobust(_) | GeometryError::Invariant(_) => "E1302",
            GeometryError::OutOfRange => "E1002",
            GeometryError::NotFound(_) => "E1101",
            GeometryError::NotApplicable(_) => "E1201",
        }
    }
}

/// Per-side face appearance (docs/03 §3.1 `material_front/back, uv_front/back`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FaceSides {
    /// Front material; `None` = the document's default front material.
    pub material_front: Option<MaterialId>,
    /// Back material; `None` = the document's default back material.
    pub material_back: Option<MaterialId>,
    /// Front UV mapping (docs/04 §4).
    pub uv_front: Option<crate::document::UvMap>,
    /// Back UV mapping.
    pub uv_back: Option<crate::document::UvMap>,
}

/// Everything needed to re-create a face on undo (docs/03 §4.4 "undo restores").
/// Loops are vertex positions in loop order (outer counter-clockwise seen from
/// the front, holes clockwise).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceSnapshot {
    /// Outer loop positions.
    pub outer: Vec<DVec3>,
    /// Hole loop positions.
    pub holes: Vec<Vec<DVec3>>,
    /// Face plane.
    pub plane: Plane,
    /// Materials and UVs.
    pub sides: FaceSides,
}

/// Elementary, invertible mesh change recorded by every kernel operation
/// (docs/03 §4, docs/01 §3.1). `FaceSplit` and `FacesMerged` are composite
/// markers emitted *in addition to* the elementary adds/removes so that
/// frontends and the pid bookkeeping in `commands::transaction` can keep
/// identities (the split face's pid goes to `new[0]`, docs/03 §4.4, §4.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MeshDelta {
    /// A vertex was created.
    VertexAdded {
        /// Id at the time of the change.
        id: VertexId,
        /// Position (stable identity).
        position: DVec3,
    },
    /// An isolated vertex was removed.
    VertexRemoved {
        /// Id at the time of the change.
        id: VertexId,
        /// Position (stable identity).
        position: DVec3,
    },
    /// A vertex moved (docs/03 §4.8).
    VertexMoved {
        /// Id at the time of the change.
        id: VertexId,
        /// Previous position.
        from: DVec3,
        /// New position.
        to: DVec3,
    },
    /// An edge was created between two vertex positions.
    EdgeAdded {
        /// Id at the time of the change.
        id: EdgeId,
        /// Start position.
        a: DVec3,
        /// End position.
        b: DVec3,
        /// Flags at creation.
        flags: EdgeFlags,
        /// Curve membership at creation.
        curve: Option<CurveId>,
    },
    /// An edge was removed.
    EdgeRemoved {
        /// Id at the time of the change.
        id: EdgeId,
        /// Start position.
        a: DVec3,
        /// End position.
        b: DVec3,
        /// Flags before removal.
        flags: EdgeFlags,
        /// Curve membership before removal.
        curve: Option<CurveId>,
    },
    /// Edge flags changed (soften/smooth/hide, docs/03 §3.2).
    EdgeFlagsChanged {
        /// Edge.
        id: EdgeId,
        /// Previous flags.
        from: EdgeFlags,
        /// New flags.
        to: EdgeFlags,
    },
    /// A face was created.
    FaceAdded {
        /// Id at the time of the change.
        id: FaceId,
        /// Full description.
        snapshot: FaceSnapshot,
    },
    /// A face was removed.
    FaceRemoved {
        /// Id at the time of the change.
        id: FaceId,
        /// Full description.
        snapshot: FaceSnapshot,
    },
    /// Materials or UVs of a face changed.
    FaceSidesChanged {
        /// Face.
        id: FaceId,
        /// Previous sides.
        from: FaceSides,
        /// New sides.
        to: FaceSides,
    },
    /// Composite marker: a face was split by a chord (docs/03 §4.4).
    FaceSplit {
        /// The removed face.
        old: FaceId,
        /// The two replacement faces; `new[0]` inherits the pid.
        new: [FaceId; 2],
    },
    /// Composite marker: two coplanar faces were merged (docs/03 §4.6).
    FacesMerged {
        /// The surviving face (keeps id, pid and material).
        kept: FaceId,
        /// The removed face.
        removed: FaceId,
    },
    /// A curve descriptor was attached to edges (docs/03 §3.3).
    CurveAdded {
        /// Id at the time of the change.
        id: CurveId,
        /// Descriptor.
        descriptor: CurveDescriptor,
        /// Member edges in order.
        members: Vec<EdgeId>,
    },
    /// A curve descriptor was dropped (edges became plain).
    CurveRemoved {
        /// Id at the time of the change.
        id: CurveId,
        /// Descriptor.
        descriptor: CurveDescriptor,
        /// Former member edges in order.
        members: Vec<EdgeId>,
    },
}

/// Receiver of [`MeshDelta`]s. `commands::Transaction` implements it to wrap
/// mesh deltas into document deltas; tests use `Vec<MeshDelta>` or [`NoDeltas`].
pub trait DeltaSink {
    /// Records one elementary change, in application order.
    fn record(&mut self, delta: MeshDelta);
}

impl DeltaSink for Vec<MeshDelta> {
    fn record(&mut self, delta: MeshDelta) {
        self.push(delta);
    }
}

/// A sink that discards deltas (bulk import fast paths, previews).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDeltas;

impl DeltaSink for NoDeltas {
    fn record(&mut self, _delta: MeshDelta) {}
}

/// Convenience for operations that need the tolerances and a sink together.
pub struct OpContext<'a> {
    /// Document tolerances (docs/03 §2).
    pub tol: &'a Tolerances,
    /// Delta receiver.
    pub sink: &'a mut dyn DeltaSink,
}
