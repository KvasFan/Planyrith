//! Implements docs/04-document-model.md §1 (the `Document` tree), §2 (identity
//! and pid allocation), §10 (`ModelInfo`), §12 (path resolution) and the
//! document-level containers of docs/01 §4. Sub-modules follow docs/01 §2.
//!
//! All mutators are `pub(crate)`: commands mutate only through
//! `commands::Transaction` (docs/01 §3.1 rule 1).
//!
//! Serialization: the in-memory types derive `serde` directly in named-field
//! mode for this draft (docs/09 §2). The spec's separate `EntitiesSer` /
//! `DefinitionSer` / `MaterialSer` / `SceneSer` / `StyleSer` forms (index-based
//! references, faces as edge-index loops) are produced by `io::native` from
//! `HalfEdgeMesh::to_indexed` when the writer is implemented (docs/09 Decisions).
//!
//! Owner key: `document`.

pub mod annotations;
pub mod attributes;
pub mod camera;
pub mod entities;
pub mod hierarchy;
pub mod materials;
pub mod scenes;
pub mod selection;
pub mod shadows;
pub mod stable_map;
pub mod styles;
pub mod tags;

pub use attributes::{AttributeDictionaries, Value};
pub use camera::{Camera, DepthRange, Projection, StandardView};
pub use entities::{
    Anchor, AnchorKind, ArrowStyle, ComponentBehavior, ComponentDefinition, ComponentInstance,
    DimStyle, Dimension, GlueTo, Group, Guide, ImageEntity, RadialKind, SectionPlane, SnapTo, Text,
};
pub use hierarchy::{Entities, EntityCounts, EntityRecord, EntityView, HierarchyError};
pub use materials::{
    ColorizeMode, FrontOrBack, ImageData, ImageFormat, ImageRef, ImageStore, Material,
    MaterialError, Materials, Rgba8, Texture, UvMap,
};
pub use scenes::{Scene, Scenes};
pub use selection::{
    CmpOp, EditContext, EntityKey, EntityPath, EntityRef, InstancePath, Measures, Query, QueryAtom,
    QueryEnv, QueryError, QueryExpr, QueryKey, QueryKeyword, SelectMode, Selection,
    measure_occurrence,
};
pub use shadows::{Date, ShadowSettings, SunModel, TimeOfDay};
pub use stable_map::StableMap;
pub use styles::{
    Background, EdgeColorMode, EdgeStyle, FaceMode, FaceStyle, Fog, ModelingStyle, Style, Styles,
    Watermark, WatermarkPlacement,
};
pub use tags::{LineStyle, Tag, TagError, TagFolder, Tags};

use std::collections::BTreeMap;

use glam::DAffine3;
use serde::{Deserialize, Serialize};

use crate::ids::{DefinitionId, EntityId, Pid, PidAllocator};
use crate::math::Aabb;
use crate::tolerance::Tolerances;
use crate::units::{Angle, Length, LengthUnit};

/// Current native format version written by this crate (docs/09 §4).
pub const FORMAT_VERSION: u32 = 1;

/// Serde helpers that store `BTreeMap<K, V>` with non-string keys as a sequence
/// of pairs, so the same types serialize to JSON (string keys only) and to
/// MessagePack.
pub(crate) mod serde_seq {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serializes a map as `[[k, v], …]`.
    pub fn serialize<K, V, S>(map: &BTreeMap<K, V>, s: S) -> Result<S::Ok, S::Error>
    where
        K: Serialize,
        V: Serialize,
        S: Serializer,
    {
        s.collect_seq(map.iter())
    }

    /// Deserializes `[[k, v], …]` into a map.
    pub fn deserialize<'de, K, V, D>(d: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        let pairs: Vec<(K, V)> = Vec::deserialize(d)?;
        Ok(pairs.into_iter().collect())
    }

    /// The same for `Option<BTreeMap<K, V>>`.
    pub mod option {
        use std::collections::BTreeMap;

        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        /// Serializes `None` as null and `Some(map)` as a pair sequence.
        pub fn serialize<K, V, S>(map: &Option<BTreeMap<K, V>>, s: S) -> Result<S::Ok, S::Error>
        where
            K: Serialize,
            V: Serialize,
            S: Serializer,
        {
            match map {
                None => s.serialize_none(),
                Some(m) => s.collect_seq(m.iter()),
            }
        }

        /// Inverse of [`serialize`].
        pub fn deserialize<'de, K, V, D>(d: D) -> Result<Option<BTreeMap<K, V>>, D::Error>
        where
            K: Deserialize<'de> + Ord,
            V: Deserialize<'de>,
            D: Deserializer<'de>,
        {
            let pairs: Option<Vec<(K, V)>> = Option::deserialize(d)?;
            Ok(pairs.map(|p| p.into_iter().collect()))
        }
    }
}

