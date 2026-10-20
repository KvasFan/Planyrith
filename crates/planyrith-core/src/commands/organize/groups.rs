//! Implements docs/05-modeling-tools.md §5.1 `make-group`, §5.2
//! `make-component` and §5.3 `explode`.
//!
//! Making a group copies the selected faces and edges (with the faces'
//! boundary edges) into a new definition built from the indexed mesh form,
//! so orientation, materials, flags and tags carry over exactly; the
//! definition origin is the selection's bounding-box minimum. Selected
//! faces, and edges no unselected face still needs, are then removed from the
//! source; edges shared with unselected faces stay on both sides (docs/05
//! Decisions). Exploding redraws the definition's geometry in the parent
//! with the occurrence transform (`add_edge` semantics, so it merges with
//! what is there) and re-creates nested occurrences.

use std::collections::{BTreeMap, BTreeSet};

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{
    FaceCopy, redraw_geometry, refuse_locked, remove_container_entity, targets_in,
};
use crate::commands::params::{Target, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{
    ComponentBehavior, ComponentDefinition, ComponentInstance, ContainerRef, Document, EntityKey,
    EntityPath, EntityRecord, EntityView, Group, Guide, Query, Selection,
};
use crate::geom::mesh::{DirectedEdgeIndex, IndexedEdge, IndexedFace, IndexedMesh};
use crate::geom::{EdgeFlags, HalfEdgeMesh};
use crate::ids::{EdgeId, EntityId, FaceId, Pid, VertexId};
use crate::math::Aabb;
use crate::math::transform::translation;

fn selection_query() -> Query {
    Query::new("selection")
}

/// A deterministic definition UUID: commands may not use randomness, so the
/// UUID is derived from the document's pid counter (unique and monotonic).
fn definition_uuid(doc: &Document) -> uuid::Uuid {
    uuid::Uuid::from_u64_pair(0x706c_7972_6974_6800, doc.info.next_pid.next_raw())
}

/// What `capture` collected from the source container.
struct Captured {
    /// The definition's contents, in definition coordinates.
    entities: crate::document::Entities,
    /// Selected faces (removed from the source).
    faces: Vec<FaceId>,
    /// Edges that go with the selection (removed when no face needs them).
    edges: Vec<EdgeId>,
    /// Non-mesh entities to move into the definition.
    others: Vec<EntityId>,
    /// Bounding-box minimum (container coordinates): the definition origin.
    origin: DVec3,
}

/// Copies the selected geometry into a new `Entities` relative to its
/// bounding-box minimum.
fn capture(
    tx: &mut Transaction<'_>,
    target: &Target,
    ids: &[EntityId],
) -> Result<Captured, CommandError> {
    let doc = tx.document();
    let source = doc
        .container(target.container)
        .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
    let mesh = source.mesh();
    let mut faces: Vec<FaceId> = Vec::new();
    let mut edges: BTreeSet<EdgeId> = BTreeSet::new();
    let mut others = Vec::new();
    for &id in ids {
        match id {
            EntityId::Face(f) => {
                faces.push(f);
                edges.extend(mesh.face_edges(f)?);
            }
            EntityId::Edge(e) => {
                edges.insert(e);
            }
            other => others.push(other),
        }
    }
    let edges: Vec<EdgeId> = edges.into_iter().collect();

    // Bounds of everything selected.
    let mut bb = Aabb::EMPTY;
    for &e in &edges {
        let s = mesh.edge_segment(e)?;
        bb = bb.including(s.a).including(s.b);
    }
    for &id in &others {
        if let (Some(def), Some(t)) = (
            source.definition_of(id).and_then(|d| doc.definition(d)),
            source.transform_of(id),
        ) {
            let inner = def.entities.local_aabb(&doc.definitions, false);
            if !inner.is_empty() {
                bb = bb.union(&inner.transformed(&t));
            }
        } else if let Some(EntityView::Guide(g)) = source.view(id) {
            bb = bb.including(match g {
                Guide::Line { origin, .. } => *origin,
                Guide::Point { pos } => *pos,
            });
        }
    }
    let origin = if bb.is_empty() { DVec3::ZERO } else { bb.min };
    let shift = translation(-origin);

    // Indexed form of the captured geometry.
    let mut vertex_index: BTreeMap<VertexId, u32> = BTreeMap::new();
    let mut indexed = IndexedMesh::default();
    let mut edge_index: BTreeMap<EdgeId, u32> = BTreeMap::new();
    for &e in &edges {
        let (a, b) = mesh.edge_endpoints(e)?;
        let mut idx = |v: VertexId, indexed: &mut IndexedMesh| -> Result<u32, CommandError> {
            if let Some(&i) = vertex_index.get(&v) {
                return Ok(i);
            }
            let i = indexed.vertices.len() as u32;
            indexed.vertices.push(mesh.position(v)? - origin);
            vertex_index.insert(v, i);
            Ok(i)
        };
        let (ia, ib) = (idx(a, &mut indexed)?, idx(b, &mut indexed)?);
        edge_index.insert(e, indexed.edges.len() as u32);
        indexed.edges.push(IndexedEdge {
            a: ia,
            b: ib,
            flags: mesh.edge(e).map(|x| x.flags).unwrap_or(EdgeFlags::NONE),
            curve: None,
        });
    }
    for &f in &faces {
        let face = mesh.face_ref(f)?;
        let mut loops = Vec::new();
        for cycle in mesh.face_cycles(f)? {
            let mut directed = Vec::with_capacity(cycle.len());
            for h in cycle {
                let he = mesh.he(h)?;
                let i = edge_index[&he.edge];
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
            plane: Some(face.plane.transformed(&shift)),
            sides: face.sides.clone(),
        });
    }
    let built = HalfEdgeMesh::from_indexed(&indexed, doc.tolerances())?;

    // Bind pids (edges then faces, the indexed order) and carry tags/hidden.
    let mut entities = crate::document::Entities::new();
    *entities.mesh_mut() = built;
    let (_, ids_map) = entities.mesh().to_indexed();
    let mut rows = Vec::new();
    for (i, &e) in edges.iter().enumerate() {
        rows.push((EntityId::Edge(ids_map.edges[i]), EntityId::Edge(e)));
    }
    for (j, &f) in faces.iter().enumerate() {
        rows.push((EntityId::Face(ids_map.faces[j]), EntityId::Face(f)));
    }
    let carried: Vec<(EntityId, Option<crate::ids::TagId>, bool)> = rows
        .iter()
        .map(|&(new, old)| (new, source.tag_of(old), source.is_hidden(old)))
        .collect();
    for (new, tag, hidden) in carried {
        let pid = tx.allocate_pid();
        entities
            .bind_pid(new, pid, None)
            .map_err(|e| CommandError::Internal(e.to_string()))?;
        if tag.is_some() {
            let _ = entities.set_tag(new, tag);
        }
        if hidden {
            let _ = entities.set_hidden(new, true);
        }
    }
    Ok(Captured {
        entities,
        faces,
        edges,
        others,
        origin,
    })
}

/// Moves the captured selection into definition `d` (already inserted) and
/// removes it from the source container.
fn hand_over(
    tx: &mut Transaction<'_>,
    target: &Target,
    captured: &Captured,
    d: crate::ids::DefinitionId,
) -> Result<(), CommandError> {
    let container = target.container;
    let (faces, edges) = (captured.faces.clone(), captured.edges.clone());
    tx.with_mesh(container, |mesh, tol, log| {
        mesh.transact(tol, log, |m, log| {
            for &f in &faces {
                if m.face(f).is_some() {
                    m.erase_face_inner(f, false, log)?;
                }
            }
            for &e in &edges {
                if m.edge(e).is_some() && m.edge_faces(e)? == [None, None] {
                    let (a, b) = m.edge_endpoints(e)?;
                    m.delete_edge(e, log)?;
                    m.delete_if_isolated(a, log)?;
                    m.delete_if_isolated(b, log)?;
                }
            }
            Ok(())
        })
    })?;
    let shift = translation(-captured.origin);
    for &id in &captured.others {
        let record = match tx.document().container(container).and_then(|e| e.view(id)) {
            Some(EntityView::Group(g)) => EntityRecord::Group(Group {
                transform: shift * g.transform,
                ..g.clone()
            }),
            Some(EntityView::Instance(i)) => EntityRecord::Instance(ComponentInstance {
                transform: shift * i.transform,
                ..i.clone()
            }),
            Some(EntityView::Image(i)) => {
                let mut i = i.clone();
                i.transform = shift * i.transform;
                EntityRecord::Image(i)
            }
            Some(EntityView::Guide(g)) => EntityRecord::Guide(match g {
                Guide::Line {
                    origin,
                    dir,
                    finite,
                } => Guide::Line {
                    origin: *origin - captured.origin,
                    dir: *dir,
                    finite: *finite,
                },
                Guide::Point { pos } => Guide::Point {
                    pos: *pos - captured.origin,
                },
            }),
            Some(EntityView::Text(t)) => EntityRecord::Text(t.clone()),
            Some(EntityView::Dimension(dim)) => EntityRecord::Dimension(dim.clone()),
            Some(EntityView::SectionPlane(s)) => {
                let mut s = s.clone();
                s.plane = s.plane.transformed(&shift);
                EntityRecord::SectionPlane(s)
            }
            _ => continue,
        };
        tx.remove_entity(container, id)?;
        tx.insert_entity(ContainerRef::Definition(d), record)?;
    }
    Ok(())
}

/// Selects the new occurrence after the command.
fn select_new(tx: &mut Transaction<'_>, pid: Pid) {
    let path = EntityPath::new(tx.context().path.clone(), EntityKey::Pid(pid));
    tx.set_selection(Selection::single(path));
}

// ----- make-group ---------------------------------------------------------------------------

/// `make-group` parameters (docs/05 §5.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MakeGroupParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// What goes into the group (default: the selection).
    #[serde(default = "selection_query")]
    pub targets: Query,
    /// Group name.
    #[serde(default)]
    pub name: Option<String>,
}

