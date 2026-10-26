//! Implements docs/05-modeling-tools.md §4.4 `move`, §4.5 `rotate` and §4.6
//! `scale`: raw geometry moves its vertices with `transform_vertices`
//! (merging and auto-fold, docs/03 §4.8); groups, instances and images change
//! their transform; guides and section planes are re-placed. With `copy`,
//! copies are made instead (linear or radial arrays with `copies`).

use std::collections::BTreeSet;

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{SideRows, mesh_piece, place_geometry, refuse_locked, targets_in};
use crate::commands::params::{PointSchema, Target, VectorSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, ToolStep, Transaction,
};
use crate::document::{Document, EntityRecord, EntityView, Guide, Query};
use crate::geom::TransformOptions;
use crate::geom::mesh::transform_indexed;
use crate::ids::{EdgeId, EntityId, FaceId, Pid, VertexId};
use crate::inference::InferenceKind;
use crate::math::Aabb;
use crate::math::transform::{rotation_about, scale_about, translation};
use crate::tolerance::Tolerances;
use crate::units::Angle;

/// How array copies are spaced (docs/05 §4.4, §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Spacing {
    /// The offset (or angle) separates consecutive copies (`*N`).
    #[default]
    Between,
    /// The offset (or angle) is the total span, divided among the copies (`/N`).
    Total,
}

fn one() -> u32 {
    1
}

fn yes() -> bool {
    true
}

fn z_axis() -> DVec3 {
    DVec3::Z
}

/// Output of `move` and `rotate` (docs/05 §4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TransformOutput {
    /// Entities moved (0 when copying).
    pub moved: usize,
    /// Pids created by copies (edges, faces, groups, instances…).
    pub copies: Vec<Pid>,
    /// Faces split by auto-fold.
    pub folded_faces: usize,
    /// Vertex pairs merged by tolerance (`W2002`).
    pub merged_vertices: usize,
}

/// Moves or copies `ids` by the world transform `world`.
fn transform_entities(
    tx: &mut Transaction<'_>,
    target: &Target,
    ids: &[EntityId],
    world: DAffine3,
    opts: TransformOptions,
) -> Result<TransformOutput, CommandError> {
    let local = target.local * world * target.world;
    let container = target.container;
    let entities = tx
        .document()
        .container(container)
        .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
    let mesh = entities.mesh();
    let mut vertices = BTreeSet::new();
    for id in ids {
        match *id {
            EntityId::Edge(e) => {
                let (a, b) = mesh.edge_endpoints(e)?;
                vertices.extend([a, b]);
            }
            EntityId::Face(f) => {
                for cycle in mesh.face_cycles(f)? {
                    for h in cycle {
                        vertices.insert(mesh.he(h)?.origin);
                    }
                }
            }
            _ => {}
        }
    }
    // Components glued to faces that move entirely go along (docs/04 §3.3).
    let glued: Vec<EntityId> = entities
        .instances()
        .filter_map(|(i, x)| {
            let id = EntityId::Instance(i);
            if ids.contains(&id) {
                return None;
            }
            let face = match entities.key_to_id(x.glued_to.as_ref()?.target)? {
                EntityId::Face(f) => f,
                _ => return None,
            };
            let all_move = mesh
                .face_cycles(face)
                .ok()?
                .iter()
                .flatten()
                .all(|h| mesh.he(*h).is_ok_and(|he| vertices.contains(&he.origin)));
            all_move.then_some(id)
        })
        .collect();
    let mut out = TransformOutput {
        moved: ids.len(),
        copies: Vec::new(),
        folded_faces: 0,
        merged_vertices: 0,
    };
    if !vertices.is_empty() {
        let set: Vec<VertexId> = vertices.into_iter().collect();
        let r = tx.with_mesh(container, |m, tol, d| {
            m.transform_vertices(&set, &local, opts, tol, d)
        })?;
        out.folded_faces = r.folded_faces.len();
        out.merged_vertices = r.merged_vertices;
        if r.merged_vertices > 0 {
            tx.warn(crate::diagnostics::Warning::new(
                crate::diagnostics::WarningCode::W2002,
                format!("{} vertex pairs merged", r.merged_vertices),
            ));
        }
    }
    for &id in ids.iter().chain(glued.iter()) {
        if let Some(record) = placed_record(tx.document(), container, id, &local) {
            match record {
                Placed::Transform(t) => tx.set_transform(container, id, t)?,
                Placed::Record(r) => tx.replace_entity(container, id, r)?,
            }
        }
    }
    Ok(out)
}

