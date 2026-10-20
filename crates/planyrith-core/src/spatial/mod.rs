//! Implements docs/03-geometry-kernel.md §6 (spatial indexes): per-container
//! BVHs over vertices, edges and faces, a per-document BVH over instance
//! occurrences, and the ray / segment / AABB / nearest queries used by picking
//! (docs/06 §8) and inference (docs/03 §7). Trees are
//! `parry3d_f64::partitioning::Bvh`; leaves are keyed by the slot index of the
//! element id so incremental updates from [`MeshDelta`]s address leaves without
//! a lookup table (docs/01 §5 "spatial indexes update incrementally").
//!
//! Ordering (docs/03 §7, docs/dev/skeleton-contract.md §2): ray hits are sorted
//! by `t`; hits whose depths differ by no more than the proximity tolerance are
//! ordered by kind priority (vertex > edge > face), then `t`, then id, so a
//! vertex within tolerance beats an edge within tolerance beats a face
//! (docs/06 §8). Every other query returns ids in ascending order.
//!
//! Owner key: `spatial`.

#![warn(dead_code, unused_variables)]

mod convert;
mod geometry;
mod provider;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use glam::{DAffine3, DVec3};
use parry3d_f64::partitioning::{Bvh, BvhBuildStrategy};
use serde::{Deserialize, Serialize};
use slotmap::Key;

use crate::commands::transaction::Delta;
use crate::document::{ContainerRef, Document, EntityKey, EntityPath, InstancePath};
use crate::geom::{EdgeFlags, HalfEdgeMesh, MeshDelta};
use crate::ids::{EdgeId, EntityId, FaceId, VertexId};
use crate::math::{Aabb, Ray, Segment};
use crate::tolerance::Tolerances;

pub use provider::{InstanceProvider, Occurrence};

use convert::{from_parry_aabb, ray_aabb_entry, to_parry_aabb};
use geometry::{
    FacePolygon, ray_plane, ray_point_closest, ray_segment_closest, segment_closest_params,
};

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

impl HitKind {
    /// Picking priority: `0` for vertices, `1` for edges, `2` for faces (lower
    /// wins, docs/06 §8).
    pub const fn priority(self) -> u8 {
        match self {
            HitKind::Vertex(_) => 0,
            HitKind::Edge(_) => 1,
            HitKind::Face(_) => 2,
        }
    }

    /// The entity hit, `None` for vertices (which are not entities, docs/04 §2).
    pub const fn entity(self) -> Option<EntityId> {
        match self {
            HitKind::Vertex(_) => None,
            HitKind::Edge(e) => Some(EntityId::Edge(e)),
            HitKind::Face(f) => Some(EntityId::Face(f)),
        }
    }

    /// `(priority, raw id)` sort key: priority first, then id (docs/03 §7).
    fn rank(self) -> (u8, u64) {
        let raw = match self {
            HitKind::Vertex(v) => v.data().as_ffi(),
            HitKind::Edge(e) => e.data().as_ffi(),
            HitKind::Face(f) => f.data().as_ffi(),
        };
        (self.priority(), raw)
    }
}

/// A ray hit on an entity occurrence (docs/03 §6).
///
/// For a vertex hit `path.target` names the lowest-id edge incident to the
/// vertex (vertices are not entities and cannot be path targets); isolated
/// vertices are therefore not reported at document level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    /// Occurrence that was hit.
    pub path: EntityPath,
    /// Element kind.
    pub kind: HitKind,
    /// Ray parameter (world units).
    pub t: f64,
    /// World-space hit point: the ray–plane intersection for faces, the closest
    /// point *on the element* for edges and vertices (what inference snaps to).
    pub point: DVec3,
}

/// A hit in a container's local frame (before path/world mapping).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalHit {
    /// Element kind.
    pub kind: HitKind,
    /// Ray parameter in the local ray's units.
    pub t: f64,
    /// Local-space hit point (see [`Hit::point`]).
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
    /// Contact point, taken on the existing edge (the mesh geometry is the
    /// authority for where a split lands, docs/03 §4.2 step 2).
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
    /// by the caller, docs/03 §7). Values below `tol.length` act as `tol.length`.
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

