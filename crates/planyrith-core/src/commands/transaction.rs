//! Implements docs/01-architecture.md §3.1 rule 1 (commands mutate only through
//! a `Transaction` that records inverse deltas) and the undo stack of §3.2:
//! `Transaction`, `Delta`, `CommittedTransaction`, `UndoStack`.
//!
//! Pid bookkeeping: `Transaction::with_mesh` runs a kernel operation, then
//! allocates pids for every edge/face it created, binds them at the end of the
//! container's order list and unbinds the ones it removed (docs/04 §2). A face
//! recorded as `FaceRemoved { id }` + `FaceAdded { id }` (a reshape, docs/03
//! Decisions) keeps its pid. New pieces of a split face or edge inherit the
//! tag and hidden state of the original; the longest piece of a split edge
//! takes over its pid, order slot and attributes, and the other pieces are not
//! reported as created (docs/08 Decisions, `last`). Undo never rewinds
//! `next_pid`.
//!
//! Recorded order of one mesh operation: side-table clears and `PidUnbound` of
//! vanished elements, then the `Mesh` deltas, then `PidBound` (and inherited
//! tags) of new elements. Replaying that list forward, or its inverses in
//! reverse, keeps every pid operation next to an existing element.
//!
//! Ids: everything except mesh edges/faces has stable ids across undo/redo
//! (`document::StableMap`, docs/04 Decisions). Edge/face ids named by deltas are
//! resolved through [`MeshAliases`] (one `geom::ReplayIds` per container),
//! which the engine keeps for the whole session.
//!
//! Owner key: `later` (depends on `document` and `mesh`).

#![warn(unused_variables, dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use glam::DAffine3;
use serde::{Deserialize, Serialize};

use crate::commands::CommandError;
use crate::diagnostics::Warning;
use crate::document::{
    AttributeDictionaries, ComponentDefinition, ContainerRef, Document, EditContext, Entities,
    EntityKey, EntityPath, EntityRecord, EntityRef, HierarchyError, Material, ModelInfo, QueryEnv,
    RenderingOptions, Scene, Selection, Style, Tag, TagFolder, Value,
};
use crate::geom::{DeltaSink, FaceSides, GeometryError, HalfEdgeMesh, MeshDelta, ReplayIds};
use crate::ids::{
    DefinitionId, EntityId, FaceId, MaterialId, Pid, SceneId, StyleId, TagFolderId, TagId,
};
use crate::io::Storage;
use crate::math::Segment;
use crate::tolerance::Tolerances;

/// Where an attribute lives (docs/04 §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttrTarget {
    /// `Document::attrs`.
    Document,
    /// An entity's dictionaries.
    Entity {
        /// Container.
        container: ContainerRef,
        /// Entity.
        id: EntityId,
    },
    /// A definition's dictionaries.
    Definition(DefinitionId),
    /// A material's dictionaries.
    Material(MaterialId),
}

/// An elementary, invertible document change (glossary "Delta"). Every variant
/// carries enough to apply it forward and backward without a snapshot of the
/// whole document (docs/01 Decisions: undo via inverse deltas).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Delta {
    /// A kernel change in one container's mesh.
    Mesh {
        /// Container.
        container: ContainerRef,
        /// The change.
        delta: MeshDelta,
    },
    /// A pid was bound to an entity at an order index (edges/faces after a mesh
    /// op, or any inserted entity).
    PidBound {
        /// Container.
        container: ContainerRef,
        /// Entity.
        id: EntityId,
        /// Pid.
        pid: Pid,
        /// Position in the container's order list.
        order_index: usize,
    },
    /// A pid binding was removed.
    PidUnbound {
        /// Container.
        container: ContainerRef,
        /// Entity.
        id: EntityId,
        /// Pid.
        pid: Pid,
        /// Former position in the order list.
        order_index: usize,
    },
    /// A non-mesh entity was inserted.
    EntityInserted {
        /// Container.
        container: ContainerRef,
        /// Entity.
        id: EntityId,
        /// Pid.
        pid: Pid,
        /// Position in the order list.
        order_index: usize,
        /// Data.
        record: Box<EntityRecord>,
    },
    /// A non-mesh entity was removed.
    EntityRemoved {
        /// Container.
        container: ContainerRef,
        /// Entity.
        id: EntityId,
        /// Pid.
        pid: Pid,
        /// Former position in the order list.
        order_index: usize,
        /// Data.
        record: Box<EntityRecord>,
    },
    /// A non-mesh entity's data changed in place.
    EntityChanged {
        /// Container.
        container: ContainerRef,
        /// Entity.
        id: EntityId,
        /// Before.
        from: Box<EntityRecord>,
        /// After.
        to: Box<EntityRecord>,
    },
    /// A transform changed (group/instance/image).
    TransformChanged {
        /// Container.
        container: ContainerRef,
        /// Entity.
        id: EntityId,
        /// Before.
        from: DAffine3,
        /// After.
        to: DAffine3,
    },
    /// Tag assignment changed (`None` = untagged).
    TagChanged {
        /// Container.
        container: ContainerRef,
        /// Entity.
        id: EntityId,
        /// Before.
        from: Option<TagId>,
        /// After.
        to: Option<TagId>,
    },
    /// Hidden flag changed (recorded only when the value really changes).
    HiddenChanged {
        /// Container.
        container: ContainerRef,
        /// Entity.
        id: EntityId,
        /// New value (inverse is `!to`).
        to: bool,
    },
    /// Locked flag changed (recorded only when the value really changes).
    LockedChanged {
        /// Container.
        container: ContainerRef,
        /// Entity.
        id: EntityId,
        /// New value.
        to: bool,
    },
    /// One attribute value changed (`None` = absent).
    AttributeChanged {
        /// Target.
        target: AttrTarget,
        /// Dictionary.
        dict: String,
        /// Key.
        key: String,
        /// Before.
        from: Option<Value>,
        /// After.
        to: Option<Value>,
    },
    /// A definition was added.
    DefinitionInserted {
        /// Id.
        id: DefinitionId,
        /// Position in `definition_order`.
        order_index: usize,
        /// Data (its `entities` may be large; boxed).
        definition: Box<ComponentDefinition>,
    },
    /// A definition was removed.
    DefinitionRemoved {
        /// Id.
        id: DefinitionId,
        /// Former position.
        order_index: usize,
        /// Data.
        definition: Box<ComponentDefinition>,
    },
    /// A definition's metadata/behavior changed (not its entities).
    DefinitionChanged {
        /// Id.
        id: DefinitionId,
        /// Before (entities left empty).
        from: Box<ComponentDefinition>,
        /// After (entities left empty).
        to: Box<ComponentDefinition>,
    },
    /// Material added.
    MaterialInserted {
        /// Id.
        id: MaterialId,
        /// Position in the material list.
        order_index: usize,
        /// Data.
        material: Box<Material>,
    },
    /// Material removed.
    MaterialRemoved {
        /// Id.
        id: MaterialId,
        /// Former position.
        order_index: usize,
        /// Data.
        material: Box<Material>,
    },
    /// Material changed.
    MaterialChanged {
        /// Id.
        id: MaterialId,
        /// Before.
        from: Box<Material>,
        /// After.
        to: Box<Material>,
    },
    /// Tag added.
    TagInserted {
        /// Id.
        id: TagId,
        /// Position in the tag list.
        order_index: usize,
        /// Data.
        tag: Tag,
    },
    /// Tag removed.
    TagRemoved {
        /// Id.
        id: TagId,
        /// Former position.
        order_index: usize,
        /// Data.
        tag: Tag,
    },
    /// Tag changed.
    TagUpdated {
        /// Id.
        id: TagId,
        /// Before.
        from: Tag,
        /// After.
        to: Tag,
    },
    /// Tag folder added.
    TagFolderInserted {
        /// Id.
        id: TagFolderId,
        /// Position in the folder list.
        order_index: usize,
        /// Data.
        folder: TagFolder,
    },
    /// Tag folder removed.
    TagFolderRemoved {
        /// Id.
        id: TagFolderId,
        /// Former position.
        order_index: usize,
        /// Data.
        folder: TagFolder,
    },
    /// Scene added at an index.
    SceneInserted {
        /// Id.
        id: SceneId,
        /// Position.
        order_index: usize,
        /// Data.
        scene: Box<Scene>,
    },
    /// Scene removed.
    SceneRemoved {
        /// Id.
        id: SceneId,
        /// Former position.
        order_index: usize,
        /// Data.
        scene: Box<Scene>,
    },
    /// Scene changed.
    SceneChanged {
        /// Id.
        id: SceneId,
        /// Before.
        from: Box<Scene>,
        /// After.
        to: Box<Scene>,
    },
    /// Scene order changed.
    SceneMoved {
        /// Id.
        id: SceneId,
        /// Before.
        from_index: usize,
        /// After.
        to_index: usize,
    },
    /// Style added.
    StyleInserted {
        /// Id.
        id: StyleId,
        /// Position in the style list.
        order_index: usize,
        /// Data.
        style: Box<Style>,
    },
    /// Style removed.
    StyleRemoved {
        /// Id.
        id: StyleId,
        /// Former position.
        order_index: usize,
        /// Data.
        style: Box<Style>,
    },
    /// Style changed.
    StyleChanged {
        /// Id.
        id: StyleId,
        /// Before.
        from: Box<Style>,
        /// After.
        to: Box<Style>,
    },
    /// Active style changed.
    ActiveStyleChanged {
        /// Before.
        from: StyleId,
        /// After.
        to: StyleId,
    },
    /// `ModelInfo` changed (units, precision, tolerances, title…); `next_pid`
    /// is copied forward, never rewound.
    InfoChanged {
        /// Before.
        from: Box<ModelInfo>,
        /// After.
        to: Box<ModelInfo>,
    },
    /// Rendering options changed.
    RenderingChanged {
        /// Before.
        from: Box<RenderingOptions>,
        /// After.
        to: Box<RenderingOptions>,
    },
}

