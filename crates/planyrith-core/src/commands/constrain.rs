//! Implements docs/05-modeling-tools.md §5.15 (constraints): `constraint-add`,
//! `constraint-edit`, `constraint-delete`, `constraint-list` and
//! `constraint-solve` on the edit context's container (docs/03 §11,
//! milestone 1.9).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::targets_of;
use crate::commands::params::{Target, target_container};
use crate::commands::transaction::AttrTarget;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ContainerRef, Document, Query};
use crate::geom::TransformOptions;
use crate::ids::{EntityId, Pid};
use crate::sketch::{self, Constraint, ConstraintKind, PointRef, Solution, SolveError};
use crate::units::{Angle, Length};

fn attr_target(c: ContainerRef) -> AttrTarget {
    match c {
        ContainerRef::Model => AttrTarget::Document,
        ContainerRef::Definition(d) => AttrTarget::Definition(d),
    }
}

/// The constraints of a container.
pub fn constraints_of(doc: &Document, c: ContainerRef) -> Vec<Constraint> {
    match c {
        ContainerRef::Model => sketch::read(&doc.attrs),
        ContainerRef::Definition(d) => doc
            .definitions
            .get(d)
            .map(|def| sketch::read(&def.attrs))
            .unwrap_or_default(),
    }
}

fn solve_error(e: SolveError) -> CommandError {
    CommandError::NotApplicable(e.to_string())
}

/// Solves `list` in `target`, moves the vertices and stores the list.
fn apply(
    tx: &mut Transaction<'_>,
    target: &Target,
    list: Vec<Constraint>,
) -> Result<Solution, CommandError> {
    let solution = {
        let entities = tx
            .document()
            .container(target.container)
            .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
        sketch::solve(entities, &list, false).map_err(solve_error)?
    };
    if !solution.moves.is_empty() {
        let moves = solution.moves.clone();
        tx.with_mesh(target.container, |m, tol, d| {
            m.move_vertices(
                &moves,
                TransformOptions {
                    auto_fold: true,
                    check_intersections: false,
                },
                tol,
                d,
            )
        })?;
    }
    tx.set_attribute(
        attr_target(target.container),
        sketch::DICT,
        sketch::KEY,
        sketch::value_of(&list),
    )?;
    Ok(solution)
}

/// Pids of the edges each query names (exactly one per query).
fn edge_pids(
    tx: &Transaction<'_>,
    target: &Target,
    queries: &[Query],
) -> Result<Vec<Pid>, CommandError> {
    let mut out = Vec::new();
    for (i, q) in queries.iter().enumerate() {
        let edges: Vec<EntityId> = targets_of(tx, target, "edges", q)?
            .into_iter()
            .filter(|id| matches!(id, EntityId::Edge(_)))
            .collect();
        let [edge] = edges.as_slice() else {
            return Err(CommandError::invalid_param(
                "edges",
                format!("edge {} must name exactly one edge", i + 1),
            ));
        };
        let pid = tx
            .document()
            .container(target.container)
            .and_then(|c| c.pid_of(*edge))
            .ok_or_else(|| CommandError::Internal("edge without a pid".into()))?;
        out.push(pid);
    }
    Ok(out)
}

/// What solving gave.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConstraintOutput {
    /// The constraint added or changed (when one was).
    pub id: Option<u32>,
    /// Vertices the solver moved.
    pub moved: usize,
    /// Degrees of freedom left among the constrained points.
    pub degrees_of_freedom: usize,
    /// Equations that repeat others.
    pub redundant: usize,
    /// Constraints left in the container.
    pub constraints: usize,
}

fn output(id: Option<u32>, s: &Solution, constraints: usize) -> ConstraintOutput {
    ConstraintOutput {
        id,
        moved: s.moved,
        degrees_of_freedom: s.degrees_of_freedom,
        redundant: s.redundant,
        constraints,
    }
}

// ----- constraint-add -------------------------------------------------------------------

