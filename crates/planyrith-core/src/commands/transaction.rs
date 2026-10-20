//! Implements docs/01-architecture.md §3.1 rule 1 (commands mutate only through
//! a `Transaction` that records inverse deltas) and the undo stack of §3.2:
//! `Transaction`, `Delta`, `CommittedTransaction`, `UndoStack`.
//!
//! Pid bookkeeping: `Transaction::with_mesh` runs a kernel operation, then
//! allocates pids for every `EdgeAdded`/`FaceAdded` delta (or re-binds the
//! recorded pid when replaying), binds them in the container's order list and
//! unbinds removed ones (docs/04 §2). Undo never rewinds `next_pid`.
//!
//! Owner key: `later` (depends on `document` and `mesh`).

use glam::DAffine3;
use serde::{Deserialize, Serialize};

use crate::commands::CommandError;
use crate::diagnostics::Warning;
use crate::document::{
    ComponentDefinition, ContainerRef, Document, EntityRecord, Material, ModelInfo,
    RenderingOptions, Scene, Style, Tag, TagFolder, Value,
};
use crate::geom::{DeltaSink, FaceSides, GeometryError, HalfEdgeMesh, MeshDelta};
use crate::ids::{
    DefinitionId, EntityId, FaceId, MaterialId, Pid, SceneId, StyleId, TagFolderId, TagId,
};
use crate::io::Storage;
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
    /// Hidden flag changed.
    HiddenChanged {
        /// Container.
        container: ContainerRef,
        /// Entity.
        id: EntityId,
        /// New value (inverse is `!to`).
        to: bool,
    },
    /// Locked flag changed.
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
        /// Data.
        material: Box<Material>,
    },
    /// Material removed.
    MaterialRemoved {
        /// Id.
        id: MaterialId,
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
        /// Data.
        tag: Tag,
    },
    /// Tag removed.
    TagRemoved {
        /// Id.
        id: TagId,
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
        /// Data.
        folder: TagFolder,
    },
    /// Tag folder removed.
    TagFolderRemoved {
        /// Id.
        id: TagFolderId,
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
        /// Data.
        style: Box<Style>,
    },
    /// Style removed.
    StyleRemoved {
        /// Id.
        id: StyleId,
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
    /// The inverse delta (swaps before/after, inserted/removed, bound/unbound).
    pub fn inverse(&self) -> Delta {
        todo!("docs/01 §3.1 inverse deltas")
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

/// Applies one delta to the document (forward direction). Private to this
/// module's owner; `Engine::undo` applies `delta.inverse()`.
pub(crate) fn apply_delta(doc: &mut Document, delta: &Delta) -> Result<(), CommandError> {
    todo!("docs/01 §3.1 delta application / replay")
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

/// An open transaction: the only way commands mutate a document
/// (docs/01 §3.1). Dropped without `commit` → caller must `rollback`.
pub struct Transaction<'a> {
    doc: &'a mut Document,
    storage: Option<&'a dyn Storage>,
    label: String,
    deltas: Vec<Delta>,
    warnings: Vec<Warning>,
    created: Vec<Pid>,
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
    fn record(&mut self, delta: MeshDelta) {
        // Only valid while a `with_mesh` scope knows the container; the scope
        // wraps deltas itself. Direct use is a programming error surfaced by
        // `debug_assert` in the implementation.
        todo!("docs/01 §3.1 mesh delta wrapping")
    }
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
        }
    }

    /// Gives file commands access to storage (docs/01 §3.1 rule 2).
    pub fn with_storage(mut self, storage: &'a dyn Storage) -> Self {
        self.storage = Some(storage);
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

    // ----- mesh ---------------------------------------------------------------

    /// Runs a kernel operation on a container's mesh, recording its deltas,
    /// then binds pids for created edges/faces and unbinds removed ones.
    pub fn with_mesh<R>(
        &mut self,
        container: ContainerRef,
        op: impl FnOnce(&mut HalfEdgeMesh, &Tolerances, &mut dyn DeltaSink) -> Result<R, GeometryError>,
    ) -> Result<R, CommandError> {
        todo!("docs/01 §3.1 transactional mesh operation")
    }

    /// Sets a face's materials/UVs.
    pub fn set_face_sides(
        &mut self,
        container: ContainerRef,
        face: FaceId,
        sides: FaceSides,
    ) -> Result<(), CommandError> {
        todo!("docs/04 §4 face material change")
    }

    // ----- entities ---------------------------------------------------------------

    /// Inserts a non-mesh entity, allocating its pid.
    pub fn insert_entity(
        &mut self,
        container: ContainerRef,
        record: EntityRecord,
    ) -> Result<(EntityId, Pid), CommandError> {
        todo!("docs/04 §3 insert entity")
    }

    /// Removes a non-mesh entity.
    pub fn remove_entity(
        &mut self,
        container: ContainerRef,
        id: EntityId,
    ) -> Result<EntityRecord, CommandError> {
        todo!("docs/04 §3 remove entity")
    }

    /// Replaces a non-mesh entity's data.
    pub fn replace_entity(
        &mut self,
        container: ContainerRef,
        id: EntityId,
        record: EntityRecord,
    ) -> Result<(), CommandError> {
        todo!("docs/04 §3 replace entity")
    }

    /// Sets a transform.
    pub fn set_transform(
        &mut self,
        container: ContainerRef,
        id: EntityId,
        transform: DAffine3,
    ) -> Result<(), CommandError> {
        todo!("docs/04 §3 set transform")
    }

    /// Sets a tag (`None` = untagged).
    pub fn set_tag(
        &mut self,
        container: ContainerRef,
        id: EntityId,
        tag: Option<TagId>,
    ) -> Result<(), CommandError> {
        todo!("docs/04 §5 set tag")
    }

    /// Sets hidden.
    pub fn set_hidden(
        &mut self,
        container: ContainerRef,
        id: EntityId,
        hidden: bool,
    ) -> Result<(), CommandError> {
        todo!("docs/04 §3 set hidden")
    }

    /// Sets locked (groups/instances only).
    pub fn set_locked(
        &mut self,
        container: ContainerRef,
        id: EntityId,
        locked: bool,
    ) -> Result<(), CommandError> {
        todo!("docs/04 §3 set locked")
    }

    /// Sets or clears (`None`) one attribute value.
    pub fn set_attribute(
        &mut self,
        target: AttrTarget,
        dict: &str,
        key: &str,
        value: Option<Value>,
    ) -> Result<(), CommandError> {
        todo!("docs/04 §9 set attribute")
    }

    // ----- document-level collections ----------------------------------------------

    /// Adds a definition.
    pub fn insert_definition(
        &mut self,
        definition: ComponentDefinition,
    ) -> Result<DefinitionId, CommandError> {
        todo!("docs/04 §3.3 insert definition")
    }

    /// Removes an unreferenced definition (`purge-unused`, explode).
    pub fn remove_definition(
        &mut self,
        id: DefinitionId,
    ) -> Result<ComponentDefinition, CommandError> {
        todo!("docs/04 §3.3 remove definition")
    }

    /// Adds a material.
    pub fn insert_material(&mut self, material: Material) -> Result<MaterialId, CommandError> {
        todo!("docs/04 §4 insert material")
    }

    /// Replaces a material.
    pub fn replace_material(
        &mut self,
        id: MaterialId,
        material: Material,
    ) -> Result<(), CommandError> {
        todo!("docs/04 §4 replace material")
    }

    /// Removes a material (faces using it fall back to default).
    pub fn remove_material(&mut self, id: MaterialId) -> Result<Material, CommandError> {
        todo!("docs/04 §4 remove material")
    }

    /// Adds a tag.
    pub fn insert_tag(&mut self, tag: Tag) -> Result<TagId, CommandError> {
        todo!("docs/04 §5 insert tag")
    }

    /// Replaces a tag.
    pub fn replace_tag(&mut self, id: TagId, tag: Tag) -> Result<(), CommandError> {
        todo!("docs/04 §5 replace tag")
    }

    /// Removes a tag; entities carrying it become untagged.
    pub fn remove_tag(&mut self, id: TagId) -> Result<Tag, CommandError> {
        todo!("docs/04 §5 remove tag")
    }

    /// Adds a scene.
    pub fn insert_scene(&mut self, scene: Scene) -> Result<SceneId, CommandError> {
        todo!("docs/04 §6 insert scene")
    }

    /// Replaces a scene.
    pub fn replace_scene(&mut self, id: SceneId, scene: Scene) -> Result<(), CommandError> {
        todo!("docs/04 §6 replace scene")
    }

    /// Removes a scene.
    pub fn remove_scene(&mut self, id: SceneId) -> Result<Scene, CommandError> {
        todo!("docs/04 §6 remove scene")
    }

    /// Moves a scene in the list.
    pub fn move_scene(&mut self, id: SceneId, index: usize) -> Result<(), CommandError> {
        todo!("docs/04 §6 reorder scene")
    }

    /// Adds a style.
    pub fn insert_style(&mut self, style: Style) -> Result<StyleId, CommandError> {
        todo!("docs/04 §7 insert style")
    }

    /// Replaces a style.
    pub fn replace_style(&mut self, id: StyleId, style: Style) -> Result<(), CommandError> {
        todo!("docs/04 §7 replace style")
    }

    /// Activates a style.
    pub fn set_active_style(&mut self, id: StyleId) -> Result<(), CommandError> {
        todo!("docs/04 §7 set active style")
    }

    /// Replaces model info (units, title, tolerances…).
    pub fn set_info(&mut self, info: ModelInfo) -> Result<(), CommandError> {
        todo!("docs/04 §10 set model info")
    }

    /// Replaces rendering options.
    pub fn set_rendering(&mut self, rendering: RenderingOptions) -> Result<(), CommandError> {
        todo!("docs/04 §1 set rendering options")
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
    /// failed commands, docs/01 §3.1).
    pub fn rollback(self) -> Result<(), CommandError> {
        todo!("docs/01 §3.1 transaction rollback")
    }

    /// Records a delta that was already applied to the document (used by the
    /// mutation helpers above).
    pub(crate) fn record_delta(&mut self, delta: Delta) {
        self.deltas.push(delta);
    }

    /// Mutable document access for the mutation helpers above.
    pub(crate) fn document_mut(&mut self) -> &mut Document {
        self.doc
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