impl Delta {
    /// `true` for changes that can alter the definition graph (occurrences
    /// inserted or re-pointed, definitions inserted).
    pub fn may_change_nesting(&self) -> bool {
        match self {
            Delta::EntityInserted { record, .. } => {
                matches!(**record, EntityRecord::Group(_) | EntityRecord::Instance(_))
            }
            Delta::EntityChanged { .. } | Delta::DefinitionInserted { .. } => true,
            _ => false,
        }
    }

    /// The inverse delta (swaps before/after, inserted/removed, bound/unbound).
    pub fn inverse(&self) -> Delta {
        match self.clone() {
            Delta::Mesh { container, delta } => Delta::Mesh {
                container,
                delta: delta.inverse(),
            },
            Delta::PidBound {
                container,
                id,
                pid,
                order_index,
            } => Delta::PidUnbound {
                container,
                id,
                pid,
                order_index,
            },
            Delta::PidUnbound {
                container,
                id,
                pid,
                order_index,
            } => Delta::PidBound {
                container,
                id,
                pid,
                order_index,
            },
            Delta::EntityInserted {
                container,
                id,
                pid,
                order_index,
                record,
            } => Delta::EntityRemoved {
                container,
                id,
                pid,
                order_index,
                record,
            },
            Delta::EntityRemoved {
                container,
                id,
                pid,
                order_index,
                record,
            } => Delta::EntityInserted {
                container,
                id,
                pid,
                order_index,
                record,
            },
            Delta::EntityChanged {
                container,
                id,
                from,
                to,
            } => Delta::EntityChanged {
                container,
                id,
                from: to,
                to: from,
            },
            Delta::TransformChanged {
                container,
                id,
                from,
                to,
            } => Delta::TransformChanged {
                container,
                id,
                from: to,
                to: from,
            },
            Delta::TagChanged {
                container,
                id,
                from,
                to,
            } => Delta::TagChanged {
                container,
                id,
                from: to,
                to: from,
            },
            Delta::HiddenChanged { container, id, to } => Delta::HiddenChanged {
                container,
                id,
                to: !to,
            },
            Delta::LockedChanged { container, id, to } => Delta::LockedChanged {
                container,
                id,
                to: !to,
            },
            Delta::AttributeChanged {
                target,
                dict,
                key,
                from,
                to,
            } => Delta::AttributeChanged {
                target,
                dict,
                key,
                from: to,
                to: from,
            },
            Delta::DefinitionInserted {
                id,
                order_index,
                definition,
            } => Delta::DefinitionRemoved {
                id,
                order_index,
                definition,
            },
            Delta::DefinitionRemoved {
                id,
                order_index,
                definition,
            } => Delta::DefinitionInserted {
                id,
                order_index,
                definition,
            },
            Delta::DefinitionChanged { id, from, to } => Delta::DefinitionChanged {
                id,
                from: to,
                to: from,
            },
            Delta::MaterialInserted {
                id,
                order_index,
                material,
            } => Delta::MaterialRemoved {
                id,
                order_index,
                material,
            },
            Delta::MaterialRemoved {
                id,
                order_index,
                material,
            } => Delta::MaterialInserted {
                id,
                order_index,
                material,
            },
            Delta::MaterialChanged { id, from, to } => Delta::MaterialChanged {
                id,
                from: to,
                to: from,
            },
            Delta::TagInserted {
                id,
                order_index,
                tag,
            } => Delta::TagRemoved {
                id,
                order_index,
                tag,
            },
            Delta::TagRemoved {
                id,
                order_index,
                tag,
            } => Delta::TagInserted {
                id,
                order_index,
                tag,
            },
            Delta::TagUpdated { id, from, to } => Delta::TagUpdated {
                id,
                from: to,
                to: from,
            },
            Delta::TagFolderInserted {
                id,
                order_index,
                folder,
            } => Delta::TagFolderRemoved {
                id,
                order_index,
                folder,
            },
            Delta::TagFolderRemoved {
                id,
                order_index,
                folder,
            } => Delta::TagFolderInserted {
                id,
                order_index,
                folder,
            },
            Delta::SceneInserted {
                id,
                order_index,
                scene,
            } => Delta::SceneRemoved {
                id,
                order_index,
                scene,
            },
            Delta::SceneRemoved {
                id,
                order_index,
                scene,
            } => Delta::SceneInserted {
                id,
                order_index,
                scene,
            },
            Delta::SceneChanged { id, from, to } => Delta::SceneChanged {
                id,
                from: to,
                to: from,
            },
            Delta::SceneMoved {
                id,
                from_index,
                to_index,
            } => Delta::SceneMoved {
                id,
                from_index: to_index,
                to_index: from_index,
            },
            Delta::StyleInserted {
                id,
                order_index,
                style,
            } => Delta::StyleRemoved {
                id,
                order_index,
                style,
            },
            Delta::StyleRemoved {
                id,
                order_index,
                style,
            } => Delta::StyleInserted {
                id,
                order_index,
                style,
            },
            Delta::StyleChanged { id, from, to } => Delta::StyleChanged {
                id,
                from: to,
                to: from,
            },
            Delta::ActiveStyleChanged { from, to } => {
                Delta::ActiveStyleChanged { from: to, to: from }
            }
            Delta::InfoChanged { from, to } => Delta::InfoChanged { from: to, to: from },
            Delta::RenderingChanged { from, to } => Delta::RenderingChanged { from: to, to: from },
        }
    }

    /// The container this delta touches, if it is container-scoped.
    pub fn container(&self) -> Option<ContainerRef> {
        match self {
            Delta::Mesh { container, .. }
            | Delta::PidBound { container, .. }
            | Delta::PidUnbound { container, .. }
            | Delta::EntityInserted { container, .. }
            | Delta::EntityRemoved { container, .. }
            | Delta::EntityChanged { container, .. }
            | Delta::TransformChanged { container, .. }
            | Delta::TagChanged { container, .. }
            | Delta::HiddenChanged { container, .. }
            | Delta::LockedChanged { container, .. } => Some(*container),
            Delta::AttributeChanged {
                target: AttrTarget::Entity { container, .. },
                ..
            } => Some(*container),
            _ => None,
        }
    }
}

/// Edge/face id aliases per container for replay (see the module
/// documentation). The engine keeps one for the whole session.
pub type MeshAliases = BTreeMap<ContainerRef, ReplayIds>;

/// The id an entity recorded as `id` has now (edges/faces through `aliases`).
fn resolve(aliases: &MeshAliases, container: ContainerRef, id: EntityId) -> EntityId {
    match (id, aliases.get(&container)) {
        (EntityId::Edge(e), Some(a)) => EntityId::Edge(a.edge(e)),
        (EntityId::Face(f), Some(a)) => EntityId::Face(a.face(f)),
        _ => id,
    }
}

fn internal(msg: impl std::fmt::Display) -> CommandError {
    CommandError::Internal(msg.to_string())
}

fn hierarchy(err: HierarchyError) -> CommandError {
    match err {
        HierarchyError::NotFound(id) => {
            CommandError::NotFound(EntityRef::Path(EntityPath::in_model(EntityKey::Id(id))))
        }
        HierarchyError::NotLockable(_)
        | HierarchyError::MeshEntity(_)
        | HierarchyError::KindMismatch(_)
        | HierarchyError::Cycle(_)
        | HierarchyError::TooDeep(_) => CommandError::NotApplicable(err.to_string()),
        HierarchyError::PidInUse(_) => internal(err),
    }
}

fn container_mut(doc: &mut Document, c: ContainerRef) -> Result<&mut Entities, CommandError> {
    doc.container_mut(c)
        .ok_or_else(|| CommandError::NotApplicable(format!("container {c:?} does not exist")))
}

/// `delta` with the edge/face ids it names replaced by the ids those
/// elements have now (mesh deltas are resolved by the replay itself).
fn with_live_ids(delta: &Delta, aliases: &MeshAliases) -> Delta {
    let mut live = delta.clone();
    match &mut live {
        Delta::PidBound { container, id, .. }
        | Delta::PidUnbound { container, id, .. }
        | Delta::TagChanged { container, id, .. }
        | Delta::HiddenChanged { container, id, .. } => *id = resolve(aliases, *container, *id),
        Delta::AttributeChanged {
            target: AttrTarget::Entity { container, id },
            ..
        } => *id = resolve(aliases, *container, *id),
        _ => {}
    }
    live
}

