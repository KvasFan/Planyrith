//! Helpers shared by the editing and organization commands of
//! docs/05-modeling-tools.md §4–§5: resolving a `Sel` into the entities of
//! the edit context, copy-on-edit of shared group definitions (docs/04 §3.1)
//! and removing groups with their private definitions.

#![warn(unused_variables, dead_code)]

use std::collections::BTreeSet;

use glam::DVec3;

use std::collections::BTreeMap;

use crate::commands::params::{Target, select};
use crate::commands::transaction::AttrTarget;
use crate::commands::{CommandError, Transaction};
use crate::document::{
    AttributeDictionaries, ContainerRef, Document, Entities, EntityRecord, EntityView, Group,
    HierarchyError, InstancePath, Query,
};
use crate::geom::mesh::{DirectedEdgeIndex, IndexedEdge, IndexedFace, IndexedMesh};
use crate::geom::{EdgeFlags, FaceSides, HalfEdgeMesh, planar};
use crate::ids::{CurveId, DefinitionId, EdgeId, EntityId, FaceId, Pid, TagId, VertexId};
use crate::math::Plane;
use crate::tolerance::Tolerances;

/// A face to re-create by [`redraw_geometry`]: outer loop points, the plane
/// whose normal gives the front side, and the materials.
pub(crate) type FaceCopy = (Vec<DVec3>, Plane, FaceSides);

/// Draws `segments` into `container` with `add_edge` (faces form by face
/// finding), then gives every face of `faces` that this call created its
/// front side and materials; a face flips only together with its
/// edge-connected group, and only when that group is new too (existing
/// geometry is never re-oriented or repainted). One atomic kernel step; pids
/// of the new elements are appended to `tx.created()`.
pub(crate) fn redraw_geometry(
    tx: &mut Transaction<'_>,
    container: ContainerRef,
    segments: &[(DVec3, DVec3, EdgeFlags)],
    faces: &[FaceCopy],
) -> Result<(), CommandError> {
    if segments.is_empty() {
        return Ok(());
    }
    tx.with_mesh(container, |mesh, tol, d| {
        mesh.transact(tol, d, |m, log| {
            let existing: BTreeSet<FaceId> = m.faces().map(|(f, _)| f).collect();
            for (a, b, flags) in segments {
                let r = m.add_edge_inner(*a, *b, tol, log)?;
                if flags.bits() == 0 {
                    continue;
                }
                for e in r.edges {
                    let old = m.edge(e).map(|x| x.flags).unwrap_or_default();
                    if old.bits() == 0 {
                        m.set_edge_flags(e, *flags, log)?;
                    }
                }
            }
            // Faces that existed before keep their orientation and sides: a
            // copy that lands on existing geometry must not change it.
            let created: BTreeSet<FaceId> = m
                .faces()
                .map(|(f, _)| f)
                .filter(|f| !existing.contains(f))
                .collect();
            let mut reversed: BTreeSet<FaceId> = BTreeSet::new();
            for (outer, plane, sides) in faces {
                let Some(f) = face_through(m, outer, tol) else {
                    continue;
                };
                if !created.contains(&f) {
                    continue;
                }
                if m.face_normal(f)?.dot(plane.normal) < 0.0 && !reversed.contains(&f) {
                    let group = planar::face_group(m, &BTreeSet::from([f]))?;
                    if group.iter().all(|g| created.contains(g)) {
                        planar::reverse_faces(m, &group, tol, log)?;
                        reversed.extend(group);
                    }
                }
                m.set_face_sides(f, sides.clone(), log)?;
            }
            Ok(())
        })
    })
}

/// The organization rows that travel with an entity when it is moved to or
/// copied into another container (docs/05 §4.4, §5.1–§5.3): tag, hidden,
/// locked and attributes.
#[derive(Debug, Clone, Default)]
pub(crate) struct SideRows {
    pub(crate) tag: Option<TagId>,
    pub(crate) hidden: bool,
    pub(crate) locked: bool,
    pub(crate) attrs: Option<AttributeDictionaries>,
}