/// Geographic location for shadows and geo-referenced exports (docs/04 §10).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GeoLocation {
    /// Latitude in degrees, north positive.
    pub lat: f64,
    /// Longitude in degrees, east positive.
    pub lon: f64,
    /// Model north measured from +Y.
    pub north_angle: Angle,
}

/// Document metadata and settings (docs/04 §10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Display unit for lengths.
    pub display_units: LengthUnit,
    /// Decimals (or fraction denominator power for architectural units).
    pub precision: u8,
    /// Decimals for angles.
    pub angle_precision: u8,
    /// Length snapping increment for interactive input.
    pub length_snap: Option<Length>,
    /// Angle snapping increment for interactive input.
    pub angle_snap: Option<Angle>,
    /// Geometric tolerances (docs/03 §2).
    pub tolerances: Tolerances,
    /// Geo-location.
    pub geo: Option<GeoLocation>,
    /// Title.
    pub title: String,
    /// Author.
    pub author: String,
    /// Description.
    pub description: String,
    /// Format version the document was last written with.
    pub format_version: u32,
    /// Next persistent id (monotonic).
    pub next_pid: PidAllocator,
    /// Document-level attributes of the info block.
    pub attrs: AttributeDictionaries,
    /// The model's drawing axes (docs/05 §5.10): origin and red/green/blue
    /// directions; identity by default.
    #[serde(default = "identity_axes", skip_serializing_if = "is_identity_axes")]
    pub axes: glam::DAffine3,
}

fn identity_axes() -> glam::DAffine3 {
    glam::DAffine3::IDENTITY
}

/// Default axes are not written, so files without moved axes stay as they
/// were.
fn is_identity_axes(axes: &glam::DAffine3) -> bool {
    axes.abs_diff_eq(glam::DAffine3::IDENTITY, 1e-15)
}

impl Default for ModelInfo {
    fn default() -> Self {
        ModelInfo {
            display_units: LengthUnit::Millimeter,
            precision: 1,
            angle_precision: 1,
            length_snap: None,
            angle_snap: None,
            tolerances: Tolerances::default(),
            geo: None,
            title: String::new(),
            author: String::new(),
            description: String::new(),
            format_version: FORMAT_VERSION,
            next_pid: PidAllocator::default(),
            attrs: AttributeDictionaries::default(),
            axes: glam::DAffine3::IDENTITY,
        }
    }
}

/// Section display switches (docs/04 §1 "RenderingOptions … section settings").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionDisplay {
    /// Draw section plane widgets.
    pub planes_visible: bool,
    /// Apply cuts.
    pub cuts_visible: bool,
    /// Draw cap fills on solids.
    pub fills_visible: bool,
}

impl Default for SectionDisplay {
    fn default() -> Self {
        SectionDisplay {
            planes_visible: true,
            cuts_visible: true,
            fills_visible: true,
        }
    }
}

/// Active rendering state stored in the document (docs/04 §1 `RenderingOptions`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RenderingOptions {
    /// Shadows.
    pub shadows: ShadowSettings,
    /// Fog on/off (the fog parameters live in the active style).
    pub fog_enabled: bool,
    /// Section display.
    pub section: SectionDisplay,
}

/// Which `Entities` container (docs/04 §3, glossary "Container").
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
pub enum ContainerRef {
    /// The model root.
    Model,
    /// A component definition's contents.
    Definition(DefinitionId),
}

/// A resolved entity path (docs/04 §12).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    /// Container holding the entity.
    pub container: ContainerRef,
    /// The entity.
    pub entity: EntityId,
    /// Its pid.
    pub pid: Pid,
    /// Container-to-world transform of this occurrence.
    pub world: DAffine3,
}

/// Path resolution errors (`E1101`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// A step or the target does not exist.
    #[error("entity not found: {0}")]
    NotFound(EntityKey),
    /// A step is not a group or instance.
    #[error("path step is not a group or instance: {0}")]
    NotAContainer(EntityKey),
    /// Deeper than `Entities::MAX_DEPTH`.
    #[error("path deeper than the nesting limit")]
    TooDeep,
}