/// The new placement of a non-mesh entity under `local`.
enum Placed {
    Transform(DAffine3),
    Record(EntityRecord),
}

fn placed_record(
    doc: &Document,
    container: crate::document::ContainerRef,
    id: EntityId,
    local: &DAffine3,
) -> Option<Placed> {
    let entities = doc.container(container)?;
    match entities.view(id)? {
        EntityView::Group(g) => Some(Placed::Transform(*local * g.transform)),
        EntityView::Instance(i) => Some(Placed::Transform(*local * i.transform)),
        EntityView::Image(i) => Some(Placed::Transform(*local * i.transform)),
        EntityView::Guide(g) => Some(Placed::Record(EntityRecord::Guide(match g {
            Guide::Line {
                origin,
                dir,
                finite,
            } => Guide::Line {
                origin: local.transform_point3(*origin),
                dir: local.transform_vector3(*dir).normalize_or_zero(),
                finite: *finite,
            },
            Guide::Point { pos } => Guide::Point {
                pos: local.transform_point3(*pos),
            },
        }))),
        EntityView::SectionPlane(s) => {
            let mut s = s.clone();
            s.plane = s.plane.transformed(local);
            Some(Placed::Record(EntityRecord::SectionPlane(s)))
        }
        _ => None,
    }
}

/// Copies `ids` under `world`. Geometry is copied exactly (faces with their
/// holes, edges with flags, curves, materials, tags and hidden state) when the
/// copy touches nothing, and otherwise redrawn with `add_edge` so it merges
/// with what is there, leaving existing faces as they are (docs/05
/// Decisions). Groups and instances become new occurrences of the same
/// definition, with the originals' tags, hidden state and attributes.
fn copy_entities(
    tx: &mut Transaction<'_>,
    target: &Target,
    ids: &[EntityId],
    world: DAffine3,
) -> Result<Vec<Pid>, CommandError> {
    let local = target.local * world * target.world;
    let container = target.container;
    let entities = tx
        .document()
        .container(container)
        .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
    let mut edges: Vec<EdgeId> = Vec::new();
    let mut faces: Vec<FaceId> = Vec::new();
    for id in ids {
        match *id {
            EntityId::Edge(e) => edges.push(e),
            EntityId::Face(f) => faces.push(f),
            _ => {}
        }
    }
    let piece = mesh_piece(entities, &faces, &edges)?;
    let edge_rows: Vec<SideRows> = piece
        .edges
        .iter()
        .map(|&e| SideRows::of(entities, EntityId::Edge(e)))
        .collect();
    let face_rows: Vec<SideRows> = piece
        .faces
        .iter()
        .map(|&f| SideRows::of(entities, EntityId::Face(f)))
        .collect();
    let mut others = Vec::new();
    for &id in ids {
        let Some(view) = entities.view(id) else {
            continue;
        };
        let record = match view {
            EntityView::Group(g) => {
                let mut g = g.clone();
                g.transform = local * g.transform;
                EntityRecord::Group(g)
            }
            EntityView::Instance(i) => {
                let mut i = i.clone();
                i.transform = local * i.transform;
                EntityRecord::Instance(i)
            }
            EntityView::Image(i) => {
                let mut i = i.clone();
                i.transform = local * i.transform;
                EntityRecord::Image(i)
            }
            _ => match placed_record(tx.document(), container, id, &local) {
                Some(Placed::Record(r)) => r,
                _ => continue,
            },
        };
        // A copy is not locked even when its original is.
        let rows = SideRows {
            locked: false,
            ..SideRows::of(entities, id)
        };
        others.push((record, rows));
    }
    let copy = transform_indexed(&piece.indexed, &local);
    let before = tx.created().len();
    place_geometry(tx, container, &copy, edge_rows, face_rows)?;
    let mut pids: Vec<Pid> = tx.created()[before..].to_vec();
    for (record, rows) in others {
        let (new, pid) = tx.insert_entity(container, record)?;
        rows.apply(tx, container, new)?;
        pids.push(pid);
    }
    Ok(pids)
}