impl HitFilter {
    /// Effective proximity: never below the merge tolerance.
    fn effective_proximity(&self, tol: &Tolerances) -> f64 {
        self.proximity.max(tol.length)
    }

    /// `max_t` as a plain bound.
    fn max_t_bound(&self) -> f64 {
        self.max_t.unwrap_or(f64::INFINITY)
    }
}

/// Leaf index of a slot-map key: its slot index (unique among live elements).
fn leaf_of<K: Key>(key: K) -> u32 {
    // `as_ffi()` packs `(version << 32) | idx`; the mask keeps the index.
    (key.data().as_ffi() & u64::from(u32::MAX)) as u32
}

/// Sorts hits by `t`; within runs of hits whose depths differ by at most
/// `proximity`, orders by kind priority, then `t`, then id (docs/03 §7).
fn order_local_hits(hits: &mut [LocalHit], proximity: f64) {
    hits.sort_by(|a, b| {
        a.t.total_cmp(&b.t)
            .then_with(|| a.kind.rank().cmp(&b.kind.rank()))
    });
    let mut start = 0;
    while start < hits.len() {
        let t0 = hits[start].t;
        let mut end = start + 1;
        while end < hits.len() && hits[end].t - t0 <= proximity {
            end += 1;
        }
        hits[start..end].sort_by(|a, b| {
            a.kind
                .rank()
                .cmp(&b.kind.rank())
                .then_with(|| a.t.total_cmp(&b.t))
        });
        start = end;
    }
}

/// Document-level counterpart of [`order_local_hits`]; the path is the last
/// tie-breaker.
fn order_hits(hits: &mut [Hit], proximity: f64) {
    let by_rank = |a: &Hit, b: &Hit| -> Ordering {
        a.kind
            .rank()
            .cmp(&b.kind.rank())
            .then_with(|| a.path.cmp(&b.path))
    };
    hits.sort_by(|a, b| a.t.total_cmp(&b.t).then_with(|| by_rank(a, b)));
    let mut start = 0;
    while start < hits.len() {
        let t0 = hits[start].t;
        let mut end = start + 1;
        while end < hits.len() && hits[end].t - t0 <= proximity {
            end += 1;
        }
        hits[start..end].sort_by(|a, b| by_rank(a, b).then_with(|| a.t.total_cmp(&b.t)));
        start = end;
    }
}

/// Per-container index: one BVH over vertex, edge and face AABBs each
/// (docs/03 §6; the vertex tree is an addition that makes the "Endpoint"
/// inference and the merge lookup of docs/03 §4.1 independent of edges).
/// Leaves are keyed by slot index and map back to ids through the `*_leaves`
/// tables.
#[derive(Debug, Clone, Default)]
pub struct ContainerIndex {
    vertex_bvh: Bvh,
    edge_bvh: Bvh,
    face_bvh: Bvh,
    vertex_leaves: BTreeMap<u32, VertexId>,
    edge_leaves: BTreeMap<u32, EdgeId>,
    face_leaves: BTreeMap<u32, FaceId>,
    /// Leaves changed since the last full build (rebuild above 30 %,
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

    /// Builds from a mesh (vertex, edge and face AABBs expanded by `tol.length`).
    pub fn build(mesh: &HalfEdgeMesh, tol: &Tolerances) -> Self {
        let mut vertex_leaves = BTreeMap::new();
        let vertex_bvh = build_bvh(mesh.vertices().map(|(id, v)| {
            let leaf = leaf_of(id);
            vertex_leaves.insert(leaf, id);
            (leaf, vertex_aabb(v.position, tol))
        }));

        let mut edge_leaves = BTreeMap::new();
        let edge_bvh = build_bvh(mesh.edges().filter_map(|(id, _)| {
            let aabb = edge_aabb(mesh, id, tol)?;
            let leaf = leaf_of(id);
            edge_leaves.insert(leaf, id);
            Some((leaf, aabb))
        }));

        let mut face_leaves = BTreeMap::new();
        let face_bvh = build_bvh(mesh.faces().filter_map(|(id, _)| {
            let aabb = face_aabb(mesh, id, tol)?;
            let leaf = leaf_of(id);
            face_leaves.insert(leaf, id);
            Some((leaf, aabb))
        }));

        ContainerIndex {
            vertex_bvh,
            edge_bvh,
            face_bvh,
            vertex_leaves,
            edge_leaves,
            face_leaves,
            changed_since_build: 0,
        }
    }