impl SideRows {
    /// The rows of `id` in `entities`.
    pub(crate) fn of(entities: &Entities, id: EntityId) -> Self {
        SideRows {
            tag: entities.tag_of(id),
            hidden: entities.is_hidden(id),
            locked: entities.is_locked(id),
            attrs: entities.attrs(id).cloned(),
        }
    }

    /// Writes the rows directly into an `Entities` that is not in the
    /// document yet (recorded as a whole by `DefinitionInserted`).
    pub(crate) fn write_into(
        self,
        entities: &mut Entities,
        id: EntityId,
    ) -> Result<(), CommandError> {
        let internal = |e: HierarchyError| CommandError::Internal(e.to_string());
        if self.tag.is_some() {
            entities.set_tag(id, self.tag).map_err(internal)?;
        }
        if self.hidden {
            entities.set_hidden(id, true).map_err(internal)?;
        }
        if self.locked {
            entities.set_locked(id, true).map_err(internal)?;
        }
        if self.attrs.is_some() {
            entities.set_attrs(id, self.attrs).map_err(internal)?;
        }
        Ok(())
    }

    /// Applies the rows through the transaction (recorded deltas).
    pub(crate) fn apply(
        self,
        tx: &mut Transaction<'_>,
        container: ContainerRef,
        id: EntityId,
    ) -> Result<(), CommandError> {
        if self.tag.is_some() {
            tx.set_tag(container, id, self.tag)?;
        }
        if self.hidden {
            tx.set_hidden(container, id, true)?;
        }
        if self.locked {
            tx.set_locked(container, id, true)?;
        }
        for (dict, entries) in self.attrs.iter().flat_map(|a| a.0.iter()) {
            for (key, value) in entries {
                let target = AttrTarget::Entity { container, id };
                tx.set_attribute(target, dict, key, Some(value.clone()))?;
            }
        }
        Ok(())
    }
}

/// Selected faces and edges of one mesh in indexed form (container
/// coordinates), with the ids behind each local index.
pub(crate) struct MeshPiece {
    /// The geometry: faces with all their loops, edges with flags, and the
    /// curves whose members are all selected.
    pub(crate) indexed: IndexedMesh,
    /// Source edge of each local edge.
    pub(crate) edges: Vec<EdgeId>,
    /// Source face of each local face.
    pub(crate) faces: Vec<FaceId>,
}

