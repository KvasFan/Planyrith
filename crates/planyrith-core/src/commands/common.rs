//! Helpers shared by the editing and organization commands of
//! docs/05-modeling-tools.md §4–§5: resolving a `Sel` into the entities of
//! the edit context, and removing groups with their private definitions.

#![warn(unused_variables, dead_code)]

use std::collections::BTreeSet;

use glam::DVec3;

use crate::commands::params::{Target, select};
use crate::commands::{CommandError, Transaction};
use crate::document::{ContainerRef, Query};
use crate::geom::{EdgeFlags, FaceSides, HalfEdgeMesh, planar};
use crate::ids::{DefinitionId, EntityId, FaceId, VertexId};
use crate::math::Plane;
use crate::tolerance::Tolerances;

/// A face to re-create by [`redraw_geometry`]: outer loop points, the plane
/// whose normal gives the front side, and the materials.
pub(crate) type FaceCopy = (Vec<DVec3>, Plane, FaceSides);

/// Draws `segments` into `container` with `add_edge` (faces form by face
/// finding), then gives every face of `faces` its front side and materials;
/// a face flips only together with its edge-connected group. One atomic
/// kernel step; pids of the new elements are appended to `tx.created()`.
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
            let mut reversed: BTreeSet<FaceId> = BTreeSet::new();
            for (outer, plane, sides) in faces {
                let Some(f) = face_through(m, outer, tol) else {
                    continue;
                };
                if m.face_normal(f)?.dot(plane.normal) < 0.0 && !reversed.contains(&f) {
                    let group = planar::face_group(m, &BTreeSet::from([f]))?;
                    planar::reverse_faces(m, &group, tol, log)?;
                    reversed.extend(group);
                }
                m.set_face_sides(f, sides.clone(), log)?;
            }
            Ok(())
        })
    })
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
    let selection = select(tx, query)?;
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

/// Removes an internal (group) definition that no group or instance uses.
pub(crate) fn purge_private_definition(
    tx: &mut Transaction<'_>,
    d: DefinitionId,
) -> Result<(), CommandError> {
    let doc = tx.document();
    let private = doc.definition(d).is_some_and(|def| def.is_internal_group);
    let used = doc.containers().into_iter().any(|c| {
        doc.container(c).is_some_and(|e| {
            e.groups().any(|(_, g)| g.definition == d)
                || e.instances().any(|(_, i)| i.definition == d)
        })
    });
    if private && !used {
        tx.remove_definition(d)?;
    }
    Ok(())
}