    /// Incremental update from mesh deltas applied to `mesh` (which is already
    /// in its post-delta state): refits moved/added leaves, removes deleted
    /// ones, and falls back to a full rebuild when more than
    /// [`Self::REBUILD_FRACTION`] of the leaves changed since the last build.
    pub fn apply_deltas(&mut self, mesh: &HalfEdgeMesh, deltas: &[MeshDelta], tol: &Tolerances) {
        if deltas.is_empty() {
            return;
        }
        let mut vertices = BTreeSet::new();
        let mut edges = BTreeSet::new();
        let mut faces = BTreeSet::new();
        for delta in deltas {
            match delta {
                MeshDelta::VertexAdded { id, .. } | MeshDelta::VertexRemoved { id, .. } => {
                    vertices.insert(*id);
                }
                MeshDelta::VertexMoved { id, .. } => {
                    moved_vertex(mesh, *id, &mut vertices, &mut edges, &mut faces);
                }
                MeshDelta::VerticesMoved { moves } => {
                    for (id, _, _) in moves {
                        moved_vertex(mesh, *id, &mut vertices, &mut edges, &mut faces);
                    }
                }
                MeshDelta::EdgeAdded { id, .. } | MeshDelta::EdgeRemoved { id, .. } => {
                    edges.insert(*id);
                }
                MeshDelta::FaceAdded { id, .. } | MeshDelta::FaceRemoved { id, .. } => {
                    faces.insert(*id);
                }
                MeshDelta::FaceSplit { old, new } => {
                    faces.insert(*old);
                    faces.extend(new.iter().copied());
                }
                MeshDelta::FacesMerged { kept, removed } => {
                    faces.insert(*kept);
                    faces.insert(*removed);
                }
                MeshDelta::EdgeFlagsChanged { .. }
                | MeshDelta::FaceSidesChanged { .. }
                | MeshDelta::CurveAdded { .. }
                | MeshDelta::CurveRemoved { .. } => {}
            }
        }

        let changed = vertices.len() + edges.len() + faces.len();
        if changed == 0 {
            return;
        }
        let total = self.vertex_leaves.len() + self.edge_leaves.len() + self.face_leaves.len();
        self.changed_since_build += changed;
        if total == 0 || self.changed_since_build as f64 > Self::REBUILD_FRACTION * total as f64 {
            self.rebuild(mesh, tol);
            return;
        }

        for v in vertices {
            let aabb = mesh.vertex(v).map(|vert| vertex_aabb(vert.position, tol));
            update_leaf(&mut self.vertex_bvh, &mut self.vertex_leaves, v, aabb);
        }
        for e in edges {
            let aabb = edge_aabb(mesh, e, tol);
            update_leaf(&mut self.edge_bvh, &mut self.edge_leaves, e, aabb);
        }
        for f in faces {
            let aabb = face_aabb(mesh, f, tol);
            update_leaf(&mut self.face_bvh, &mut self.face_leaves, f, aabb);
        }
    }

    /// Full rebuild.
    pub fn rebuild(&mut self, mesh: &HalfEdgeMesh, tol: &Tolerances) {
        *self = Self::build(mesh, tol);
    }

    /// Vertices whose (tolerance-expanded) AABB intersects `aabb`, sorted by id.
    pub fn vertices_in_aabb(&self, aabb: &Aabb) -> Vec<VertexId> {
        collect_leaves(&self.vertex_bvh, &self.vertex_leaves, aabb)
    }

    /// Edges whose AABB intersects `aabb`, sorted by id.
    pub fn edges_in_aabb(&self, aabb: &Aabb) -> Vec<EdgeId> {
        collect_leaves(&self.edge_bvh, &self.edge_leaves, aabb)
    }

