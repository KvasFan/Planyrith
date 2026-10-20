//! Implements docs/03-geometry-kernel.md §3 (the half-edge mesh), §4.1–§4.8
//! (fundamental operations) and the indexed form of docs/09 §2 used by the
//! native file format (`from_indexed` / `to_indexed`) and importers
//! (`from_triangles_merged`, docs/09 §6).
//!
//! Owner key: `mesh` (docs/dev/skeleton-contract.md).
//!
//! Layout: this file holds the element types, the accessors, the private
//! topology primitives (make/remove edge, attach/detach face, split edge) and
//! the public operation signatures. The operations themselves live in child
//! modules: `edit` (docs/03 §4.2–§4.8), `indexed` (docs/09 §2, §6),
//! `analysis` (§3.1 validation, §4.13) and `replay` (undo/redo of recorded
//! deltas, docs/01 §3.1).
//!
//! Implementation notes (recorded as decisions in docs/03):
//! - Loop connectivity (`next`/`prev`) is authoritative only for half-edges
//!   that belong to a face loop. Free half-edges point `next`/`prev` at their
//!   twin. Loops are re-wired from geometry whenever a face is created, split
//!   or merged; there is no global rotation system.
//! - A face whose region survives an operation keeps its [`FaceId`]: the change
//!   is recorded as `FaceRemoved { id }` followed by `FaceAdded { id }` with the
//!   same id ("reshape"). Splits keep the old id for one piece
//!   (`FaceSplit { old, new: [old, other] }`).
//! - Public operations are transactional: they work on the live mesh and
//!   restore a snapshot taken on entry when they fail, and forward their
//!   deltas to the sink only on success.

#![warn(unused_variables, dead_code)]

mod analysis;
mod edit;
mod indexed;
mod replay;
#[cfg(test)]
mod tests;

pub use replay::ReplayIds;

use std::collections::BTreeSet;

use glam::{DAffine3, DVec2, DVec3};
use serde::{Deserialize, Serialize};
use slotmap::{Key, SecondaryMap, SlotMap};

use crate::geom::planar;
use crate::geom::{DeltaSink, FaceSides, FaceSnapshot, GeometryError, MeshDelta};
use crate::ids::{CurveId, EdgeId, FaceId, HalfEdgeId, LoopId, VertexId};
use crate::math::{Aabb, Plane, PlaneFrame, Segment, polygon};
use crate::tolerance::Tolerances;
use crate::units::Angle;

/// Edge rendering/shading flags (docs/03 §3.2). A plain `u8` newtype instead of
/// the `bitflags` crate (not in the approved dependency list, docs/02 §1).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct EdgeFlags(pub u8);

impl EdgeFlags {
    /// No flags.
    pub const NONE: EdgeFlags = EdgeFlags(0);
    /// Shading normals averaged across this edge (smooth shading).
    pub const SOFT: EdgeFlags = EdgeFlags(0b0001);
    /// Edge not drawn when both faces are visible.
    pub const SMOOTH: EdgeFlags = EdgeFlags(0b0010);
    /// Never drawn unless "show hidden geometry" is on.
    pub const HIDDEN: EdgeFlags = EdgeFlags(0b0100);
    /// Reserved.
    pub const CAST_SHADOW_PROFILE: EdgeFlags = EdgeFlags(0b1000);

    /// `true` when every bit of `other` is set.
    pub const fn contains(self, other: EdgeFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Union.
    pub const fn union(self, other: EdgeFlags) -> EdgeFlags {
        EdgeFlags(self.0 | other.0)
    }

    /// Difference.
    pub const fn difference(self, other: EdgeFlags) -> EdgeFlags {
        EdgeFlags(self.0 & !other.0)
    }

    /// Sets or clears `other`.
    pub const fn with(self, other: EdgeFlags, on: bool) -> EdgeFlags {
        if on {
            self.union(other)
        } else {
            self.difference(other)
        }
    }

    /// Raw bits (file format, docs/09 §2 `flags: u8`).
    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for EdgeFlags {
    type Output = EdgeFlags;
    fn bitor(self, rhs: EdgeFlags) -> EdgeFlags {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for EdgeFlags {
    fn bitor_assign(&mut self, rhs: EdgeFlags) {
        self.0 |= rhs.0;
    }
}

/// A mesh vertex (docs/03 §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vertex {
    /// Position in container-local millimeters.
    pub position: DVec3,
}

/// A directed half-edge (docs/03 §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HalfEdge {
    /// Start vertex.
    pub origin: VertexId,
    /// The opposite half-edge of the same edge.
    pub twin: HalfEdgeId,
    /// Next half-edge around the face loop (or around the wire chain).
    pub next: HalfEdgeId,
    /// Previous half-edge around the face loop.
    pub prev: HalfEdgeId,
    /// Owning edge.
    pub edge: EdgeId,
    /// Face on the left of this half-edge; `None` for an open side.
    pub face: Option<FaceId>,
}

/// An undirected edge (docs/03 §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// One of the two half-edges.
    pub he: HalfEdgeId,
    /// Rendering/shading flags.
    pub flags: EdgeFlags,
    /// Curve descriptor this edge belongs to, if any (docs/03 §3.3).
    pub curve: Option<CurveId>,
}

/// A planar face (docs/03 §3.1). The outer loop is counter-clockwise seen from
/// the front (along `+plane.normal`); holes are clockwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Face {
    /// Outer boundary loop.
    pub outer: LoopId,
    /// Hole loops.
    pub holes: Vec<LoopId>,
    /// Face plane; all loop vertices lie within `tolerances.planarity` of it.
    pub plane: Plane,
    /// Materials and UVs per side.
    pub sides: FaceSides,
}

/// A boundary loop (docs/03 §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loop {
    /// First half-edge of the cycle.
    pub start: HalfEdgeId,
    /// Owning face.
    pub face: FaceId,
}

/// Metadata grouping consecutive edges into one curve (docs/03 §3.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CurveDescriptor {
    /// Circular arc.
    Arc {
        /// Center.
        center: DVec3,
        /// Unit plane normal.
        normal: DVec3,
        /// Radius in mm.
        radius: f64,
        /// Start angle in the arc plane frame.
        start_angle: Angle,
        /// End angle in the arc plane frame.
        end_angle: Angle,
        /// Number of segments.
        segments: u32,
    },
    /// Full circle.
    Circle {
        /// Center.
        center: DVec3,
        /// Unit plane normal.
        normal: DVec3,
        /// Radius in mm.
        radius: f64,
        /// Number of segments.
        segments: u32,
    },
    /// Regular polygon.
    Polygon {
        /// Center.
        center: DVec3,
        /// Unit plane normal.
        normal: DVec3,
        /// Radius in mm (circumscribed when `inscribed`, docs/05 §3.6).
        radius: f64,
        /// Number of sides.
        sides: u32,
        /// `true` when the polygon is inscribed in the radius circle.
        inscribed: bool,
    },
    /// Freehand polyline (docs/05 §3.2).
    Freehand {
        /// Sampled points.
        points: Vec<DVec3>,
    },
}

/// Result of [`HalfEdgeMesh::add_edge`] (docs/03 §4.2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AddEdgeResult {
    /// The chain of edges created (after splitting at intersections/vertices).
    pub edges: Vec<EdgeId>,
    /// Faces created by face finding.
    pub new_faces: Vec<FaceId>,
    /// Faces split by a chord: `(old, new_a, new_b)`.
    pub split_faces: Vec<(FaceId, FaceId, FaceId)>,
    /// Existing edges that were split: `(old, [part_a, part_b])`.
    pub split_edges: Vec<(EdgeId, [EdgeId; 2])>,
}

