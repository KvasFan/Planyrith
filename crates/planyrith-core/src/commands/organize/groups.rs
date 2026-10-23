//! Implements docs/05-modeling-tools.md §5.1 `make-group`, §5.2
//! `make-component` and §5.3 `explode`.
//!
//! Making a group copies the selected faces and edges (with the faces'
//! boundary edges) into a new definition built from the indexed mesh form,
//! so orientation, materials, flags, curves and organization rows carry over
//! exactly; selected non-mesh entities move in with their rows. The
//! definition is complete before it is inserted (undo/redo restore it from
//! that snapshot); the definition origin is the selection's bounding-box
//! minimum. Selected faces, and edges no unselected face still needs, are
//! then removed from the source; edges shared with unselected faces stay on
//! both sides (docs/05 Decisions). Exploding inserts the definition's
//! geometry exactly when it touches nothing in the parent, and otherwise
//! redraws it with `add_edge` semantics so it merges with what is there.

use std::collections::BTreeSet;

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{
    SideRows, definition_uuid, mesh_piece, place_geometry, refuse_locked, remove_container_entity,
    targets_in,
};
use crate::commands::params::{Target, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{
    ComponentBehavior, ComponentDefinition, ComponentInstance, ContainerRef, Document, Entities,
    EntityKey, EntityPath, EntityRecord, EntityView, Group, Guide, Query, Selection,
};
use crate::geom::HalfEdgeMesh;
use crate::geom::mesh::transform_indexed;
use crate::ids::{EdgeId, EntityId, FaceId, Pid};
use crate::math::Aabb;
use crate::math::transform::translation;

fn selection_query() -> Query {
    Query::new("selection")
}

/// A non-mesh entity's record after moving its container through `t`
/// (`None` for mesh elements). Texts and dimensions keep their anchors as
/// they are (annotations arrive with milestone 0.6).
fn moved_record(view: EntityView<'_>, t: &DAffine3) -> Option<EntityRecord> {
    Some(match view {
        EntityView::Group(g) => EntityRecord::Group(Group {
            transform: *t * g.transform,
            ..g.clone()
        }),
        EntityView::Instance(i) => EntityRecord::Instance(ComponentInstance {
            transform: *t * i.transform,
            ..i.clone()
        }),
        EntityView::Image(i) => {
            let mut i = i.clone();
            i.transform = *t * i.transform;
            EntityRecord::Image(i)
        }
        EntityView::Guide(g) => EntityRecord::Guide(match g {
            Guide::Line {
                origin,
                dir,
                finite,
            } => {
                let moved = t.transform_vector3(*dir);
                let k = moved.length();
                Guide::Line {
                    origin: t.transform_point3(*origin),
                    dir: moved.normalize_or_zero(),
                    finite: finite.map(|(a, b)| (a * k, b * k)),
                }
            }
            Guide::Point { pos } => Guide::Point {
                pos: t.transform_point3(*pos),
            },
        }),
        EntityView::Text(text) => EntityRecord::Text(text.clone()),
        EntityView::Dimension(dim) => EntityRecord::Dimension(dim.clone()),
        EntityView::SectionPlane(s) => {
            let mut s = s.clone();
            s.plane = s.plane.transformed(t);
            EntityRecord::SectionPlane(s)
        }
        EntityView::Edge(_) | EntityView::Face(_) => return None,
    })
}

/// What `capture` collected from the source container.
struct Captured {
    /// The complete definition contents, in definition coordinates.
    entities: Entities,
    /// Selected faces (removed from the source).
    faces: Vec<FaceId>,
    /// Edges that go with the selection (removed when no face needs them).
    edges: Vec<EdgeId>,
    /// Non-mesh entities that moved into the definition.
    others: Vec<EntityId>,
    /// Bounding-box minimum (container coordinates): the definition origin.
    origin: DVec3,
}

/// Copies the selection into a new, complete `Entities` relative to its
/// bounding-box minimum: faces and edges with curves, flags, materials and
/// organization rows, plus the selected non-mesh entities with theirs. The
/// definition must be complete before it is inserted, because undo/redo
/// restore it from the `DefinitionInserted` snapshot.
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

    let piece = mesh_piece(source, &faces, &edges)?;
    let indexed = transform_indexed(&piece.indexed, &shift);
    let built = HalfEdgeMesh::from_indexed(&indexed, doc.tolerances())?;

    // Organization rows: mesh elements (edges then faces, the indexed order)
    // and the moved non-mesh entities.
    let mut entities = Entities::new();
    *entities.mesh_mut() = built;
    let (_, ids_map) = entities.mesh().to_indexed();
    let mut mesh_rows = Vec::new();
    for (i, &e) in piece.edges.iter().enumerate() {
        let id = EntityId::Edge(e);
        mesh_rows.push((EntityId::Edge(ids_map.edges[i]), SideRows::of(source, id)));
    }
    for (j, &f) in piece.faces.iter().enumerate() {
        let id = EntityId::Face(f);
        mesh_rows.push((EntityId::Face(ids_map.faces[j]), SideRows::of(source, id)));
    }
    let moved: Vec<(EntityRecord, SideRows)> = others
        .iter()
        .filter_map(|&id| {
            let record = moved_record(source.view(id)?, &shift)?;
            Some((record, SideRows::of(source, id)))
        })
        .collect();
    let internal = |e: crate::document::HierarchyError| CommandError::Internal(e.to_string());
    for (new, rows) in mesh_rows {
        let pid = tx.allocate_pid();
        entities.bind_pid(new, pid, None).map_err(internal)?;
        rows.write_into(&mut entities, new)?;
    }
    for (record, rows) in moved {
        let pid = tx.allocate_pid();
        let new = entities.insert(record, pid).map_err(internal)?;
        rows.write_into(&mut entities, new)?;
    }
    Ok(Captured {
        entities,
        faces,
        edges,
        others,
        origin,
    })
}