    /// Faces whose AABB intersects `aabb`, sorted by id.
    pub fn faces_in_aabb(&self, aabb: &Aabb) -> Vec<FaceId> {
        collect_leaves(&self.face_bvh, &self.face_leaves, aabb)
    }

    /// Ray cast in the container's local frame; hits sorted by `t`, with hits
    /// within the proximity tolerance of each other ordered by kind priority
    /// (vertex > edge > face), then id (docs/03 §7 tie-breaking, docs/06 §8).
    ///
    /// Faces are hit exactly (ray–plane then point-in-face); edges and vertices
    /// are hit when their distance to the ray is within the effective proximity
    /// (`max(filter.proximity, tol.length)`). Edges flagged `HIDDEN` are skipped
    /// unless `filter.include_hidden`. Section planes are applied by
    /// [`DocumentIndex`], which knows the container's planes.
    pub fn ray_cast_local(
        &self,
        mesh: &HalfEdgeMesh,
        ray: &Ray,
        filter: &HitFilter,
        tol: &Tolerances,
    ) -> Vec<LocalHit> {
        let mut hits = Vec::new();
        if ray.dir.length_squared() < 0.5 {
            // A zero direction (see `Ray::new`) cannot hit anything.
            return hits;
        }
        let proximity = filter.effective_proximity(tol);
        let max_t = filter.max_t_bound();

        if filter.faces {
            let candidates = self.face_bvh.leaves(|node| {
                ray_aabb_entry(ray, &from_parry_aabb(&node.aabb()), max_t).is_some()
            });
            for leaf in candidates {
                let Some(&id) = self.face_leaves.get(&leaf) else {
                    continue;
                };
                let Some(face) = mesh.face(id) else {
                    continue;
                };
                let Some(t) = ray_plane(ray, &face.plane, tol) else {
                    continue;
                };
                if t > max_t {
                    continue;
                }
                let point = ray.at(t);
                let Ok(polygon) = FacePolygon::of(mesh, id) else {
                    continue;
                };
                if polygon.contains(point, tol) {
                    hits.push(LocalHit {
                        kind: HitKind::Face(id),
                        t,
                        point,
                    });
                }
            }
        }

        if filter.edges {
            let candidates = self.edge_bvh.leaves(|node| {
                let aabb = from_parry_aabb(&node.aabb()).expanded(proximity);
                ray_aabb_entry(ray, &aabb, max_t + proximity).is_some()
            });
            for leaf in candidates {
                let Some(&id) = self.edge_leaves.get(&leaf) else {
                    continue;
                };
                let Some(edge) = mesh.edge(id) else {
                    continue;
                };
                if !filter.include_hidden && edge.flags.contains(EdgeFlags::HIDDEN) {
                    continue;
                }
                let Ok(segment) = mesh.edge_segment(id) else {
                    continue;
                };
                let (t, t_edge, distance) = ray_segment_closest(ray, &segment, max_t, tol);
                if distance <= proximity {
                    hits.push(LocalHit {
                        kind: HitKind::Edge(id),
                        t,
                        point: segment.point_at(t_edge),
                    });
                }
            }
        }

        if filter.vertices {
            let candidates = self.vertex_bvh.leaves(|node| {
                let aabb = from_parry_aabb(&node.aabb()).expanded(proximity);
                ray_aabb_entry(ray, &aabb, max_t + proximity).is_some()
            });
            for leaf in candidates {
                let Some(&id) = self.vertex_leaves.get(&leaf) else {
                    continue;
                };
                let Some(vertex) = mesh.vertex(id) else {
                    continue;
                };
                let (t, distance) = ray_point_closest(ray, vertex.position, max_t);
                if distance <= proximity {
                    hits.push(LocalHit {
                        kind: HitKind::Vertex(id),
                        t,
                        point: vertex.position,
                    });
                }
            }
        }

        order_local_hits(&mut hits, proximity);
        hits
    }