/// `make-group` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MakeGroupOutput {
    /// The new group.
    pub group: Pid,
}

/// The `make-group` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct MakeGroup;

impl Command for MakeGroup {
    type Params = MakeGroupParams;
    type Output = MakeGroupOutput;

    fn name(&self) -> &'static str {
        "make-group"
    }

    fn validate(&self, _doc: &Document, p: &MakeGroupParams) -> Result<(), CommandError> {
        p.targets
            .parse()
            .map(|_| ())
            .map_err(|e| CommandError::invalid_param("targets", e.to_string()))
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MakeGroupParams,
    ) -> Result<MakeGroupOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let ids = targets_in(tx, &target, &p.targets)?;
        if ids.is_empty() {
            return Err(CommandError::NotFound(crate::document::EntityRef::Query(
                p.targets,
            )));
        }
        refuse_locked(tx, target.container, &ids)?;
        let uuid = definition_uuid(tx.document());
        let captured = capture(tx, &target, &ids)?;
        let name = p.name.unwrap_or_default();
        let mut def = ComponentDefinition::new(uuid, format!("Group {}", uuid.as_u64_pair().1));
        def.is_internal_group = true;
        def.entities = captured.entities.clone();
        let d = tx.insert_definition(def)?;
        hand_over(tx, &target, &captured, d)?;
        let (_, pid) = tx.insert_entity(
            target.container,
            EntityRecord::Group(Group {
                definition: d,
                transform: translation(captured.origin),
                name,
            }),
        )?;
        select_new(tx, pid);
        Ok(MakeGroupOutput { group: pid })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "make-group".into(),
            category: CommandCategory::Organize,
            label: "Make Group".into(),
            view_only: false,
            steps: Vec::new(),
        }
    }
}