/// Extracts `faces` (with their boundary edges) and `edges` from the mesh
/// of `entities`. Edges come in the container's order (pid order), not in
/// slot order, so the result does not depend on the slot layout (which a
/// save/reload renumbers).
pub(crate) fn mesh_piece(
    entities: &Entities,
    faces: &[FaceId],
    edges: &[EdgeId],
) -> Result<MeshPiece, CommandError> {
    let mesh = entities.mesh();
    let mut edge_set: BTreeSet<EdgeId> = edges.iter().copied().collect();
    for &f in faces {
        edge_set.extend(mesh.face_edges(f)?);
    }
    let rank: BTreeMap<EntityId, usize> = entities
        .order()
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();
    let mut edges: Vec<EdgeId> = edge_set.iter().copied().collect();
    edges.sort_by_key(|e| {
        (
            rank.get(&EntityId::Edge(*e)).copied().unwrap_or(usize::MAX),
            *e,
        )
    });

    // Curves whose members all come along keep their descriptor.
    let mut indexed = IndexedMesh::default();
    let mut curve_index: BTreeMap<CurveId, u32> = BTreeMap::new();
    for &e in &edges {
        let Some(c) = mesh.edge(e).and_then(|x| x.curve) else {
            continue;
        };
        if curve_index.contains_key(&c) {
            continue;
        }
        if let (true, Some(descriptor)) = (
            mesh.curve_edges(c)?.iter().all(|m| edge_set.contains(m)),
            mesh.curve(c),
        ) {
            curve_index.insert(c, indexed.curves.len() as u32);
            indexed.curves.push(descriptor.clone());
        }
    }

    let mut vertex_index: BTreeMap<VertexId, u32> = BTreeMap::new();
    let mut edge_index: BTreeMap<EdgeId, u32> = BTreeMap::new();
    for &e in &edges {
        let (a, b) = mesh.edge_endpoints(e)?;
        let mut idx = |v: VertexId, indexed: &mut IndexedMesh| -> Result<u32, CommandError> {
            if let Some(&i) = vertex_index.get(&v) {
                return Ok(i);
            }
            let i = indexed.vertices.len() as u32;
            indexed.vertices.push(mesh.position(v)?);
            vertex_index.insert(v, i);
            Ok(i)
        };
        let (ia, ib) = (idx(a, &mut indexed)?, idx(b, &mut indexed)?);
        edge_index.insert(e, indexed.edges.len() as u32);
        let edge = mesh.edge(e);
        indexed.edges.push(IndexedEdge {
            a: ia,
            b: ib,
            flags: edge.map(|x| x.flags).unwrap_or(EdgeFlags::NONE),
            curve: edge
                .and_then(|x| x.curve)
                .and_then(|c| curve_index.get(&c).copied()),
        });
    }
    for &f in faces {
        let face = mesh.face_ref(f)?;
        let mut loops = Vec::new();
        for cycle in mesh.face_cycles(f)? {
            let mut directed = Vec::with_capacity(cycle.len());
            for h in cycle {
                let he = mesh.he(h)?;
                let i = edge_index
                    .get(&he.edge)
                    .copied()
                    .ok_or_else(|| CommandError::Internal(format!("{} not captured", he.edge)))?;
                let forward = mesh.edge(he.edge).is_some_and(|x| x.he == h);
                directed.push(if forward {
                    DirectedEdgeIndex::forward(i)
                } else {
                    DirectedEdgeIndex::reversed(i)
                });
            }
            loops.push(directed);
        }
        indexed.faces.push(IndexedFace {
            loops,
            plane: Some(face.plane),
            sides: face.sides.clone(),
        });
    }
    Ok(MeshPiece {
        indexed,
        edges,
        faces: faces.to_vec(),
    })
}

/// Places `piece` (already in the coordinates of `container`) there: as is
/// when it touches nothing (`HalfEdgeMesh::insert_disjoint`), otherwise
/// redrawn with `add_edge` so it merges with what is there (docs/05 §5.3,
/// Decisions). Inserted elements get `edge_rows` / `face_rows` by local
/// index; pids go to `tx.created()`.
pub(crate) fn place_geometry(
    tx: &mut Transaction<'_>,
    container: ContainerRef,
    piece: &IndexedMesh,
    edge_rows: Vec<SideRows>,
    face_rows: Vec<SideRows>,
) -> Result<(), CommandError> {
    if piece.edges.is_empty() {
        return Ok(());
    }
    let inserted = tx.with_mesh(container, |m, tol, log| m.insert_disjoint(piece, tol, log))?;
    let Some(new) = inserted else {
        let (segments, faces) = redraw_input(piece)?;
        return redraw_geometry(tx, container, &segments, &faces);
    };
    for (rows, &e) in edge_rows.into_iter().zip(&new.edges) {
        rows.apply(tx, container, EntityId::Edge(e))?;
    }
    for (rows, &f) in face_rows.into_iter().zip(&new.faces) {
        rows.apply(tx, container, EntityId::Face(f))?;
    }
    Ok(())
}

/// The `add_edge` fallback input of an indexed piece: every edge, and each
/// face's outer loop with its plane and sides.
#[allow(clippy::type_complexity)]
fn redraw_input(
    piece: &IndexedMesh,
) -> Result<(Vec<(DVec3, DVec3, EdgeFlags)>, Vec<FaceCopy>), CommandError> {
    let point = |i: u32| {
        piece
            .vertices
            .get(i as usize)
            .copied()
            .ok_or_else(|| CommandError::Internal(format!("vertex index {i} out of range")))
    };
    let segments = piece
        .edges
        .iter()
        .map(|e| Ok((point(e.a)?, point(e.b)?, e.flags)))
        .collect::<Result<Vec<_>, CommandError>>()?;
    let mut faces = Vec::new();
    for face in &piece.faces {
        let Some(outer) = face.loops.first() else {
            continue;
        };
        let ring = outer
            .iter()
            .map(|de| {
                let e = piece.edges.get(de.index() as usize).ok_or_else(|| {
                    CommandError::Internal(format!("edge index {} out of range", de.index()))
                })?;
                point(if de.is_reversed() { e.b } else { e.a })
            })
            .collect::<Result<Vec<_>, CommandError>>()?;
        let Some(plane) = face.plane else {
            continue;
        };
        faces.push((ring, plane, face.sides.clone()));
    }
    Ok((segments, faces))
}