    /// Edges intersecting or touching `segment` within `tol.length`, used by
    /// `add_edge` splitting (docs/03 §4.2 step 2). Sorted by `t_segment`, then
    /// edge id. Contacts at the segment's or the edge's endpoints are included;
    /// the caller decides whether a touch at an existing vertex is a split.
    pub fn segment_intersections(
        &self,
        mesh: &HalfEdgeMesh,
        segment: &Segment,
        tol: &Tolerances,
    ) -> Vec<SegmentHit> {
        let query = segment.aabb().expanded(tol.length);
        let mut hits = Vec::new();
        for id in self.edges_in_aabb(&query) {
            let Ok(edge_segment) = mesh.edge_segment(id) else {
                continue;
            };
            let (t_segment, t_edge) = segment_closest_params(segment, &edge_segment, tol);
            let on_query = segment.point_at(t_segment);
            let on_edge = edge_segment.point_at(t_edge);
            if on_query.distance_squared(on_edge) <= tol.length_sq {
                hits.push(SegmentHit {
                    edge: id,
                    t_segment,
                    t_edge,
                    point: on_edge,
                });
            }
        }
        hits.sort_by(|a, b| {
            a.t_segment
                .total_cmp(&b.t_segment)
                .then_with(|| a.edge.cmp(&b.edge))
        });
        hits
    }

    /// Closest edge to a segment within `max_distance`: `(edge, distance)`.
    /// Exact distance ties resolve to the lower id.
    pub fn nearest_edge_to_segment(
        &self,
        mesh: &HalfEdgeMesh,
        segment: &Segment,
        max_distance: f64,
        tol: &Tolerances,
    ) -> Option<(EdgeId, f64)> {
        let query = segment.aabb().expanded(max_distance.max(0.0) + tol.length);
        let mut best: Option<(f64, EdgeId)> = None;
        for id in self.edges_in_aabb(&query) {
            let Ok(edge_segment) = mesh.edge_segment(id) else {
                continue;
            };
            let (s, t) = segment_closest_params(segment, &edge_segment, tol);
            let distance = segment.point_at(s).distance(edge_segment.point_at(t));
            if distance <= max_distance {
                let candidate = (distance, id);
                if best.is_none_or(|b| {
                    candidate.0.total_cmp(&b.0).then(candidate.1.cmp(&b.1)) == Ordering::Less
                }) {
                    best = Some(candidate);
                }
            }
        }
        best.map(|(distance, id)| (id, distance))
    }

    /// Closest vertex to `p` within `max_distance`: `(vertex, distance)`
    /// (docs/03 §4.1 merge lookup). Exact distance ties resolve to the lower id.
    pub fn nearest_vertex(
        &self,
        mesh: &HalfEdgeMesh,
        p: DVec3,
        max_distance: f64,
    ) -> Option<(VertexId, f64)> {
        let query = Aabb::from_point(p).expanded(max_distance.max(0.0));
        let mut best: Option<(f64, VertexId)> = None;
        for id in self.vertices_in_aabb(&query) {
            let Some(vertex) = mesh.vertex(id) else {
                continue;
            };
            let distance = vertex.position.distance(p);
            if distance <= max_distance {
                let candidate = (distance, id);
                if best.is_none_or(|b| {
                    candidate.0.total_cmp(&b.0).then(candidate.1.cmp(&b.1)) == Ordering::Less
                }) {
                    best = Some(candidate);
                }
            }
        }
        best.map(|(distance, id)| (id, distance))
    }