// ----- make-component -----------------------------------------------------------------------

/// `make-component` parameters (docs/05 §5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MakeComponentParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// What goes into the component (default: the selection).
    #[serde(default = "selection_query")]
    pub targets: Query,
    /// Unique definition name.
    pub name: String,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Behavior as JSON (`{"glue_to": "horizontal", "cuts_opening": true}`).
    #[serde(default)]
    pub behavior: Option<serde_json::Value>,
    /// Replace the selection with an instance (true) or only create the definition.
    #[serde(default = "yes")]
    pub replace_selection: bool,
}

fn yes() -> bool {
    true
}

/// `make-component` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MakeComponentOutput {
    /// The definition's UUID.
    pub definition: String,
    /// The instance replacing the selection, if any.
    pub instance: Option<Pid>,
}

/// The `make-component` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct MakeComponent;

fn behavior_of(p: &MakeComponentParams) -> Result<ComponentBehavior, CommandError> {
    let defaults = serde_json::to_value(ComponentBehavior::default())
        .map_err(|e| CommandError::Internal(e.to_string()))?;
    let Some(given) = &p.behavior else {
        return Ok(ComponentBehavior::default());
    };
    let (Some(base), Some(overrides)) = (defaults.as_object(), given.as_object()) else {
        return Err(CommandError::invalid_param(
            "behavior",
            "expected a JSON object",
        ));
    };
    let mut merged = base.clone();
    for (k, v) in overrides {
        if !merged.contains_key(k) {
            return Err(CommandError::invalid_param(
                "behavior",
                format!("unknown field `{k}`"),
            ));
        }
        merged.insert(k.clone(), v.clone());
    }
    serde_json::from_value(serde_json::Value::Object(merged))
        .map_err(|e| CommandError::invalid_param("behavior", e.to_string()))
}