/// Applies one delta to the document (forward direction) and returns it as
/// applied, with current edge/face/vertex ids, for event consumers.
/// `Engine::undo` applies `delta.inverse()` for a transaction's deltas in
/// reverse order.
pub(crate) fn apply_delta(
    doc: &mut Document,
    delta: &Delta,
    aliases: &mut MeshAliases,
) -> Result<Delta, CommandError> {
    let live = with_live_ids(delta, aliases);
    match delta {
        Delta::Mesh { container, delta } => {
            let tol = *doc.tolerances();
            let entities = container_mut(doc, *container)?;
            let ids = aliases.entry(*container).or_default();
            let created = |ids: &ReplayIds| match delta {
                MeshDelta::FaceAdded { id, .. } => Some(EntityId::Face(ids.face(*id))),
                MeshDelta::EdgeAdded { id, .. } => Some(EntityId::Edge(ids.edge(*id))),
                _ => None,
            };
            let before = created(ids);
            let applied = entities.mesh_mut().replay(delta, ids, &tol)?;
            // A reshaped face (same id removed and added) is re-created under a
            // new id by replay; its pid and side tables follow it.
            if let (Some(prev), Some(now)) = (before, created(ids))
                && prev != now
                && !entities.contains(prev)
                && entities.pid_of(prev).is_some()
            {
                entities.rekey(prev, now).map_err(hierarchy)?;
            }
            return Ok(Delta::Mesh {
                container: *container,
                delta: applied,
            });
        }
        Delta::PidBound {
            container,
            id,
            pid,
            order_index,
        } => {
            let id = resolve(aliases, *container, *id);
            container_mut(doc, *container)?
                .bind_pid(id, *pid, Some(*order_index))
                .map_err(hierarchy)?;
        }
        Delta::PidUnbound { container, id, .. } => {
            let id = resolve(aliases, *container, *id);
            container_mut(doc, *container)?
                .unbind_pid(id)
                .map_err(hierarchy)?;
        }
        Delta::EntityInserted {
            container,
            id,
            pid,
            order_index,
            record,
        } => {
            container_mut(doc, *container)?
                .restore(*id, (**record).clone(), *pid, *order_index)
                .map_err(hierarchy)?;
        }
        Delta::EntityRemoved { container, id, .. } => {
            container_mut(doc, *container)?
                .remove(*id)
                .map_err(hierarchy)?;
        }
        Delta::EntityChanged {
            container, id, to, ..
        } => {
            container_mut(doc, *container)?
                .replace(*id, (**to).clone())
                .map_err(hierarchy)?;
        }
        Delta::TransformChanged {
            container, id, to, ..
        } => {
            container_mut(doc, *container)?
                .set_transform(*id, *to)
                .map_err(hierarchy)?;
        }
        Delta::TagChanged {
            container, id, to, ..
        } => {
            let id = resolve(aliases, *container, *id);
            container_mut(doc, *container)?
                .set_tag(id, *to)
                .map_err(hierarchy)?;
        }
        Delta::HiddenChanged { container, id, to } => {
            let id = resolve(aliases, *container, *id);
            container_mut(doc, *container)?
                .set_hidden(id, *to)
                .map_err(hierarchy)?;
        }
        Delta::LockedChanged { container, id, to } => {
            container_mut(doc, *container)?
                .set_locked(*id, *to)
                .map_err(hierarchy)?;
        }
        Delta::AttributeChanged {
            target,
            dict,
            key,
            to,
            ..
        } => {
            let target = match *target {
                AttrTarget::Entity { container, id } => AttrTarget::Entity {
                    container,
                    id: resolve(aliases, container, id),
                },
                other => other,
            };
            write_attribute(doc, target, dict, key, to.clone())?;
        }
        Delta::DefinitionInserted {
            id,
            order_index,
            definition,
        } => {
            if !doc.restore_definition(*id, (**definition).clone(), *order_index) {
                return Err(internal(format!("cannot restore {id}")));
            }
            // The snapshot brings its mesh back under the ids it was recorded
            // with; make them the current members of their alias classes so
            // later deltas that name any alias resolve to them.
            let ids = aliases.entry(ContainerRef::Definition(*id)).or_default();
            let mesh = definition.entities.mesh();
            for (e, _) in mesh.edges() {
                ids.alias_edge(e, e);
            }
            for (f, _) in mesh.faces() {
                ids.alias_face(f, f);
            }
            for (c, _) in mesh.curves() {
                ids.alias_curve(c, c);
            }
        }
        Delta::DefinitionRemoved { id, .. } => {
            doc.remove_definition(*id)
                .ok_or_else(|| internal(format!("{id} is not in the document")))?;
        }
        Delta::DefinitionChanged { id, to, .. } => {
            let def = doc
                .definitions
                .get_mut(*id)
                .ok_or_else(|| internal(format!("{id} is not in the document")))?;
            let entities = std::mem::take(&mut def.entities);
            *def = (**to).clone();
            def.entities = entities;
        }
        Delta::MaterialInserted {
            id,
            order_index,
            material,
        } => doc
            .materials
            .restore(*id, (**material).clone(), *order_index)
            .map_err(internal)?,
        Delta::MaterialRemoved { id, .. } => {
            doc.materials.remove(*id).map_err(internal)?;
        }
        Delta::MaterialChanged { id, to, .. } => {
            doc.materials
                .replace(*id, (**to).clone())
                .map_err(internal)?;
        }
        Delta::TagInserted {
            id,
            order_index,
            tag,
        } => doc
            .tags
            .restore(*id, tag.clone(), *order_index)
            .map_err(internal)?,
        Delta::TagRemoved { id, .. } => {
            doc.tags.remove(*id).map_err(internal)?;
        }
        Delta::TagUpdated { id, to, .. } => {
            doc.tags.replace(*id, to.clone()).map_err(internal)?;
        }
        Delta::TagFolderInserted {
            id,
            order_index,
            folder,
        } => doc
            .tags
            .restore_folder(*id, folder.clone(), *order_index)
            .map_err(internal)?,
        Delta::TagFolderRemoved { id, .. } => {
            doc.tags.remove_folder(*id).map_err(internal)?;
        }
        Delta::SceneInserted {
            id,
            order_index,
            scene,
        } => {
            if !doc.scenes.restore(*id, (**scene).clone(), *order_index) {
                return Err(internal(format!("cannot restore {id}")));
            }
        }
        Delta::SceneRemoved { id, .. } => {
            doc.scenes
                .remove(*id)
                .ok_or_else(|| internal(format!("{id} is not in the document")))?;
        }
        Delta::SceneChanged { id, to, .. } => {
            doc.scenes
                .replace(*id, (**to).clone())
                .ok_or_else(|| internal(format!("{id} is not in the document")))?;
        }
        Delta::SceneMoved { id, to_index, .. } => {
            doc.scenes
                .reorder(*id, *to_index)
                .ok_or_else(|| internal(format!("{id} is not in the document")))?;
        }
        Delta::StyleInserted {
            id,
            order_index,
            style,
        } => {
            if !doc.styles.restore(*id, (**style).clone(), *order_index) {
                return Err(internal(format!("cannot restore {id}")));
            }
        }
        Delta::StyleRemoved { id, .. } => {
            doc.styles
                .remove(*id)
                .ok_or_else(|| internal(format!("cannot remove {id}")))?;
        }
        Delta::StyleChanged { id, to, .. } => {
            doc.styles
                .replace(*id, (**to).clone())
                .ok_or_else(|| internal(format!("{id} is not in the document")))?;
        }
        Delta::ActiveStyleChanged { to, .. } => {
            doc.styles
                .set_active(*to)
                .ok_or_else(|| internal(format!("{to} is not in the document")))?;
        }
        Delta::InfoChanged { to, .. } => {
            let next_pid = doc.info.next_pid;
            doc.info = (**to).clone();
            doc.info.next_pid = next_pid;
        }
        Delta::RenderingChanged { to, .. } => doc.rendering = (**to).clone(),
    }
    Ok(live)
}

/// The attribute dictionaries a target names.
fn attributes_mut(
    doc: &mut Document,
    target: AttrTarget,
) -> Result<&mut AttributeDictionaries, CommandError> {
    Ok(match target {
        AttrTarget::Document => &mut doc.attrs,
        AttrTarget::Entity { container, id } => container_mut(doc, container)?
            .attrs_mut(id)
            .map_err(hierarchy)?,
        AttrTarget::Definition(d) => {
            &mut doc
                .definitions
                .get_mut(d)
                .ok_or_else(|| CommandError::NotApplicable(format!("{d} does not exist")))?
                .attrs
        }
        AttrTarget::Material(_) => {
            return Err(internal(
                "material attributes are written by write_attribute",
            ));
        }
    })
}