/// The whole document (docs/04 §1). Everything the user edits is reachable
/// from here; GUI layout, hover and tool state are not part of it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// Metadata, units, tolerances, pid counter.
    pub info: ModelInfo,
    /// The root container ("model space").
    pub model: Entities,
    /// Component definitions.
    pub definitions: StableMap<DefinitionId, ComponentDefinition>,
    /// Definition insertion order (file order, docs/09 §2).
    pub definition_order: Vec<DefinitionId>,
    /// Materials.
    pub materials: Materials,
    /// Tags.
    pub tags: Tags,
    /// Scenes.
    pub scenes: Scenes,
    /// Styles and the active one.
    pub styles: Styles,
    /// Active rendering state.
    pub rendering: RenderingOptions,
    /// Last camera (not undoable).
    pub camera: Camera,
    /// Document attributes.
    pub attrs: AttributeDictionaries,
    /// Image bytes; stored as separate container parts, not in the body.
    #[serde(skip)]
    pub images: ImageStore,
}

impl Default for Document {
    fn default() -> Self {
        Document {
            info: ModelInfo::default(),
            model: Entities::new(),
            definitions: StableMap::with_key(),
            definition_order: Vec::new(),
            materials: Materials::default(),
            tags: Tags::default(),
            scenes: Scenes::default(),
            styles: Styles::default(),
            rendering: RenderingOptions::default(),
            camera: Camera::default(),
            attrs: AttributeDictionaries::default(),
            images: ImageStore::default(),
        }
    }
}

impl Document {
    /// A new empty document with default materials, tags and style.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates the next persistent id (docs/04 §2).
    pub(crate) fn allocate_pid(&mut self) -> Pid {
        self.info.next_pid.allocate()
    }

    /// The document tolerances.
    pub fn tolerances(&self) -> &Tolerances {
        &self.info.tolerances
    }

    /// A container by reference.
    pub fn container(&self, c: ContainerRef) -> Option<&Entities> {
        match c {
            ContainerRef::Model => Some(&self.model),
            ContainerRef::Definition(id) => self.definitions.get(id).map(|d| &d.entities),
        }
    }

    /// Mutable container access for `commands::transaction` and file loading.
    pub(crate) fn container_mut(&mut self, c: ContainerRef) -> Option<&mut Entities> {
        match c {
            ContainerRef::Model => Some(&mut self.model),
            ContainerRef::Definition(id) => self.definitions.get_mut(id).map(|d| &mut d.entities),
        }
    }

    /// All containers in a deterministic order: the model, then definitions in
    /// insertion order.
    pub fn containers(&self) -> Vec<ContainerRef> {
        std::iter::once(ContainerRef::Model)
            .chain(
                self.definition_order
                    .iter()
                    .map(|id| ContainerRef::Definition(*id)),
            )
            .collect()
    }

    /// A definition by id.
    pub fn definition(&self, id: DefinitionId) -> Option<&ComponentDefinition> {
        self.definitions.get(id)
    }

    /// Definition by name (non-internal definitions only).
    pub fn definition_by_name(&self, name: &str) -> Option<DefinitionId> {
        self.definition_order.iter().copied().find(|id| {
            self.definitions
                .get(*id)
                .is_some_and(|d| !d.is_internal_group && d.name == name)
        })
    }

    /// Adds a definition at the end of the order.
    pub(crate) fn add_definition(&mut self, def: ComponentDefinition) -> DefinitionId {
        let id = self.definitions.insert(def);
        self.definition_order.push(id);
        id
    }

    /// Puts a removed definition back under its id at `index` in the order
    /// (undo of [`Document::remove_definition`]); `false` if the id is not a
    /// removed definition of this document.
    pub(crate) fn restore_definition(
        &mut self,
        id: DefinitionId,
        def: ComponentDefinition,
        index: usize,
    ) -> bool {
        if self.definitions.restore(id, def).is_err() {
            return false;
        }
        self.definition_order
            .insert(index.min(self.definition_order.len()), id);
        true
    }

    /// Removes a definition (callers ensure no instance references it). The
    /// id stays reserved for [`Document::restore_definition`].
    pub(crate) fn remove_definition(
        &mut self,
        id: DefinitionId,
    ) -> Option<(ComponentDefinition, usize)> {
        let index = self.definition_order.iter().position(|d| *d == id)?;
        let def = self.definitions.remove(id)?;
        self.definition_order.remove(index);
        Some((def, index))
    }

