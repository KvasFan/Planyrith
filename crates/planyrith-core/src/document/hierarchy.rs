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
use slotmap::SlotMap;

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
}

/// One container of entities (docs/04 §3): the model root or a definition's
/// contents. Insertion order across all kinds is kept in `order` and MUST be
/// preserved by save/load and undo/redo (docs/04 §2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entities {
    mesh: HalfEdgeMesh,
    groups: SlotMap<GroupId, Group>,
    instances: SlotMap<InstanceId, ComponentInstance>,
    guides: SlotMap<GuideId, Guide>,
    dimensions: SlotMap<DimensionId, Dimension>,
    texts: SlotMap<TextId, Text>,
    images: SlotMap<ImageId, ImageEntity>,
    section_planes: SlotMap<SectionPlaneId, SectionPlane>,
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
        todo!("docs/04 §3 insert entity")
    }

    /// Inserts a non-mesh entity at `order_index` (undo of a removal).
    pub(crate) fn insert_at(
        &mut self,
        record: EntityRecord,
        pid: Pid,
        order_index: usize,
    ) -> Result<EntityId, HierarchyError> {
        todo!("docs/04 §2 order-preserving re-insertion")
    }

    /// Removes a non-mesh entity with all its side-table rows; returns the
    /// record, its pid and its former order index.
    pub(crate) fn remove(
        &mut self,
        id: EntityId,
    ) -> Result<(EntityRecord, Pid, usize), HierarchyError> {
        todo!("docs/04 §3 remove entity")
    }

    /// Replaces a non-mesh entity's data in place (same id and pid).
    pub(crate) fn replace(
        &mut self,
        id: EntityId,
        record: EntityRecord,
    ) -> Result<EntityRecord, HierarchyError> {
        todo!("docs/04 §3 replace entity")
    }

    /// Binds a pid to a mesh edge or face created by a mesh operation and
    /// appends it to the order (or inserts at `order_index`).
    pub(crate) fn bind_pid(
        &mut self,
        id: EntityId,
        pid: Pid,
        order_index: Option<usize>,
    ) -> Result<(), HierarchyError> {
        todo!("docs/04 §2 bind pid to mesh entity")
    }

    /// Unbinds a mesh entity's pid and side-table rows after the mesh removed
    /// it; returns the pid and former order index.
    pub(crate) fn unbind_pid(&mut self, id: EntityId) -> Result<(Pid, usize), HierarchyError> {
        todo!("docs/04 §2 unbind pid")
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
    pub fn groups(&self) -> slotmap::basic::Iter<'_, GroupId, Group> {
        self.groups.iter()
    }

    /// Instances in slot order.
    pub fn instances(&self) -> slotmap::basic::Iter<'_, InstanceId, ComponentInstance> {
        self.instances.iter()
    }

    /// Guides in slot order.
    pub fn guides(&self) -> slotmap::basic::Iter<'_, GuideId, Guide> {
        self.guides.iter()
    }

    /// Dimensions in slot order.
    pub fn dimensions(&self) -> slotmap::basic::Iter<'_, DimensionId, Dimension> {
        self.dimensions.iter()
    }

    /// Texts in slot order.
    pub fn texts(&self) -> slotmap::basic::Iter<'_, TextId, Text> {
        self.texts.iter()
    }

    /// Images in slot order.
    pub fn images(&self) -> slotmap::basic::Iter<'_, ImageId, ImageEntity> {
        self.images.iter()
    }

    /// Section planes in slot order.
    pub fn section_planes(&self) -> slotmap::basic::Iter<'_, SectionPlaneId, SectionPlane> {
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
        todo!("docs/04 §3 set transform")
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
        todo!("docs/04 §5 set tag")
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
        todo!("docs/04 §3 set hidden")
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
        todo!("docs/04 §3 set locked")
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
        todo!("docs/04 §9 entity attributes")
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
        definitions: &SlotMap<DefinitionId, ComponentDefinition>,
        visible_only: bool,
    ) -> Aabb {
        todo!("docs/08 §2 bbox field")
    }

    /// Maximum nesting depth below this container (docs/04 §14, cycle check in
    /// docs/04 §13).
    pub fn nesting_depth(
        &self,
        definitions: &SlotMap<DefinitionId, ComponentDefinition>,
    ) -> Result<usize, HierarchyError> {
        todo!("docs/04 §13 nesting depth / cycle detection")
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

    #[test]
    fn container_serializes_with_pair_sequences() {
        let e = Entities::new();
        let json = serde_json::to_value(&e).unwrap();
        assert!(json["pids"].is_array());
        let back: Entities = serde_json::from_value(json).unwrap();
        assert!(back.is_empty());
    }
}
