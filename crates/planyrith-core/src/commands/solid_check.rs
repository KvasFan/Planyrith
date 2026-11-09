//! Implements docs/05-modeling-tools.md §8.1 `solid-check` and §8.2
//! `solid-repair`: whether each visible object is a solid ready for 3D
//! printing, and the repairs that need no judgement (docs/03 §4.14).

use std::collections::BTreeMap;

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::tool_steps;
use crate::commands::params::{command_context, select};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::selection::occurrence_name;
use crate::document::{ContainerRef, Document, EditContext, InstancePath, Query, Selection};
use crate::geom::solid_check::{RepairOptions, RepairReport, SolidDiagnosis, diagnose, repair};
use crate::io::render_mesh::{DrawItem, draw_list};

fn yes() -> bool {
    true
}

/// `solid-check` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SolidCheckParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Only the objects these entities belong to (every visible object).
    #[serde(default)]
    pub targets: Option<Query>,
    /// Include hidden entities and tags.
    #[serde(default)]
    pub include_hidden: bool,
}

/// One object (an occurrence of a group, component or the loose model
/// geometry) as the check found it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SolidCheckObject {
    /// The occurrence (empty: the model's loose geometry).
    pub path: InstancePath,
    /// Its name (group, instance or component name; `Model` for the root).
    pub name: String,
    /// Closed, with every face facing out: ready to print.
    pub printable: bool,
    /// Faces.
    pub faces: usize,
    /// Edge-connected groups of faces (separate parts in one object).
    pub shells: usize,
    /// Edges with a face on one side only.
    pub open_edges: usize,
    /// Closed chains of open edges (missing faces).
    pub holes: usize,
    /// Edges bounding no face.
    pub wire_edges: usize,
    /// Points where parts touch only at a corner.
    pub pinched_vertices: usize,
    /// Closed shells facing inward (reversed faces).
    pub inside_out_shells: usize,
    /// Hollows inside a shell (fine for printing).
    pub voids: usize,
    /// Enclosed volume in cubic millimeters, when printable.
    pub volume_mm3: Option<f64>,
    /// Size along the model axes, millimeters.
    pub size_mm: [f64; 3],
}

/// `solid-check` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SolidCheckOutput {
    /// Every object with faces, in drawing order.
    pub objects: Vec<SolidCheckObject>,
    /// There is at least one object and every object is printable.
    pub printable: bool,
}

/// `solid-repair` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SolidRepairParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Only the objects these entities belong to (every visible object
    /// that is not printable).
    #[serde(default)]
    pub targets: Option<Query>,
    /// Include hidden entities and tags.
    #[serde(default)]
    pub include_hidden: bool,
    /// Close holes: flat ones with one face, others with triangles.
    #[serde(default = "yes")]
    pub fill_holes: bool,
    /// Remove edges that bound no face.
    #[serde(default = "yes")]
    pub remove_wire_edges: bool,
    /// Turn inward-facing closed shells that are not hollows right side
    /// out.
    #[serde(default = "yes")]
    pub reverse_inside_out: bool,
}

/// `solid-repair` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SolidRepairOutput {
    /// Groups, components or the model whose geometry was repaired (a
    /// component counts once for all its instances).
    pub repaired: usize,
    /// Holes closed.
    pub holes_filled: usize,
    /// Faces added to close them.
    pub faces_added: usize,
    /// Holes that could not be closed (lone faces, holes that fold over
    /// themselves).
    pub holes_left: usize,
    /// Wire edges removed.
    pub wire_edges_removed: usize,
    /// Shells turned right side out.
    pub shells_reversed: usize,
    /// Locked objects left alone.
    pub locked: usize,
    /// Every object checked is now printable.
    pub printable: bool,
}