    /// Resolves an entity path to its container, id, pid and world transform
    /// (docs/04 §12, docs/08 §5.3).
    pub fn resolve(&self, path: &EntityPath) -> Result<Resolved, PathError> {
        let (container, world) = self.resolve_container(&path.instances)?;
        let entities = self
            .container(container)
            .ok_or(PathError::NotFound(path.target))?;
        let entity = entities
            .key_to_id(path.target)
            .ok_or(PathError::NotFound(path.target))?;
        Ok(Resolved {
            container,
            entity,
            pid: entities.pid_of(entity).unwrap_or(Pid::NONE),
            world,
        })
    }

    /// Resolves an instance path to the container it opens and its world
    /// transform (docs/04 §11 edit context).
    pub fn resolve_container(
        &self,
        path: &InstancePath,
    ) -> Result<(ContainerRef, DAffine3), PathError> {
        if path.depth() > Entities::MAX_DEPTH {
            return Err(PathError::TooDeep);
        }
        let mut container = ContainerRef::Model;
        let mut world = DAffine3::IDENTITY;
        for &step in &path.steps {
            let entities = self.container(container).ok_or(PathError::NotFound(step))?;
            let id = entities.key_to_id(step).ok_or(PathError::NotFound(step))?;
            let (Some(definition), Some(transform)) =
                (entities.definition_of(id), entities.transform_of(id))
            else {
                return Err(PathError::NotAContainer(step));
            };
            container = ContainerRef::Definition(definition);
            world *= transform;
        }
        Ok((container, world))
    }

    /// The number of groups and instances that use each definition, over
    /// every container (docs/04 §3.3 `usage_count`; a group's private
    /// definition counts its group).
    pub fn usage_counts(&self) -> BTreeMap<DefinitionId, u32> {
        let mut counts: BTreeMap<DefinitionId, u32> =
            self.definitions.iter().map(|(d, _)| (d, 0)).collect();
        for c in self.containers() {
            if let Some(entities) = self.container(c) {
                let users = entities
                    .groups()
                    .map(|(_, g)| g.definition)
                    .chain(entities.instances().map(|(_, i)| i.definition));
                for d in users {
                    if let Some(n) = counts.get_mut(&d) {
                        *n = n.saturating_add(1);
                    }
                }
            }
        }
        counts
    }

    /// Recomputes every definition's cached `usage_count` (after commands,
    /// undo/redo and repair; the cache is derived data, not undoable state).
    pub fn refresh_usage_counts(&mut self) {
        for (d, n) in self.usage_counts() {
            if let Some(def) = self.definitions.get_mut(d) {
                def.usage_count = n;
            }
        }
    }

    /// The deepest nesting in the document, or the first violation of
    /// docs/04 §13 (cycle) or §14 (hard depth limit), over the model and
    /// every definition (also those no occurrence reaches).
    pub fn nesting_depth(&self) -> Result<usize, HierarchyError> {
        let mut memo = BTreeMap::new();
        let mut depth = 0;
        for c in self.containers() {
            let Some(entities) = self.container(c) else {
                continue;
            };
            let mut stack: Vec<DefinitionId> = match c {
                ContainerRef::Model => Vec::new(),
                ContainerRef::Definition(d) => vec![d],
            };
            depth = depth.max(entities.depth_below(&self.definitions, &mut memo, &mut stack)?);
        }
        Ok(depth)
    }

    /// The first locked group or instance on `path`, if any: its contents
    /// cannot be edited until it is unlocked (docs/05 §5.13).
    pub fn locked_step(&self, path: &InstancePath) -> Option<EntityKey> {
        let mut container = ContainerRef::Model;
        for &step in &path.steps {
            let entities = self.container(container)?;
            let id = entities.key_to_id(step)?;
            if entities.is_locked(id) {
                return Some(step);
            }
            container = ContainerRef::Definition(entities.definition_of(id)?);
        }
        None
    }

    /// Finds the pid in any container (pids are document-unique, docs/04 §2).
    pub fn locate_pid(&self, pid: Pid) -> Option<(ContainerRef, EntityId)> {
        self.containers().into_iter().find_map(|c| {
            self.container(c)
                .and_then(|entities| entities.id_of(pid))
                .map(|id| (c, id))
        })
    }

    /// World-space bounds of the model (instances expanded).
    pub fn aabb(&self) -> Aabb {
        self.model.local_aabb(&self.definitions, false)
    }