    /// Number of indexed vertices.
    pub fn vertex_count(&self) -> usize {
        self.vertex_leaves.len()
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

/// Builds a tree from `(leaf index, box)` pairs. Leaf indices are arbitrary
/// (slot indices here); `Bvh::from_iter` assumes dense `0..n` indices for one
/// or two leaves, so those trees are grown by insertion instead.
fn build_bvh(leaves: impl Iterator<Item = (u32, Aabb)>) -> Bvh {
    let leaves: Vec<(u32, Aabb)> = leaves.collect();
    if leaves.len() <= 2 {
        let mut bvh = Bvh::new();
        for (leaf, aabb) in leaves {
            bvh.insert(to_parry_aabb(&aabb), leaf);
        }
        return bvh;
    }
    Bvh::from_iter(
        BvhBuildStrategy::Binned,
        leaves
            .into_iter()
            .map(|(leaf, aabb)| (leaf as usize, to_parry_aabb(&aabb))),
    )
}

/// Leaf box of a vertex.
fn vertex_aabb(position: DVec3, tol: &Tolerances) -> Aabb {
    Aabb::from_point(position).expanded(tol.length)
}

/// Leaf box of an edge, `None` when the edge is gone.
fn edge_aabb(mesh: &HalfEdgeMesh, id: EdgeId, tol: &Tolerances) -> Option<Aabb> {
    mesh.edge_segment(id)
        .ok()
        .map(|s| s.aabb().expanded(tol.length))
}

/// Leaf box of a face (outer loop; holes lie inside it), `None` when gone.
fn face_aabb(mesh: &HalfEdgeMesh, id: FaceId, tol: &Tolerances) -> Option<Aabb> {
    let face = mesh.face(id)?;
    let points = geometry::loop_positions(mesh, face.outer).ok()?;
    let aabb = Aabb::from_points(points);
    (!aabb.is_empty()).then(|| aabb.expanded(tol.length))
}

/// Records a moved vertex and every edge and face around it as changed.
fn moved_vertex(
    mesh: &HalfEdgeMesh,
    id: VertexId,
    vertices: &mut BTreeSet<VertexId>,
    edges: &mut BTreeSet<EdgeId>,
    faces: &mut BTreeSet<FaceId>,
) {
    vertices.insert(id);
    for he in mesh.vertex_star(id) {
        let Some(h) = mesh.half_edge(*he) else {
            continue;
        };
        edges.insert(h.edge);
        faces.extend(h.face);
        if let Some(twin) = mesh.half_edge(h.twin) {
            faces.extend(twin.face);
        }
    }
}

/// Inserts/updates the leaf of `id` when `aabb` is `Some`, removes it otherwise.
fn update_leaf<K: Key + Ord>(
    bvh: &mut Bvh,
    leaves: &mut BTreeMap<u32, K>,
    id: K,
    aabb: Option<Aabb>,
) {
    let leaf = leaf_of(id);
    match aabb {
        Some(aabb) => {
            bvh.insert(to_parry_aabb(&aabb), leaf);
            leaves.insert(leaf, id);
        }
        None => {
            if leaves.get(&leaf) == Some(&id) {
                bvh.remove(leaf);
                leaves.remove(&leaf);
            }
        }
    }
}

/// Ids of the leaves intersecting `aabb`, ascending and deduplicated.
fn collect_leaves<K: Key + Ord>(bvh: &Bvh, leaves: &BTreeMap<u32, K>, aabb: &Aabb) -> Vec<K> {
    if aabb.is_empty() {
        return Vec::new();
    }
    let query = to_parry_aabb(aabb);
    let mut out: Vec<K> = bvh
        .intersect_aabb(&query)
        .filter_map(|leaf| leaves.get(&leaf).copied())
        .collect();
    out.sort();
    out.dedup();
    out
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
    /// World-space bounds (tolerance-expanded).
    pub aabb: Aabb,
    /// Hidden through a hidden step or tag (skipped unless
    /// [`HitFilter::include_hidden`]).
    pub hidden: bool,
}

/// Per-document index: a BVH over instance world AABBs for broad-phase picking
/// plus one [`ContainerIndex`] per container (docs/03 §6). Built from any
/// [`InstanceProvider`]; the [`Document`] methods are thin wrappers.
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
        Self::build_from(doc)
    }

    /// Builds everything from any scene provider.
    pub fn build_from<P: InstanceProvider + ?Sized>(provider: &P) -> Self {
        let mut index = Self::new();
        index.rebuild_occurrences(provider);
        index
    }

    /// Incremental update from committed deltas (docs/01 §5): mesh deltas refit
    /// the affected container indexes; the instance tree is rebuilt from the
    /// provider (its leaf count is small next to the meshes).
    pub fn apply_deltas(&mut self, doc: &Document, deltas: &[Delta]) {
        self.apply_deltas_from(doc, deltas);
    }