/// The visible occurrences with faces that `targets` names (all of them
/// without targets).
pub(crate) fn objects(
    tx: &Transaction<'_>,
    common: &CommonParams,
    targets: Option<&Query>,
    include_hidden: bool,
) -> Result<Vec<DrawItem>, CommandError> {
    let selection = match targets {
        Some(q) => Some(select(tx, &command_context(tx, common), "targets", q)?),
        None => None,
    };
    let doc = tx.document();
    let items = draw_list(
        doc,
        selection.as_ref().unwrap_or(&Selection::new()),
        &EditContext::model(),
        include_hidden,
    );
    Ok(items
        .into_iter()
        .filter(|item| include_hidden || !item.hidden)
        .filter(|item| selection.is_none() || item.selected || !item.selected_faces.is_empty())
        .filter(|item| {
            doc.container(item.container)
                .is_some_and(|c| c.mesh().face_count() > 0)
        })
        .collect())
}

fn describe(doc: &Document, item: &DrawItem, d: &SolidDiagnosis) -> SolidCheckObject {
    let mesh = doc.container(item.container).map(|c| c.mesh());
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for (_, v) in mesh.into_iter().flat_map(|m| m.vertices()) {
        let p = item.world.transform_point3(v.position);
        lo = lo.min(p);
        hi = hi.max(p);
    }
    let size = if lo.x.is_finite() {
        hi - lo
    } else {
        DVec3::ZERO
    };
    let printable = d.is_printable();
    let volume_mm3 = mesh
        .filter(|_| printable)
        .and_then(|m| m.volume().ok())
        .map(|v| v * item.world.matrix3.determinant().abs());
    SolidCheckObject {
        path: item.path.clone(),
        name: occurrence_name(doc, &item.path),
        printable,
        faces: d.faces,
        shells: d.shells,
        open_edges: d.open_edges.len(),
        holes: d.holes.len(),
        wire_edges: d.wire_edges.len(),
        pinched_vertices: d.pinched_vertices.len(),
        inside_out_shells: d.inside_out.len(),
        voids: d.voids,
        volume_mm3,
        size_mm: size.to_array(),
    }
}

fn check(tx: &Transaction<'_>, items: &[DrawItem]) -> Result<SolidCheckOutput, CommandError> {
    let doc = tx.document();
    let mut cache: BTreeMap<ContainerRef, SolidDiagnosis> = BTreeMap::new();
    let mut objects = Vec::with_capacity(items.len());
    for item in items {
        let Some(entities) = doc.container(item.container) else {
            continue;
        };
        let d = match cache.entry(item.container) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => {
                e.insert(diagnose(entities.mesh(), tx.tolerances())?)
            }
        };
        objects.push(describe(doc, item, d));
    }
    let printable = !objects.is_empty() && objects.iter().all(|o| o.printable);
    Ok(SolidCheckOutput { objects, printable })
}

/// The `solid-check` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct SolidCheck;

impl Command for SolidCheck {
    type Params = SolidCheckParams;
    type Output = SolidCheckOutput;

    fn name(&self) -> &'static str {
        "solid-check"
    }

    fn validate(&self, _: &Document, _: &SolidCheckParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SolidCheckParams,
    ) -> Result<SolidCheckOutput, CommandError> {
        let items = objects(tx, &p.common, p.targets.as_ref(), p.include_hidden)?;
        check(tx, &items)
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "solid-check".into(),
            label: "Check Solids".into(),
            category: CommandCategory::Solid,
            view_only: true,
            steps: Vec::new(),
        }
    }
}

/// The `solid-repair` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct SolidRepair;

impl Command for SolidRepair {
    type Params = SolidRepairParams;
    type Output = SolidRepairOutput;