/// Sets (`Some`) or removes (`None`) one attribute; returns the previous value.
fn write_attribute(
    doc: &mut Document,
    target: AttrTarget,
    dict: &str,
    key: &str,
    value: Option<Value>,
) -> Result<Option<Value>, CommandError> {
    if let AttrTarget::Material(m) = target {
        let mut material = doc
            .materials
            .get(m)
            .cloned()
            .ok_or_else(|| CommandError::NotApplicable(format!("{m} does not exist")))?;
        let previous = match value {
            Some(v) => material.attrs.set(dict, key, v),
            None => material.attrs.remove(dict, key),
        };
        doc.materials.replace(m, material).map_err(internal)?;
        return Ok(previous);
    }
    let attrs = attributes_mut(doc, target)?;
    Ok(match value {
        Some(v) => attrs.set(dict, key, v),
        None => attrs.remove(dict, key),
    })
}

/// A committed transaction stored in history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommittedTransaction {
    /// Undo label.
    pub label: String,
    /// Deltas in application order.
    pub deltas: Vec<Delta>,
    /// Warnings produced.
    pub warnings: Vec<Warning>,
    /// Pids created (for `last`).
    pub created: Vec<Pid>,
}

/// Engine state a command may read (`selection`, `last`, edit context) and the
/// view changes it may request; neither is part of the undo history
/// (docs/04 §11).
#[derive(Debug, Clone, Default)]
pub struct TxView {
    /// Current selection.
    pub selection: Selection,
    /// Pids created by the previous command.
    pub last: Vec<Pid>,
    /// Current edit context.
    pub context: EditContext,
    /// Selection to apply after commit.
    pub selection_out: Option<Selection>,
    /// Edit context to apply after commit.
    pub context_out: Option<EditContext>,
}

/// An open transaction: the only way commands mutate a document
/// (docs/01 §3.1). Dropped without `commit` → caller must `rollback`.
pub struct Transaction<'a> {
    doc: &'a mut Document,
    storage: Option<&'a dyn Storage>,
    label: String,
    deltas: Vec<Delta>,
    warnings: Vec<Warning>,
    created: Vec<Pid>,
    view: TxView,
}

impl std::fmt::Debug for Transaction<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transaction")
            .field("label", &self.label)
            .field("deltas", &self.deltas.len())
            .field("warnings", &self.warnings.len())
            .finish_non_exhaustive()
    }
}

impl DeltaSink for Transaction<'_> {
    /// Records a mesh delta that the caller already applied to the **model**
    /// container's mesh. Prefer [`Transaction::with_mesh`], which also keeps
    /// pids in sync and works for any container.
    fn record(&mut self, delta: MeshDelta) {
        self.deltas.push(Delta::Mesh {
            container: ContainerRef::Model,
            delta,
        });
    }
}

/// Side tables of an entity captured before it disappears.
struct SideRows {
    tag: Option<TagId>,
    hidden: bool,
    attrs: Option<AttributeDictionaries>,
}

impl<'a> Transaction<'a> {
    /// Opens a transaction on `doc`.
    pub fn new(doc: &'a mut Document, label: impl Into<String>) -> Self {
        Transaction {
            doc,
            storage: None,
            label: label.into(),
            deltas: Vec::new(),
            warnings: Vec::new(),
            created: Vec::new(),
            view: TxView::default(),
        }
    }

    /// Gives file commands access to storage (docs/01 §3.1 rule 2).
    pub fn with_storage(mut self, storage: &'a dyn Storage) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Hands the engine's selection, `last` pids and edit context to the
    /// command (queries may refer to them).
    pub fn with_view(mut self, selection: Selection, last: Vec<Pid>, context: EditContext) -> Self {
        self.view.selection = selection;
        self.view.last = last;
        self.view.context = context;
        self
    }

    /// Read access to the document.
    pub fn document(&self) -> &Document {
        self.doc
    }

    /// Storage handed to file commands, if any.
    pub fn storage(&self) -> Option<&dyn Storage> {
        self.storage
    }

    /// The undo label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Deltas recorded so far.
    pub fn deltas(&self) -> &[Delta] {
        &self.deltas
    }

    /// Warnings recorded so far.
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    /// Treats `pid` as not created by this command: it stays in the
    /// document but `last` does not name it (fragments, docs/08 Decisions).
    pub fn forget_created(&mut self, pid: Pid) {
        self.created.retain(|p| *p != pid);
    }

    /// Pids created so far.
    pub fn created(&self) -> &[Pid] {
        &self.created
    }

    /// Adds a warning to the output (docs/01 §7).
    pub fn warn(&mut self, warning: Warning) {
        self.warnings.push(warning);
    }

    /// Document tolerances.
    pub fn tolerances(&self) -> &Tolerances {
        self.doc.tolerances()
    }

    /// Allocates a pid (monotonic; not undone).
    pub fn allocate_pid(&mut self) -> Pid {
        self.doc.allocate_pid()
    }

    /// The query environment: engine selection, `last`, edit context.
    pub fn query_env(&self) -> QueryEnv<'_> {
        QueryEnv {
            selection: &self.view.selection,
            last: &self.view.last,
            context: &self.view.context,
        }
    }

    /// The edit context the command runs in.
    pub fn context(&self) -> &EditContext {
        &self.view.context
    }

    /// Asks the engine to replace the selection after commit (not undoable).
    pub fn set_selection(&mut self, selection: Selection) {
        self.view.selection_out = Some(selection);
    }

    /// Asks the engine to change the edit context after commit (not undoable).
    pub fn set_context(&mut self, context: EditContext) {
        self.view.context_out = Some(context);
    }

    /// Takes the view changes requested by the command.
    pub fn take_view_changes(&mut self) -> (Option<Selection>, Option<EditContext>) {
        (self.view.selection_out.take(), self.view.context_out.take())
    }

    fn entities(&self, c: ContainerRef) -> Result<&Entities, CommandError> {
        self.doc
            .container(c)
            .ok_or_else(|| CommandError::NotApplicable(format!("container {c:?} does not exist")))
    }

    // ----- mesh ---------------------------------------------------------------

    /// Runs a kernel operation on a container's mesh, recording its deltas,
    /// then binds pids for created edges/faces and unbinds removed ones.
    pub fn with_mesh<R>(
        &mut self,
        container: ContainerRef,
        op: impl FnOnce(&mut HalfEdgeMesh, &Tolerances, &mut dyn DeltaSink) -> Result<R, GeometryError>,
    ) -> Result<R, CommandError> {
        let tol = *self.doc.tolerances();
        let mut recorded: Vec<MeshDelta> = Vec::new();
        let result = op(
            container_mut(self.doc, container)?.mesh_mut(),
            &tol,
            &mut recorded,
        )?;
        self.absorb_mesh_deltas(container, recorded)?;
        Ok(result)
    }