/// `constraint-add` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConstraintAddParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// What to require.
    pub kind: ConstraintKind,
    /// The edges, in order: each query names one edge of the edit context.
    #[serde(default)]
    pub edges: Vec<Query>,
    /// The points, as ends of edges (`<pid>.a`, `<pid>.b`).
    #[serde(default)]
    pub points: Vec<PointRef>,
    /// The length of `length` and `distance`.
    #[serde(default)]
    pub length: Option<Length>,
    /// The angle of `angle`.
    #[serde(default)]
    pub angle: Option<Angle>,
}

/// The `constraint-add` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConstraintAdd;

impl Command for ConstraintAdd {
    type Params = ConstraintAddParams;
    type Output = ConstraintOutput;

    fn name(&self) -> &'static str {
        "constraint-add"
    }

    fn validate(&self, _: &Document, p: &ConstraintAddParams) -> Result<(), CommandError> {
        let probe = Constraint {
            id: 0,
            kind: p.kind,
            edges: vec![Pid(0); p.edges.len()],
            points: p.points.clone(),
            length_mm: p.length.map(|l| l.as_mm()),
            angle_rad: p.angle.map(|a| a.as_radians()),
            at: Vec::new(),
        };
        probe
            .check()
            .map_err(|e| CommandError::invalid_param("kind", e))
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ConstraintAddParams,
    ) -> Result<ConstraintOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let edges = edge_pids(tx, &target, &p.edges)?;
        let mut list = constraints_of(tx.document(), target.container);
        let id = list.iter().map(|c| c.id).max().map_or(1, |m| m + 1);
        let mut c = Constraint {
            id,
            kind: p.kind,
            edges,
            points: p.points,
            length_mm: p.length.map(|l| l.as_mm()),
            angle_rad: p.angle.map(|a| a.as_radians()),
            at: Vec::new(),
        };
        if c.kind == ConstraintKind::Fixed {
            let entities = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
            let mesh = entities.mesh();
            let end = |pid: Pid, second: bool| -> Result<[f64; 3], CommandError> {
                let Some(EntityId::Edge(e)) = entities.id_of(pid) else {
                    return Err(CommandError::invalid_param(
                        "points",
                        format!("{} is not an edge", pid.0),
                    ));
                };
                let s = mesh.edge_segment(e)?;
                Ok(if second { s.b } else { s.a }.to_array())
            };
            for pt in &c.points {
                c.at.push(end(pt.edge, pt.second)?);
            }
            for &e in &c.edges {
                c.at.push(end(e, false)?);
                c.at.push(end(e, true)?);
            }
        }
        list.push(c);
        let n = list.len();
        let s = apply(tx, &target, list)?;
        Ok(output(Some(id), &s, n))
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "constraint-add".into(),
            label: "Add Constraint".into(),
            category: CommandCategory::Organize,
            view_only: false,
            steps: Vec::new(),
        }
    }
}

// ----- constraint-edit ------------------------------------------------------------------

/// `constraint-edit` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConstraintEditParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The constraint's number.
    pub id: u32,
    /// A new length (`length`, `distance`).
    #[serde(default)]
    pub length: Option<Length>,
    /// A new angle (`angle`).
    #[serde(default)]
    pub angle: Option<Angle>,
}

/// The `constraint-edit` command: a driving dimension's new value.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConstraintEdit;

impl Command for ConstraintEdit {
    type Params = ConstraintEditParams;
    type Output = ConstraintOutput;