impl Command for MakeComponent {
    type Params = MakeComponentParams;
    type Output = MakeComponentOutput;

    fn name(&self) -> &'static str {
        "make-component"
    }

    fn validate(&self, doc: &Document, p: &MakeComponentParams) -> Result<(), CommandError> {
        p.targets
            .parse()
            .map_err(|e| CommandError::invalid_param("targets", e.to_string()))?;
        if p.name.trim().is_empty() {
            return Err(CommandError::invalid_param("name", "must not be empty"));
        }
        if doc.definition_by_name(&p.name).is_some() {
            return Err(CommandError::invalid_param(
                "name",
                format!("a component named `{}` exists", p.name),
            ));
        }
        behavior_of(p).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MakeComponentParams,
    ) -> Result<MakeComponentOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let ids = targets_in(tx, &target, &p.targets)?;
        if ids.is_empty() {
            return Err(CommandError::NotFound(crate::document::EntityRef::Query(
                p.targets,
            )));
        }
        refuse_locked(tx, target.container, &ids)?;
        let uuid = definition_uuid(tx.document());
        let captured = capture(tx, &target, &ids)?;
        let mut def = ComponentDefinition::new(uuid, p.name.clone());
        def.description = p.description.clone();
        def.behavior = behavior_of(&p)?;
        def.entities = captured.entities.clone();
        let d = tx.insert_definition(def)?;
        let mut instance = None;
        if p.replace_selection {
            hand_over(tx, &target, &captured, d)?;
            let (_, pid) = tx.insert_entity(
                target.container,
                EntityRecord::Instance(ComponentInstance {
                    definition: d,
                    transform: translation(captured.origin),
                    name: None,
                    material_override: None,
                    glued_to: None,
                    dynamic_attrs: Default::default(),
                }),
            )?;
            select_new(tx, pid);
            instance = Some(pid);
        }
        Ok(MakeComponentOutput {
            definition: uuid.to_string(),
            instance,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "make-component".into(),
            category: CommandCategory::Organize,
            label: "Make Component".into(),
            view_only: false,
            steps: Vec::new(),
        }
    }
}

// ----- explode ------------------------------------------------------------------------------

/// `explode` parameters (docs/05 §5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExplodeParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Groups and instances to explode (default: the selection).
    #[serde(default = "selection_query")]
    pub targets: Query,
}

