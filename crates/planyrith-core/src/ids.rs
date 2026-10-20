//! Implements docs/04-document-model.md §2 (identity) and the `ids.rs` entry of
//! docs/01-architecture.md §2: typed ids as newtypes over generational slot-map
//! keys, the persistent id `Pid`, and the `EntityId` sum type used by containers.
//!
//! Rules (AGENTS.md §3): ids are opaque newtypes; raw integers never appear in
//! public APIs. Slot-map keys are 64-bit `(index, generation)` pairs and are
//! unique within one `Entities` container (or one mesh); `Pid`s are unique within
//! a document and stable across save/load and undo/redo.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use slotmap::Key;

slotmap::new_key_type! {
    /// Mesh vertex (docs/03 §3.1). Vertices are not entities and have no `Pid`.
    pub struct VertexId;
    /// Directed half-edge record (docs/03 §3.1).
    pub struct HalfEdgeId;
    /// Undirected edge owning two half-edges (docs/03 §3.1).
    pub struct EdgeId;
    /// Planar face with one outer loop and holes (docs/03 §3.1).
    pub struct FaceId;
    /// Boundary loop of a face (docs/03 §3.1).
    pub struct LoopId;
    /// Curve descriptor grouping consecutive edges (docs/03 §3.3).
    pub struct CurveId;
    /// Group entity (docs/04 §3.1).
    pub struct GroupId;
    /// Component instance entity (docs/04 §3.2).
    pub struct InstanceId;
    /// Guide line or point (docs/04 §3.4).
    pub struct GuideId;
    /// Dimension entity (docs/04 §3.5).
    pub struct DimensionId;
    /// Screen text entity (docs/04 §3.6).
    pub struct TextId;
    /// Image entity (docs/04 §3.7).
    pub struct ImageId;
    /// Section plane entity (docs/04 §3.8).
    pub struct SectionPlaneId;
    /// Component definition (docs/04 §3.3), document-level.
    pub struct DefinitionId;
    /// Material (docs/04 §4), document-level.
    pub struct MaterialId;
    /// Tag (docs/04 §5), document-level.
    pub struct TagId;
    /// Tag folder (docs/04 §5), document-level.
    pub struct TagFolderId;
    /// Scene (docs/04 §6), document-level.
    pub struct SceneId;
    /// Style preset (docs/04 §7), document-level.
    pub struct StyleId;
}

/// Implements `JsonSchema` for a slot-map key type, mirroring the serde form that
/// `slotmap` emits (`{ "idx": u32, "version": u32 }`) so command schemas can
/// reference typed ids.
macro_rules! impl_key_schema {
    ($($id:ident),* $(,)?) => {$(
        impl JsonSchema for $id {
            fn schema_name() -> Cow<'static, str> {
                Cow::Borrowed(stringify!($id))
            }

            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "object",
                    "description": concat!("Typed slot-map key `", stringify!($id), "` (index + generation)."),
                    "properties": {
                        "idx": { "type": "integer", "minimum": 0 },
                        "version": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["idx", "version"]
                })
            }
        }

        impl fmt::Display for $id {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}#{:x}", stringify!($id), self.data().as_ffi())
            }
        }
    )*};
}

impl_key_schema!(
    VertexId,
    HalfEdgeId,
    EdgeId,
    FaceId,
    LoopId,
    CurveId,
    GroupId,
    InstanceId,
    GuideId,
    DimensionId,
    TextId,
    ImageId,
    SectionPlaneId,
    DefinitionId,
    MaterialId,
    TagId,
    TagFolderId,
    SceneId,
    StyleId,
);

/// Persistent id of an entity, unique within a document and stable across
/// save/load and undo/redo (docs/04 §2). Allocated from the monotonic counter
/// `ModelInfo::next_pid`; `0` is never allocated and means "none".
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(transparent)]
pub struct Pid(pub u64);

impl Pid {
    /// The reserved "no entity" value.
    pub const NONE: Pid = Pid(0);

    /// Wraps a raw persistent id.
    pub const fn new(raw: u64) -> Self {
        Pid(raw)
    }

    /// Returns the raw value (for serialization and CLI output only).
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// `true` for [`Pid::NONE`].
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.0)
    }
}

/// Monotonic allocator behind `ModelInfo::next_pid` (docs/04 §2, §10).
///
/// Serializes transparently as the next free `u64`, which is exactly the file
/// field `next_pid`. Undo never rewinds the counter (docs/04 §13 checks
/// monotonicity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PidAllocator {
    next: u64,
}

impl Default for PidAllocator {
    fn default() -> Self {
        PidAllocator { next: 1 }
    }
}

impl PidAllocator {
    /// Allocator whose first allocation returns `Pid(1)`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocator resuming after a loaded document; `next` must be ≥ 1.
    pub fn starting_at(next: u64) -> Self {
        PidAllocator { next: next.max(1) }
    }

    /// Returns a fresh pid and advances the counter.
    pub fn allocate(&mut self) -> Pid {
        let pid = Pid(self.next);
        self.next += 1;
        pid
    }

    /// The pid the next call to [`PidAllocator::allocate`] would return.
    pub fn peek(&self) -> Pid {
        Pid(self.next)
    }

    /// Makes sure `pid` can never be allocated again (used when merging
    /// documents or repairing files).
    pub fn observe(&mut self, pid: Pid) {
        if pid.0 >= self.next {
            self.next = pid.0 + 1;
        }
    }
}