    /// [`Self::apply_deltas`] for any scene provider.
    pub fn apply_deltas_from<P: InstanceProvider + ?Sized>(
        &mut self,
        provider: &P,
        deltas: &[Delta],
    ) {
        let tol = *provider.tolerances();
        let mut by_container: BTreeMap<ContainerRef, Vec<MeshDelta>> = BTreeMap::new();
        for delta in deltas {
            if let Delta::Mesh { container, delta } = delta {
                by_container
                    .entry(*container)
                    .or_default()
                    .push(delta.clone());
            }
        }
        for (container, mesh_deltas) in by_container {
            let Some(mesh) = provider.mesh(container) else {
                self.containers.remove(&container);
                continue;
            };
            match self.containers.get_mut(&container) {
                Some(index) => index.apply_deltas(mesh, &mesh_deltas, &tol),
                None => {
                    self.containers
                        .insert(container, ContainerIndex::build(mesh, &tol));
                }
            }
        }
        self.rebuild_occurrences(provider);
    }

    /// Re-enumerates occurrences, rebuilds the instance BVH, builds indexes for
    /// new containers and drops indexes of containers no longer referenced.
    fn rebuild_occurrences<P: InstanceProvider + ?Sized>(&mut self, provider: &P) {
        let tol = *provider.tolerances();
        let occurrences = provider.occurrences();
        let referenced: BTreeSet<ContainerRef> = occurrences.iter().map(|o| o.container).collect();
        self.containers.retain(|c, _| referenced.contains(c));
        for container in &referenced {
            if self.containers.contains_key(container) {
                continue;
            }
            if let Some(mesh) = provider.mesh(*container) {
                self.containers
                    .insert(*container, ContainerIndex::build(mesh, &tol));
            }
        }

        self.leaves.clear();
        for occurrence in occurrences {
            let Some(mesh) = provider.mesh(occurrence.container) else {
                continue;
            };
            let local = mesh.aabb();
            if local.is_empty() {
                continue;
            }
            let aabb = local.transformed(&occurrence.world).expanded(tol.length);
            self.leaves.push(InstanceLeaf {
                path: occurrence.path,
                container: occurrence.container,
                world: occurrence.world,
                aabb,
                hidden: occurrence.hidden,
            });
        }
        self.instance_bvh = build_bvh(
            self.leaves
                .iter()
                .enumerate()
                .map(|(i, leaf)| (i as u32, leaf.aabb)),
        );
    }

    /// The index of one container.
    pub fn container(&self, c: ContainerRef) -> Option<&ContainerIndex> {
        self.containers.get(&c)
    }

    /// Occurrences whose world AABB intersects `aabb`, in occurrence order.
    pub fn aabb_query(&self, aabb: &Aabb) -> Vec<&InstanceLeaf> {
        if aabb.is_empty() {
            return Vec::new();
        }
        let query = to_parry_aabb(aabb);
        let mut indices: Vec<u32> = self.instance_bvh.intersect_aabb(&query).collect();
        indices.sort_unstable();
        indices.dedup();
        indices
            .into_iter()
            .filter_map(|i| self.leaves.get(i as usize))
            .collect()
    }

    /// World-space ray cast through all occurrences (broad phase on instances,
    /// narrow phase in each container's local frame); sorted by `t` with the
    /// tolerance grouping of [`ContainerIndex::ray_cast_local`].
    pub fn ray_cast(&self, doc: &Document, ray: &Ray, filter: &HitFilter) -> Vec<Hit> {
        self.ray_cast_from(doc, ray, filter)
    }