/// Runs the move-or-copy of a transform family `at(s)` (s = array step).
#[allow(clippy::too_many_arguments)]
fn run_transform(
    tx: &mut Transaction<'_>,
    common: &CommonParams,
    targets: &Query,
    copy: bool,
    copies: u32,
    spacing: Spacing,
    opts: TransformOptions,
    at: impl Fn(f64) -> DAffine3,
) -> Result<TransformOutput, CommandError> {
    let target = target_container(tx, common)?;
    let ids = targets_in(tx, &target, targets)?;
    if ids.is_empty() {
        return Err(CommandError::NotFound(crate::document::EntityRef::Query(
            targets.clone(),
        )));
    }
    if !copy {
        refuse_locked(tx, target.container, &ids)?;
        return transform_entities(tx, &target, &ids, at(1.0), opts);
    }
    let n = copies.max(1);
    let mut out = TransformOutput {
        moved: 0,
        copies: Vec::new(),
        folded_faces: 0,
        merged_vertices: 0,
    };
    for k in 1..=n {
        let s = match spacing {
            Spacing::Between => f64::from(k),
            Spacing::Total => f64::from(k) / f64::from(n),
        };
        out.copies.extend(copy_entities(tx, &target, &ids, at(s))?);
    }
    Ok(out)
}

fn check_copies(copies: u32) -> Result<(), CommandError> {
    if !(1..=10_000).contains(&copies) {
        return Err(CommandError::OutOfRange {
            name: "copies".into(),
            reason: "must be between 1 and 10000".into(),
        });
    }
    Ok(())
}

fn parse_targets(q: &Query) -> Result<(), CommandError> {
    q.parse()
        .map(|_| ())
        .map_err(|e| CommandError::invalid_param("targets", e.to_string()))
}

// ----- move ---------------------------------------------------------------------------------

/// `move` parameters (docs/05 §4.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MoveParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// What to move.
    pub targets: Query,
    /// Grab point (with `to`).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub from: Option<DVec3>,
    /// Destination of the grab point.
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub to: Option<DVec3>,
    /// Displacement (instead of `from`/`to`).
    #[serde(default)]
    #[schemars(with = "Option<VectorSchema>")]
    pub delta: Option<DVec3>,
    /// Copy instead of moving.
    #[serde(default)]
    pub copy: bool,
    /// Number of copies (linear array).
    #[serde(default = "one")]
    pub copies: u32,
    /// Spacing of the copies.
    #[serde(default)]
    pub spacing: Spacing,
    /// Split edges and faces where moved geometry crosses other geometry.
    #[serde(default)]
    pub check_intersections: bool,
    /// Fold faces that would become non-planar.
    #[serde(default = "yes")]
    pub auto_fold: bool,
}

impl MoveParams {
    fn delta(&self) -> Result<DVec3, CommandError> {
        match (self.delta, self.from, self.to) {
            (Some(d), _, _) => Ok(d),
            (None, Some(a), Some(b)) => Ok(b - a),
            (None, None, Some(b)) => Ok(b),
            _ => Err(CommandError::invalid_param(
                "delta",
                "give delta or from/to",
            )),
        }
    }
}

/// The `move` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Move;

impl Command for Move {
    type Params = MoveParams;
    type Output = TransformOutput;

    fn name(&self) -> &'static str {
        "move"
    }

    fn validate(&self, _doc: &Document, p: &MoveParams) -> Result<(), CommandError> {
        parse_targets(&p.targets)?;
        check_copies(p.copies)?;
        check_point("delta", p.delta()?)
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MoveParams,
    ) -> Result<TransformOutput, CommandError> {
        let delta = p.delta()?;
        let opts = TransformOptions {
            auto_fold: p.auto_fold,
            check_intersections: p.check_intersections,
        };
        run_transform(
            tx,
            &p.common,
            &p.targets,
            p.copy,
            p.copies,
            p.spacing,
            opts,
            |s| translation(delta * s),
        )
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "move".into(),
            category: CommandCategory::Edit,
            label: "Move".into(),
            view_only: false,
            steps: vec![
                ToolStep {
                    prompt: "Pick the grab point".into(),
                    value_field: String::new(),
                    inference: InferenceKind::ALL.to_vec(),
                },
                ToolStep {
                    prompt: "Pick the destination (Ctrl copies)".into(),
                    value_field: "distance or coordinate; *N / N x copies, /N divides".into(),
                    inference: InferenceKind::ALL.to_vec(),
                },
            ],
        }
    }
}

// ----- rotate -------------------------------------------------------------------------------