    /// World bounds of an entity occurrence (docs/06 §7 zoom to selection):
    /// an edge's segment, a face's loops, or a group's or instance's
    /// contents; `None` for other entities and paths that do not resolve.
    pub fn occurrence_aabb(&self, path: &EntityPath) -> Option<Aabb> {
        let resolved = self.resolve(path).ok()?;
        let entities = self.container(resolved.container)?;
        let mesh = entities.mesh();
        let world = resolved.world;
        let bounds = match resolved.entity {
            EntityId::Edge(e) => mesh.edge_segment(e).ok()?.aabb().transformed(&world),
            EntityId::Face(f) => mesh.face_aabb(f).ok()?.transformed(&world),
            id => {
                let def = self.definition(entities.definition_of(id)?)?;
                let local = entities.transform_of(id)?;
                def.entities
                    .local_aabb(&self.definitions, false)
                    .transformed(&(world * local))
            }
        };
        (!bounds.is_empty()).then_some(bounds)
    }

    /// Lightweight clone for previews: copy-on-write at container granularity
    /// (docs/01 §3.1 rule 4). The draft clones eagerly; the optimization is
    /// transparent to callers.
    pub fn fork_for_preview(&self) -> Document {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_document_has_defaults() {
        let mut doc = Document::new();
        assert_eq!(doc.materials.len(), 2);
        assert_eq!(doc.tags.len(), 1);
        assert_eq!(doc.styles.len(), 1);
        assert_eq!(doc.containers(), vec![ContainerRef::Model]);
        assert_eq!(doc.allocate_pid(), Pid(1));
        assert_eq!(doc.allocate_pid(), Pid(2));
    }

    #[test]
    fn document_round_trips_through_msgpack_named() {
        let doc = Document::new();
        let bytes = rmp_serde::to_vec_named(&doc).unwrap();
        let back: Document = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(back.info, doc.info);
        assert_eq!(back.materials.len(), doc.materials.len());
        assert_eq!(back.tags.len(), doc.tags.len());
        assert_eq!(back.styles.len(), doc.styles.len());
    }

    #[test]
    fn paths_resolve_through_groups_and_pids_are_found_anywhere() {
        let mut doc = Document::new();
        let def = doc.add_definition(ComponentDefinition::new(uuid::Uuid::nil(), "Cabinet"));
        let tol = *doc.tolerances();
        let r = doc.definitions[def]
            .entities
            .mesh_mut()
            .add_edge(
                glam::DVec3::ZERO,
                glam::DVec3::X,
                &tol,
                &mut crate::geom::NoDeltas,
            )
            .unwrap();
        let edge = EntityId::Edge(r.edges[0]);
        doc.definitions[def]
            .entities
            .bind_pid(edge, Pid(10), None)
            .unwrap();
        let shift = DAffine3::from_translation(glam::DVec3::new(1.0, 2.0, 3.0));
        doc.model
            .insert(
                EntityRecord::Instance(ComponentInstance {
                    definition: def,
                    transform: shift,
                    name: Some("left".into()),
                    material_override: None,
                    glued_to: None,
                    dynamic_attrs: AttributeDictionaries::default(),
                }),
                Pid(11),
            )
            .unwrap();
        let path = EntityPath::new(
            InstancePath::root().child(EntityKey::Pid(Pid(11))),
            EntityKey::Pid(Pid(10)),
        );
        let resolved = doc.resolve(&path).unwrap();
        assert_eq!(resolved.container, ContainerRef::Definition(def));
        assert_eq!(resolved.entity, edge);
        assert_eq!(resolved.world, shift);
        assert_eq!(
            doc.locate_pid(Pid(10)),
            Some((ContainerRef::Definition(def), edge))
        );
        assert_eq!(doc.locate_pid(Pid(99)), None);
        let bad = EntityPath::new(
            InstancePath::root().child(EntityKey::Pid(Pid(10))),
            EntityKey::Pid(Pid(10)),
        );
        assert_eq!(
            doc.resolve(&bad),
            Err(PathError::NotFound(EntityKey::Pid(Pid(10))))
        );
        assert_eq!(doc.aabb().max, glam::DVec3::new(2.0, 2.0, 3.0));
    }

    #[test]
    fn definitions_keep_insertion_order() {
        let mut doc = Document::new();
        let a = doc.add_definition(ComponentDefinition::new(uuid::Uuid::nil(), "A"));
        let b = doc.add_definition(ComponentDefinition::new(uuid::Uuid::nil(), "B"));
        assert_eq!(
            doc.containers(),
            vec![
                ContainerRef::Model,
                ContainerRef::Definition(a),
                ContainerRef::Definition(b)
            ]
        );
        assert_eq!(doc.definition_by_name("B"), Some(b));
        assert_eq!(doc.remove_definition(a).map(|(_, i)| i), Some(0));
        assert_eq!(
            doc.containers(),
            vec![ContainerRef::Model, ContainerRef::Definition(b)]
        );
    }
}