    /// Pid and side-table bookkeeping after a kernel operation (module
    /// documentation).
    fn absorb_mesh_deltas(
        &mut self,
        container: ContainerRef,
        recorded: Vec<MeshDelta>,
    ) -> Result<(), CommandError> {
        let mut removed: Vec<EntityId> = Vec::new();
        let mut added: Vec<EntityId> = Vec::new();
        let mut removed_edges: Vec<(EntityId, Segment)> = Vec::new();
        let mut split_from: BTreeMap<FaceId, FaceId> = BTreeMap::new();
        for d in &recorded {
            match d {
                MeshDelta::EdgeRemoved { id, a, b, .. } => {
                    removed.push(EntityId::Edge(*id));
                    removed_edges.push((EntityId::Edge(*id), Segment::new(*a, *b)));
                }
                MeshDelta::FaceRemoved { id, .. } => removed.push(EntityId::Face(*id)),
                MeshDelta::EdgeAdded { id, .. } => added.push(EntityId::Edge(*id)),
                MeshDelta::FaceAdded { id, .. } => added.push(EntityId::Face(*id)),
                MeshDelta::FaceSplit { old, new } => {
                    // A piece of a piece inherits from the face that existed
                    // before the operation.
                    let root = split_from.get(old).copied().unwrap_or(*old);
                    split_from.insert(new[1], root);
                }
                _ => {}
            }
        }
        let tol = *self.doc.tolerances();
        let entities = self.entities(container)?;
        let mut seen = BTreeSet::new();
        let vanished: Vec<EntityId> = removed
            .into_iter()
            .filter(|id| seen.insert(*id))
            .filter(|id| !entities.contains(*id) && entities.pid_of(*id).is_some())
            .collect();
        let mut seen = BTreeSet::new();
        let appeared: Vec<EntityId> = added
            .into_iter()
            .filter(|id| seen.insert(*id))
            .filter(|id| entities.contains(*id) && entities.pid_of(*id).is_none())
            .collect();

        // Rows of vanished elements, and what new pieces inherit.
        let mut rows: BTreeMap<EntityId, SideRows> = BTreeMap::new();
        for &id in &vanished {
            rows.insert(
                id,
                SideRows {
                    tag: entities.tag_of(id),
                    hidden: entities.is_hidden(id),
                    attrs: entities.attrs(id).cloned(),
                },
            );
        }
        let mut inherit: BTreeMap<EntityId, (Option<TagId>, bool)> = BTreeMap::new();
        // Pieces of a vanished edge: (piece, length, starts at the old start).
        let mut pieces: BTreeMap<EntityId, Vec<(EntityId, f64, bool)>> = BTreeMap::new();
        for &id in &appeared {
            match id {
                EntityId::Face(f) => {
                    if let Some(&old) = split_from.get(&f) {
                        let old = EntityId::Face(old);
                        inherit.insert(id, (entities.tag_of(old), entities.is_hidden(old)));
                    }
                }
                EntityId::Edge(e) => {
                    let seg = entities.mesh().edge_segment(e)?;
                    let parent = removed_edges.iter().find(|(_, s)| {
                        tol.point_on_segment(seg.a, s) && tol.point_on_segment(seg.b, s)
                    });
                    if let Some((old, s)) = parent
                        && let Some(r) = rows.get(old)
                    {
                        inherit.insert(id, (r.tag, r.hidden));
                        let at_start = tol.same_point(seg.a, s.a) || tol.same_point(seg.b, s.a);
                        pieces
                            .entry(*old)
                            .or_default()
                            .push((id, seg.length(), at_start));
                    }
                }
                _ => {}
            }
        }
        // The longest piece of a split edge keeps its pid and attributes (ties:
        // the piece at the old start); the other pieces are fragments of an
        // existing edge, not something the command created (docs/08 Decisions).
        let mut keeps: BTreeMap<EntityId, EntityId> = BTreeMap::new();
        let mut fragments: BTreeSet<EntityId> = BTreeSet::new();
        // Pieces of an edge this transaction made are made by it too.
        let made_here: BTreeSet<Pid> = self.created.iter().copied().collect();
        for (old, list) in &pieces {
            let mut best: Option<(EntityId, f64, bool)> = None;
            for &(piece, len, at_start) in list {
                let better = match best {
                    None => true,
                    Some((_, best_len, best_start)) => {
                        tol.definitely_lt_len(best_len, len)
                            || (tol.approx_eq_len(len, best_len) && at_start && !best_start)
                    }
                };
                if better {
                    best = Some((piece, len, at_start));
                }
            }
            if let Some((keeper, _, _)) = best {
                keeps.insert(keeper, *old);
                if !entities
                    .pid_of(*old)
                    .is_some_and(|p| made_here.contains(&p))
                {
                    fragments.extend(list.iter().map(|p| p.0).filter(|p| *p != keeper));
                }
            }
        }
        // Old edge → (unbind sequence, pid, order index, attributes).
        type KeptRow = (usize, Pid, usize, Option<AttributeDictionaries>);
        let mut kept_rows: BTreeMap<EntityId, KeptRow> = BTreeMap::new();

        // 1. Clears and unbinds of vanished elements (recorded first).
        let mut pre = Vec::new();
        for id in vanished {
            let r = rows.remove(&id).unwrap_or(SideRows {
                tag: None,
                hidden: false,
                attrs: None,
            });
            if let Some(attrs) = &r.attrs {
                for (dict, entries) in &attrs.0 {
                    for (key, value) in entries {
                        pre.push(Delta::AttributeChanged {
                            target: AttrTarget::Entity { container, id },
                            dict: dict.clone(),
                            key: key.clone(),
                            from: Some(value.clone()),
                            to: None,
                        });
                    }
                }
            }
            if r.hidden {
                pre.push(Delta::HiddenChanged {
                    container,
                    id,
                    to: false,
                });
            }
            if r.tag.is_some() {
                pre.push(Delta::TagChanged {
                    container,
                    id,
                    from: r.tag,
                    to: None,
                });
            }
            let (pid, order_index) = container_mut(self.doc, container)?
                .unbind_pid(id)
                .map_err(hierarchy)?;
            if keeps.values().any(|old| *old == id) {
                let seq = kept_rows.len();
                kept_rows.insert(id, (seq, pid, order_index, r.attrs.clone()));
            }
            pre.push(Delta::PidUnbound {
                container,
                id,
                pid,
                order_index,
            });
        }
        self.deltas.extend(pre);

        // 2. The kernel deltas.
        self.deltas.extend(
            recorded
                .into_iter()
                .map(|delta| Delta::Mesh { container, delta }),
        );

        // 3. Binds of new elements, then inherited rows.
        // Pieces that keep a pid go back to their order slots first, in reverse
        // unbind order, so the order list is restored exactly.
        let unbind_seq = |id: &EntityId| {
            keeps
                .get(id)
                .and_then(|old| kept_rows.get(old))
                .map(|row| row.0)
        };
        let mut appeared = appeared;
        appeared.sort_by_key(|id| std::cmp::Reverse(unbind_seq(id)));
        for id in appeared {
            let kept = keeps.get(&id).and_then(|old| kept_rows.remove(old));
            let (pid, order_index, attrs) = match kept {
                Some((_, pid, index, attrs)) => (pid, index, attrs),
                None => (self.doc.allocate_pid(), usize::MAX, None),
            };
            let entities = container_mut(self.doc, container)?;
            let order_index = order_index.min(entities.len());
            entities
                .bind_pid(id, pid, Some(order_index))
                .map_err(hierarchy)?;
            self.deltas.push(Delta::PidBound {
                container,
                id,
                pid,
                order_index,
            });
            if !keeps.contains_key(&id) && !fragments.contains(&id) {
                self.created.push(pid);
            }
            for (dict, entries) in attrs.iter().flat_map(|a| a.0.iter()) {
                for (key, value) in entries {
                    let target = AttrTarget::Entity { container, id };
                    self.set_attribute(target, dict, key, Some(value.clone()))?;
                }
            }
            if let Some((tag, hidden)) = inherit.remove(&id) {
                if tag.is_some() {
                    self.set_tag(container, id, tag)?;
                }
                if hidden {
                    self.set_hidden(container, id, true)?;
                }
            }
        }
        Ok(())
    }

    /// Exchanges the pids (and order slots and attributes) of two mesh
    /// entities of one container: identity moves from one to the other
    /// (docs/05 Decisions, drawing on a face).
    pub fn swap_pids(
        &mut self,
        container: ContainerRef,
        a: EntityId,
        b: EntityId,
    ) -> Result<(), CommandError> {
        if a == b {
            return Ok(());
        }
        let entities = self.entities(container)?;
        let (attrs_a, attrs_b) = (entities.attrs(a).cloned(), entities.attrs(b).cloned());
        let entities = container_mut(self.doc, container)?;
        let (pa, ia) = entities.unbind_pid(a).map_err(hierarchy)?;
        let (pb, ib) = entities.unbind_pid(b).map_err(hierarchy)?;
        // Re-bind in reverse unbind order so the slots come back exactly.
        entities.bind_pid(a, pb, Some(ib)).map_err(hierarchy)?;
        entities.bind_pid(b, pa, Some(ia)).map_err(hierarchy)?;
        self.deltas.extend([
            Delta::PidUnbound {
                container,
                id: a,
                pid: pa,
                order_index: ia,
            },
            Delta::PidUnbound {
                container,
                id: b,
                pid: pb,
                order_index: ib,
            },
            Delta::PidBound {
                container,
                id: a,
                pid: pb,
                order_index: ib,
            },
            Delta::PidBound {
                container,
                id: b,
                pid: pa,
                order_index: ia,
            },
        ]);
        // Attributes belong to the identity: they follow the pids.
        for (id, old, new) in [(a, &attrs_a, &attrs_b), (b, &attrs_b, &attrs_a)] {
            let keys: BTreeSet<(String, String)> = old
                .iter()
                .chain(new.iter())
                .flat_map(|x| {
                    x.0.iter()
                        .flat_map(|(d, e)| e.keys().map(move |k| (d.clone(), k.clone())))
                })
                .collect();
            for (dict, key) in keys {
                let value = new
                    .as_ref()
                    .and_then(|x| x.0.get(&dict))
                    .and_then(|e| e.get(&key));
                let target = AttrTarget::Entity { container, id };
                self.set_attribute(target, &dict, &key, value.cloned())?;
            }
        }
        Ok(())
    }

    /// Sets a face's materials/UVs.
    pub fn set_face_sides(
        &mut self,
        container: ContainerRef,
        face: FaceId,
        sides: FaceSides,
    ) -> Result<(), CommandError> {
        for side in [sides.material_front, sides.material_back]
            .into_iter()
            .flatten()
        {
            if self.doc.materials.get(side).is_none() {
                return Err(CommandError::invalid_param(
                    "material",
                    format!("{side} does not exist"),
                ));
            }
        }
        self.with_mesh(container, |mesh, _, d| mesh.set_face_sides(face, sides, d))
    }

    // ----- entities ---------------------------------------------------------------