/// `rotate` parameters (docs/05 §4.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RotateParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// What to rotate.
    pub targets: Query,
    /// Center of rotation.
    #[schemars(with = "PointSchema")]
    pub center: DVec3,
    /// Rotation axis (+Z).
    #[serde(default = "z_axis")]
    #[schemars(with = "VectorSchema")]
    pub axis: DVec3,
    /// Angle, counter-clockwise about the axis.
    #[serde(default)]
    pub angle: Option<Angle>,
    /// First reference point (with `to_point`; the angle between them).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub from_point: Option<DVec3>,
    /// Second reference point.
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub to_point: Option<DVec3>,
    /// Copy instead of rotating.
    #[serde(default)]
    pub copy: bool,
    /// Number of copies (radial array).
    #[serde(default = "one")]
    pub copies: u32,
    /// Spacing of the copies.
    #[serde(default)]
    pub spacing: Spacing,
    /// Fold faces that would become non-planar.
    #[serde(default = "yes")]
    pub auto_fold: bool,
}

impl RotateParams {
    fn angle(&self, tol: &Tolerances) -> Result<f64, CommandError> {
        if let Some(a) = self.angle {
            return Ok(a.as_radians());
        }
        let (Some(a), Some(b)) = (self.from_point, self.to_point) else {
            return Err(CommandError::invalid_param(
                "angle",
                "give angle or from_point/to_point",
            ));
        };
        let axis = self
            .axis
            .try_normalize()
            .ok_or_else(|| CommandError::invalid_param("axis", "must be non-zero"))?;
        let u = a - self.center;
        let v = b - self.center;
        let u = u - axis * u.dot(axis);
        let v = v - axis * v.dot(axis);
        for (name, w) in [("from_point", u), ("to_point", v)] {
            if w.length_squared() <= tol.length_sq {
                return Err(CommandError::invalid_param(
                    name,
                    "must not lie on the axis",
                ));
            }
        }
        Ok(libm::atan2(axis.dot(u.cross(v)), u.dot(v)))
    }
}

/// The `rotate` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Rotate;

impl Command for Rotate {
    type Params = RotateParams;
    type Output = TransformOutput;

    fn name(&self) -> &'static str {
        "rotate"
    }

    fn validate(&self, doc: &Document, p: &RotateParams) -> Result<(), CommandError> {
        parse_targets(&p.targets)?;
        check_copies(p.copies)?;
        check_point("center", p.center)?;
        if p.axis.try_normalize().is_none() {
            return Err(CommandError::invalid_param("axis", "must be non-zero"));
        }
        p.angle(doc.tolerances()).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: RotateParams,
    ) -> Result<TransformOutput, CommandError> {
        let angle = p.angle(tx.tolerances())?;
        let (center, axis) = (p.center, p.axis.normalize());
        let opts = TransformOptions {
            auto_fold: p.auto_fold,
            check_intersections: false,
        };
        run_transform(
            tx,
            &p.common,
            &p.targets,
            p.copy,
            p.copies,
            p.spacing,
            opts,
            |s| rotation_about(center, axis, Angle::radians(angle * s)),
        )
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "rotate".into(),
            category: CommandCategory::Edit,
            label: "Rotate".into(),
            view_only: false,
            steps: vec![
                ToolStep {
                    prompt: "Pick the center".into(),
                    value_field: String::new(),
                    inference: InferenceKind::ALL.to_vec(),
                },
                ToolStep {
                    prompt: "Pick the first reference point".into(),
                    value_field: String::new(),
                    inference: InferenceKind::ALL.to_vec(),
                },
                ToolStep {
                    prompt: "Pick the second reference point (Ctrl copies)".into(),
                    value_field: "angle; *N / N x copies, /N divides".into(),
                    inference: InferenceKind::ALL.to_vec(),
                },
            ],
        }
    }
}

// ----- scale --------------------------------------------------------------------------------

/// Where the scale pivot is by default (docs/05 §4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ScaleAbout {
    /// The bounding-box corner opposite the dragged grip (the minimum corner).
    #[default]
    Opposite,
    /// The bounding-box center.
    Center,
}

/// `scale` parameters (docs/05 §4.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScaleParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// What to scale.
    pub targets: Query,
    /// Uniform factor.
    #[serde(default)]
    pub factor: Option<f64>,
    /// Per-axis factors `[sx, sy, sz]` (negative mirrors).
    #[serde(default)]
    pub factors: Option<Vec<f64>>,
    /// Fixed point; by default from `about`.
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub pivot: Option<DVec3>,
    /// Default pivot.
    #[serde(default)]
    pub about: ScaleAbout,
    /// Fold faces that would become non-planar.
    #[serde(default = "yes")]
    pub auto_fold: bool,
}