    /// [`Self::ray_cast`] for any scene provider.
    pub fn ray_cast_from<P: InstanceProvider + ?Sized>(
        &self,
        provider: &P,
        ray: &Ray,
        filter: &HitFilter,
    ) -> Vec<Hit> {
        let tol = provider.tolerances();
        let mut hits = Vec::new();
        if ray.dir.length_squared() < 0.5 {
            return hits;
        }
        let proximity = filter.effective_proximity(tol);
        let max_t = filter.max_t_bound();

        let mut candidates: Vec<u32> = self
            .instance_bvh
            .leaves(|node| {
                let aabb = from_parry_aabb(&node.aabb()).expanded(proximity);
                ray_aabb_entry(ray, &aabb, max_t + proximity).is_some()
            })
            .collect();
        candidates.sort_unstable();
        candidates.dedup();

        for index in candidates {
            let Some(leaf) = self.leaves.get(index as usize) else {
                continue;
            };
            if leaf.hidden && !filter.include_hidden {
                continue;
            }
            let (Some(container_index), Some(mesh)) = (
                self.containers.get(&leaf.container),
                provider.mesh(leaf.container),
            ) else {
                continue;
            };
            let Some(local) = LocalFrame::new(&leaf.world, ray) else {
                continue;
            };
            let local_filter = HitFilter {
                proximity: proximity * local.distance_scale,
                max_t: filter.max_t.map(|m| m * local.param_scale),
                ..*filter
            };
            let local_hits = container_index.ray_cast_local(mesh, &local.ray, &local_filter, tol);
            if local_hits.is_empty() {
                continue;
            }
            let sections = if filter.respect_sections {
                provider.active_section_planes(leaf.container)
            } else {
                Vec::new()
            };

            for hit in local_hits {
                if sections
                    .iter()
                    .any(|plane| plane.signed_distance(hit.point) > tol.length)
                {
                    continue;
                }
                let target = match hit.kind {
                    HitKind::Vertex(v) => {
                        let Some(edge) = lowest_incident_edge(mesh, v) else {
                            continue;
                        };
                        EntityId::Edge(edge)
                    }
                    HitKind::Edge(e) => EntityId::Edge(e),
                    HitKind::Face(f) => EntityId::Face(f),
                };
                if !filter.include_hidden && provider.is_entity_hidden(leaf.container, target) {
                    continue;
                }
                let point = leaf.world.transform_point3(hit.point);
                let t = ray.closest_param(point);
                if t > max_t {
                    continue;
                }
                // The local tolerance was conservative; confirm in world units.
                if hit.kind.priority() < 2 && ray.at(t).distance(point) > proximity {
                    continue;
                }
                hits.push(Hit {
                    path: EntityPath::new(leaf.path.clone(), EntityKey::Id(target)),
                    kind: hit.kind,
                    t,
                    point,
                });
            }
        }

        order_hits(&mut hits, proximity);
        hits
    }

    /// Number of instance occurrences indexed (occurrences of empty containers
    /// are not indexed).
    pub fn occurrence_count(&self) -> usize {
        self.leaves.len()
    }
}

/// A world ray expressed in a container's local frame.
struct LocalFrame {
    /// The local ray (unit direction).
    ray: Ray,
    /// Local parameter per world parameter (`|M⁻¹ · dir|`).
    param_scale: f64,
    /// Conservative local distance per world distance (Frobenius norm of the
    /// inverse linear part, an upper bound of its operator norm).
    distance_scale: f64,
}

impl LocalFrame {
    /// `None` when the transform is singular or non-finite.
    fn new(world: &DAffine3, ray: &Ray) -> Option<Self> {
        let inverse = world.inverse();
        let origin = inverse.transform_point3(ray.origin);
        let dir = inverse.transform_vector3(ray.dir);
        let param_scale = dir.length();
        let m = inverse.matrix3;
        let distance_scale =
            (m.x_axis.length_squared() + m.y_axis.length_squared() + m.z_axis.length_squared())
                .sqrt();
        if !origin.is_finite()
            || !dir.is_finite()
            || param_scale.is_nan()
            || param_scale <= f64::MIN_POSITIVE
            || !distance_scale.is_finite()
        {
            return None;
        }
        Some(LocalFrame {
            ray: Ray::new(origin, dir),
            param_scale,
            distance_scale,
        })
    }
}

/// The lowest-id edge incident to `v`, `None` for an isolated vertex.
fn lowest_incident_edge(mesh: &HalfEdgeMesh, v: VertexId) -> Option<EdgeId> {
    mesh.vertex_star(v)
        .iter()
        .filter_map(|he| mesh.half_edge(*he).map(|h| h.edge))
        .min()
}