    /// Inserts a non-mesh entity, allocating its pid.
    pub fn insert_entity(
        &mut self,
        container: ContainerRef,
        record: EntityRecord,
    ) -> Result<(EntityId, Pid), CommandError> {
        self.check_record(container, &record)?;
        let pid = self.doc.allocate_pid();
        let entities = container_mut(self.doc, container)?;
        let id = entities.insert(record.clone(), pid).map_err(hierarchy)?;
        let order_index = entities.len() - 1;
        self.deltas.push(Delta::EntityInserted {
            container,
            id,
            pid,
            order_index,
            record: Box::new(record),
        });
        self.created.push(pid);
        Ok((id, pid))
    }

    /// Groups and instances must name an existing definition and must not
    /// nest a definition inside itself (docs/04 §13).
    fn check_record(
        &self,
        container: ContainerRef,
        record: &EntityRecord,
    ) -> Result<(), CommandError> {
        let definition = match record {
            EntityRecord::Group(g) => g.definition,
            EntityRecord::Instance(i) => i.definition,
            _ => return Ok(()),
        };
        if self.doc.definition(definition).is_none() {
            return Err(CommandError::invalid_param(
                "definition",
                format!("{definition} does not exist"),
            ));
        }
        if let ContainerRef::Definition(host) = container
            && self.definition_reaches(definition, host)
        {
            return Err(CommandError::NotApplicable(
                "a component cannot contain itself".into(),
            ));
        }
        Ok(())
    }

    /// `true` when `from` is `target` or nests it at any depth.
    fn definition_reaches(&self, from: DefinitionId, target: DefinitionId) -> bool {
        let mut stack = vec![from];
        let mut seen = BTreeSet::new();
        while let Some(d) = stack.pop() {
            if d == target {
                return true;
            }
            if !seen.insert(d) {
                continue;
            }
            if let Some(def) = self.doc.definition(d) {
                stack.extend(def.entities.groups().map(|(_, g)| g.definition));
                stack.extend(def.entities.instances().map(|(_, i)| i.definition));
            }
        }
        false
    }

    /// Records the clearing of an entity's tag, hidden and locked state and
    /// attributes (so that undo of a removal restores them), then clears them.
    fn clear_side_rows(
        &mut self,
        container: ContainerRef,
        id: EntityId,
    ) -> Result<(), CommandError> {
        let entities = self.entities(container)?;
        let attrs = entities.attrs(id).cloned();
        let locked = entities.is_locked(id);
        if let Some(attrs) = attrs {
            for (dict, entries) in &attrs.0 {
                for key in entries.keys() {
                    self.set_attribute(AttrTarget::Entity { container, id }, dict, key, None)?;
                }
            }
        }
        if locked {
            self.set_locked(container, id, false)?;
        }
        self.set_hidden(container, id, false)?;
        self.set_tag(container, id, None)
    }

    /// Removes a non-mesh entity.
    pub fn remove_entity(
        &mut self,
        container: ContainerRef,
        id: EntityId,
    ) -> Result<EntityRecord, CommandError> {
        if !self.entities(container)?.contains(id) {
            return Err(hierarchy(HierarchyError::NotFound(id)));
        }
        self.clear_side_rows(container, id)?;
        let (record, pid, order_index) = container_mut(self.doc, container)?
            .remove(id)
            .map_err(hierarchy)?;
        self.deltas.push(Delta::EntityRemoved {
            container,
            id,
            pid,
            order_index,
            record: Box::new(record.clone()),
        });
        Ok(record)
    }

    /// Replaces a non-mesh entity's data.
    pub fn replace_entity(
        &mut self,
        container: ContainerRef,
        id: EntityId,
        record: EntityRecord,
    ) -> Result<(), CommandError> {
        self.check_record(container, &record)?;
        let from = container_mut(self.doc, container)?
            .replace(id, record.clone())
            .map_err(hierarchy)?;
        if from != record {
            self.deltas.push(Delta::EntityChanged {
                container,
                id,
                from: Box::new(from),
                to: Box::new(record),
            });
        }
        Ok(())
    }

    /// Sets a transform.
    pub fn set_transform(
        &mut self,
        container: ContainerRef,
        id: EntityId,
        transform: DAffine3,
    ) -> Result<(), CommandError> {
        if !transform.is_finite() || transform.matrix3.determinant().abs() <= f64::EPSILON {
            return Err(CommandError::invalid_param(
                "transform",
                "the transform must be finite and invertible",
            ));
        }
        let from = container_mut(self.doc, container)?
            .set_transform(id, transform)
            .map_err(hierarchy)?;
        if from != transform {
            self.deltas.push(Delta::TransformChanged {
                container,
                id,
                from,
                to: transform,
            });
        }
        Ok(())
    }

    /// Sets a tag (`None` = untagged; the `Untagged` tag id is stored as `None`).
    pub fn set_tag(
        &mut self,
        container: ContainerRef,
        id: EntityId,
        tag: Option<TagId>,
    ) -> Result<(), CommandError> {
        let tag = tag.filter(|t| *t != self.doc.tags.untagged());
        if let Some(t) = tag
            && self.doc.tags.get(t).is_none()
        {
            return Err(CommandError::invalid_param(
                "tag",
                format!("{t} does not exist"),
            ));
        }
        let from = container_mut(self.doc, container)?
            .set_tag(id, tag)
            .map_err(hierarchy)?;
        if from != tag {
            self.deltas.push(Delta::TagChanged {
                container,
                id,
                from,
                to: tag,
            });
        }
        Ok(())
    }

    /// Sets hidden.
    pub fn set_hidden(
        &mut self,
        container: ContainerRef,
        id: EntityId,
        hidden: bool,
    ) -> Result<(), CommandError> {
        let before = container_mut(self.doc, container)?
            .set_hidden(id, hidden)
            .map_err(hierarchy)?;
        if before != hidden {
            self.deltas.push(Delta::HiddenChanged {
                container,
                id,
                to: hidden,
            });
        }
        Ok(())
    }

    /// Sets locked (groups/instances only).
    pub fn set_locked(
        &mut self,
        container: ContainerRef,
        id: EntityId,
        locked: bool,
    ) -> Result<(), CommandError> {
        let before = container_mut(self.doc, container)?
            .set_locked(id, locked)
            .map_err(hierarchy)?;
        if before != locked {
            self.deltas.push(Delta::LockedChanged {
                container,
                id,
                to: locked,
            });
        }
        Ok(())
    }

    /// Sets or clears (`None`) one attribute value.
    pub fn set_attribute(
        &mut self,
        target: AttrTarget,
        dict: &str,
        key: &str,
        value: Option<Value>,
    ) -> Result<(), CommandError> {
        let from = write_attribute(self.doc, target, dict, key, value.clone())?;
        if from != value {
            self.deltas.push(Delta::AttributeChanged {
                target,
                dict: dict.to_string(),
                key: key.to_string(),
                from,
                to: value,
            });
        }
        Ok(())
    }

    // ----- document-level collections ----------------------------------------------

    /// Adds a definition.
    pub fn insert_definition(
        &mut self,
        definition: ComponentDefinition,
    ) -> Result<DefinitionId, CommandError> {
        let id = self.doc.add_definition(definition.clone());
        self.deltas.push(Delta::DefinitionInserted {
            id,
            order_index: self.doc.definition_order.len() - 1,
            definition: Box::new(definition),
        });
        Ok(id)
    }

    /// Removes an unreferenced definition (`purge-unused`, explode).
    pub fn remove_definition(
        &mut self,
        id: DefinitionId,
    ) -> Result<ComponentDefinition, CommandError> {
        if self.doc.definition(id).is_none() {
            return Err(CommandError::invalid_param(
                "definition",
                format!("{id} does not exist"),
            ));
        }
        let referenced = self.doc.containers().into_iter().any(|c| {
            self.doc.container(c).is_some_and(|e| {
                e.groups().any(|(_, g)| g.definition == id)
                    || e.instances().any(|(_, i)| i.definition == id)
            })
        });
        if referenced {
            return Err(CommandError::NotApplicable(format!(
                "{id} is still used by a group or instance"
            )));
        }
        let (definition, order_index) = self
            .doc
            .remove_definition(id)
            .ok_or_else(|| internal(format!("{id} vanished")))?;
        self.deltas.push(Delta::DefinitionRemoved {
            id,
            order_index,
            definition: Box::new(definition.clone()),
        });
        Ok(definition)
    }

    /// Replaces a definition's metadata and behavior (its entities are kept).
    pub fn update_definition(
        &mut self,
        id: DefinitionId,
        mut definition: ComponentDefinition,
    ) -> Result<(), CommandError> {
        definition.entities = Entities::new();
        let current = self.doc.definitions.get_mut(id).ok_or_else(|| {
            CommandError::invalid_param("definition", format!("{id} does not exist"))
        })?;
        let entities = std::mem::take(&mut current.entities);
        let from = std::mem::replace(current, definition.clone());
        current.entities = entities;
        self.deltas.push(Delta::DefinitionChanged {
            id,
            from: Box::new(from),
            to: Box::new(definition),
        });
        Ok(())
    }

