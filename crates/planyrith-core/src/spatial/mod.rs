//! Implements docs/03-geometry-kernel.md §6 (spatial indexes): per-container
//! BVHs over edges and faces, a per-document BVH over instance occurrences, and
//! the ray / segment / AABB queries used by picking (docs/06 §8) and inference
//! (docs/03 §7). Trees are `parry3d_f64::partitioning::Bvh`.
//!
//! Owner key: `spatial`.

use std::collections::BTreeMap;

use glam::{DAffine3, DVec3};
use parry3d_f64::partitioning::Bvh;
use serde::{Deserialize, Serialize};

use crate::commands::transaction::Delta;
use crate::document::{ContainerRef, Document, EntityPath, InstancePath};
use crate::geom::{HalfEdgeMesh, MeshDelta};
use crate::ids::{EdgeId, FaceId, VertexId};
use crate::math::{Aabb, Ray, Segment};
use crate::tolerance::Tolerances;

/// What a ray hit (docs/03 §6 `Hit::kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HitKind {
    /// A vertex (within the pixel tolerance of the ray).
    Vertex(VertexId),
    /// An edge.
    Edge(EdgeId),
    /// A face interior.
    Face(FaceId),
}

/// A ray hit on an entity occurrence (docs/03 §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    /// Occurrence that was hit.
    pub path: EntityPath,
    /// Element kind.
    pub kind: HitKind,
    /// Ray parameter (world units).
    pub t: f64,
    /// World-space hit point.
    pub point: DVec3,
}

/// A hit in a container's local frame (before path/world mapping).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalHit {
    /// Element kind.
    pub kind: HitKind,
    /// Ray parameter in the local ray's units.
    pub t: f64,
    /// Local-space hit point.
    pub point: DVec3,
}

/// Segment–edge contact reported by [`ContainerIndex::segment_intersections`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentHit {
    /// The edge.
    pub edge: EdgeId,
    /// Parameter on the query segment.
    pub t_segment: f64,
    /// Parameter on the edge.
    pub t_edge: f64,
    /// Contact point.
    pub point: DVec3,
}

/// Filters for ray casts (docs/06 §8 picking rules).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HitFilter {
    /// Report vertices.
    pub vertices: bool,
    /// Report edges.
    pub edges: bool,
    /// Report faces.
    pub faces: bool,
    /// Include hidden entities and hidden tags.
    pub include_hidden: bool,
    /// Ignore hits behind active section planes.
    pub respect_sections: bool,
    /// World-space tolerance for vertex/edge proximity (converted from pixels
    /// by the caller, docs/03 §7).
    pub proximity: f64,
    /// Maximum ray parameter.
    pub max_t: Option<f64>,
}

impl Default for HitFilter {
    fn default() -> Self {
        HitFilter {
            vertices: true,
            edges: true,
            faces: true,
            include_hidden: false,
            respect_sections: true,
            proximity: 0.0,
            max_t: None,
        }
    }
}

/// Per-container index: one BVH over edge AABBs and one over face AABBs
/// (docs/03 §6). Leaf indices map through `edge_leaves` / `face_leaves`.
#[derive(Debug, Clone, Default)]
pub struct ContainerIndex {
    edge_bvh: Bvh,
    face_bvh: Bvh,
    edge_leaves: Vec<EdgeId>,
    face_leaves: Vec<FaceId>,
    /// Fraction of leaves changed since the last full build (rebuild above 30 %,
    /// docs/01 §5).
    changed_since_build: usize,
}

impl ContainerIndex {
    /// Changed-leaf fraction above which `apply_deltas` rebuilds (docs/01 §5).
    pub const REBUILD_FRACTION: f64 = 0.3;

    /// Empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds from a mesh (edge and face AABBs expanded by `tol.length`).
    pub fn build(mesh: &HalfEdgeMesh, tol: &Tolerances) -> Self {
        todo!("docs/03 §6 build container index")
    }

    /// Incremental update from mesh deltas: refit touched leaves, insert/remove,
    /// full rebuild when more than [`Self::REBUILD_FRACTION`] changed.
    pub fn apply_deltas(&mut self, mesh: &HalfEdgeMesh, deltas: &[MeshDelta], tol: &Tolerances) {
        todo!("docs/03 §6 incremental refit")
    }

    /// Full rebuild.
    pub fn rebuild(&mut self, mesh: &HalfEdgeMesh, tol: &Tolerances) {
        *self = Self::build(mesh, tol);
    }