/// The face whose outer loop runs through exactly `points`.
pub(crate) fn face_through(m: &HalfEdgeMesh, points: &[DVec3], tol: &Tolerances) -> Option<FaceId> {
    let wanted: BTreeSet<VertexId> = points
        .iter()
        .map(|&p| m.find_vertex_near(p, tol))
        .collect::<Option<_>>()?;
    let first = *wanted.iter().next()?;
    for &h in m.vertex_star(first) {
        for side in [Some(h), m.twin_of(h).ok()].into_iter().flatten() {
            let Some(f) = m.he(side).ok()?.face else {
                continue;
            };
            let outer: BTreeSet<VertexId> = m.face_vertices(f).ok()?.into_iter().collect();
            if outer == wanted {
                return Some(f);
            }
        }
    }
    None
}

/// The entities a query matches inside the target container, in selection
/// order (occurrences in other containers are ignored: commands work in the
/// edit context only, docs/05 §4.3).
pub(crate) fn targets_in(
    tx: &Transaction<'_>,
    target: &Target,
    query: &Query,
) -> Result<Vec<EntityId>, CommandError> {
    targets_of(tx, target, "targets", query)
}

/// [`targets_in`] for a `Sel` parameter with another name (errors name it).
pub(crate) fn targets_of(
    tx: &Transaction<'_>,
    target: &Target,
    param: &str,
    query: &Query,
) -> Result<Vec<EntityId>, CommandError> {
    let selection = select(tx, &target.context, param, query)?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for path in selection.iter() {
        if let Ok(r) = tx.document().resolve(path)
            && r.container == target.container
            && seen.insert(r.entity)
        {
            out.push(r.entity);
        }
    }
    Ok(out)
}

/// A deterministic definition UUID (docs/09 Decisions): commands may not
/// use randomness, yet UUIDs identify components across documents, so the
/// UUID is a hash of the name, the contents and the document's pid counter
/// (unique within the document) — two documents give the same UUID only to
/// identical components made at the same point of their history.
pub(crate) fn definition_uuid(
    doc: &Document,
    name: &str,
    entities: &crate::document::Entities,
) -> uuid::Uuid {
    let mut h = blake3::Hasher::new();
    h.update(b"planyrith component definition\0");
    h.update(&doc.info.next_pid.next_raw().to_le_bytes());
    h.update(name.as_bytes());
    h.update(b"\0");
    if let Ok(bytes) = rmp_serde::to_vec_named(entities) {
        h.update(&bytes);
    }
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&h.finalize().as_bytes()[..16]);
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

/// The number in the default name of a new group (`Group <n>`): the pid
/// counter, unique within the document.
pub(crate) fn group_number(doc: &Document) -> u64 {
    doc.info.next_pid.next_raw()
}