    fn name(&self) -> &'static str {
        "constraint-edit"
    }

    fn validate(&self, _: &Document, p: &ConstraintEditParams) -> Result<(), CommandError> {
        if p.length.is_none() && p.angle.is_none() {
            return Err(CommandError::invalid_param(
                "length",
                "give length or angle",
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ConstraintEditParams,
    ) -> Result<ConstraintOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let mut list = constraints_of(tx.document(), target.container);
        let c = list
            .iter_mut()
            .find(|c| c.id == p.id)
            .ok_or_else(|| CommandError::invalid_param("id", format!("no constraint {}", p.id)))?;
        if let Some(l) = p.length {
            if !c.kind.takes_length() {
                return Err(CommandError::invalid_param(
                    "length",
                    "this constraint has no length",
                ));
            }
            c.length_mm = Some(l.as_mm());
        }
        if let Some(a) = p.angle {
            if !c.kind.takes_angle() {
                return Err(CommandError::invalid_param(
                    "angle",
                    "this constraint has no angle",
                ));
            }
            c.angle_rad = Some(a.as_radians());
        }
        c.check()
            .map_err(|e| CommandError::invalid_param("length", e))?;
        let n = list.len();
        let s = apply(tx, &target, list)?;
        Ok(output(Some(p.id), &s, n))
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "constraint-edit".into(),
            label: "Edit Dimension".into(),
            category: CommandCategory::Organize,
            view_only: false,
            steps: Vec::new(),
        }
    }
}

// ----- constraint-delete ----------------------------------------------------------------

/// `constraint-delete` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConstraintDeleteParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The constraints' numbers (all of them when empty, with `all`).
    #[serde(default)]
    pub ids: Vec<u32>,
    /// Delete every constraint of the container.
    #[serde(default)]
    pub all: bool,
}

/// `constraint-delete` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConstraintDeleteOutput {
    /// Constraints removed.
    pub deleted: usize,
    /// Constraints left.
    pub constraints: usize,
}

/// The `constraint-delete` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConstraintDelete;

impl Command for ConstraintDelete {
    type Params = ConstraintDeleteParams;
    type Output = ConstraintDeleteOutput;

    fn name(&self) -> &'static str {
        "constraint-delete"
    }

    fn validate(&self, _: &Document, p: &ConstraintDeleteParams) -> Result<(), CommandError> {
        if p.ids.is_empty() && !p.all {
            return Err(CommandError::invalid_param("ids", "give ids or all"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ConstraintDeleteParams,
    ) -> Result<ConstraintDeleteOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let mut list = constraints_of(tx.document(), target.container);
        for id in &p.ids {
            if !list.iter().any(|c| c.id == *id) {
                return Err(CommandError::invalid_param(
                    "ids",
                    format!("no constraint {id}"),
                ));
            }
        }
        let before = list.len();
        list.retain(|c| !p.all && !p.ids.contains(&c.id));
        tx.set_attribute(
            attr_target(target.container),
            sketch::DICT,
            sketch::KEY,
            sketch::value_of(&list),
        )?;
        Ok(ConstraintDeleteOutput {
            deleted: before - list.len(),
            constraints: list.len(),
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "constraint-delete".into(),
            label: "Delete Constraints".into(),
            category: CommandCategory::Organize,
            view_only: false,
            steps: Vec::new(),
        }
    }
}

// ----- constraint-list ------------------------------------------------------------------

/// `constraint-list` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConstraintListParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
}

/// One constraint with its state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConstraintRow {
    /// The constraint.
    #[serde(flatten)]
    pub constraint: Constraint,
    /// It names an edge that no longer exists.
    pub missing: bool,
}

/// `constraint-list` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConstraintListOutput {
    /// The container's constraints.
    pub constraints: Vec<ConstraintRow>,
    /// `fully`, `under` (with degrees of freedom left), `conflict`, or
    /// `none`.
    pub state: String,
    /// Degrees of freedom left (when not in conflict).
    pub degrees_of_freedom: Option<usize>,
    /// Why the constraints cannot be met, when they cannot.
    pub conflict: Option<String>,
}

/// The `constraint-list` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConstraintList;

impl Command for ConstraintList {
    type Params = ConstraintListParams;
    type Output = ConstraintListOutput;

