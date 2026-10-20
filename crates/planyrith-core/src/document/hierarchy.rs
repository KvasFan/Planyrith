//! Implements docs/04-document-model.md §3 (the `Entities` container), §12
//! (paths) and the instance-resolution helpers of docs/01 §4: one half-edge
//! mesh plus slot maps of non-mesh entities, an insertion-order list, and the
//! per-entity side tables (pid, tag, hidden, locked, attributes).
//!
//! Side tables are `BTreeMap`/`BTreeSet` keyed by [`EntityId`] (ordered, so
//! iteration is deterministic) and serialized as pair sequences; the spec's
//! `SecondaryMap` wording assumed a single key type (docs/04 Decisions).
//!
//! Owner key: `document`.

use std::collections::{BTreeMap, BTreeSet};

use glam::DAffine3;
use serde::{Deserialize, Serialize};

use crate::document::StableMap;
use crate::document::attributes::AttributeDictionaries;
use crate::document::entities::{
    ComponentDefinition, ComponentInstance, Dimension, Group, Guide, ImageEntity, SectionPlane,
    Text,
};
use crate::geom::{Edge, Face, HalfEdgeMesh};
use crate::ids::{
    DefinitionId, DimensionId, EntityId, EntityKind, GroupId, GuideId, ImageId, InstanceId, Pid,
    SectionPlaneId, TagId, TextId,
};
use crate::math::Aabb;

/// An owned non-mesh entity, used for insertion and for undo snapshots
/// (docs/01 §3.1 deltas). Edges and faces are created through mesh operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EntityRecord {
    /// Group.
    Group(Group),
    /// Component instance.
    Instance(ComponentInstance),
    /// Guide.
    Guide(Guide),
    /// Dimension.
    Dimension(Dimension),
    /// Screen text.
    Text(Text),
    /// Image entity.
    Image(ImageEntity),
    /// Section plane.
    SectionPlane(SectionPlane),
}

impl EntityRecord {
    /// The kind this record becomes when inserted.
    pub const fn kind(&self) -> EntityKind {
        match self {
            EntityRecord::Group(_) => EntityKind::Group,
            EntityRecord::Instance(_) => EntityKind::Instance,
            EntityRecord::Guide(_) => EntityKind::Guide,
            EntityRecord::Dimension(_) => EntityKind::Dimension,
            EntityRecord::Text(_) => EntityKind::Text,
            EntityRecord::Image(_) => EntityKind::Image,
            EntityRecord::SectionPlane(_) => EntityKind::SectionPlane,
        }
    }
}