    fn name(&self) -> &'static str {
        "solid-repair"
    }

    fn validate(&self, _: &Document, p: &SolidRepairParams) -> Result<(), CommandError> {
        if !(p.fill_holes || p.remove_wire_edges || p.reverse_inside_out) {
            return Err(CommandError::invalid_param(
                "fill_holes",
                "choose at least one repair",
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SolidRepairParams,
    ) -> Result<SolidRepairOutput, CommandError> {
        let items = objects(tx, &p.common, p.targets.as_ref(), p.include_hidden)?;
        let options = RepairOptions {
            fill_holes: p.fill_holes,
            remove_wire_edges: p.remove_wire_edges,
            reverse_inside_out: p.reverse_inside_out,
        };
        let mut out = SolidRepairOutput {
            repaired: 0,
            holes_filled: 0,
            faces_added: 0,
            holes_left: 0,
            wire_edges_removed: 0,
            shells_reversed: 0,
            locked: 0,
            printable: false,
        };
        let mut done: Vec<ContainerRef> = Vec::new();
        for item in &items {
            if done.contains(&item.container) {
                continue;
            }
            done.push(item.container);
            let diagnosis = match tx.document().container(item.container) {
                Some(entities) => diagnose(entities.mesh(), tx.tolerances())?,
                None => continue,
            };
            if diagnosis.is_printable() {
                continue;
            }
            if tx.document().locked_step(&item.path).is_some() {
                out.locked += 1;
                continue;
            }
            let report: RepairReport =
                tx.with_mesh(item.container, |mesh, tol, d| repair(mesh, options, tol, d))?;
            if report != RepairReport::default() {
                out.repaired += 1;
            }
            out.holes_filled += report.holes_filled;
            out.faces_added += report.faces_added;
            out.holes_left += report.holes_left;
            out.wire_edges_removed += report.wire_edges_removed;
            out.shells_reversed += report.shells_reversed;
        }
        out.printable = check(tx, &items)?.printable;
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "solid-repair".into(),
            label: "Repair Solids".into(),
            category: CommandCategory::Solid,
            view_only: false,
            steps: tool_steps(&[("Repair the objects that are not solids", "")]),
        }
    }
}

/// Registers the check and repair commands.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(SolidCheck);
    registry.register(SolidRepair);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn engine() -> Engine {
        Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
    }

    fn run(e: &mut Engine, name: &str, params: Value) -> Value {
        e.execute_erased(name, params).unwrap()
    }

    /// A 100 × 50 × 20 mm box made into a group.
    fn boxed(e: &mut Engine) {
        run(
            e,
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [100, 50, 0] }),
        );
        run(
            e,
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 20 }),
        );
        run(e, "make-group", json!({ "targets": "all" }));
    }

    #[test]
    fn a_pushed_box_is_printable_with_its_volume_and_size() {
        let mut e = engine();
        boxed(&mut e);
        let out = run(&mut e, "solid-check", json!({}));
        assert_eq!(out["printable"], true, "{out}");
        let object = &out["objects"][0];
        assert_eq!(out["objects"].as_array().unwrap().len(), 1);
        assert_eq!(object["faces"], 6);
        let volume = object["volume_mm3"].as_f64().unwrap();
        assert!((volume - 100_000.0).abs() < 1e-6, "{volume}");
        assert_eq!(object["size_mm"], json!([100.0, 50.0, 20.0]));
        assert_eq!(e.history().entries().len(), 3, "the check is not an edit");
    }

    #[test]
    fn repair_closes_a_missing_face_and_undoes_in_one_step() {
        let mut e = engine();
        run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [100, 50, 0] }),
        );
        run(
            &mut e,
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 20 }),
        );
        run(
            &mut e,
            "delete",
            json!({ "targets": "face & normal:z & plane:xy@20" }),
        );
        let out = run(&mut e, "solid-check", json!({}));
        assert_eq!(out["printable"], false);
        assert_eq!(out["objects"][0]["name"], "Model");
        assert_eq!(out["objects"][0]["holes"], 1);
        assert_eq!(out["objects"][0]["open_edges"], 4);

        let repaired = run(&mut e, "solid-repair", json!({}));
        assert_eq!(repaired["holes_filled"], 1, "{repaired}");
        assert_eq!(repaired["printable"], true);
        assert_eq!(run(&mut e, "solid-check", json!({}))["printable"], true);
        e.undo().unwrap();
        assert_eq!(
            run(&mut e, "solid-check", json!({}))["objects"][0]["holes"],
            1
        );
    }

    #[test]
    fn nothing_to_print_is_not_printable() {
        let mut e = engine();
        let out = run(&mut e, "solid-check", json!({}));
        assert_eq!(out, json!({ "objects": [], "printable": false }));
        assert!(
            e.execute_erased(
                "solid-repair",
                json!({ "fill_holes": false, "remove_wire_edges": false, "reverse_inside_out": false })
            )
            .is_err()
        );
    }
}
