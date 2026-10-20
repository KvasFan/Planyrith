//! Implements docs/01-architecture.md §3.1 (undo/redo by replaying recorded
//! deltas) for the half-edge mesh of docs/03-geometry-kernel.md §4.
//!
//! Vertices, edges and faces are located by **position** (docs/03 Decisions:
//! invariant 3 makes a position a stable identity). Deltas that only name an
//! id (`EdgeFlagsChanged`, `FaceSidesChanged`, curve deltas) resolve it through
//! [`ReplayIds`], which remembers which id a replay re-created for each
//! recorded id.

use std::collections::BTreeMap;

use glam::DVec3;

use super::{HalfEdgeMesh, broken, not_found};
use crate::geom::{GeometryError, MeshDelta, NoDeltas};
use crate::ids::{CurveId, EdgeId, FaceId, HalfEdgeId, VertexId};
use crate::tolerance::Tolerances;

/// Equivalence classes of ids that denote the same element across replays.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IdAliases<K: Ord + Copy> {
    /// Any id ever seen → the first (recorded) id of its class.
    canonical: BTreeMap<K, K>,
    /// Class → the id that currently denotes it in the mesh.
    current: BTreeMap<K, K>,
}

impl<K: Ord + Copy> Default for IdAliases<K> {
    fn default() -> Self {
        IdAliases {
            canonical: BTreeMap::new(),
            current: BTreeMap::new(),
        }
    }
}

impl<K: Ord + Copy> IdAliases<K> {
    fn resolve(&self, id: K) -> K {
        let class = self.canonical.get(&id).copied().unwrap_or(id);
        self.current.get(&class).copied().unwrap_or(id)
    }

    fn record(&mut self, recorded: K, created: K) {
        let class = self.canonical.get(&recorded).copied().unwrap_or(recorded);
        self.current.insert(class, created);
        self.canonical.insert(created, class);
    }
}

/// Id aliases for [`HalfEdgeMesh::replay`]: slot-map ids cannot be re-created
/// at a chosen index, so a replayed `EdgeAdded { id }` creates a new id; later
/// deltas that name the recorded id (or any earlier alias of it) resolve to the
/// new one. Keep one instance per container across undo/redo cycles, or seed
/// it from the pid bindings (docs/04 §2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayIds {
    edges: IdAliases<EdgeId>,
    faces: IdAliases<FaceId>,
    curves: IdAliases<CurveId>,
}

impl ReplayIds {
    /// No aliases.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current id of the edge recorded as `e`.
    pub fn edge(&self, e: EdgeId) -> EdgeId {
        self.edges.resolve(e)
    }

    /// The current id of the face recorded as `f`.
    pub fn face(&self, f: FaceId) -> FaceId {
        self.faces.resolve(f)
    }

    /// The current id of the curve recorded as `c`.
    pub fn curve(&self, c: CurveId) -> CurveId {
        self.curves.resolve(c)
    }

    /// Declares that `created` now denotes the edge recorded as `recorded`.
    pub fn alias_edge(&mut self, recorded: EdgeId, created: EdgeId) {
        self.edges.record(recorded, created);
    }

    /// Declares that `created` now denotes the face recorded as `recorded`.
    pub fn alias_face(&mut self, recorded: FaceId, created: FaceId) {
        self.faces.record(recorded, created);
    }

    /// Declares that `created` now denotes the curve recorded as `recorded`.
    pub fn alias_curve(&mut self, recorded: CurveId, created: CurveId) {
        self.curves.record(recorded, created);
    }
}