/// `explode` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExplodeOutput {
    /// Occurrences exploded.
    pub exploded: usize,
    /// Pids created in the parent.
    pub created: Vec<Pid>,
}

/// The `explode` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Explode;

/// Redraws one occurrence's contents into its parent container.
fn explode_one(
    tx: &mut Transaction<'_>,
    container: ContainerRef,
    id: EntityId,
) -> Result<(), CommandError> {
    let doc = tx.document();
    let Some(entities) = doc.container(container) else {
        return Ok(());
    };
    let (Some(d), Some(t)) = (entities.definition_of(id), entities.transform_of(id)) else {
        return Ok(());
    };
    let override_material = match entities.view(id) {
        Some(EntityView::Instance(i)) => i.material_override,
        _ => None,
    };
    let Some(def) = doc.definition(d) else {
        return Ok(());
    };
    let mesh = def.entities.mesh();
    let segments = mesh
        .edges()
        .map(|(e, edge)| {
            let s = mesh.edge_segment(e)?;
            Ok((t.transform_point3(s.a), t.transform_point3(s.b), edge.flags))
        })
        .collect::<Result<Vec<_>, CommandError>>()?;
    let faces: Vec<FaceCopy> = mesh
        .faces()
        .map(|(_, face)| {
            let outer = mesh.loop_positions(face.outer)?;
            let mut sides = face.sides.clone();
            if sides.material_front.is_none() {
                sides.material_front = override_material;
            }
            if sides.material_back.is_none() {
                sides.material_back = override_material;
            }
            Ok((
                outer.iter().map(|&p| t.transform_point3(p)).collect(),
                face.plane.transformed(&t),
                sides,
            ))
        })
        .collect::<Result<_, CommandError>>()?;
    let nested: Vec<EntityRecord> = def
        .entities
        .iter()
        .filter_map(|cid| match def.entities.view(cid)? {
            EntityView::Group(g) => Some(EntityRecord::Group(Group {
                transform: t * g.transform,
                ..g.clone()
            })),
            EntityView::Instance(i) => Some(EntityRecord::Instance(ComponentInstance {
                transform: t * i.transform,
                ..i.clone()
            })),
            EntityView::Guide(g) => Some(EntityRecord::Guide(match g {
                Guide::Line {
                    origin,
                    dir,
                    finite,
                } => Guide::Line {
                    origin: t.transform_point3(*origin),
                    dir: t.transform_vector3(*dir).normalize_or_zero(),
                    finite: *finite,
                },
                Guide::Point { pos } => Guide::Point {
                    pos: t.transform_point3(*pos),
                },
            })),
            _ => None,
        })
        .collect();
    redraw_geometry(tx, container, &segments, &faces)?;
    for record in nested {
        tx.insert_entity(container, record)?;
    }
    remove_container_entity(tx, container, id)
}

impl Command for Explode {
    type Params = ExplodeParams;
    type Output = ExplodeOutput;

    fn name(&self) -> &'static str {
        "explode"
    }

    fn validate(&self, _doc: &Document, p: &ExplodeParams) -> Result<(), CommandError> {
        p.targets
            .parse()
            .map(|_| ())
            .map_err(|e| CommandError::invalid_param("targets", e.to_string()))
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ExplodeParams,
    ) -> Result<ExplodeOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let ids: Vec<EntityId> = targets_in(tx, &target, &p.targets)?
            .into_iter()
            .filter(|id| matches!(id, EntityId::Group(_) | EntityId::Instance(_)))
            .collect();
        refuse_locked(tx, target.container, &ids)?;
        let before = tx.created().len();
        for &id in &ids {
            explode_one(tx, target.container, id)?;
        }
        Ok(ExplodeOutput {
            exploded: ids.len(),
            created: tx.created()[before..].to_vec(),
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "explode".into(),
            category: CommandCategory::Organize,
            label: "Explode".into(),
            view_only: false,
            steps: Vec::new(),
        }
    }
}