/// Copy-on-edit of groups (docs/04 §3.1): before group `id` (in `container`)
/// or its contents change, every *other* group that shares its definition
/// moves to a copy of that definition with fresh pids. The edited group
/// keeps its definition, ids and pids, so paths and selections into it stay
/// valid. Instances are never forked (components are shared on purpose).
pub(crate) fn make_group_private(
    tx: &mut Transaction<'_>,
    container: ContainerRef,
    id: EntityId,
) -> Result<(), CommandError> {
    let doc = tx.document();
    let Some(EntityView::Group(group)) = doc.container(container).and_then(|e| e.view(id)) else {
        return Ok(());
    };
    let d = group.definition;
    let mut others = Vec::new();
    for c in doc.containers() {
        if let Some(entities) = doc.container(c) {
            for (g, x) in entities.groups() {
                if x.definition == d && (c, EntityId::Group(g)) != (container, id) {
                    others.push((c, EntityId::Group(g)));
                }
            }
        }
    }
    let Some(mut copy) = doc.definition(d).cloned() else {
        return Ok(());
    };
    if others.is_empty() {
        return Ok(());
    }
    copy.name = format!("Group {}", group_number(doc));
    copy.uuid = definition_uuid(doc, &copy.name, &copy.entities);
    let pids: Vec<Pid> = (0..copy.entities.len())
        .map(|_| tx.allocate_pid())
        .collect();
    copy.entities.reassign_pids(pids);
    let fork = tx.insert_definition(copy)?;
    for (c, g) in others {
        let Some(EntityView::Group(x)) = tx.document().container(c).and_then(|e| e.view(g)) else {
            continue;
        };
        let record = EntityRecord::Group(Group {
            definition: fork,
            ..x.clone()
        });
        tx.replace_entity(c, g, record)?;
    }
    Ok(())
}

/// Copy-on-edit along an edit-context path (top-down): every group the path
/// passes through becomes private before the context's contents change.
pub(crate) fn make_path_private(
    tx: &mut Transaction<'_>,
    path: &InstancePath,
) -> Result<(), CommandError> {
    let mut container = ContainerRef::Model;
    for &step in &path.steps {
        let Some(entities) = tx.document().container(container) else {
            return Ok(());
        };
        let Some(id) = entities.key_to_id(step) else {
            return Ok(());
        };
        let Some(d) = entities.definition_of(id) else {
            return Ok(());
        };
        make_group_private(tx, container, id)?;
        container = ContainerRef::Definition(d);
    }
    Ok(())
}

/// `E1201` when a locked group or instance is among `ids` (locked containers
/// cannot be edited, moved or erased until unlocked, docs/05 §5.13).
pub(crate) fn refuse_locked(
    tx: &Transaction<'_>,
    container: ContainerRef,
    ids: &[EntityId],
) -> Result<(), CommandError> {
    let Some(entities) = tx.document().container(container) else {
        return Ok(());
    };
    if let Some(id) = ids.iter().find(|id| entities.is_locked(**id)) {
        return Err(CommandError::NotApplicable(format!(
            "{id} is locked; unlock it first"
        )));
    }
    Ok(())
}

/// Removes a group or instance; a group's private definition goes with it
/// when nothing else uses it.
pub(crate) fn remove_container_entity(
    tx: &mut Transaction<'_>,
    container: ContainerRef,
    id: EntityId,
) -> Result<(), CommandError> {
    let definition = tx
        .document()
        .container(container)
        .and_then(|e| e.definition_of(id));
    tx.remove_entity(container, id)?;
    if let Some(d) = definition {
        purge_private_definition(tx, d)?;
    }
    Ok(())
}

/// Removes an internal (group) definition that no group or instance uses,
/// then, in turn, the private definitions of the groups it contained that
/// nothing uses any more. Component definitions stay (`purge-unused`).
pub(crate) fn purge_private_definition(
    tx: &mut Transaction<'_>,
    d: DefinitionId,
) -> Result<(), CommandError> {
    let mut work = vec![d];
    let mut seen = BTreeSet::new();
    while let Some(d) = work.pop() {
        if !seen.insert(d) {
            continue;
        }
        let doc = tx.document();
        let private = doc.definition(d).is_some_and(|def| def.is_internal_group);
        let used = doc.containers().into_iter().any(|c| {
            doc.container(c).is_some_and(|e| {
                e.groups().any(|(_, g)| g.definition == d)
                    || e.instances().any(|(_, i)| i.definition == d)
            })
        });
        if private && !used {
            let removed = tx.remove_definition(d)?;
            let inner: Vec<DefinitionId> = removed
                .entities
                .iter()
                .filter_map(|id| removed.entities.definition_of(id))
                .collect();
            // Depth-first in container order: deterministic.
            work.extend(inner.into_iter().rev());
        }
    }
    Ok(())
}