impl HalfEdgeMesh {
    /// Applies one recorded delta without recording anything (docs/01 §3.1
    /// replay). Undo applies `delta.inverse()` for a transaction's deltas in
    /// reverse order; redo applies the deltas in order. Composite markers are
    /// no-ops. Not atomic; see [`HalfEdgeMesh::replay_all`].
    pub fn replay(
        &mut self,
        delta: &MeshDelta,
        ids: &mut ReplayIds,
        tol: &Tolerances,
    ) -> Result<(), GeometryError> {
        let d = &mut NoDeltas;
        match delta {
            MeshDelta::VertexAdded { position, .. } => {
                if self.find_vertex_near(*position, tol).is_some() {
                    return Err(broken("replayed vertex coincides with an existing one"));
                }
                self.new_vertex(*position, d);
            }
            MeshDelta::VertexRemoved { position, .. } => {
                let v = self.vertex_at(*position, tol)?;
                self.delete_vertex(v, d)?;
            }
            MeshDelta::VertexMoved { from, to, .. } => {
                let v = self.vertex_at(*from, tol)?;
                self.set_position_raw(v, *to)?;
            }
            MeshDelta::VerticesMoved { moves } => {
                let resolved = moves
                    .iter()
                    .map(|(_, from, to)| Ok((self.vertex_at(*from, tol)?, *to)))
                    .collect::<Result<Vec<(VertexId, DVec3)>, GeometryError>>()?;
                for (v, to) in resolved {
                    self.set_position_raw(v, to)?;
                }
            }
            MeshDelta::EdgeAdded {
                id, a, b, flags, ..
            } => {
                let va = self.vertex_at(*a, tol)?;
                let vb = self.vertex_at(*b, tol)?;
                let e = self.new_edge(va, vb, *flags, d)?;
                ids.alias_edge(*id, e);
            }
            MeshDelta::EdgeRemoved { a, b, .. } => {
                let va = self.vertex_at(*a, tol)?;
                let vb = self.vertex_at(*b, tol)?;
                let e = self
                    .edge_between(va, vb)
                    .ok_or_else(|| not_found(format!("edge {a:?}–{b:?}")))?;
                self.delete_edge(e, d)?;
            }
            MeshDelta::EdgeFlagsChanged { id, to, .. } => {
                self.set_edge_flags(ids.edge(*id), *to, d)?;
            }
            MeshDelta::FaceAdded { id, snapshot } => {
                let outer = self.cycle_at(&snapshot.outer, tol)?;
                let holes = snapshot
                    .holes
                    .iter()
                    .map(|h| self.cycle_at(h, tol))
                    .collect::<Result<Vec<_>, _>>()?;
                let f = self.create_face(
                    &outer,
                    &holes,
                    snapshot.plane,
                    snapshot.sides.clone(),
                    tol,
                    d,
                )?;
                ids.alias_face(*id, f);
            }
            MeshDelta::FaceRemoved { snapshot, .. } => {
                let outer = self.cycle_at(&snapshot.outer, tol)?;
                let first = outer
                    .first()
                    .copied()
                    .ok_or_else(|| broken("empty face snapshot"))?;
                let f = self
                    .he(first)?
                    .face
                    .ok_or_else(|| not_found("face of a replayed snapshot"))?;
                self.remove_face(f, d)?;
            }
            MeshDelta::FaceSidesChanged { id, to, .. } => {
                self.set_face_sides(ids.face(*id), to.clone(), d)?;
            }
            MeshDelta::FaceSplit { .. } | MeshDelta::FacesMerged { .. } => {}
            MeshDelta::CurveAdded {
                id,
                descriptor,
                members,
            } => {
                let members: Vec<EdgeId> = members.iter().map(|&e| ids.edge(e)).collect();
                let c = self.add_curve_inner(descriptor.clone(), &members, false, d)?;
                ids.alias_curve(*id, c);
            }
            MeshDelta::CurveRemoved { id, .. } => {
                self.drop_curve_inner(ids.curve(*id), d)?;
            }
        }
        Ok(())
    }

    /// Replays a sequence atomically: on failure the mesh is restored and the
    /// error returned.
    pub fn replay_all<'a>(
        &mut self,
        deltas: impl IntoIterator<Item = &'a MeshDelta>,
        ids: &mut ReplayIds,
        tol: &Tolerances,
    ) -> Result<(), GeometryError> {
        let backup = self.clone();
        let backup_ids = ids.clone();
        for delta in deltas {
            if let Err(err) = self.replay(delta, ids, tol) {
                *self = backup;
                *ids = backup_ids;
                return Err(err);
            }
        }
        Ok(())
    }

    /// The vertex at a recorded position.
    fn vertex_at(&self, p: DVec3, tol: &Tolerances) -> Result<VertexId, GeometryError> {
        self.find_vertex_near(p, tol)
            .ok_or_else(|| not_found(format!("vertex at {p:?}")))
    }

    /// The half-edge cycle through recorded loop positions.
    fn cycle_at(
        &self,
        points: &[DVec3],
        tol: &Tolerances,
    ) -> Result<Vec<HalfEdgeId>, GeometryError> {
        let vertices = points
            .iter()
            .map(|&p| self.vertex_at(p, tol))
            .collect::<Result<Vec<_>, _>>()?;
        let n = vertices.len();
        (0..n)
            .map(|i| {
                self.half_edge_from_to(vertices[i], vertices[(i + 1) % n])
                    .ok_or_else(|| not_found("half-edge of a replayed loop"))
            })
            .collect()
    }
}
