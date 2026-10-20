//! Implements docs/03-geometry-kernel.md §3 (the half-edge mesh), §4.1–§4.8
//! (fundamental operations) and the indexed form of docs/09 §2 used by the
//! native file format (`from_indexed` / `to_indexed`) and importers
//! (`from_triangles_merged`, docs/09 §6).
//!
//! Owner key: `mesh` (docs/dev/skeleton-contract.md).

use glam::{DAffine3, DVec3};
use serde::{Deserialize, Serialize};
use slotmap::{SecondaryMap, SlotMap};

use crate::geom::{DeltaSink, FaceSides, FaceSnapshot, GeometryError};
use crate::ids::{CurveId, EdgeId, FaceId, HalfEdgeId, LoopId, VertexId};
use crate::math::{Aabb, Plane, Segment};
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
        todo!("docs/03 §3.1 edge faces")
    }

    /// Half-edges of a loop in cycle order starting at `Loop::start`.
    pub fn loop_half_edges(&self, l: LoopId) -> Result<Vec<HalfEdgeId>, GeometryError> {
        todo!("docs/03 §3.1 loop walk")
    }

    /// Vertices of a loop in cycle order.
    pub fn loop_vertices(&self, l: LoopId) -> Result<Vec<VertexId>, GeometryError> {
        todo!("docs/03 §3.1 loop vertices")
    }

    /// Edges of a loop in cycle order.
    pub fn loop_edges(&self, l: LoopId) -> Result<Vec<EdgeId>, GeometryError> {
        todo!("docs/03 §3.1 loop edges")
    }

    /// Outer-loop vertices of a face in counter-clockwise order.
    pub fn face_vertices(&self, f: FaceId) -> Result<Vec<VertexId>, GeometryError> {
        todo!("docs/03 §3.1 face vertices")
    }

    /// All edges bounding a face (outer loop then holes).
    pub fn face_edges(&self, f: FaceId) -> Result<Vec<EdgeId>, GeometryError> {
        todo!("docs/03 §3.1 face edges")
    }

    /// Faces adjacent to `f` across its boundary edges (deduplicated, ordered by id).
    pub fn face_neighbors(&self, f: FaceId) -> Result<Vec<FaceId>, GeometryError> {
        todo!("docs/03 §4.9 coplanar adjacency")
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
        todo!("docs/03 §2 face area (degeneracy threshold length²)")
    }

    /// Snapshot of a face for deltas and undo.
    pub fn face_snapshot(&self, f: FaceId) -> Result<FaceSnapshot, GeometryError> {
        todo!("docs/03 §4 face snapshot")
    }

    /// Bounding box of all vertices (local coordinates).
    pub fn aabb(&self) -> Aabb {
        Aabb::from_points(self.vertices.values().map(|v| v.position))
    }

    /// Edges that belong to curve `c`, in curve order.
    pub fn curve_edges(&self, c: CurveId) -> Result<Vec<EdgeId>, GeometryError> {
        todo!("docs/03 §3.3 curve members")
    }

    /// `true` while all member edges exist and no member edge is used by a face
    /// other than the two the curve was created with (docs/03 §3.3).
    pub fn curve_is_intact(&self, c: CurveId) -> Result<bool, GeometryError> {
        todo!("docs/03 §3.3 curve intactness")
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
        todo!("docs/03 §4.1 add_vertex")
    }

    /// Adds a straight edge, splitting against existing geometry and running
    /// face finding on every touched vertex (docs/03 §4.2). `a ≈ b` is
    /// `Degenerate`.
    pub fn add_edge(
        &mut self,
        a: DVec3,
        b: DVec3,
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<AddEdgeResult, GeometryError> {
        todo!("docs/03 §4.2 add_edge")
    }

    /// Erases an edge with the face merge/delete rules of docs/03 §4.6 and
    /// removes vertices that became isolated.
    pub fn erase_edge(
        &mut self,
        e: EdgeId,
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<EraseEdgeResult, GeometryError> {
        todo!("docs/03 §4.6 erase_edge")
    }

    /// Changes flags only (the `erase_edge_keep_faces` variant of docs/03 §4.6
    /// used by Soften/Smooth/Hide).
    pub fn set_edge_flags(
        &mut self,
        e: EdgeId,
        flags: EdgeFlags,
        sink: &mut dyn DeltaSink,
    ) -> Result<(), GeometryError> {
        todo!("docs/03 §4.6 flag-only variant")
    }

    /// Deletes a face; its edges stay unless `erase_orphans` (docs/03 §4.7).
    pub fn erase_face(
        &mut self,
        f: FaceId,
        erase_orphans: bool,
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<EraseFaceResult, GeometryError> {
        todo!("docs/03 §4.7 erase_face")
    }

    /// Replaces a face's materials/UVs.
    pub fn set_face_sides(
        &mut self,
        f: FaceId,
        sides: FaceSides,
        sink: &mut dyn DeltaSink,
    ) -> Result<(), GeometryError> {
        todo!("docs/03 §3.1 face sides")
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
        todo!("docs/03 §4.8 transform_vertices")
    }

    /// Splits face `f` by a chord (a chain of existing interior edges whose
    /// endpoints lie on the face boundary); returns the two new faces with
    /// `[0]` inheriting the pid (docs/03 §4.4).
    pub fn split_face_by_chord(
        &mut self,
        f: FaceId,
        chord: &[EdgeId],
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<[FaceId; 2], GeometryError> {
        todo!("docs/03 §4.4 split_face_by_chord")
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
        todo!("docs/03 §4.6 merge_faces")
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
        todo!("docs/03 §4.3 face creation from loops")
    }

    /// Attaches a curve descriptor to consecutive edges (docs/03 §3.3); marks
    /// inter-segment edges `SOFT | SMOOTH`.
    pub fn add_curve(
        &mut self,
        descriptor: CurveDescriptor,
        members: &[EdgeId],
        sink: &mut dyn DeltaSink,
    ) -> Result<CurveId, GeometryError> {
        todo!("docs/03 §3.3 add_curve")
    }

    /// Drops a descriptor; member edges become plain (docs/03 §3.3).
    pub fn drop_curve(
        &mut self,
        c: CurveId,
        sink: &mut dyn DeltaSink,
    ) -> Result<(), GeometryError> {
        todo!("docs/03 §3.3 drop_curve")
    }

    // ----- analysis -----------------------------------------------------------

    /// Checks every invariant of docs/03 §3.1; empty on success.
    pub fn validate(&self, tol: &Tolerances) -> Vec<MeshViolation> {
        todo!("docs/03 §3.1 validate")
    }

    /// `true` when the mesh is a closed, consistently oriented 2-manifold with
    /// no wire edges (docs/03 §4.13).
    pub fn is_solid(&self) -> bool {
        todo!("docs/03 §4.13 is_solid")
    }

    /// Signed volume in mm³ for a solid (`NotApplicable` otherwise).
    pub fn volume(&self) -> Result<f64, GeometryError> {
        todo!("docs/03 §4.13 solid volume")
    }

    // ----- bulk conversion ---------------------------------------------------

    /// Rebuilds the half-edge structure from the indexed form (docs/09 §2),
    /// recomputing missing planes and validating invariants.
    pub fn from_indexed(indexed: &IndexedMesh, tol: &Tolerances) -> Result<Self, GeometryError> {
        todo!("docs/09 §2 HalfEdgeMesh::from_indexed")
    }

    /// Converts to the indexed form. Vertex, edge and face order follow slot
    /// order; the returned maps give the local index of every id so the
    /// container can attach pids (docs/09 §2).
    pub fn to_indexed(&self) -> (IndexedMesh, IndexedIds) {
        todo!("docs/09 §2 HalfEdgeMesh::to_indexed")
    }

    /// Importer fast path: builds a mesh from triangles, merging coplanar
    /// neighbors and softening edges below `opts.soften_angle` (docs/09 §6).
    pub fn from_triangles_merged(
        positions: &[DVec3],
        triangles: &[[u32; 3]],
        opts: TriangleMergeOptions,
        tol: &Tolerances,
    ) -> Result<TriangleMergeResult, GeometryError> {
        todo!("docs/09 §6 from_triangles_merged")
    }

    /// Transforms every vertex (bulk, no merging; used by explode/make-unique
    /// when re-parenting whole containers).
    pub fn transform_all(&mut self, t: &DAffine3) {
        for v in self.vertices.values_mut() {
            v.position = t.transform_point3(v.position);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_flags_set_operations() {
        let f = EdgeFlags::SOFT | EdgeFlags::SMOOTH;
        assert!(f.contains(EdgeFlags::SOFT));
        assert!(!f.contains(EdgeFlags::HIDDEN));
        assert_eq!(f.with(EdgeFlags::HIDDEN, true).bits(), 0b0111);
        assert_eq!(f.difference(EdgeFlags::SOFT), EdgeFlags::SMOOTH);
        assert_eq!(EdgeFlags::default(), EdgeFlags::NONE);
    }

    #[test]
    fn directed_edge_index_round_trips() {
        let d = DirectedEdgeIndex::reversed(42);
        assert!(d.is_reversed());
        assert_eq!(d.index(), 42);
        assert!(!DirectedEdgeIndex::forward(42).is_reversed());
    }

    #[test]
    fn empty_mesh_accessors() {
        let mesh = HalfEdgeMesh::new();
        assert!(mesh.is_empty());
        assert_eq!(
            mesh.vertex_count() + mesh.edge_count() + mesh.face_count(),
            0
        );
        assert!(mesh.aabb().is_empty());
        assert!(mesh.vertex_star(VertexId::default()).is_empty());
        assert!(matches!(
            mesh.edge_endpoints(EdgeId::default()),
            Err(GeometryError::NotFound(_))
        ));
    }

    #[test]
    fn mesh_serializes_in_named_field_mode() {
        let mesh = HalfEdgeMesh::new();
        let bytes = rmp_serde::to_vec_named(&mesh).unwrap();
        let back: HalfEdgeMesh = rmp_serde::from_slice(&bytes).unwrap();
        assert!(back.is_empty());
    }
}