/// Result of [`HalfEdgeMesh::erase_edge`] (docs/03 §4.6).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EraseEdgeResult {
    /// Faces deleted because they lost a boundary edge.
    pub faces_removed: Vec<FaceId>,
    /// Coplanar neighbors merged: `(kept, removed)`.
    pub faces_merged: Option<(FaceId, FaceId)>,
    /// Vertices deleted because they became isolated.
    pub vertices_removed: Vec<VertexId>,
}

/// Result of [`HalfEdgeMesh::erase_face`] (docs/03 §4.7).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EraseFaceResult {
    /// Edges removed with `erase_orphans`.
    pub edges_removed: Vec<EdgeId>,
}

/// Options for [`HalfEdgeMesh::transform_vertices`] (docs/03 §4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransformOptions {
    /// Insert hidden soft edges to keep faces planar (docs/03 §4.8 step 2).
    pub auto_fold: bool,
    /// Re-check intersections with other edges/faces (Move default: off).
    pub check_intersections: bool,
}

impl Default for TransformOptions {
    fn default() -> Self {
        TransformOptions {
            auto_fold: true,
            check_intersections: false,
        }
    }
}

/// Result of [`HalfEdgeMesh::transform_vertices`] (docs/03 §4.8).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransformResult {
    /// Vertex pairs merged by tolerance (`W2002`).
    pub merged_vertices: usize,
    /// Edges collapsed because both endpoints merged.
    pub collapsed_edges: Vec<EdgeId>,
    /// Faces that were auto-folded (new hidden edges inside).
    pub folded_faces: Vec<FaceId>,
    /// Fold edges inserted (`SOFT | SMOOTH | HIDDEN`).
    pub fold_edges: Vec<EdgeId>,
}

/// An edge index with a direction bit, as stored in face loops of the native
/// format (docs/09 §2 "edge index, sign bit = direction").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DirectedEdgeIndex(pub u32);

impl DirectedEdgeIndex {
    const DIR_BIT: u32 = 1 << 31;

    /// Forward reference to edge `index` (`a → b`).
    pub const fn forward(index: u32) -> Self {
        DirectedEdgeIndex(index & !Self::DIR_BIT)
    }

    /// Reversed reference to edge `index` (`b → a`).
    pub const fn reversed(index: u32) -> Self {
        DirectedEdgeIndex(index | Self::DIR_BIT)
    }

    /// Edge index without the direction bit.
    pub const fn index(self) -> u32 {
        self.0 & !Self::DIR_BIT
    }

    /// `true` when the loop traverses the edge from `b` to `a`.
    pub const fn is_reversed(self) -> bool {
        self.0 & Self::DIR_BIT != 0
    }
}

/// Edge record of [`IndexedMesh`] (docs/09 §2 `edges`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedEdge {
    /// Start vertex index.
    pub a: u32,
    /// End vertex index.
    pub b: u32,
    /// Flags.
    pub flags: EdgeFlags,
    /// Index into `IndexedMesh::curves`.
    pub curve: Option<u32>,
}

/// Face record of [`IndexedMesh`] (docs/09 §2 `faces`); loop 0 is the outer loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexedFace {
    /// Loops as directed edge indices; `loops[0]` is the outer loop.
    pub loops: Vec<Vec<DirectedEdgeIndex>>,
    /// Stored plane; recomputed on load when `None` (docs/09 §2).
    pub plane: Option<Plane>,
    /// Materials and UVs.
    pub sides: FaceSides,
}

/// The slot-layout-independent form of a mesh (docs/09 §2 `EntitiesSer`
/// geometry part). Pids are attached by the container, not here.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexedMesh {
    /// Vertex positions; index = local vertex index.
    pub vertices: Vec<DVec3>,
    /// Edges by vertex index.
    pub edges: Vec<IndexedEdge>,
    /// Faces by directed edge index.
    pub faces: Vec<IndexedFace>,
    /// Curve descriptors referenced by `IndexedEdge::curve`.
    pub curves: Vec<CurveDescriptor>,
}

/// Options for [`HalfEdgeMesh::from_triangles_merged`] (docs/09 §6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TriangleMergeOptions {
    /// Merge coplanar adjacent triangles into faces.
    pub merge_coplanar: bool,
    /// Edges between faces meeting below this angle become `SOFT | SMOOTH`.
    pub soften_angle: Angle,
}

impl Default for TriangleMergeOptions {
    fn default() -> Self {
        TriangleMergeOptions {
            merge_coplanar: true,
            soften_angle: Angle::degrees(20.0),
        }
    }
}

/// Result of [`HalfEdgeMesh::from_triangles_merged`].
#[derive(Debug, Clone)]
pub struct TriangleMergeResult {
    /// The built mesh.
    pub mesh: HalfEdgeMesh,
    /// Coplanar merges performed (`W2001`).
    pub merged_faces: usize,
    /// Input edges shared by more than two triangles (dropped, reported).
    pub non_manifold_edges: usize,
    /// Degenerate input triangles skipped.
    pub degenerate_triangles: usize,
}

/// One violated invariant found by [`HalfEdgeMesh::validate`] (docs/03 §3.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshViolation {
    /// Invariant number per docs/03 §3.1 (1–5), or 0 for structural breakage.
    pub invariant: u8,
    /// Human-readable detail including the offending ids.
    pub detail: String,
}

/// The half-edge mesh of one `Entities` container (docs/03 §3.1). Struct of
/// slot maps; ids are stable while the element exists and never reused with the
/// same generation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HalfEdgeMesh {
    vertices: SlotMap<VertexId, Vertex>,
    half_edges: SlotMap<HalfEdgeId, HalfEdge>,
    edges: SlotMap<EdgeId, Edge>,
    faces: SlotMap<FaceId, Face>,
    loops: SlotMap<LoopId, Loop>,
    curves: SlotMap<CurveId, CurveDescriptor>,
    /// Outgoing half-edges per vertex (the star), angularly unsorted.
    vertex_edges: SecondaryMap<VertexId, Vec<HalfEdgeId>>,
    /// Member edges of every curve, in curve order (docs/03 §3.3).
    #[serde(default)]
    curve_members: SecondaryMap<CurveId, Vec<EdgeId>>,
}

/// `NotFound` for an id that is not in the mesh.
fn not_found(id: impl std::fmt::Display) -> GeometryError {
    GeometryError::NotFound(id.to_string())
}

/// `Invariant` with a message (structural breakage or a violated rule).
fn broken(msg: impl Into<String>) -> GeometryError {
    GeometryError::Invariant(msg.into())
}

impl HalfEdgeMesh {
    /// An empty mesh.
    pub fn new() -> Self {
        Self::default()
    }

    // ----- counts and accessors ------------------------------------------------

    /// `true` when the mesh has no vertices.
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    /// Number of vertices.
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    /// Number of edges.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Number of faces.
    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    /// Number of curve descriptors.
    pub fn curve_count(&self) -> usize {
        self.curves.len()
    }

    /// Vertex by id.
    pub fn vertex(&self, id: VertexId) -> Option<&Vertex> {
        self.vertices.get(id)
    }