    /// Adds a material.
    pub fn insert_material(&mut self, material: Material) -> Result<MaterialId, CommandError> {
        let id = self
            .doc
            .materials
            .insert(material)
            .map_err(|e| CommandError::invalid_param("name", e.to_string()))?;
        let stored = self
            .doc
            .materials
            .get(id)
            .cloned()
            .ok_or_else(|| internal("lost material"))?;
        self.deltas.push(Delta::MaterialInserted {
            id,
            order_index: self.doc.materials.position(id).unwrap_or(0),
            material: Box::new(stored),
        });
        Ok(id)
    }

    /// Replaces a material.
    pub fn replace_material(
        &mut self,
        id: MaterialId,
        material: Material,
    ) -> Result<(), CommandError> {
        let from = self
            .doc
            .materials
            .replace(id, material)
            .map_err(|e| CommandError::invalid_param("material", e.to_string()))?;
        let to = self
            .doc
            .materials
            .get(id)
            .cloned()
            .ok_or_else(|| internal("lost material"))?;
        if from != to {
            self.deltas.push(Delta::MaterialChanged {
                id,
                from: Box::new(from),
                to: Box::new(to),
            });
        }
        Ok(())
    }

    /// Removes a material (faces using it fall back to default).
    pub fn remove_material(&mut self, id: MaterialId) -> Result<Material, CommandError> {
        if self.doc.materials.get(id).is_none() {
            return Err(CommandError::invalid_param(
                "material",
                format!("{id} does not exist"),
            ));
        }
        for c in self.doc.containers() {
            let entities = self.entities(c)?;
            let faces: Vec<(FaceId, FaceSides)> = entities
                .mesh()
                .faces()
                .filter(|(_, f)| {
                    f.sides.material_front == Some(id) || f.sides.material_back == Some(id)
                })
                .map(|(f, face)| {
                    let mut sides = face.sides.clone();
                    if sides.material_front == Some(id) {
                        sides.material_front = None;
                    }
                    if sides.material_back == Some(id) {
                        sides.material_back = None;
                    }
                    (f, sides)
                })
                .collect();
            let instances: Vec<(EntityId, EntityRecord)> = entities
                .instances()
                .filter(|(_, i)| i.material_override == Some(id))
                .map(|(k, i)| {
                    let mut i = i.clone();
                    i.material_override = None;
                    (EntityId::Instance(k), EntityRecord::Instance(i))
                })
                .collect();
            for (f, sides) in faces {
                self.with_mesh(c, |mesh, _, d| mesh.set_face_sides(f, sides, d))?;
            }
            for (k, record) in instances {
                self.replace_entity(c, k, record)?;
            }
        }
        let order_index = self.doc.materials.position(id).unwrap_or(0);
        let material = self
            .doc
            .materials
            .remove(id)
            .map_err(|e| CommandError::NotApplicable(e.to_string()))?;
        self.deltas.push(Delta::MaterialRemoved {
            id,
            order_index,
            material: Box::new(material.clone()),
        });
        Ok(material)
    }

    /// Adds a tag.
    pub fn insert_tag(&mut self, tag: Tag) -> Result<TagId, CommandError> {
        let id = self
            .doc
            .tags
            .insert(tag.clone())
            .map_err(|e| CommandError::invalid_param("name", e.to_string()))?;
        self.deltas.push(Delta::TagInserted {
            id,
            order_index: self.doc.tags.position(id).unwrap_or(0),
            tag,
        });
        Ok(id)
    }

    /// Replaces a tag.
    pub fn replace_tag(&mut self, id: TagId, tag: Tag) -> Result<(), CommandError> {
        let from = self
            .doc
            .tags
            .replace(id, tag.clone())
            .map_err(|e| CommandError::invalid_param("tag", e.to_string()))?;
        if from != tag {
            self.deltas.push(Delta::TagUpdated { id, from, to: tag });
        }
        Ok(())
    }

    /// Removes a tag; entities carrying it become untagged.
    pub fn remove_tag(&mut self, id: TagId) -> Result<Tag, CommandError> {
        if id == self.doc.tags.untagged() || self.doc.tags.get(id).is_none() {
            return Err(CommandError::invalid_param(
                "tag",
                "the tag does not exist or cannot be removed",
            ));
        }
        for c in self.doc.containers() {
            let tagged = self.entities(c)?.with_tag(id);
            for e in tagged {
                self.set_tag(c, e, None)?;
            }
        }
        let order_index = self.doc.tags.position(id).unwrap_or(0);
        let tag = self
            .doc
            .tags
            .remove(id)
            .map_err(|e| CommandError::NotApplicable(e.to_string()))?;
        self.deltas.push(Delta::TagRemoved {
            id,
            order_index,
            tag: tag.clone(),
        });
        Ok(tag)
    }

    /// Adds a tag folder.
    pub fn insert_tag_folder(&mut self, folder: TagFolder) -> Result<TagFolderId, CommandError> {
        let id = self
            .doc
            .tags
            .insert_folder(folder.clone())
            .map_err(|e| CommandError::invalid_param("folder", e.to_string()))?;
        self.deltas.push(Delta::TagFolderInserted {
            id,
            order_index: self.doc.tags.folder_position(id).unwrap_or(0),
            folder,
        });
        Ok(id)
    }

    /// Removes an empty tag folder.
    pub fn remove_tag_folder(&mut self, id: TagFolderId) -> Result<TagFolder, CommandError> {
        let order_index = self.doc.tags.folder_position(id).unwrap_or(0);
        let folder = self
            .doc
            .tags
            .remove_folder(id)
            .map_err(|e| CommandError::NotApplicable(e.to_string()))?;
        self.deltas.push(Delta::TagFolderRemoved {
            id,
            order_index,
            folder: folder.clone(),
        });
        Ok(folder)
    }

    /// Adds a scene.
    pub fn insert_scene(&mut self, scene: Scene) -> Result<SceneId, CommandError> {
        if self.doc.scenes.len() >= crate::document::Scenes::MAX {
            return Err(CommandError::NotApplicable("too many scenes".into()));
        }
        let id = self.doc.scenes.insert(scene.clone());
        self.deltas.push(Delta::SceneInserted {
            id,
            order_index: self.doc.scenes.index_of(id).unwrap_or(0),
            scene: Box::new(scene),
        });
        Ok(id)
    }

    /// Replaces a scene.
    pub fn replace_scene(&mut self, id: SceneId, scene: Scene) -> Result<(), CommandError> {
        let from =
            self.doc.scenes.replace(id, scene.clone()).ok_or_else(|| {
                CommandError::invalid_param("scene", format!("{id} does not exist"))
            })?;
        if from != scene {
            self.deltas.push(Delta::SceneChanged {
                id,
                from: Box::new(from),
                to: Box::new(scene),
            });
        }
        Ok(())
    }

    /// Removes a scene.
    pub fn remove_scene(&mut self, id: SceneId) -> Result<Scene, CommandError> {
        let (scene, order_index) =
            self.doc.scenes.remove(id).ok_or_else(|| {
                CommandError::invalid_param("scene", format!("{id} does not exist"))
            })?;
        self.deltas.push(Delta::SceneRemoved {
            id,
            order_index,
            scene: Box::new(scene.clone()),
        });
        Ok(scene)
    }

    /// Moves a scene in the list.
    pub fn move_scene(&mut self, id: SceneId, index: usize) -> Result<(), CommandError> {
        let from_index =
            self.doc.scenes.index_of(id).ok_or_else(|| {
                CommandError::invalid_param("scene", format!("{id} does not exist"))
            })?;
        self.doc
            .scenes
            .reorder(id, index)
            .ok_or_else(|| internal("scene vanished"))?;
        let to_index = self.doc.scenes.index_of(id).unwrap_or(from_index);
        if to_index != from_index {
            self.deltas.push(Delta::SceneMoved {
                id,
                from_index,
                to_index,
            });
        }
        Ok(())
    }

    /// Adds a style.
    pub fn insert_style(&mut self, style: Style) -> Result<StyleId, CommandError> {
        let id = self.doc.styles.insert(style.clone());
        self.deltas.push(Delta::StyleInserted {
            id,
            order_index: self.doc.styles.position(id).unwrap_or(0),
            style: Box::new(style),
        });
        Ok(id)
    }

    /// Replaces a style.
    pub fn replace_style(&mut self, id: StyleId, style: Style) -> Result<(), CommandError> {
        let from =
            self.doc.styles.replace(id, style.clone()).ok_or_else(|| {
                CommandError::invalid_param("style", format!("{id} does not exist"))
            })?;
        if from != style {
            self.deltas.push(Delta::StyleChanged {
                id,
                from: Box::new(from),
                to: Box::new(style),
            });
        }
        Ok(())
    }