impl ScaleParams {
    fn factors(&self) -> Result<DVec3, CommandError> {
        let f = match (&self.factors, self.factor) {
            (Some(v), _) if v.len() == 3 => DVec3::new(v[0], v[1], v[2]),
            (Some(v), _) if v.len() == 1 => DVec3::splat(v[0]),
            (Some(_), _) => {
                return Err(CommandError::invalid_param(
                    "factors",
                    "give one or three factors",
                ));
            }
            (None, Some(f)) => DVec3::splat(f),
            (None, None) => {
                return Err(CommandError::invalid_param(
                    "factor",
                    "give factor or factors",
                ));
            }
        };
        if !f.is_finite() || f.abs().min_element() < 1e-9 {
            return Err(CommandError::OutOfRange {
                name: "factors".into(),
                reason: "factors must be finite and non-zero".into(),
            });
        }
        Ok(f)
    }
}

/// `scale` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScaleOutput {
    /// Factors applied.
    pub factors: [f64; 3],
    /// World bounds of the targets afterwards (`None` when empty).
    pub bbox_after: Option<[[f64; 3]; 2]>,
}

/// World bounds of entities in the target container.
fn bounds(tx: &Transaction<'_>, target: &Target, ids: &[EntityId]) -> Result<Aabb, CommandError> {
    let doc = tx.document();
    let entities = doc
        .container(target.container)
        .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
    let mesh = entities.mesh();
    let mut bb = Aabb::EMPTY;
    for &id in ids {
        match id {
            EntityId::Edge(e) => {
                let s = mesh.edge_segment(e)?;
                bb = bb
                    .including(target.world.transform_point3(s.a))
                    .including(target.world.transform_point3(s.b));
            }
            EntityId::Face(f) => {
                for p in mesh.loop_positions(mesh.face_ref(f)?.outer)? {
                    bb = bb.including(target.world.transform_point3(p));
                }
            }
            _ => {
                if let (Some(def), Some(t)) = (
                    entities.definition_of(id).and_then(|d| doc.definition(d)),
                    entities.transform_of(id),
                ) {
                    let inner = def.entities.local_aabb(&doc.definitions, false);
                    if !inner.is_empty() {
                        bb = bb.union(&inner.transformed(&(target.world * t)));
                    }
                }
            }
        }
    }
    Ok(bb)
}

/// The `scale` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Scale;

impl Command for Scale {
    type Params = ScaleParams;
    type Output = ScaleOutput;

    fn name(&self) -> &'static str {
        "scale"
    }

    fn validate(&self, _doc: &Document, p: &ScaleParams) -> Result<(), CommandError> {
        parse_targets(&p.targets)?;
        p.factors().map(|_| ())
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: ScaleParams) -> Result<ScaleOutput, CommandError> {
        let factors = p.factors()?;
        let target = target_container(tx, &p.common)?;
        let ids = targets_in(tx, &target, &p.targets)?;
        if ids.is_empty() {
            return Err(CommandError::NotFound(crate::document::EntityRef::Query(
                p.targets.clone(),
            )));
        }
        refuse_locked(tx, target.container, &ids)?;
        let before = bounds(tx, &target, &ids)?;
        let pivot = match (p.pivot, p.about) {
            (Some(pivot), _) => pivot,
            (None, ScaleAbout::Opposite) => before.min,
            (None, ScaleAbout::Center) => before.center(),
        };
        let opts = TransformOptions {
            auto_fold: p.auto_fold,
            check_intersections: false,
        };
        transform_entities(tx, &target, &ids, scale_about(pivot, factors), opts)?;
        let alive: Vec<EntityId> = ids
            .into_iter()
            .filter(|id| {
                tx.document()
                    .container(target.container)
                    .is_some_and(|e| e.contains(*id))
            })
            .collect();
        let after = bounds(tx, &target, &alive)?;
        Ok(ScaleOutput {
            factors: factors.to_array(),
            bbox_after: (!after.is_empty()).then(|| [after.min.to_array(), after.max.to_array()]),
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "scale".into(),
            category: CommandCategory::Edit,
            label: "Scale".into(),
            view_only: false,
            steps: vec![ToolStep {
                prompt: "Drag a grip (Ctrl about center, Shift uniform)".into(),
                value_field: "factor, factors or a length with units".into(),
                inference: InferenceKind::ALL.to_vec(),
            }],
        }
    }
}