/// A borrowed view of any entity in a container.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EntityView<'a> {
    /// Mesh edge.
    Edge(&'a Edge),
    /// Mesh face.
    Face(&'a Face),
    /// Group.
    Group(&'a Group),
    /// Component instance.
    Instance(&'a ComponentInstance),
    /// Guide.
    Guide(&'a Guide),
    /// Dimension.
    Dimension(&'a Dimension),
    /// Screen text.
    Text(&'a Text),
    /// Image entity.
    Image(&'a ImageEntity),
    /// Section plane.
    SectionPlane(&'a SectionPlane),
}

/// Per-kind entity counts (docs/08 §2 `info`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct EntityCounts {
    /// Mesh vertices.
    pub vertices: usize,
    /// Mesh edges.
    pub edges: usize,
    /// Mesh faces.
    pub faces: usize,
    /// Groups.
    pub groups: usize,
    /// Component instances.
    pub instances: usize,
    /// Guides.
    pub guides: usize,
    /// Dimensions.
    pub dimensions: usize,
    /// Texts.
    pub texts: usize,
    /// Images.
    pub images: usize,
    /// Section planes.
    pub section_planes: usize,
}

/// Errors from container bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HierarchyError {
    /// The id is not in this container.
    #[error("entity not found in container: {0}")]
    NotFound(EntityId),
    /// The pid is already bound in this container.
    #[error("pid already bound: {0}")]
    PidInUse(Pid),
    /// Edges and faces are managed through the mesh, not `insert`/`remove`.
    #[error("mesh entities are created and removed through mesh operations: {0}")]
    MeshEntity(EntityId),
    /// Only groups and instances can be locked (docs/04 §3 `locked`).
    #[error("only groups and instances can be locked: {0}")]
    NotLockable(EntityId),
    /// Nesting deeper than the hard limit of docs/04 §14.
    #[error("nesting depth {0} exceeds the limit")]
    TooDeep(usize),
    /// A definition contains an instance of itself, directly or not
    /// (docs/04 §13: cyclic nesting is forbidden).
    #[error("cyclic component nesting through {0}")]
    Cycle(DefinitionId),
    /// The record or operation does not match the entity's kind (e.g.
    /// replacing a group with a guide, setting the transform of a text).
    #[error("operation does not apply to this kind of entity: {0}")]
    KindMismatch(EntityId),
}

/// One container of entities (docs/04 §3): the model root or a definition's
/// contents. Insertion order across all kinds is kept in `order` and MUST be
/// preserved by save/load and undo/redo (docs/04 §2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entities {
    mesh: HalfEdgeMesh,
    groups: StableMap<GroupId, Group>,
    instances: StableMap<InstanceId, ComponentInstance>,
    guides: StableMap<GuideId, Guide>,
    dimensions: StableMap<DimensionId, Dimension>,
    texts: StableMap<TextId, Text>,
    images: StableMap<ImageId, ImageEntity>,
    section_planes: StableMap<SectionPlaneId, SectionPlane>,
    /// Insertion order across all kinds (edges and faces included once bound).
    order: Vec<EntityId>,
    #[serde(with = "crate::document::serde_seq")]
    pids: BTreeMap<EntityId, Pid>,
    by_pid: BTreeMap<Pid, EntityId>,
    #[serde(with = "crate::document::serde_seq")]
    attrs: BTreeMap<EntityId, AttributeDictionaries>,
    /// Tag per entity; absent = `Tags::untagged()` (docs/04 §5).
    #[serde(with = "crate::document::serde_seq")]
    tag_of: BTreeMap<EntityId, TagId>,
    hidden: BTreeSet<EntityId>,
    /// Groups/instances only.
    locked: BTreeSet<EntityId>,
}

impl Entities {
    /// Hard nesting limit (docs/04 §14).
    pub const MAX_DEPTH: usize = 256;

    /// An empty container.
    pub fn new() -> Self {
        Self::default()
    }

    // ----- mesh ---------------------------------------------------------------

    /// The container's mesh.
    pub fn mesh(&self) -> &HalfEdgeMesh {
        &self.mesh
    }

    /// Mutable mesh access, reserved for `commands::transaction` and file loading.
    pub(crate) fn mesh_mut(&mut self) -> &mut HalfEdgeMesh {
        &mut self.mesh
    }

    // ----- order and identity --------------------------------------------------

    /// Number of entities with a pid (edges/faces bound + non-mesh entities).
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// `true` when nothing has been inserted or bound.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Insertion order across all kinds.
    pub fn order(&self) -> &[EntityId] {
        &self.order
    }

    /// Entities in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.order.iter().copied()
    }

    /// Pid of an entity.
    pub fn pid_of(&self, id: EntityId) -> Option<Pid> {
        self.pids.get(&id).copied()
    }

    /// Entity with a pid.
    pub fn id_of(&self, pid: Pid) -> Option<EntityId> {
        self.by_pid.get(&pid).copied()
    }

    /// The id an [`EntityKey`](crate::document::EntityKey) denotes here, if
    /// it exists.
    pub fn key_to_id(&self, key: crate::document::EntityKey) -> Option<EntityId> {
        match key {
            crate::document::EntityKey::Id(id) => self.contains(id).then_some(id),
            crate::document::EntityKey::Pid(pid) => self.id_of(pid),
        }
    }

    /// `true` when the id exists in this container (mesh ids are checked in the mesh).
    pub fn contains(&self, id: EntityId) -> bool {
        match id {
            EntityId::Edge(e) => self.mesh.edge(e).is_some(),
            EntityId::Face(f) => self.mesh.face(f).is_some(),
            EntityId::Group(g) => self.groups.contains_key(g),
            EntityId::Instance(i) => self.instances.contains_key(i),
            EntityId::Guide(g) => self.guides.contains_key(g),
            EntityId::Dimension(d) => self.dimensions.contains_key(d),
            EntityId::Text(t) => self.texts.contains_key(t),
            EntityId::Image(i) => self.images.contains_key(i),
            EntityId::SectionPlane(s) => self.section_planes.contains_key(s),
        }
    }

    /// Inserts a non-mesh entity at the end of the order with the given pid.
    pub(crate) fn insert(
        &mut self,
        record: EntityRecord,
        pid: Pid,
    ) -> Result<EntityId, HierarchyError> {
        self.insert_at(record, pid, self.order.len())
    }

    /// Inserts a non-mesh entity at `order_index` (undo of a removal).
    pub(crate) fn insert_at(
        &mut self,
        record: EntityRecord,
        pid: Pid,
        order_index: usize,
    ) -> Result<EntityId, HierarchyError> {
        if self.by_pid.contains_key(&pid) {
            return Err(HierarchyError::PidInUse(pid));
        }
        let id = match record {
            EntityRecord::Group(g) => EntityId::Group(self.groups.insert(g)),
            EntityRecord::Instance(i) => EntityId::Instance(self.instances.insert(i)),
            EntityRecord::Guide(g) => EntityId::Guide(self.guides.insert(g)),
            EntityRecord::Dimension(d) => EntityId::Dimension(self.dimensions.insert(d)),
            EntityRecord::Text(t) => EntityId::Text(self.texts.insert(t)),
            EntityRecord::Image(i) => EntityId::Image(self.images.insert(i)),
            EntityRecord::SectionPlane(s) => EntityId::SectionPlane(self.section_planes.insert(s)),
        };
        self.order.insert(order_index.min(self.order.len()), id);
        self.pids.insert(id, pid);
        self.by_pid.insert(pid, id);
        Ok(id)
    }

    /// Puts a removed non-mesh entity back under its old id, pid and order
    /// position (undo of [`Entities::remove`]; ids are stable, docs/04
    /// Decisions).
    pub(crate) fn restore(
        &mut self,
        id: EntityId,
        record: EntityRecord,
        pid: Pid,
        order_index: usize,
    ) -> Result<(), HierarchyError> {
        if self.by_pid.contains_key(&pid) {
            return Err(HierarchyError::PidInUse(pid));
        }
        let restored = match (id, record) {
            (EntityId::Group(k), EntityRecord::Group(v)) => self.groups.restore(k, v).is_ok(),
            (EntityId::Instance(k), EntityRecord::Instance(v)) => {
                self.instances.restore(k, v).is_ok()
            }
            (EntityId::Guide(k), EntityRecord::Guide(v)) => self.guides.restore(k, v).is_ok(),
            (EntityId::Dimension(k), EntityRecord::Dimension(v)) => {
                self.dimensions.restore(k, v).is_ok()
            }
            (EntityId::Text(k), EntityRecord::Text(v)) => self.texts.restore(k, v).is_ok(),
            (EntityId::Image(k), EntityRecord::Image(v)) => self.images.restore(k, v).is_ok(),
            (EntityId::SectionPlane(k), EntityRecord::SectionPlane(v)) => {
                self.section_planes.restore(k, v).is_ok()
            }
            (EntityId::Edge(_) | EntityId::Face(_), _) => {
                return Err(HierarchyError::MeshEntity(id));
            }
            _ => return Err(HierarchyError::KindMismatch(id)),
        };
        if !restored {
            return Err(HierarchyError::NotFound(id));
        }
        self.order.insert(order_index.min(self.order.len()), id);
        self.pids.insert(id, pid);
        self.by_pid.insert(pid, id);
        Ok(())
    }

    /// Removes a non-mesh entity with all its side-table rows; returns the
    /// record, its pid and its former order index. The id stays reserved for
    /// [`Entities::restore`].
    pub(crate) fn remove(
        &mut self,
        id: EntityId,
    ) -> Result<(EntityRecord, Pid, usize), HierarchyError> {
        let not_found = || HierarchyError::NotFound(id);
        let record = match id {
            EntityId::Edge(_) | EntityId::Face(_) => return Err(HierarchyError::MeshEntity(id)),
            EntityId::Group(g) => EntityRecord::Group(self.groups.remove(g).ok_or_else(not_found)?),
            EntityId::Instance(i) => {
                EntityRecord::Instance(self.instances.remove(i).ok_or_else(not_found)?)
            }
            EntityId::Guide(g) => EntityRecord::Guide(self.guides.remove(g).ok_or_else(not_found)?),
            EntityId::Dimension(d) => {
                EntityRecord::Dimension(self.dimensions.remove(d).ok_or_else(not_found)?)
            }
            EntityId::Text(t) => EntityRecord::Text(self.texts.remove(t).ok_or_else(not_found)?),
            EntityId::Image(i) => EntityRecord::Image(self.images.remove(i).ok_or_else(not_found)?),
            EntityId::SectionPlane(s) => {
                EntityRecord::SectionPlane(self.section_planes.remove(s).ok_or_else(not_found)?)
            }
        };
        let (pid, index) = self.forget(id);
        Ok((record, pid, index))
    }

    /// Drops `id` from the order and every side table; returns its pid
    /// (`Pid::NONE` when it had none) and former order index.
    fn forget(&mut self, id: EntityId) -> (Pid, usize) {
        let index = self
            .order
            .iter()
            .position(|x| *x == id)
            .unwrap_or(self.order.len());
        if index < self.order.len() {
            self.order.remove(index);
        }
        let pid = self.pids.remove(&id).unwrap_or(Pid::NONE);
        self.by_pid.remove(&pid);
        self.attrs.remove(&id);
        self.tag_of.remove(&id);
        self.hidden.remove(&id);
        self.locked.remove(&id);
        (pid, index)
    }

    /// Replaces a non-mesh entity's data in place (same id and pid).
    pub(crate) fn replace(
        &mut self,
        id: EntityId,
        record: EntityRecord,
    ) -> Result<EntityRecord, HierarchyError> {
        fn swap<K: slotmap::Key, T>(
            map: &mut StableMap<K, T>,
            key: K,
            value: T,
            id: EntityId,
        ) -> Result<T, HierarchyError> {
            let slot = map.get_mut(key).ok_or(HierarchyError::NotFound(id))?;
            Ok(std::mem::replace(slot, value))
        }
        Ok(match (id, record) {
            (EntityId::Group(k), EntityRecord::Group(v)) => {
                EntityRecord::Group(swap(&mut self.groups, k, v, id)?)
            }
            (EntityId::Instance(k), EntityRecord::Instance(v)) => {
                EntityRecord::Instance(swap(&mut self.instances, k, v, id)?)
            }
            (EntityId::Guide(k), EntityRecord::Guide(v)) => {
                EntityRecord::Guide(swap(&mut self.guides, k, v, id)?)
            }
            (EntityId::Dimension(k), EntityRecord::Dimension(v)) => {
                EntityRecord::Dimension(swap(&mut self.dimensions, k, v, id)?)
            }
            (EntityId::Text(k), EntityRecord::Text(v)) => {
                EntityRecord::Text(swap(&mut self.texts, k, v, id)?)
            }
            (EntityId::Image(k), EntityRecord::Image(v)) => {
                EntityRecord::Image(swap(&mut self.images, k, v, id)?)
            }
            (EntityId::SectionPlane(k), EntityRecord::SectionPlane(v)) => {
                EntityRecord::SectionPlane(swap(&mut self.section_planes, k, v, id)?)
            }
            (EntityId::Edge(_) | EntityId::Face(_), _) => {
                return Err(HierarchyError::MeshEntity(id));
            }
            _ => return Err(HierarchyError::KindMismatch(id)),
        })
    }

    /// Binds a pid to a mesh edge or face created by a mesh operation and
    /// appends it to the order (or inserts at `order_index`).
    pub(crate) fn bind_pid(
        &mut self,
        id: EntityId,
        pid: Pid,
        order_index: Option<usize>,
    ) -> Result<(), HierarchyError> {
        if !matches!(id, EntityId::Edge(_) | EntityId::Face(_)) {
            return Err(HierarchyError::KindMismatch(id));
        }
        if !self.contains(id) {
            return Err(HierarchyError::NotFound(id));
        }
        if self.by_pid.contains_key(&pid) {
            return Err(HierarchyError::PidInUse(pid));
        }
        if let Some(old) = self.pids.get(&id) {
            return Err(HierarchyError::PidInUse(*old));
        }
        let index = order_index
            .unwrap_or(self.order.len())
            .min(self.order.len());
        self.order.insert(index, id);
        self.pids.insert(id, pid);
        self.by_pid.insert(pid, id);
        Ok(())
    }

    /// Unbinds a mesh entity's pid and side-table rows after the mesh removed
    /// it; returns the pid and former order index.
    pub(crate) fn unbind_pid(&mut self, id: EntityId) -> Result<(Pid, usize), HierarchyError> {
        if !self.pids.contains_key(&id) {
            return Err(HierarchyError::NotFound(id));
        }
        Ok(self.forget(id))
    }

    /// Moves a mesh entity's pid binding, order slot and side-table rows from
    /// `old` (no longer in the mesh) to `new` (re-created by replay; docs/03
    /// Decisions "reshape keeps the pid").
    pub(crate) fn rekey(&mut self, old: EntityId, new: EntityId) -> Result<(), HierarchyError> {
        let pid = self
            .pids
            .remove(&old)
            .ok_or(HierarchyError::NotFound(old))?;
        if self.pids.contains_key(&new) {
            self.pids.insert(old, pid);
            return Err(HierarchyError::PidInUse(pid));
        }
        self.pids.insert(new, pid);
        self.by_pid.insert(pid, new);
        if let Some(slot) = self.order.iter_mut().find(|x| **x == old) {
            *slot = new;
        }
        if let Some(a) = self.attrs.remove(&old) {
            self.attrs.insert(new, a);
        }
        if let Some(t) = self.tag_of.remove(&old) {
            self.tag_of.insert(new, t);
        }
        if self.hidden.remove(&old) {
            self.hidden.insert(new);
        }
        Ok(())
    }

    // ----- typed access ----------------------------------------------------------

    /// Group by id.
    pub fn group(&self, id: GroupId) -> Option<&Group> {
        self.groups.get(id)
    }

    /// Instance by id.
    pub fn instance(&self, id: InstanceId) -> Option<&ComponentInstance> {
        self.instances.get(id)
    }

    /// Guide by id.
    pub fn guide(&self, id: GuideId) -> Option<&Guide> {
        self.guides.get(id)
    }

    /// Dimension by id.
    pub fn dimension(&self, id: DimensionId) -> Option<&Dimension> {
        self.dimensions.get(id)
    }

    /// Text by id.
    pub fn text(&self, id: TextId) -> Option<&Text> {
        self.texts.get(id)
    }

    /// Image by id.
    pub fn image(&self, id: ImageId) -> Option<&ImageEntity> {
        self.images.get(id)
    }

    /// Section plane by id.
    pub fn section_plane(&self, id: SectionPlaneId) -> Option<&SectionPlane> {
        self.section_planes.get(id)
    }

    /// Any entity by id.
    pub fn view(&self, id: EntityId) -> Option<EntityView<'_>> {
        Some(match id {
            EntityId::Edge(e) => EntityView::Edge(self.mesh.edge(e)?),
            EntityId::Face(f) => EntityView::Face(self.mesh.face(f)?),
            EntityId::Group(g) => EntityView::Group(self.groups.get(g)?),
            EntityId::Instance(i) => EntityView::Instance(self.instances.get(i)?),
            EntityId::Guide(g) => EntityView::Guide(self.guides.get(g)?),
            EntityId::Dimension(d) => EntityView::Dimension(self.dimensions.get(d)?),
            EntityId::Text(t) => EntityView::Text(self.texts.get(t)?),
            EntityId::Image(i) => EntityView::Image(self.images.get(i)?),
            EntityId::SectionPlane(s) => EntityView::SectionPlane(self.section_planes.get(s)?),
        })
    }

    /// Groups in slot order.
    pub fn groups(&self) -> impl Iterator<Item = (GroupId, &Group)> + '_ {
        self.groups.iter()
    }

    /// Instances in slot order.
    pub fn instances(&self) -> impl Iterator<Item = (InstanceId, &ComponentInstance)> + '_ {
        self.instances.iter()
    }

    /// Guides in slot order.
    pub fn guides(&self) -> impl Iterator<Item = (GuideId, &Guide)> + '_ {
        self.guides.iter()
    }

    /// Dimensions in slot order.
    pub fn dimensions(&self) -> impl Iterator<Item = (DimensionId, &Dimension)> + '_ {
        self.dimensions.iter()
    }

    /// Texts in slot order.
    pub fn texts(&self) -> impl Iterator<Item = (TextId, &Text)> + '_ {
        self.texts.iter()
    }

    /// Images in slot order.
    pub fn images(&self) -> impl Iterator<Item = (ImageId, &ImageEntity)> + '_ {
        self.images.iter()
    }

    /// Section planes in slot order.
    pub fn section_planes(&self) -> impl Iterator<Item = (SectionPlaneId, &SectionPlane)> + '_ {
        self.section_planes.iter()
    }

    /// The definition behind a group or instance.
    pub fn definition_of(&self, id: EntityId) -> Option<DefinitionId> {
        match id {
            EntityId::Group(g) => self.groups.get(g).map(|g| g.definition),
            EntityId::Instance(i) => self.instances.get(i).map(|i| i.definition),
            _ => None,
        }
    }

    /// The local transform of a group, instance or image.
    pub fn transform_of(&self, id: EntityId) -> Option<DAffine3> {
        match id {
            EntityId::Group(g) => self.groups.get(g).map(|g| g.transform),
            EntityId::Instance(i) => self.instances.get(i).map(|i| i.transform),
            EntityId::Image(i) => self.images.get(i).map(|i| i.transform),
            _ => None,
        }
    }

    /// Sets the local transform of a group, instance or image; returns the old one.
    pub(crate) fn set_transform(
        &mut self,
        id: EntityId,
        t: DAffine3,
    ) -> Result<DAffine3, HierarchyError> {
        let slot = match id {
            EntityId::Group(g) => self.groups.get_mut(g).map(|x| &mut x.transform),
            EntityId::Instance(i) => self.instances.get_mut(i).map(|x| &mut x.transform),
            EntityId::Image(i) => self.images.get_mut(i).map(|x| &mut x.transform),
            _ => return Err(HierarchyError::KindMismatch(id)),
        };
        let slot = slot.ok_or(HierarchyError::NotFound(id))?;
        Ok(std::mem::replace(slot, t))
    }

    // ----- side tables ------------------------------------------------------------

    /// Tag of an entity; `None` means `Untagged` (docs/04 §5).
    pub fn tag_of(&self, id: EntityId) -> Option<TagId> {
        self.tag_of.get(&id).copied()
    }

    /// Sets (or clears, with `None`) the tag; returns the previous value.
    pub(crate) fn set_tag(
        &mut self,
        id: EntityId,
        tag: Option<TagId>,
    ) -> Result<Option<TagId>, HierarchyError> {
        self.require(id)?;
        Ok(match tag {
            Some(t) => self.tag_of.insert(id, t),
            None => self.tag_of.remove(&id),
        })
    }

    /// `NotFound` unless the entity exists.
    fn require(&self, id: EntityId) -> Result<(), HierarchyError> {
        if self.contains(id) {
            Ok(())
        } else {
            Err(HierarchyError::NotFound(id))
        }
    }

    /// `true` when the entity is hidden (docs/04 §5 visibility rule).
    pub fn is_hidden(&self, id: EntityId) -> bool {
        self.hidden.contains(&id)
    }

    /// Sets hidden; returns the previous state.
    pub(crate) fn set_hidden(
        &mut self,
        id: EntityId,
        hidden: bool,
    ) -> Result<bool, HierarchyError> {
        self.require(id)?;
        Ok(if hidden {
            !self.hidden.insert(id)
        } else {
            self.hidden.remove(&id)
        })
    }

    /// `true` when the group/instance is locked.
    pub fn is_locked(&self, id: EntityId) -> bool {
        self.locked.contains(&id)
    }

    /// Sets locked (groups/instances only); returns the previous state.
    pub(crate) fn set_locked(
        &mut self,
        id: EntityId,
        locked: bool,
    ) -> Result<bool, HierarchyError> {
        if !matches!(id, EntityId::Group(_) | EntityId::Instance(_)) {
            return Err(HierarchyError::NotLockable(id));
        }
        self.require(id)?;
        Ok(if locked {
            !self.locked.insert(id)
        } else {
            self.locked.remove(&id)
        })
    }

    /// Attributes of an entity (`None` when it has none).
    pub fn attrs(&self, id: EntityId) -> Option<&AttributeDictionaries> {
        self.attrs.get(&id)
    }

    /// Mutable attributes, created on demand.
    pub(crate) fn attrs_mut(
        &mut self,
        id: EntityId,
    ) -> Result<&mut AttributeDictionaries, HierarchyError> {
        self.require(id)?;
        Ok(self.attrs.entry(id).or_default())
    }

    /// Replaces (or with `None` removes) an entity's whole attribute set;
    /// returns the previous one (undo of attribute edits, docs/01 §3.1).
    pub(crate) fn set_attrs(
        &mut self,
        id: EntityId,
        attrs: Option<AttributeDictionaries>,
    ) -> Result<Option<AttributeDictionaries>, HierarchyError> {
        self.require(id)?;
        Ok(match attrs {
            Some(a) => self.attrs.insert(id, a),
            None => self.attrs.remove(&id),
        })
    }

    /// Entities carrying `tag`, in order (used when a tag is deleted).
    pub fn with_tag(&self, tag: TagId) -> Vec<EntityId> {
        self.order
            .iter()
            .copied()
            .filter(|id| self.tag_of.get(id) == Some(&tag))
            .collect()
    }

    // ----- aggregates -------------------------------------------------------------

    /// Per-kind counts.
    pub fn counts(&self) -> EntityCounts {
        EntityCounts {
            vertices: self.mesh.vertex_count(),
            edges: self.mesh.edge_count(),
            faces: self.mesh.face_count(),
            groups: self.groups.len(),
            instances: self.instances.len(),
            guides: self.guides.len(),
            dimensions: self.dimensions.len(),
            texts: self.texts.len(),
            images: self.images.len(),
            section_planes: self.section_planes.len(),
        }
    }

    /// Local bounding box including nested instances (recursing through
    /// `definitions`), excluding guides and hidden entities when `visible_only`.
    pub fn local_aabb(
        &self,
        definitions: &StableMap<DefinitionId, ComponentDefinition>,
        visible_only: bool,
    ) -> Aabb {
        self.aabb_at_depth(definitions, visible_only, 0)
    }

    fn aabb_at_depth(
        &self,
        definitions: &StableMap<DefinitionId, ComponentDefinition>,
        visible_only: bool,
        depth: usize,
    ) -> Aabb {
        let shown = |id: EntityId| !visible_only || !self.hidden.contains(&id);
        let mut bb = Aabb::EMPTY;
        for (e, _) in self.mesh.edges() {
            if shown(EntityId::Edge(e))
                && let Ok(s) = self.mesh.edge_segment(e)
            {
                bb = bb.including(s.a).including(s.b);
            }
        }
        if visible_only {
            // A visible face whose boundary edges are hidden still counts.
            for (f, face) in self.mesh.faces() {
                if shown(EntityId::Face(f))
                    && let Ok(points) = self.mesh.loop_vertices(face.outer)
                {
                    for v in points {
                        if let Ok(p) = self.mesh.position(v) {
                            bb = bb.including(p);
                        }
                    }
                }
            }
        } else {
            bb = bb.union(&self.mesh.aabb());
        }
        if depth < Self::MAX_DEPTH {
            let nested = self
                .groups
                .iter()
                .map(|(g, x)| (EntityId::Group(g), x.definition, x.transform))
                .chain(
                    self.instances
                        .iter()
                        .map(|(i, x)| (EntityId::Instance(i), x.definition, x.transform)),
                );
            for (id, definition, transform) in nested {
                if !shown(id) {
                    continue;
                }
                if let Some(def) = definitions.get(definition) {
                    let inner = def
                        .entities
                        .aabb_at_depth(definitions, visible_only, depth + 1);
                    if !inner.is_empty() {
                        bb = bb.union(&inner.transformed(&transform));
                    }
                }
            }
        }
        for (i, image) in &self.images {
            if shown(EntityId::Image(i)) {
                let (w, h) = (image.width.0, image.height.0);
                for corner in [
                    glam::DVec3::ZERO,
                    glam::DVec3::new(w, 0.0, 0.0),
                    glam::DVec3::new(w, h, 0.0),
                    glam::DVec3::new(0.0, h, 0.0),
                ] {
                    bb = bb.including(image.transform.transform_point3(corner));
                }
            }
        }
        bb
    }

    /// Maximum nesting depth below this container (docs/04 §14, cycle check in
    /// docs/04 §13).
    pub fn nesting_depth(
        &self,
        definitions: &StableMap<DefinitionId, ComponentDefinition>,
    ) -> Result<usize, HierarchyError> {
        let mut memo = BTreeMap::new();
        let mut stack = Vec::new();
        self.depth_below(definitions, &mut memo, &mut stack)
    }

    /// Depth below this container with memoization per definition and the
    /// current definition chain for cycle detection.
    fn depth_below(
        &self,
        definitions: &StableMap<DefinitionId, ComponentDefinition>,
        memo: &mut BTreeMap<DefinitionId, usize>,
        stack: &mut Vec<DefinitionId>,
    ) -> Result<usize, HierarchyError> {
        let children: BTreeSet<DefinitionId> = self
            .groups
            .values()
            .map(|g| g.definition)
            .chain(self.instances.values().map(|i| i.definition))
            .collect();
        let mut depth = 0;
        for d in children {
            if stack.contains(&d) {
                return Err(HierarchyError::Cycle(d));
            }
            let below = match memo.get(&d) {
                Some(&known) => known,
                None => {
                    let Some(def) = definitions.get(d) else {
                        continue;
                    };
                    stack.push(d);
                    let known = def.entities.depth_below(definitions, memo, stack)?;
                    stack.pop();
                    memo.insert(d, known);
                    known
                }
            };
            depth = depth.max(below + 1);
            if depth > Self::MAX_DEPTH {
                return Err(HierarchyError::TooDeep(depth));
            }
        }
        Ok(depth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_container_counts_and_order() {
        let e = Entities::new();
        assert!(e.is_empty());
        assert_eq!(e.counts(), EntityCounts::default());
        assert!(e.iter().next().is_none());
        assert!(e.pid_of(EntityId::Group(GroupId::default())).is_none());
    }

    fn group(definition: DefinitionId) -> EntityRecord {
        EntityRecord::Group(Group {
            definition,
            transform: DAffine3::IDENTITY,
            name: "g".into(),
        })
    }

    fn guide() -> EntityRecord {
        EntityRecord::Guide(Guide::Point {
            pos: glam::DVec3::ZERO,
        })
    }

    #[test]
    fn insert_remove_and_reinsert_keep_order_and_side_tables_clean() {
        let mut e = Entities::new();
        let a = e.insert(guide(), Pid(1)).unwrap();
        let b = e.insert(group(DefinitionId::default()), Pid(2)).unwrap();
        let c = e.insert(guide(), Pid(3)).unwrap();
        assert_eq!(e.order(), &[a, b, c]);
        assert_eq!(
            e.insert(guide(), Pid(2)),
            Err(HierarchyError::PidInUse(Pid(2)))
        );

        e.set_hidden(b, true).unwrap();
        e.set_locked(b, true).unwrap();
        assert_eq!(e.set_locked(a, true), Err(HierarchyError::NotLockable(a)));
        e.attrs_mut(b).unwrap();
        let (record, pid, index) = e.remove(b).unwrap();
        assert_eq!((pid, index), (Pid(2), 1));
        assert!(!e.is_hidden(b) && !e.is_locked(b) && e.attrs(b).is_none());
        assert_eq!(e.id_of(Pid(2)), None);

        let b2 = e.insert_at(record, pid, index).unwrap();
        assert_eq!(e.order(), &[a, b2, c]);
        assert_eq!(e.pid_of(b2), Some(Pid(2)));
        assert_eq!(
            e.replace(a, group(DefinitionId::default())),
            Err(HierarchyError::KindMismatch(a))
        );
        assert!(e.replace(a, guide()).is_ok());
    }

    #[test]
    fn mesh_entities_bind_and_unbind_pids() {
        let mut e = Entities::new();
        let tol = crate::tolerance::Tolerances::default();
        let r = e
            .mesh_mut()
            .add_edge(
                glam::DVec3::ZERO,
                glam::DVec3::X,
                &tol,
                &mut crate::geom::NoDeltas,
            )
            .unwrap();
        let edge = EntityId::Edge(r.edges[0]);
        assert_eq!(e.remove(edge), Err(HierarchyError::MeshEntity(edge)));
        e.bind_pid(edge, Pid(7), None).unwrap();
        assert_eq!(e.id_of(Pid(7)), Some(edge));
        assert_eq!(e.order(), &[edge]);
        e.set_tag(edge, Some(TagId::default())).unwrap();
        assert_eq!(e.unbind_pid(edge), Ok((Pid(7), 0)));
        assert!(e.is_empty() && e.tag_of(edge).is_none());
    }

    #[test]
    fn nesting_depth_detects_cycles() {
        let mut defs: StableMap<DefinitionId, ComponentDefinition> = StableMap::with_key();
        let inner = defs.insert(ComponentDefinition::new(uuid::Uuid::nil(), "inner"));
        let outer = defs.insert(ComponentDefinition::new(uuid::Uuid::nil(), "outer"));
        defs[outer].entities.insert(group(inner), Pid(1)).unwrap();
        let mut root = Entities::new();
        root.insert(group(outer), Pid(2)).unwrap();
        assert_eq!(root.nesting_depth(&defs), Ok(2));
        // inner → outer closes a cycle.
        defs[inner].entities.insert(group(outer), Pid(3)).unwrap();
        assert!(matches!(
            root.nesting_depth(&defs),
            Err(HierarchyError::Cycle(_))
        ));
    }

    #[test]
    fn local_aabb_includes_transformed_nested_geometry() {
        let tol = crate::tolerance::Tolerances::default();
        let mut defs: StableMap<DefinitionId, ComponentDefinition> = StableMap::with_key();
        let d = defs.insert(ComponentDefinition::new(uuid::Uuid::nil(), "d"));
        defs[d]
            .entities
            .mesh_mut()
            .add_edge(
                glam::DVec3::ZERO,
                glam::DVec3::new(10.0, 0.0, 0.0),
                &tol,
                &mut crate::geom::NoDeltas,
            )
            .unwrap();
        let mut root = Entities::new();
        let g = root
            .insert(
                EntityRecord::Group(Group {
                    definition: d,
                    transform: DAffine3::from_translation(glam::DVec3::new(0.0, 0.0, 5.0)),
                    name: String::new(),
                }),
                Pid(1),
            )
            .unwrap();
        let bb = root.local_aabb(&defs, true);
        assert_eq!(bb.max, glam::DVec3::new(10.0, 0.0, 5.0));
        root.set_hidden(g, true).unwrap();
        assert!(root.local_aabb(&defs, true).is_empty());
        assert!(!root.local_aabb(&defs, false).is_empty());
    }

    #[test]
    fn container_serializes_with_pair_sequences() {
        let e = Entities::new();
        let json = serde_json::to_value(&e).unwrap();
        assert!(json["pids"].is_array());
        let back: Entities = serde_json::from_value(json).unwrap();
        assert!(back.is_empty());
    }
}