/// The kind of an addressable thing, including vertices for selection queries
/// and picking (docs/08 §5 `kind`), even though vertices are not entities.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum EntityKind {
    /// Mesh vertex (not an entity, no pid).
    Vertex,
    /// Mesh edge.
    Edge,
    /// Mesh face.
    Face,
    /// Group.
    Group,
    /// Component instance.
    Instance,
    /// Guide line or point.
    Guide,
    /// Dimension.
    Dimension,
    /// Screen text.
    Text,
    /// Image entity.
    Image,
    /// Section plane.
    SectionPlane,
}

impl EntityKind {
    /// The query-language keyword for this kind (docs/08 §5 `kind`).
    pub const fn keyword(self) -> &'static str {
        match self {
            EntityKind::Vertex => "vertex",
            EntityKind::Edge => "edge",
            EntityKind::Face => "face",
            EntityKind::Group => "group",
            EntityKind::Instance => "instance",
            EntityKind::Guide => "guide",
            EntityKind::Dimension => "dimension",
            EntityKind::Text => "text",
            EntityKind::Image => "image",
            EntityKind::SectionPlane => "section",
        }
    }

    /// Parses a query-language kind keyword.
    pub fn from_keyword(word: &str) -> Option<Self> {
        Some(match word {
            "vertex" => EntityKind::Vertex,
            "edge" => EntityKind::Edge,
            "face" => EntityKind::Face,
            "group" => EntityKind::Group,
            "instance" => EntityKind::Instance,
            "guide" => EntityKind::Guide,
            "dimension" => EntityKind::Dimension,
            "text" => EntityKind::Text,
            "image" => EntityKind::Image,
            "section" => EntityKind::SectionPlane,
            _ => return None,
        })
    }
}

impl fmt::Display for EntityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.keyword())
    }
}

/// Id of an entity inside one `Entities` container (docs/04 §2, glossary
/// "Entity"). A sum over the typed ids so that one ordered side table can be
/// keyed by any entity; derived `Ord` makes `BTreeMap<EntityId, _>` iteration
/// deterministic (AGENTS.md §2 determinism).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub enum EntityId {
    /// Mesh edge.
    Edge(EdgeId),
    /// Mesh face.
    Face(FaceId),
    /// Group.
    Group(GroupId),
    /// Component instance.
    Instance(InstanceId),
    /// Guide.
    Guide(GuideId),
    /// Dimension.
    Dimension(DimensionId),
    /// Screen text.
    Text(TextId),
    /// Image entity.
    Image(ImageId),
    /// Section plane.
    SectionPlane(SectionPlaneId),
}

impl EntityId {
    /// The kind of this entity.
    pub const fn kind(self) -> EntityKind {
        match self {
            EntityId::Edge(_) => EntityKind::Edge,
            EntityId::Face(_) => EntityKind::Face,
            EntityId::Group(_) => EntityKind::Group,
            EntityId::Instance(_) => EntityKind::Instance,
            EntityId::Guide(_) => EntityKind::Guide,
            EntityId::Dimension(_) => EntityKind::Dimension,
            EntityId::Text(_) => EntityKind::Text,
            EntityId::Image(_) => EntityKind::Image,
            EntityId::SectionPlane(_) => EntityKind::SectionPlane,
        }
    }

    /// `true` for groups and component instances (the only entities that can be
    /// steps of an instance path and that can be locked, docs/04 §3).
    pub const fn is_container_instance(self) -> bool {
        matches!(self, EntityId::Group(_) | EntityId::Instance(_))
    }

    /// `true` for edges and faces (owned by the container's mesh).
    pub const fn is_mesh_entity(self) -> bool {
        matches!(self, EntityId::Edge(_) | EntityId::Face(_))
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntityId::Edge(id) => write!(f, "{id}"),
            EntityId::Face(id) => write!(f, "{id}"),
            EntityId::Group(id) => write!(f, "{id}"),
            EntityId::Instance(id) => write!(f, "{id}"),
            EntityId::Guide(id) => write!(f, "{id}"),
            EntityId::Dimension(id) => write!(f, "{id}"),
            EntityId::Text(id) => write!(f, "{id}"),
            EntityId::Image(id) => write!(f, "{id}"),
            EntityId::SectionPlane(id) => write!(f, "{id}"),
        }
    }
}

macro_rules! impl_from_typed_id {
    ($($variant:ident($id:ident)),* $(,)?) => {$(
        impl From<$id> for EntityId {
            fn from(id: $id) -> Self {
                EntityId::$variant(id)
            }
        }
    )*};
}

impl_from_typed_id!(
    Edge(EdgeId),
    Face(FaceId),
    Group(GroupId),
    Instance(InstanceId),
    Guide(GuideId),
    Dimension(DimensionId),
    Text(TextId),
    Image(ImageId),
    SectionPlane(SectionPlaneId),
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_allocator_is_monotonic_and_skips_zero() {
        let mut alloc = PidAllocator::new();
        assert_eq!(alloc.allocate(), Pid(1));
        assert_eq!(alloc.allocate(), Pid(2));
        alloc.observe(Pid(10));
        assert_eq!(alloc.peek(), Pid(11));
        assert!(Pid::NONE.is_none());
    }

    #[test]
    fn entity_kind_keywords_round_trip() {
        for kind in [
            EntityKind::Vertex,
            EntityKind::Edge,
            EntityKind::Face,
            EntityKind::Group,
            EntityKind::Instance,
            EntityKind::Guide,
            EntityKind::Dimension,
            EntityKind::Text,
            EntityKind::Image,
            EntityKind::SectionPlane,
        ] {
            assert_eq!(EntityKind::from_keyword(kind.keyword()), Some(kind));
        }
    }

    #[test]
    fn typed_ids_have_json_schemas() {
        let schema = schemars::schema_for!(FaceId);
        let json = serde_json::to_value(schema).unwrap();
        assert_eq!(json["type"], "object");
    }
}