    /// Removes a style that is not active.
    pub fn remove_style(&mut self, id: StyleId) -> Result<Style, CommandError> {
        let order_index =
            self.doc.styles.position(id).ok_or_else(|| {
                CommandError::invalid_param("style", format!("{id} does not exist"))
            })?;
        let style = self.doc.styles.remove(id).ok_or_else(|| {
            CommandError::NotApplicable("the active style cannot be removed".into())
        })?;
        self.deltas.push(Delta::StyleRemoved {
            id,
            order_index,
            style: Box::new(style.clone()),
        });
        Ok(style)
    }

    /// Activates a style.
    pub fn set_active_style(&mut self, id: StyleId) -> Result<(), CommandError> {
        let from =
            self.doc.styles.set_active(id).ok_or_else(|| {
                CommandError::invalid_param("style", format!("{id} does not exist"))
            })?;
        if from != id {
            self.deltas.push(Delta::ActiveStyleChanged { from, to: id });
        }
        Ok(())
    }

    /// Replaces model info (units, title, tolerances…). `next_pid` is never
    /// rewound: the current counter is kept.
    pub fn set_info(&mut self, mut info: ModelInfo) -> Result<(), CommandError> {
        info.tolerances
            .validate()
            .map_err(|e| CommandError::invalid_param("tolerances", e.to_string()))?;
        info.next_pid = self.doc.info.next_pid;
        if info != self.doc.info {
            let from = std::mem::replace(&mut self.doc.info, info.clone());
            self.deltas.push(Delta::InfoChanged {
                from: Box::new(from),
                to: Box::new(info),
            });
        }
        Ok(())
    }

    /// Replaces rendering options.
    pub fn set_rendering(&mut self, rendering: RenderingOptions) -> Result<(), CommandError> {
        if rendering != self.doc.rendering {
            let from = std::mem::replace(&mut self.doc.rendering, rendering.clone());
            self.deltas.push(Delta::RenderingChanged {
                from: Box::new(from),
                to: Box::new(rendering),
            });
        }
        Ok(())
    }

    // ----- lifecycle --------------------------------------------------------------

    /// Finishes the transaction; the document already holds the changes.
    pub fn commit(self) -> CommittedTransaction {
        CommittedTransaction {
            label: self.label,
            deltas: self.deltas,
            warnings: self.warnings,
            created: self.created,
        }
    }

    /// Reverts every recorded delta in reverse order (apply-or-rollback for
    /// failed commands, docs/01 §3.1). Pass the session's alias table: the
    /// replay re-creates edges and faces under new ids, and older history
    /// entries must find them through their alias classes.
    pub fn rollback(self, aliases: &mut MeshAliases) -> Result<(), CommandError> {
        for delta in self.deltas.iter().rev() {
            apply_delta(self.doc, &delta.inverse(), aliases)?;
        }
        Ok(())
    }
}

/// One undo step: one or more transactions grouped under a label (docs/01 §3.2
/// "grouped by user action").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoEntry {
    /// Label shown in the Edit menu.
    pub label: String,
    /// Transactions in order.
    pub transactions: Vec<CommittedTransaction>,
}

/// Linear undo/redo history with optional grouping (docs/01 §3.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UndoStack {
    entries: Vec<UndoEntry>,
    /// Number of entries currently applied (`entries[..cursor]`).
    cursor: usize,
    open_group: Option<UndoEntry>,
    /// Maximum entries kept (`None` = unlimited).
    limit: Option<usize>,
}

impl UndoStack {
    /// Empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Empty history keeping at most `limit` entries.
    pub fn with_limit(limit: usize) -> Self {
        UndoStack {
            limit: Some(limit),
            ..Self::default()
        }
    }

    /// Records a committed transaction: into the open group if any, otherwise
    /// as a new entry; discards the redo tail.
    pub fn push(&mut self, tx: CommittedTransaction) {
        if let Some(group) = &mut self.open_group {
            group.transactions.push(tx);
            return;
        }
        self.entries.truncate(self.cursor);
        self.entries.push(UndoEntry {
            label: tx.label.clone(),
            transactions: vec![tx],
        });
        if let Some(limit) = self.limit
            && self.entries.len() > limit
        {
            let excess = self.entries.len() - limit;
            self.entries.drain(..excess);
        }
        self.cursor = self.entries.len();
    }

    /// Starts a group; subsequent pushes go into it until `end_group`.
    pub fn begin_group(&mut self, label: &str) -> bool {
        if self.open_group.is_some() {
            return false;
        }
        self.open_group = Some(UndoEntry {
            label: label.to_string(),
            transactions: Vec::new(),
        });
        true
    }

    /// `true` while a group is open.
    pub fn group_open(&self) -> bool {
        self.open_group.is_some()
    }

    /// Closes the group and records it as one entry (nothing if it is empty).
    pub fn end_group(&mut self) -> bool {
        let Some(group) = self.open_group.take() else {
            return false;
        };
        if group.transactions.is_empty() {
            return true;
        }
        self.entries.truncate(self.cursor);
        self.entries.push(group);
        self.cursor = self.entries.len();
        true
    }

    /// `true` when an entry can be undone.
    pub fn can_undo(&self) -> bool {
        self.cursor > 0 && self.open_group.is_none()
    }

    /// `true` when an entry can be redone.
    pub fn can_redo(&self) -> bool {
        self.cursor < self.entries.len() && self.open_group.is_none()
    }

    /// The entry the next undo inverts, without moving the cursor.
    pub fn undo_entry(&self) -> Option<&UndoEntry> {
        self.can_undo()
            .then(|| self.entries.get(self.cursor - 1))
            .flatten()
    }

    /// The entry the next redo re-applies, without moving the cursor.
    pub fn redo_entry(&self) -> Option<&UndoEntry> {
        self.can_redo()
            .then(|| self.entries.get(self.cursor))
            .flatten()
    }

    /// Moves the cursor back and returns the entry to invert.
    pub fn step_back(&mut self) -> Option<&UndoEntry> {
        if !self.can_undo() {
            return None;
        }
        self.cursor -= 1;
        self.entries.get(self.cursor)
    }

    /// Moves the cursor forward and returns the entry to re-apply.
    pub fn step_forward(&mut self) -> Option<&UndoEntry> {
        if !self.can_redo() {
            return None;
        }
        let entry = self.entries.get(self.cursor);
        self.cursor += 1;
        entry
    }

    /// Label of the next undo, if any.
    pub fn undo_label(&self) -> Option<&str> {
        self.can_undo()
            .then(|| self.entries[self.cursor - 1].label.as_str())
    }

    /// Label of the next redo, if any.
    pub fn redo_label(&self) -> Option<&str> {
        self.can_redo()
            .then(|| self.entries[self.cursor].label.as_str())
    }

    /// All entries (applied first, then the redo tail).
    pub fn entries(&self) -> &[UndoEntry] {
        &self.entries
    }

    /// Number of applied entries.
    pub fn applied(&self) -> usize {
        self.cursor
    }

    /// Clears everything.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.cursor = 0;
        self.open_group = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(label: &str) -> CommittedTransaction {
        CommittedTransaction {
            label: label.into(),
            deltas: Vec::new(),
            warnings: Vec::new(),
            created: Vec::new(),
        }
    }

    #[test]
    fn undo_redo_cursor_moves() {
        let mut h = UndoStack::new();
        h.push(tx("a"));
        h.push(tx("b"));
        assert_eq!(h.undo_label(), Some("b"));
        assert_eq!(h.step_back().map(|e| e.label.as_str()), Some("b"));
        assert_eq!(h.redo_label(), Some("b"));
        assert_eq!(h.step_forward().map(|e| e.label.as_str()), Some("b"));
        assert!(h.step_forward().is_none());
        h.step_back();
        h.push(tx("c"));
        assert_eq!(
            h.entries()
                .iter()
                .map(|e| e.label.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert!(!h.can_redo());
    }

    #[test]
    fn groups_collapse_into_one_entry() {
        let mut h = UndoStack::new();
        assert!(h.begin_group("Make stairs"));
        assert!(!h.begin_group("nested"));
        h.push(tx("a"));
        h.push(tx("b"));
        assert!(!h.can_undo());
        assert!(h.end_group());
        assert_eq!(h.applied(), 1);
        assert_eq!(h.entries()[0].transactions.len(), 2);
        assert_eq!(h.undo_label(), Some("Make stairs"));
    }

    #[test]
    fn limit_drops_oldest() {
        let mut h = UndoStack::with_limit(2);
        h.push(tx("a"));
        h.push(tx("b"));
        h.push(tx("c"));
        assert_eq!(
            h.entries()
                .iter()
                .map(|e| e.label.as_str())
                .collect::<Vec<_>>(),
            ["b", "c"]
        );
        assert_eq!(h.applied(), 2);
    }

    #[test]
    fn transaction_commit_carries_label() {
        let mut doc = Document::new();
        let mut t = Transaction::new(&mut doc, "Test");
        t.warn(Warning::new(
            crate::diagnostics::WarningCode::W2001,
            "merged",
        ));
        let pid = t.allocate_pid();
        let committed = t.commit();
        assert_eq!(committed.label, "Test");
        assert_eq!(committed.warnings.len(), 1);
        assert_eq!(pid, Pid(1));
    }
}