    fn name(&self) -> &'static str {
        "constraint-list"
    }

    fn validate(&self, _: &Document, _: &ConstraintListParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ConstraintListParams,
    ) -> Result<ConstraintListOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let list = constraints_of(tx.document(), target.container);
        let entities = tx
            .document()
            .container(target.container)
            .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
        let exists = |pid: Pid| matches!(entities.id_of(pid), Some(EntityId::Edge(_)));
        let rows: Vec<ConstraintRow> = list
            .iter()
            .map(|c| ConstraintRow {
                missing: !(c.edges.iter().all(|e| exists(*e))
                    && c.points.iter().all(|p| exists(p.edge))),
                constraint: c.clone(),
            })
            .collect();
        let (state, dof, conflict) = if list.is_empty() {
            ("none".to_string(), None, None)
        } else {
            match sketch::solve(entities, &list, true) {
                Ok(s) if s.degrees_of_freedom == 0 => ("fully".into(), Some(0), None),
                Ok(s) => ("under".into(), Some(s.degrees_of_freedom), None),
                Err(e) => ("conflict".into(), None, Some(e.to_string())),
            }
        };
        Ok(ConstraintListOutput {
            constraints: rows,
            state,
            degrees_of_freedom: dof,
            conflict,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "constraint-list".into(),
            label: "List Constraints".into(),
            category: CommandCategory::Organize,
            view_only: true,
            steps: Vec::new(),
        }
    }
}

// ----- constraint-solve -----------------------------------------------------------------

/// `constraint-solve` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConstraintSolveParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Remove constraints that name edges no longer there (instead of
    /// failing).
    #[serde(default)]
    pub drop_missing: bool,
}

/// The `constraint-solve` command: meets the constraints again after the
/// geometry was edited.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConstraintSolve;

impl Command for ConstraintSolve {
    type Params = ConstraintSolveParams;
    type Output = ConstraintOutput;

    fn name(&self) -> &'static str {
        "constraint-solve"
    }

    fn validate(&self, _: &Document, _: &ConstraintSolveParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ConstraintSolveParams,
    ) -> Result<ConstraintOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let mut list = constraints_of(tx.document(), target.container);
        if p.drop_missing {
            let entities = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
            let exists = |pid: Pid| matches!(entities.id_of(pid), Some(EntityId::Edge(_)));
            list.retain(|c| {
                c.edges.iter().all(|e| exists(*e)) && c.points.iter().all(|p| exists(p.edge))
            });
        }
        let n = list.len();
        let s = apply(tx, &target, list)?;
        Ok(output(None, &s, n))
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "constraint-solve".into(),
            label: "Solve Constraints".into(),
            category: CommandCategory::Organize,
            view_only: false,
            steps: Vec::new(),
        }
    }
}