    /// Edges whose AABB intersects `aabb`, sorted by id.
    pub fn edges_in_aabb(&self, aabb: &Aabb) -> Vec<EdgeId> {
        todo!("docs/03 §6 aabb_query edges")
    }

    /// Faces whose AABB intersects `aabb`, sorted by id.
    pub fn faces_in_aabb(&self, aabb: &Aabb) -> Vec<FaceId> {
        todo!("docs/03 §6 aabb_query faces")
    }

    /// Ray cast in the container's local frame; hits sorted by `t`, then kind
    /// priority (vertex > edge > face), then id (docs/03 §7 tie-breaking).
    pub fn ray_cast_local(
        &self,
        mesh: &HalfEdgeMesh,
        ray: &Ray,
        filter: &HitFilter,
        tol: &Tolerances,
    ) -> Vec<LocalHit> {
        todo!("docs/03 §6 ray_cast")
    }

    /// Edges intersecting or touching `segment` within `tol.length`, used by
    /// `add_edge` splitting (docs/03 §4.2 step 2).
    pub fn segment_intersections(
        &self,
        mesh: &HalfEdgeMesh,
        segment: &Segment,
        tol: &Tolerances,
    ) -> Vec<SegmentHit> {
        todo!("docs/03 §6 segment_intersections")
    }

    /// Closest edge to a segment within `max_distance`: `(edge, distance)`.
    pub fn nearest_edge_to_segment(
        &self,
        mesh: &HalfEdgeMesh,
        segment: &Segment,
        max_distance: f64,
        tol: &Tolerances,
    ) -> Option<(EdgeId, f64)> {
        todo!("docs/03 §6 nearest_edge_to_segment")
    }

    /// Closest vertex to `p` within `max_distance`: `(vertex, distance)`
    /// (docs/03 §4.1 merge lookup).
    pub fn nearest_vertex(
        &self,
        mesh: &HalfEdgeMesh,
        p: DVec3,
        max_distance: f64,
    ) -> Option<(VertexId, f64)> {
        todo!("docs/03 §4.1 nearest vertex")
    }

    /// Number of indexed edges.
    pub fn edge_count(&self) -> usize {
        self.edge_leaves.len()
    }

    /// Number of indexed faces.
    pub fn face_count(&self) -> usize {
        self.face_leaves.len()
    }
}

/// One instance occurrence in the document index.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceLeaf {
    /// Path to the occurrence.
    pub path: InstancePath,
    /// Container it opens.
    pub container: ContainerRef,
    /// Container-to-world transform.
    pub world: DAffine3,
    /// World-space bounds.
    pub aabb: Aabb,
}

/// Per-document index: a BVH over instance world AABBs for broad-phase picking
/// plus one [`ContainerIndex`] per container (docs/03 §6).
#[derive(Debug, Clone, Default)]
pub struct DocumentIndex {
    instance_bvh: Bvh,
    leaves: Vec<InstanceLeaf>,
    containers: BTreeMap<ContainerRef, ContainerIndex>,
}

impl DocumentIndex {
    /// Empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds everything from the document.
    pub fn build(doc: &Document) -> Self {
        todo!("docs/03 §6 build document index")
    }

    /// Incremental update from committed deltas (docs/01 §5).
    pub fn apply_deltas(&mut self, doc: &Document, deltas: &[Delta]) {
        todo!("docs/03 §6 incremental document index update")
    }

    /// The index of one container.
    pub fn container(&self, c: ContainerRef) -> Option<&ContainerIndex> {
        self.containers.get(&c)
    }

    /// Occurrences whose world AABB intersects `aabb`.
    pub fn aabb_query(&self, aabb: &Aabb) -> Vec<&InstanceLeaf> {
        todo!("docs/03 §6 instance aabb_query")
    }

    /// World-space ray cast through all occurrences (broad phase on instances,
    /// narrow phase in each container's local frame); sorted by `t`.
    pub fn ray_cast(&self, doc: &Document, ray: &Ray, filter: &HitFilter) -> Vec<Hit> {
        todo!("docs/03 §6 document ray_cast")
    }

    /// Number of instance occurrences indexed.
    pub fn occurrence_count(&self) -> usize {
        self.leaves.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_indexes() {
        let c = ContainerIndex::new();
        assert_eq!(c.edge_count() + c.face_count(), 0);
        let d = DocumentIndex::new();
        assert_eq!(d.occurrence_count(), 0);
        assert!(d.container(ContainerRef::Model).is_none());
        assert_eq!(HitFilter::default().proximity, 0.0);
    }
}