/// Removes the captured selection from the source container (the
/// definition already holds its copy). Edges an unselected face still needs
/// stay (docs/05 Decisions).
fn remove_captured(
    tx: &mut Transaction<'_>,
    target: &Target,
    captured: &Captured,
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
    for &id in &captured.others {
        tx.remove_entity(container, id)?;
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
        let mut captured = capture(tx, &target, &ids)?;
        let name = p.name.unwrap_or_default();
        let mut def = ComponentDefinition::new(uuid, format!("Group {}", uuid.as_u64_pair().1));
        def.is_internal_group = true;
        def.entities = std::mem::take(&mut captured.entities);
        let d = tx.insert_definition(def)?;
        remove_captured(tx, &target, &captured)?;
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
        let mut captured = capture(tx, &target, &ids)?;
        let mut def = ComponentDefinition::new(uuid, p.name.clone());
        def.description = p.description.clone();
        def.behavior = behavior_of(&p)?;
        def.entities = std::mem::take(&mut captured.entities);
        let d = tx.insert_definition(def)?;
        let mut instance = None;
        if p.replace_selection {
            remove_captured(tx, &target, &captured)?;
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

/// Moves one occurrence's contents into its parent container under the
/// occurrence transform. Geometry that touches nothing in the parent is
/// inserted exactly (faces with their holes, face-less edges, curves, flags);
/// geometry that touches the parent is redrawn with `add_edge` so it merges
/// with what is there (docs/05 §5.3, Decisions). Organization rows and
/// nested entities come along.
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
    let inner = &def.entities;
    let mesh = inner.mesh();
    // Container order (pid order), not slot order: the result must not depend
    // on the slot layout, which a save/reload renumbers.
    let edge_order: Vec<EdgeId> = inner
        .order()
        .iter()
        .filter_map(|id| match id {
            EntityId::Edge(e) => Some(*e),
            _ => None,
        })
        .collect();
    let face_order: Vec<FaceId> = inner
        .order()
        .iter()
        .filter_map(|id| match id {
            EntityId::Face(f) => Some(*f),
            _ => None,
        })
        .collect();
    let (indexed, local) = mesh.to_indexed_ordered(&edge_order, &face_order);
    let mut piece = transform_indexed(&indexed, &t);
    for face in &mut piece.faces {
        if face.sides.material_front.is_none() {
            face.sides.material_front = override_material;
        }
        if face.sides.material_back.is_none() {
            face.sides.material_back = override_material;
        }
    }
    let edge_rows: Vec<SideRows> = local
        .edges
        .iter()
        .map(|&e| SideRows::of(inner, EntityId::Edge(e)))
        .collect();
    let face_rows: Vec<SideRows> = local
        .faces
        .iter()
        .map(|&f| SideRows::of(inner, EntityId::Face(f)))
        .collect();
    let nested: Vec<(EntityRecord, SideRows)> = inner
        .iter()
        .filter_map(|cid| {
            let record = moved_record(inner.view(cid)?, &t)?;
            Some((record, SideRows::of(inner, cid)))
        })
        .collect();
    place_geometry(tx, container, &piece, edge_rows, face_rows)?;
    for (record, rows) in nested {
        let (new, _) = tx.insert_entity(container, record)?;
        rows.apply(tx, container, new)?;
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