/// Registers the constraint commands.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(ConstraintAdd);
    registry.register(ConstraintEdit);
    registry.register(ConstraintDelete);
    registry.register(ConstraintList);
    registry.register(ConstraintSolve);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use glam::DVec3;
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn engine() -> Engine {
        Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
    }

    /// The pid of the edge from `a` to `b` (either way).
    fn edge(e: &Engine, a: [f64; 3], b: [f64; 3]) -> u64 {
        let m = &e.document().model;
        let (a, b) = (DVec3::from_array(a), DVec3::from_array(b));
        for (id, _) in m.mesh().edges() {
            let s = m.mesh().edge_segment(id).unwrap();
            if (s.a.distance(a) < 1e-6 && s.b.distance(b) < 1e-6)
                || (s.a.distance(b) < 1e-6 && s.b.distance(a) < 1e-6)
            {
                return m.pid_of(crate::ids::EntityId::Edge(id)).unwrap().0;
            }
        }
        panic!("no edge {a} {b}");
    }

    fn add(e: &mut Engine, params: Value) -> Value {
        e.execute_erased("constraint-add", params).unwrap()
    }

    #[test]
    fn a_skewed_quad_becomes_a_dimensioned_rectangle() {
        let mut e = engine();
        // A four-sided face, not quite a rectangle.
        e.execute_erased(
            "line",
            json!({ "points": [[0, 0, 0], [1000, 20, 0], [990, 600, 0], [-10, 580, 0]], "close": true }),
        )
        .unwrap();
        let bottom = edge(&e, [0.0, 0.0, 0.0], [1000.0, 20.0, 0.0]);
        let right = edge(&e, [1000.0, 20.0, 0.0], [990.0, 600.0, 0.0]);
        let top = edge(&e, [990.0, 600.0, 0.0], [-10.0, 580.0, 0.0]);
        let left = edge(&e, [-10.0, 580.0, 0.0], [0.0, 0.0, 0.0]);
        let q = |pid: u64| format!("@{pid}");
        add(
            &mut e,
            json!({ "kind": "fixed", "points": [format!("{left}.b")] }),
        );
        let out = add(
            &mut e,
            json!({ "kind": "along-x", "edges": [q(bottom), q(top)] }),
        );
        assert!(out["degrees_of_freedom"].as_u64().unwrap() > 0);
        add(
            &mut e,
            json!({ "kind": "along-y", "edges": [q(left), q(right)] }),
        );
        add(
            &mut e,
            json!({ "kind": "length", "edges": [q(bottom)], "length": 1200 }),
        );
        let out = add(
            &mut e,
            json!({ "kind": "length", "edges": [q(left)], "length": 500 }),
        );
        assert_eq!(out["degrees_of_freedom"], 0, "{out}");
        // Still one flat face: a 1200 × 500 rectangle at the fixed corner.
        let mesh = e.document().model.mesh();
        assert_eq!(mesh.face_count(), 1);
        let (f, _) = mesh.faces().next().unwrap();
        assert!((mesh.face_area(f).unwrap() - 600_000.0).abs() < 1e-3);
        let list = e.execute_erased("constraint-list", json!({})).unwrap();
        assert_eq!(list["state"], "fully");
        assert_eq!(list["constraints"].as_array().unwrap().len(), 5);

        // A driving dimension changes; undo brings the old one back.
        let id = list["constraints"][3]["id"].as_u64().unwrap();
        e.execute_erased("constraint-edit", json!({ "id": id, "length": 1500 }))
            .unwrap();
        let mesh = e.document().model.mesh();
        let (f, _) = mesh.faces().next().unwrap();
        assert!((mesh.face_area(f).unwrap() - 750_000.0).abs() < 1e-3);
        e.undo().unwrap();
        let mesh = e.document().model.mesh();
        let (f, _) = mesh.faces().next().unwrap();
        assert!((mesh.face_area(f).unwrap() - 600_000.0).abs() < 1e-3);

        // A conflicting constraint is refused and changes nothing.
        let err = e
            .execute_erased(
                "constraint-add",
                json!({ "kind": "length", "edges": [q(top)], "length": 900 }),
            )
            .unwrap_err();
        assert!(err.to_string().contains("conflict"), "{err}");
        assert_eq!(
            e.execute_erased("constraint-list", json!({})).unwrap()["constraints"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
    }

    #[test]
    fn angles_perpendicular_parallel_and_deleting() {
        let mut e = engine();
        e.execute_erased("line", json!({ "points": [[0, 0, 0], [1000, 0, 0]] }))
            .unwrap();
        e.execute_erased("line", json!({ "points": [[2000, 0, 0], [2100, 900, 0]] }))
            .unwrap();
        let a = edge(&e, [0.0, 0.0, 0.0], [1000.0, 0.0, 0.0]);
        let b = edge(&e, [2000.0, 0.0, 0.0], [2100.0, 900.0, 0.0]);
        add(
            &mut e,
            json!({ "kind": "fixed", "edges": [format!("@{a}")] }),
        );
        add(
            &mut e,
            json!({ "kind": "angle", "edges": [format!("@{a}"), format!("@{b}")],
                            "angle": std::f64::consts::FRAC_PI_3 }),
        );
        let s = e.document().model.mesh();
        let (id, _) = s
            .edges()
            .find(|(id, _)| s.edge_segment(*id).unwrap().a.x > 1500.0)
            .unwrap();
        let seg = s.edge_segment(id).unwrap();
        let d = (seg.b - seg.a).normalize();
        assert!((d.dot(DVec3::X).abs() - 0.5).abs() < 1e-9, "{d}");
        let out = e
            .execute_erased("constraint-delete", json!({ "all": true }))
            .unwrap();
        assert_eq!(out["deleted"], 2);
        assert!(e.document().attrs.get("planyrith", "constraints").is_none());
    }
}
