//! Helpers shared by the editing and organization commands of
//! docs/05-modeling-tools.md §4–§5: resolving a `Sel` into the entities of
//! the edit context, and removing groups with their private definitions.

#![warn(unused_variables, dead_code)]

use std::collections::BTreeSet;

use crate::commands::params::{Target, select};
use crate::commands::{CommandError, Transaction};
use crate::document::{ContainerRef, Query};
use crate::ids::{DefinitionId, EntityId};

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