    /// Half-edge by id.
    pub fn half_edge(&self, id: HalfEdgeId) -> Option<&HalfEdge> {
        self.half_edges.get(id)
    }

    /// Edge by id.
    pub fn edge(&self, id: EdgeId) -> Option<&Edge> {
        self.edges.get(id)
    }

    /// Face by id.
    pub fn face(&self, id: FaceId) -> Option<&Face> {
        self.faces.get(id)
    }

    /// Loop by id.
    pub fn loop_at(&self, id: LoopId) -> Option<&Loop> {
        self.loops.get(id)
    }

    /// Curve descriptor by id.
    pub fn curve(&self, id: CurveId) -> Option<&CurveDescriptor> {
        self.curves.get(id)
    }

    /// Vertices in slot order (deterministic for a given operation history).
    pub fn vertices(&self) -> slotmap::basic::Iter<'_, VertexId, Vertex> {
        self.vertices.iter()
    }

    /// Edges in slot order.
    pub fn edges(&self) -> slotmap::basic::Iter<'_, EdgeId, Edge> {
        self.edges.iter()
    }

    /// Faces in slot order.
    pub fn faces(&self) -> slotmap::basic::Iter<'_, FaceId, Face> {
        self.faces.iter()
    }

    /// Curves in slot order.
    pub fn curves(&self) -> slotmap::basic::Iter<'_, CurveId, CurveDescriptor> {
        self.curves.iter()
    }

    /// Outgoing half-edges of a vertex (unsorted; see
    /// [`planar::sort_star_by_angle`](crate::geom::planar::sort_star_by_angle)).
    pub fn vertex_star(&self, v: VertexId) -> &[HalfEdgeId] {
        self.vertex_edges.get(v).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Endpoints `(origin of he, origin of twin)` of an edge.
    pub fn edge_endpoints(&self, e: EdgeId) -> Result<(VertexId, VertexId), GeometryError> {
        let edge = self
            .edges
            .get(e)
            .ok_or_else(|| GeometryError::NotFound(e.to_string()))?;
        let he = self
            .half_edges
            .get(edge.he)
            .ok_or_else(|| GeometryError::Invariant(format!("{e} has no half-edge")))?;
        let twin = self
            .half_edges
            .get(he.twin)
            .ok_or_else(|| GeometryError::Invariant(format!("{e} has no twin")))?;
        Ok((he.origin, twin.origin))
    }

    /// The edge as a segment in local coordinates.
    pub fn edge_segment(&self, e: EdgeId) -> Result<Segment, GeometryError> {
        let (a, b) = self.edge_endpoints(e)?;
        Ok(Segment::new(self.position(a)?, self.position(b)?))
    }

    /// Edge length in mm.
    pub fn edge_length(&self, e: EdgeId) -> Result<f64, GeometryError> {
        Ok(self.edge_segment(e)?.length())
    }

    /// Position of a vertex.
    pub fn position(&self, v: VertexId) -> Result<DVec3, GeometryError> {
        self.vertices
            .get(v)
            .map(|vert| vert.position)
            .ok_or_else(|| GeometryError::NotFound(v.to_string()))
    }

    /// The faces bordering an edge: `(left of he, left of twin)`.
    pub fn edge_faces(&self, e: EdgeId) -> Result<[Option<FaceId>; 2], GeometryError> {
        let edge = self.edges.get(e).ok_or_else(|| not_found(e))?;
        let he = self.he(edge.he)?;
        let twin = self.he(he.twin)?;
        Ok([he.face, twin.face])
    }

    /// Half-edges of a loop in cycle order starting at `Loop::start`.
    pub fn loop_half_edges(&self, l: LoopId) -> Result<Vec<HalfEdgeId>, GeometryError> {
        let lp = self.loops.get(l).ok_or_else(|| not_found(l))?;
        let limit = self.half_edges.len();
        let mut out = Vec::new();
        let mut h = lp.start;
        loop {
            out.push(h);
            h = self.he(h)?.next;
            if h == lp.start {
                return Ok(out);
            }
            if out.len() > limit {
                return Err(broken(format!("{l} does not close")));
            }
        }
    }

    /// Vertices of a loop in cycle order.
    pub fn loop_vertices(&self, l: LoopId) -> Result<Vec<VertexId>, GeometryError> {
        self.loop_half_edges(l)?
            .into_iter()
            .map(|h| self.he(h).map(|he| he.origin))
            .collect()
    }

    /// Edges of a loop in cycle order.
    pub fn loop_edges(&self, l: LoopId) -> Result<Vec<EdgeId>, GeometryError> {
        self.loop_half_edges(l)?
            .into_iter()
            .map(|h| self.he(h).map(|he| he.edge))
            .collect()
    }

    /// Outer-loop vertices of a face in counter-clockwise order.
    pub fn face_vertices(&self, f: FaceId) -> Result<Vec<VertexId>, GeometryError> {
        let outer = self.face_ref(f)?.outer;
        self.loop_vertices(outer)
    }

    /// All edges bounding a face (outer loop then holes).
    pub fn face_edges(&self, f: FaceId) -> Result<Vec<EdgeId>, GeometryError> {
        let mut out = Vec::new();
        for l in self.face_loops(f)? {
            out.extend(self.loop_edges(l)?);
        }
        Ok(out)
    }

    /// Faces adjacent to `f` across its boundary edges (deduplicated, ordered by id).
    pub fn face_neighbors(&self, f: FaceId) -> Result<Vec<FaceId>, GeometryError> {
        let mut out = BTreeSet::new();
        for cycle in self.face_cycles(f)? {
            for h in cycle {
                let twin = self.he(self.twin_of(h)?)?;
                if let Some(g) = twin.face
                    && g != f
                {
                    out.insert(g);
                }
            }
        }
        Ok(out.into_iter().collect())
    }

    /// Unit normal of a face (`plane.normal`).
    pub fn face_normal(&self, f: FaceId) -> Result<DVec3, GeometryError> {
        self.faces
            .get(f)
            .map(|face| face.plane.normal)
            .ok_or_else(|| GeometryError::NotFound(f.to_string()))
    }

    /// Area of a face in mm² (outer loop minus holes).
    pub fn face_area(&self, f: FaceId) -> Result<f64, GeometryError> {
        let face = self.face_ref(f)?;
        let frame = face.plane.frame();
        let mut area = polygon::area(&self.loop_points_2d(face.outer, &frame)?);
        for hole in &face.holes {
            area -= polygon::area(&self.loop_points_2d(*hole, &frame)?);
        }
        Ok(area)
    }

    /// Snapshot of a face for deltas and undo.
    pub fn face_snapshot(&self, f: FaceId) -> Result<FaceSnapshot, GeometryError> {
        let face = self.face_ref(f)?;
        let outer = self.loop_positions(face.outer)?;
        let holes = face
            .holes
            .iter()
            .map(|&h| self.loop_positions(h))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(FaceSnapshot {
            outer,
            holes,
            plane: face.plane,
            sides: face.sides.clone(),
        })
    }

    /// Bounding box of all vertices (local coordinates).
    pub fn aabb(&self) -> Aabb {
        Aabb::from_points(self.vertices.values().map(|v| v.position))
    }

    /// Edges that belong to curve `c`, in curve order.
    pub fn curve_edges(&self, c: CurveId) -> Result<Vec<EdgeId>, GeometryError> {
        if !self.curves.contains_key(c) {
            return Err(not_found(c));
        }
        Ok(self.curve_members.get(c).cloned().unwrap_or_default())
    }

    /// `true` while all member edges exist and no member edge is used by a face
    /// other than the two the curve was created with (docs/03 §3.3).
    ///
    /// The draft tests the rule topologically: the member edges together border
    /// at most two distinct faces (one per side of the curve), so splitting or
    /// merging an unrelated part of a neighboring face keeps the curve intact,
    /// while extruding it (one side face per segment) does not (docs/03
    /// Decisions).
    pub fn curve_is_intact(&self, c: CurveId) -> Result<bool, GeometryError> {
        if !self.curves.contains_key(c) {
            return Err(not_found(c));
        }
        let members = self.curve_members.get(c).map(Vec::as_slice).unwrap_or(&[]);
        if members.is_empty() {
            return Ok(false);
        }
        let mut faces = BTreeSet::new();
        for &e in members {
            match self.edges.get(e) {
                Some(edge) if edge.curve == Some(c) => {}
                _ => return Ok(false),
            }
            faces.extend(self.edge_faces(e)?.into_iter().flatten());
        }
        Ok(faces.len() <= 2)
    }

    // ----- fundamental operations (docs/03 §4) --------------------------------

    /// Returns the vertex within `tol.length` of `p`, or creates one
    /// (docs/03 §4.1). Fails with `OutOfRange` beyond the model extent.
    pub fn add_vertex(
        &mut self,
        p: DVec3,
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<VertexId, GeometryError> {
        self.find_or_add_vertex(p, tol, sink)
    }

    /// Adds a straight edge, splitting against existing geometry and running
    /// face finding on every touched vertex (docs/03 §4.2). `a ≈ b` is
    /// `Degenerate`.
    ///
    /// `AddEdgeResult::edges` is the chain covering `[a, b]` in order from `a`
    /// to `b`, including pre-existing edges that already covered part of it.
    pub fn add_edge(
        &mut self,
        a: DVec3,
        b: DVec3,
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<AddEdgeResult, GeometryError> {
        self.transact(tol, sink, |m, d| m.add_edge_inner(a, b, tol, d))
    }

    /// Erases an edge with the face merge/delete rules of docs/03 §4.6 and
    /// removes vertices that became isolated. When two coplanar faces merge,
    /// the larger one keeps its id and appearance (`faces_merged` is
    /// `(kept, removed)`).
    pub fn erase_edge(
        &mut self,
        e: EdgeId,
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<EraseEdgeResult, GeometryError> {
        self.transact(tol, sink, |m, d| m.erase_edge_inner(e, tol, d))
    }

    /// Changes flags only (the `erase_edge_keep_faces` variant of docs/03 §4.6
    /// used by Soften/Smooth/Hide).
    pub fn set_edge_flags(
        &mut self,
        e: EdgeId,
        flags: EdgeFlags,
        sink: &mut dyn DeltaSink,
    ) -> Result<(), GeometryError> {
        let edge = self.edges.get_mut(e).ok_or_else(|| not_found(e))?;
        if edge.flags != flags {
            let from = edge.flags;
            edge.flags = flags;
            sink.record(MeshDelta::EdgeFlagsChanged {
                id: e,
                from,
                to: flags,
            });
        }
        Ok(())
    }

    /// Deletes a face; its edges stay unless `erase_orphans` (docs/03 §4.7).
    pub fn erase_face(
        &mut self,
        f: FaceId,
        erase_orphans: bool,
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<EraseFaceResult, GeometryError> {
        self.transact(tol, sink, |m, d| m.erase_face_inner(f, erase_orphans, d))
    }

    /// Replaces a face's materials/UVs.
    pub fn set_face_sides(
        &mut self,
        f: FaceId,
        sides: FaceSides,
        sink: &mut dyn DeltaSink,
    ) -> Result<(), GeometryError> {
        let face = self.faces.get_mut(f).ok_or_else(|| not_found(f))?;
        if face.sides != sides {
            let from = std::mem::replace(&mut face.sides, sides.clone());
            sink.record(MeshDelta::FaceSidesChanged {
                id: f,
                from,
                to: sides,
            });
        }
        Ok(())
    }

    /// Moves a set of vertices by `t`, merging by tolerance and auto-folding
    /// faces that would become non-planar (docs/03 §4.8).
    pub fn transform_vertices(
        &mut self,
        set: &[VertexId],
        t: &DAffine3,
        opts: TransformOptions,
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<TransformResult, GeometryError> {
        self.transact(tol, sink, |m, d| {
            m.transform_vertices_inner(set, t, opts, tol, d)
        })
    }

    /// Splits face `f` by a chord (a chain of existing interior edges whose
    /// endpoints lie on the face boundary); returns the two new faces with
    /// `[0]` inheriting the pid (docs/03 §4.4). `[0]` is `f` itself, reshaped
    /// in place (docs/03 Decisions).
    pub fn split_face_by_chord(
        &mut self,
        f: FaceId,
        chord: &[EdgeId],
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<[FaceId; 2], GeometryError> {
        self.transact(tol, sink, |m, d| {
            m.split_face_by_chord_inner(f, chord, tol, d)
        })
    }

    /// Merges two coplanar faces sharing at least one edge into `kept`
    /// (loops unioned, holes recomputed, shared edges removed) (docs/03 §4.6 step 1).
    pub fn merge_faces(
        &mut self,
        kept: FaceId,
        removed: FaceId,
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<FaceId, GeometryError> {
        self.transact(tol, sink, |m, d| m.merge_faces_inner(kept, removed, tol, d))
    }

    /// Creates a face from an existing closed edge cycle (plus optional hole
    /// cycles). Used by face finding and by file loading; validates planarity.
    pub fn add_face_from_loops(
        &mut self,
        outer: &[HalfEdgeId],
        holes: &[Vec<HalfEdgeId>],
        plane: Plane,
        sides: FaceSides,
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<FaceId, GeometryError> {
        // `create_face` validates everything before it mutates, so it is
        // atomic without a snapshot.
        self.create_face(outer, holes, plane, sides, tol, sink)
    }

    /// Attaches a curve descriptor to consecutive edges (docs/03 §3.3); marks
    /// inter-segment edges `SOFT | SMOOTH`.
    ///
    /// "Inter-segment edges" are the non-member edges that leave a vertex
    /// between two members and separate two faces that both contain a member
    /// edge (the side faces of an extruded curve). A curve drawn on a plane has
    /// none.
    pub fn add_curve(
        &mut self,
        descriptor: CurveDescriptor,
        members: &[EdgeId],
        sink: &mut dyn DeltaSink,
    ) -> Result<CurveId, GeometryError> {
        self.add_curve_inner(descriptor, members, true, sink)
    }

    /// Drops a descriptor; member edges become plain (docs/03 §3.3).
    pub fn drop_curve(
        &mut self,
        c: CurveId,
        sink: &mut dyn DeltaSink,
    ) -> Result<(), GeometryError> {
        self.drop_curve_inner(c, sink)
    }

    // ----- analysis -----------------------------------------------------------

    /// Checks every invariant of docs/03 §3.1; empty on success.
    pub fn validate(&self, tol: &Tolerances) -> Vec<MeshViolation> {
        self.validate_inner(tol, true)
    }

    /// `true` when the mesh is a closed, consistently oriented 2-manifold with
    /// no wire edges (docs/03 §4.13).
    pub fn is_solid(&self) -> bool {
        self.is_solid_inner()
    }

    /// Signed volume in mm³ for a solid (`NotApplicable` otherwise).
    pub fn volume(&self) -> Result<f64, GeometryError> {
        self.volume_inner()
    }

    // ----- bulk conversion ---------------------------------------------------

    /// Rebuilds the half-edge structure from the indexed form (docs/09 §2),
    /// recomputing missing planes and validating invariants.
    pub fn from_indexed(indexed: &IndexedMesh, tol: &Tolerances) -> Result<Self, GeometryError> {
        indexed::from_indexed(indexed, tol)
    }

    /// Converts to the indexed form. Vertex, edge and face order follow slot
    /// order; the returned maps give the local index of every id so the
    /// container can attach pids (docs/09 §2).
    pub fn to_indexed(&self) -> (IndexedMesh, IndexedIds) {
        indexed::to_indexed(self)
    }

    /// The indexed form in a canonical order independent of slot layout:
    /// `edge_order`/`face_order` first (e.g. a container's order list), the
    /// rest in slot order; vertices by first use (docs/09 §2, docs/12 §2).
    pub fn to_indexed_ordered(
        &self,
        edge_order: &[EdgeId],
        face_order: &[FaceId],
    ) -> (IndexedMesh, IndexedIds) {
        indexed::to_indexed_ordered(self, edge_order, face_order)
    }

    /// [`HalfEdgeMesh::from_indexed`] without the final validation and with
    /// planarity relaxed, for loading before the document tolerances are
    /// known; `planyrith validate` checks the result with the real ones.
    pub(crate) fn from_indexed_lenient(indexed: &IndexedMesh) -> Result<Self, GeometryError> {
        indexed::from_indexed_lenient(indexed)
    }

    /// Importer fast path: builds a mesh from triangles, merging coplanar
    /// neighbors and softening edges below `opts.soften_angle` (docs/09 §6).
    pub fn from_triangles_merged(
        positions: &[DVec3],
        triangles: &[[u32; 3]],
        opts: TriangleMergeOptions,
        tol: &Tolerances,
    ) -> Result<TriangleMergeResult, GeometryError> {
        indexed::from_triangles_merged(positions, triangles, opts, tol)
    }

    /// Transforms every vertex and face plane (bulk, no merging; used by
    /// explode/make-unique when re-parenting whole containers).
    pub fn transform_all(&mut self, t: &DAffine3) {
        for v in self.vertices.values_mut() {
            v.position = t.transform_point3(v.position);
        }
        for face in self.faces.values_mut() {
            face.plane = face.plane.transformed(t);
        }
    }
}

// ----- internal primitives ------------------------------------------------------

impl HalfEdgeMesh {
    /// Half-edge by id, or `Invariant` (a dangling reference is structural breakage).
    pub(crate) fn he(&self, h: HalfEdgeId) -> Result<&HalfEdge, GeometryError> {
        self.half_edges
            .get(h)
            .ok_or_else(|| broken(format!("missing {h}")))
    }

    fn he_mut(&mut self, h: HalfEdgeId) -> Result<&mut HalfEdge, GeometryError> {
        self.half_edges
            .get_mut(h)
            .ok_or_else(|| broken(format!("missing {h}")))
    }

    /// Face by id, or `NotFound`.
    pub(crate) fn face_ref(&self, f: FaceId) -> Result<&Face, GeometryError> {
        self.faces.get(f).ok_or_else(|| not_found(f))
    }

    /// Twin of a half-edge.
    pub(crate) fn twin_of(&self, h: HalfEdgeId) -> Result<HalfEdgeId, GeometryError> {
        Ok(self.he(h)?.twin)
    }

    /// End vertex of a half-edge (origin of its twin).
    pub(crate) fn dest(&self, h: HalfEdgeId) -> Result<VertexId, GeometryError> {
        let twin = self.he(h)?.twin;
        Ok(self.he(twin)?.origin)
    }

    /// The half-edge running `a → b`, if the edge exists.
    pub(crate) fn half_edge_from_to(&self, a: VertexId, b: VertexId) -> Option<HalfEdgeId> {
        self.vertex_star(a)
            .iter()
            .copied()
            .find(|&h| self.dest(h).ok() == Some(b))
    }

    /// The edge between two vertices, if any.
    pub(crate) fn edge_between(&self, a: VertexId, b: VertexId) -> Option<EdgeId> {
        self.half_edge_from_to(a, b)
            .and_then(|h| self.half_edges.get(h))
            .map(|he| he.edge)
    }

    /// The half-edge of `e` whose origin is `edge.he`'s origin (`a → b`).
    pub(crate) fn edge_half_edges(
        &self,
        e: EdgeId,
    ) -> Result<(HalfEdgeId, HalfEdgeId), GeometryError> {
        let edge = self.edges.get(e).ok_or_else(|| not_found(e))?;
        Ok((edge.he, self.twin_of(edge.he)?))
    }

    /// The vertex closest to `p` within `tol.length` (ties: first in slot
    /// order). A linear scan; a per-mesh grid replaces it once importers need
    /// it (docs/03 Decisions).
    pub(crate) fn find_vertex_near(&self, p: DVec3, tol: &Tolerances) -> Option<VertexId> {
        let mut best: Option<(f64, VertexId)> = None;
        for (v, vert) in &self.vertices {
            let d2 = vert.position.distance_squared(p);
            if d2 <= tol.length_sq && best.is_none_or(|(bd, _)| d2 < bd) {
                best = Some((d2, v));
            }
        }
        best.map(|(_, v)| v)
    }

    /// Loops of a face, outer first.
    pub(crate) fn face_loops(&self, f: FaceId) -> Result<Vec<LoopId>, GeometryError> {
        let face = self.face_ref(f)?;
        let mut out = Vec::with_capacity(1 + face.holes.len());
        out.push(face.outer);
        out.extend(face.holes.iter().copied());
        Ok(out)
    }

    /// Half-edge cycles of a face, outer first.
    pub(crate) fn face_cycles(&self, f: FaceId) -> Result<Vec<Vec<HalfEdgeId>>, GeometryError> {
        self.face_loops(f)?
            .into_iter()
            .map(|l| self.loop_half_edges(l))
            .collect()
    }

    /// Vertex positions of a loop in cycle order.
    pub(crate) fn loop_positions(&self, l: LoopId) -> Result<Vec<DVec3>, GeometryError> {
        self.loop_vertices(l)?
            .into_iter()
            .map(|v| self.position(v))
            .collect()
    }

    /// Loop positions in a plane frame.
    pub(crate) fn loop_points_2d(
        &self,
        l: LoopId,
        frame: &PlaneFrame,
    ) -> Result<Vec<DVec2>, GeometryError> {
        Ok(frame.project_all(&self.loop_positions(l)?))
    }

    /// Origin positions of a half-edge cycle.
    pub(crate) fn cycle_positions(
        &self,
        cycle: &[HalfEdgeId],
    ) -> Result<Vec<DVec3>, GeometryError> {
        cycle
            .iter()
            .map(|&h| self.position(self.he(h)?.origin))
            .collect()
    }

    /// Bounding box of a face's outer loop (local coordinates).
    pub fn face_aabb(&self, f: FaceId) -> Result<Aabb, GeometryError> {
        let outer = self.face_ref(f)?.outer;
        Ok(Aabb::from_points(self.loop_positions(outer)?))
    }

    /// Runs `op` as one atomic operation: on failure the mesh is restored to
    /// its state on entry and no delta reaches `sink`; on success the deltas
    /// are forwarded in application order (docs/03 §4 "apply-or-rollback").
    ///
    /// The draft snapshots the whole mesh; replaying the inverse deltas is the
    /// planned optimization (docs/03 Decisions).
    pub(crate) fn transact<R>(
        &mut self,
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
        op: impl FnOnce(&mut Self, &mut Vec<MeshDelta>) -> Result<R, GeometryError>,
    ) -> Result<R, GeometryError> {
        let backup = self.clone();
        let mut local: Vec<MeshDelta> = Vec::new();
        let outcome = op(self, &mut local).and_then(|r| self.debug_check(tol).map(|()| r));
        match outcome {
            Ok(r) => {
                for delta in local {
                    sink.record(delta);
                }
                Ok(r)
            }
            Err(err) => {
                *self = backup;
                Err(err)
            }
        }
    }

    /// Test builds validate the structural invariants after every
    /// transactional operation.
    #[cfg(test)]
    fn debug_check(&self, tol: &Tolerances) -> Result<(), GeometryError> {
        let violations = self.validate_inner(tol, false);
        if violations.is_empty() {
            Ok(())
        } else {
            Err(broken(format!(
                "post-operation check failed: {violations:?}"
            )))
        }
    }

    #[cfg(not(test))]
    fn debug_check(&self, _tol: &Tolerances) -> Result<(), GeometryError> {
        Ok(())
    }

    // ----- vertices -------------------------------------------------------------

    /// Inserts a vertex without merging (callers checked for neighbors).
    pub(crate) fn new_vertex(&mut self, p: DVec3, d: &mut dyn DeltaSink) -> VertexId {
        let v = self.vertices.insert(Vertex { position: p });
        self.vertex_edges.insert(v, Vec::new());
        d.record(MeshDelta::VertexAdded { id: v, position: p });
        v
    }

    /// docs/03 §4.1: the vertex within tolerance of `p`, or a new one.
    pub(crate) fn find_or_add_vertex(
        &mut self,
        p: DVec3,
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
    ) -> Result<VertexId, GeometryError> {
        if !tol.in_range(p) {
            return Err(GeometryError::OutOfRange);
        }
        Ok(match self.find_vertex_near(p, tol) {
            Some(v) => v,
            None => self.new_vertex(p, d),
        })
    }

    /// Removes an isolated vertex.
    pub(crate) fn delete_vertex(
        &mut self,
        v: VertexId,
        d: &mut dyn DeltaSink,
    ) -> Result<(), GeometryError> {
        let position = self.position(v)?;
        if !self.vertex_star(v).is_empty() {
            return Err(broken(format!("{v} still has edges")));
        }
        self.vertices.remove(v);
        self.vertex_edges.remove(v);
        d.record(MeshDelta::VertexRemoved { id: v, position });
        Ok(())
    }

    /// Removes `v` when it exists and has no edges; `true` when removed.
    pub(crate) fn delete_if_isolated(
        &mut self,
        v: VertexId,
        d: &mut dyn DeltaSink,
    ) -> Result<bool, GeometryError> {
        if self.vertices.contains_key(v) && self.vertex_star(v).is_empty() {
            self.delete_vertex(v, d)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Sets a position without recording (batch moves record one
    /// `VerticesMoved`).
    pub(crate) fn set_position_raw(&mut self, v: VertexId, p: DVec3) -> Result<(), GeometryError> {
        let vert = self.vertices.get_mut(v).ok_or_else(|| not_found(v))?;
        vert.position = p;
        Ok(())
    }

    // ----- edges ----------------------------------------------------------------

    /// Creates a free edge `a → b` (no faces). Fails if the edge exists.
    pub(crate) fn new_edge(
        &mut self,
        a: VertexId,
        b: VertexId,
        flags: EdgeFlags,
        d: &mut dyn DeltaSink,
    ) -> Result<EdgeId, GeometryError> {
        let pa = self.position(a)?;
        let pb = self.position(b)?;
        if a == b {
            return Err(GeometryError::Degenerate("edge endpoints coincide".into()));
        }
        if !self.vertex_edges.contains_key(a) || !self.vertex_edges.contains_key(b) {
            return Err(broken("vertex without a star"));
        }
        if self.half_edge_from_to(a, b).is_some() {
            return Err(broken(format!("an edge {a}–{b} already exists")));
        }
        let e = self.edges.insert(Edge {
            he: HalfEdgeId::null(),
            flags,
            curve: None,
        });
        let h = self.half_edges.insert(HalfEdge {
            origin: a,
            twin: HalfEdgeId::null(),
            next: HalfEdgeId::null(),
            prev: HalfEdgeId::null(),
            edge: e,
            face: None,
        });
        let t = self.half_edges.insert(HalfEdge {
            origin: b,
            twin: h,
            next: h,
            prev: h,
            edge: e,
            face: None,
        });
        let he = self.he_mut(h)?;
        he.twin = t;
        he.next = t;
        he.prev = t;
        if let Some(edge) = self.edges.get_mut(e) {
            edge.he = h;
        }
        if let Some(star) = self.vertex_edges.get_mut(a) {
            star.push(h);
        }
        if let Some(star) = self.vertex_edges.get_mut(b) {
            star.push(t);
        }
        d.record(MeshDelta::EdgeAdded {
            id: e,
            a: pa,
            b: pb,
            flags,
            curve: None,
        });
        Ok(e)
    }

    /// Removes an edge that borders no face; drops its curve first.
    pub(crate) fn delete_edge(
        &mut self,
        e: EdgeId,
        d: &mut dyn DeltaSink,
    ) -> Result<(), GeometryError> {
        let (h, t) = self.edge_half_edges(e)?;
        if self.he(h)?.face.is_some() || self.he(t)?.face.is_some() {
            return Err(broken(format!("{e} still borders a face")));
        }
        if let Some(c) = self.edges.get(e).and_then(|edge| edge.curve) {
            self.drop_curve_inner(c, d)?;
        }
        let a = self.he(h)?.origin;
        let b = self.he(t)?.origin;
        let (pa, pb) = (self.position(a)?, self.position(b)?);
        let flags = self.edges.get(e).map(|edge| edge.flags).unwrap_or_default();
        if let Some(star) = self.vertex_edges.get_mut(a) {
            star.retain(|&x| x != h);
        }
        if let Some(star) = self.vertex_edges.get_mut(b) {
            star.retain(|&x| x != t);
        }
        self.half_edges.remove(h);
        self.half_edges.remove(t);
        self.edges.remove(e);
        d.record(MeshDelta::EdgeRemoved {
            id: e,
            a: pa,
            b: pb,
            flags,
            curve: None,
        });
        Ok(())
    }

    /// Splits edge `e` at `p` (strictly inside it, reusing a vertex already
    /// at `p`). Faces on either side keep their ids and gain the new vertex.
    /// Returns the split vertex and the two pieces `[a → m, m → b]` where
    /// `a → b` is the direction of `edge.he`.
    pub(crate) fn split_edge_at(
        &mut self,
        e: EdgeId,
        p: DVec3,
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
    ) -> Result<(VertexId, [EdgeId; 2]), GeometryError> {
        let (h, t) = self.edge_half_edges(e)?;
        let flags = self.edges.get(e).map(|edge| edge.flags).unwrap_or_default();
        let a = self.he(h)?.origin;
        let b = self.he(t)?.origin;
        let seg = Segment::new(self.position(a)?, self.position(b)?);
        if !tol.point_inside_segment(p, &seg) {
            return Err(broken(format!("split point is not inside {e}")));
        }
        let existing = self.find_vertex_near(p, tol);

        let mut faces: Vec<FaceId> = Vec::new();
        for side in [h, t] {
            if let Some(f) = self.he(side)?.face
                && !faces.contains(&f)
            {
                faces.push(f);
            }
        }
        faces.sort();
        let mut saved = Vec::with_capacity(faces.len());
        for f in faces {
            let cycles = self.face_cycles(f)?;
            let face = self.face_ref(f)?;
            let (plane, sides) = (face.plane, face.sides.clone());
            self.detach_face(f, d)?;
            saved.push((f, cycles, plane, sides));
        }

        self.delete_edge(e, d)?;
        let m = match existing {
            Some(v) => v,
            None => self.new_vertex(p, d),
        };
        let e1 = self.new_edge(a, m, flags, d)?;
        let e2 = self.new_edge(m, b, flags, d)?;
        let (h1, t1) = self.edge_half_edges(e1)?;
        let (h2, t2) = self.edge_half_edges(e2)?;

        for (f, cycles, plane, sides) in saved {
            let cycles: Vec<Vec<HalfEdgeId>> = cycles
                .into_iter()
                .map(|cycle| {
                    cycle
                        .into_iter()
                        .flat_map(|x| {
                            if x == h {
                                vec![h1, h2]
                            } else if x == t {
                                vec![t2, t1]
                            } else {
                                vec![x]
                            }
                        })
                        .collect()
                })
                .collect();
            let (outer, holes) = cycles
                .split_first()
                .ok_or_else(|| broken(format!("{f} had no loops")))?;
            self.attach_face(f, outer, holes, plane, sides, tol, d)?;
        }
        Ok((m, [e1, e2]))
    }

    // ----- faces ----------------------------------------------------------------

    /// Checks that `cycle` is a closed chain of existing half-edges that border
    /// no face and are not used twice.
    fn check_cycle(
        &self,
        cycle: &[HalfEdgeId],
        seen: &mut BTreeSet<HalfEdgeId>,
    ) -> Result<(), GeometryError> {
        if cycle.len() < 3 {
            return Err(GeometryError::Degenerate(
                "a face loop needs at least three edges".into(),
            ));
        }
        for (i, &h) in cycle.iter().enumerate() {
            let he = self.he(h)?;
            if let Some(f) = he.face {
                return Err(broken(format!("{h} already borders {f}")));
            }
            if !seen.insert(h) {
                return Err(broken(format!("{h} appears twice in the face loops")));
            }
            let next = cycle[(i + 1) % cycle.len()];
            if self.dest(h)? != self.he(next)?.origin {
                return Err(broken("face loop is not a closed chain"));
            }
        }
        Ok(())
    }

    /// Planarity and orientation of face loops against `plane` (docs/03 §3.1
    /// invariant 2): outer counter-clockwise seen from the front, holes
    /// clockwise, every vertex within `planarity`, no zero-area loop.
    fn check_face_geometry(
        &self,
        outer: &[HalfEdgeId],
        holes: &[Vec<HalfEdgeId>],
        plane: &Plane,
        tol: &Tolerances,
    ) -> Result<(), GeometryError> {
        if (plane.normal.length() - 1.0).abs() > 1e-9 {
            return Err(GeometryError::Degenerate(
                "face plane normal is not a unit vector".into(),
            ));
        }
        let frame = plane.frame();
        let check_loop = |cycle: &[HalfEdgeId], want_ccw: bool| -> Result<(), GeometryError> {
            let points = self.cycle_positions(cycle)?;
            if !tol.points_on_plane(plane, &points) {
                return Err(GeometryError::NotApplicable(
                    "face loop is not planar within tolerance".into(),
                ));
            }
            let area = polygon::signed_area(&frame.project_all(&points));
            if area.abs() <= tol.length_sq {
                return Err(GeometryError::Degenerate("face loop has zero area".into()));
            }
            if (area > 0.0) != want_ccw {
                return Err(broken(if want_ccw {
                    "outer loop must be counter-clockwise seen from the front"
                } else {
                    "hole loops must be clockwise seen from the front"
                }));
            }
            Ok(())
        };
        check_loop(outer, true)?;
        for hole in holes {
            check_loop(hole, false)?;
        }
        Ok(())
    }

    /// Wires a validated cycle as a loop of face `f`.
    fn wire_loop(&mut self, f: FaceId, cycle: &[HalfEdgeId]) -> Result<LoopId, GeometryError> {
        let n = cycle.len();
        for i in 0..n {
            let next = cycle[(i + 1) % n];
            let prev = cycle[(i + n - 1) % n];
            let he = self.he_mut(cycle[i])?;
            he.next = next;
            he.prev = prev;
            he.face = Some(f);
        }
        Ok(self.loops.insert(Loop {
            start: cycle[0],
            face: f,
        }))
    }

    /// Gives the detached face `f` new loops, plane and sides, recording
    /// `FaceAdded { id: f }`. Validates before mutating.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn attach_face(
        &mut self,
        f: FaceId,
        outer: &[HalfEdgeId],
        holes: &[Vec<HalfEdgeId>],
        plane: Plane,
        sides: FaceSides,
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
    ) -> Result<(), GeometryError> {
        match self.faces.get(f) {
            Some(face) if face.outer.is_null() => {}
            Some(_) => return Err(broken(format!("{f} is already attached"))),
            None => return Err(not_found(f)),
        }
        let mut seen = BTreeSet::new();
        self.check_cycle(outer, &mut seen)?;
        for hole in holes {
            self.check_cycle(hole, &mut seen)?;
        }
        self.check_face_geometry(outer, holes, &plane, tol)?;
        let outer_loop = self.wire_loop(f, outer)?;
        let mut hole_loops = Vec::with_capacity(holes.len());
        for hole in holes {
            hole_loops.push(self.wire_loop(f, hole)?);
        }
        let face = self.faces.get_mut(f).ok_or_else(|| not_found(f))?;
        face.outer = outer_loop;
        face.holes = hole_loops;
        face.plane = plane;
        face.sides = sides;
        let snapshot = self.face_snapshot(f)?;
        d.record(MeshDelta::FaceAdded { id: f, snapshot });
        Ok(())
    }

    /// Creates a new face from free half-edge cycles (atomic).
    pub(crate) fn create_face(
        &mut self,
        outer: &[HalfEdgeId],
        holes: &[Vec<HalfEdgeId>],
        plane: Plane,
        sides: FaceSides,
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
    ) -> Result<FaceId, GeometryError> {
        let f = self.faces.insert(Face {
            outer: LoopId::null(),
            holes: Vec::new(),
            plane,
            sides: FaceSides::default(),
        });
        if let Err(err) = self.attach_face(f, outer, holes, plane, sides, tol, d) {
            self.faces.remove(f);
            return Err(err);
        }
        Ok(f)
    }

    /// Releases a face's half-edges and loops, recording `FaceRemoved`. The
    /// face slot stays (detached) so the face can be re-attached with the same
    /// id; [`Self::discard_detached_face`] drops it instead.
    pub(crate) fn detach_face(
        &mut self,
        f: FaceId,
        d: &mut dyn DeltaSink,
    ) -> Result<FaceSnapshot, GeometryError> {
        let snapshot = self.face_snapshot(f)?;
        for l in self.face_loops(f)? {
            for h in self.loop_half_edges(l)? {
                let twin = self.twin_of(h)?;
                let he = self.he_mut(h)?;
                he.face = None;
                he.next = twin;
                he.prev = twin;
            }
            self.loops.remove(l);
        }
        if let Some(face) = self.faces.get_mut(f) {
            face.outer = LoopId::null();
            face.holes.clear();
        }
        d.record(MeshDelta::FaceRemoved {
            id: f,
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    /// Drops the slot of a detached face (its removal was already recorded).
    pub(crate) fn discard_detached_face(&mut self, f: FaceId) -> Result<(), GeometryError> {
        match self.faces.get(f) {
            Some(face) if face.outer.is_null() => {
                self.faces.remove(f);
                Ok(())
            }
            Some(_) => Err(broken(format!("{f} is attached"))),
            None => Err(not_found(f)),
        }
    }

    /// Detaches and drops a face.
    pub(crate) fn remove_face(
        &mut self,
        f: FaceId,
        d: &mut dyn DeltaSink,
    ) -> Result<FaceSnapshot, GeometryError> {
        let snapshot = self.detach_face(f, d)?;
        self.discard_detached_face(f)?;
        Ok(snapshot)
    }

    // ----- curves ---------------------------------------------------------------

    /// docs/03 §3.3 `add_curve`; `soften` marks inter-segment edges.
    pub(crate) fn add_curve_inner(
        &mut self,
        descriptor: CurveDescriptor,
        members: &[EdgeId],
        soften: bool,
        d: &mut dyn DeltaSink,
    ) -> Result<CurveId, GeometryError> {
        if members.is_empty() {
            return Err(GeometryError::Degenerate(
                "a curve needs at least one edge".into(),
            ));
        }
        let mut seen = BTreeSet::new();
        for (i, &e) in members.iter().enumerate() {
            let edge = self.edges.get(e).ok_or_else(|| not_found(e))?;
            if edge.curve.is_some() {
                return Err(GeometryError::NotApplicable(format!(
                    "{e} already belongs to a curve"
                )));
            }
            if !seen.insert(e) {
                return Err(GeometryError::NotApplicable(format!("{e} is listed twice")));
            }
            if i > 0 {
                let (a0, b0) = self.edge_endpoints(members[i - 1])?;
                let (a1, b1) = self.edge_endpoints(e)?;
                if a0 != a1 && a0 != b1 && b0 != a1 && b0 != b1 {
                    return Err(GeometryError::NotApplicable(
                        "curve members must be consecutive edges".into(),
                    ));
                }
            }
        }
        let c = self.curves.insert(descriptor.clone());
        for &e in members {
            if let Some(edge) = self.edges.get_mut(e) {
                edge.curve = Some(c);
            }
        }
        self.curve_members.insert(c, members.to_vec());
        d.record(MeshDelta::CurveAdded {
            id: c,
            descriptor,
            members: members.to_vec(),
        });
        if soften {
            self.soften_curve_junctions(c, d)?;
        }
        Ok(c)
    }

    /// Marks `SOFT | SMOOTH` the non-member edges at the curve's inner
    /// vertices that separate two faces both containing a member edge.
    fn soften_curve_junctions(
        &mut self,
        c: CurveId,
        d: &mut dyn DeltaSink,
    ) -> Result<(), GeometryError> {
        let members = self.curve_members.get(c).cloned().unwrap_or_default();
        let member_set: BTreeSet<EdgeId> = members.iter().copied().collect();
        let n = members.len();
        let mut junctions = BTreeSet::new();
        for i in 0..n {
            if i + 1 == n && n < 3 {
                break;
            }
            let (a0, b0) = self.edge_endpoints(members[i])?;
            let (a1, b1) = self.edge_endpoints(members[(i + 1) % n])?;
            for v in [a0, b0] {
                if v == a1 || v == b1 {
                    junctions.insert(v);
                }
            }
        }
        let touches_curve = |mesh: &Self, f: FaceId| -> Result<bool, GeometryError> {
            Ok(mesh.face_edges(f)?.iter().any(|e| member_set.contains(e)))
        };
        for v in junctions {
            for h in self.vertex_star(v).to_vec() {
                let e = self.he(h)?.edge;
                if member_set.contains(&e) {
                    continue;
                }
                let [Some(fa), Some(fb)] = self.edge_faces(e)? else {
                    continue;
                };
                if fa == fb || !touches_curve(self, fa)? || !touches_curve(self, fb)? {
                    continue;
                }
                let flags = self.edges.get(e).map(|x| x.flags).unwrap_or_default();
                self.set_edge_flags(e, flags | EdgeFlags::SOFT | EdgeFlags::SMOOTH, d)?;
            }
        }
        Ok(())
    }

    /// docs/03 §3.3 `drop_curve`.
    pub(crate) fn drop_curve_inner(
        &mut self,
        c: CurveId,
        d: &mut dyn DeltaSink,
    ) -> Result<(), GeometryError> {
        let descriptor = self.curves.remove(c).ok_or_else(|| not_found(c))?;
        let members = self.curve_members.remove(c).unwrap_or_default();
        for &e in &members {
            if let Some(edge) = self.edges.get_mut(e)
                && edge.curve == Some(c)
            {
                edge.curve = None;
            }
        }
        d.record(MeshDelta::CurveRemoved {
            id: c,
            descriptor,
            members,
        });
        Ok(())
    }

    /// Faces coplanar with `plane` whose bounding box meets `bbox` (a linear
    /// scan with cheap rejections; docs/03 Decisions).
    pub(crate) fn coplanar_faces_near(
        &self,
        plane: &Plane,
        bbox: &Aabb,
        tol: &Tolerances,
    ) -> Result<Vec<FaceId>, GeometryError> {
        let mut out = Vec::new();
        for (f, face) in &self.faces {
            if face.outer.is_null() || !tol.is_coplanar(&face.plane, plane) {
                continue;
            }
            if self.face_aabb(f)?.intersects(bbox) {
                out.push(f);
            }
        }
        Ok(out)
    }

    /// Edges whose bounding box meets `bbox` (linear scan).
    pub(crate) fn edges_near(&self, bbox: &Aabb) -> Result<Vec<EdgeId>, GeometryError> {
        let mut out = Vec::new();
        for (e, _) in &self.edges {
            if self.edge_segment(e)?.aabb().intersects(bbox) {
                out.push(e);
            }
        }
        Ok(out)
    }

    /// Runs face finding (docs/03 §4.3) for the given touched vertices, where
    /// only regions bounded by a `seeds` edge may become new faces.
    pub(crate) fn find_faces_seeded(
        &mut self,
        touched: &[VertexId],
        seeds: &[EdgeId],
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
    ) -> Result<planar::FaceFindingResult, GeometryError> {
        planar::find_faces_seeded(self, touched, seeds, tol, d)
    }
}

/// Id ↔ local index maps produced by [`HalfEdgeMesh::to_indexed`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexedIds {
    /// `vertices[i]` is the id of local vertex `i`.
    pub vertices: Vec<VertexId>,
    /// `edges[i]` is the id of local edge `i`.
    pub edges: Vec<EdgeId>,
    /// `faces[i]` is the id of local face `i`.
    pub faces: Vec<FaceId>,
    /// `curves[i]` is the id of local curve `i`.
    pub curves: Vec<CurveId>,
}
