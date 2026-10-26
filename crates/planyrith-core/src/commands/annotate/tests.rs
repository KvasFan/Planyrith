//! Tests of the annotation and guide commands (docs/05 §6–§7): dimensions
//! re-measure, survive saving, detach; texts default to measurements;
//! guides and resizing.

use std::sync::Arc;

use glam::DVec3;
use serde_json::json;

use crate::commands::{CommandRegistry, Engine};
use crate::document::annotations::dimension_view;
use crate::document::{ContainerRef, Document, EntityView, Guide};
use crate::ids::EntityId;
use crate::io::NoProgress;
use crate::io::native::{WriteOptions, read_document, write_document};

fn engine() -> Engine {
    Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
}

fn run(e: &mut Engine, name: &str, params: serde_json::Value) -> serde_json::Value {
    e.execute_erased(name, params)
        .unwrap_or_else(|err| panic!("{name}: {err}"))
}

fn reopen(e: &Engine) -> Engine {
    let opts = WriteOptions {
        deterministic: true,
        include_history: false,
        ..WriteOptions::default()
    };
    let file = write_document(e.document(), &[], &opts, &NoProgress).unwrap();
    let loaded = read_document(&file, &NoProgress).unwrap();
    Engine::new(loaded.document, Arc::new(CommandRegistry::with_builtins()))
}

/// The value and text of the model's first dimension.
fn measure(e: &Engine) -> (f64, String, bool) {
    let doc = e.document();
    let (_, dim) = doc.model.dimensions().next().expect("a dimension");
    let v = dimension_view(doc, ContainerRef::Model, dim);
    (v.value, v.text, v.detached)
}

/// The pid of the model edge between two points.
fn edge_between(e: &Engine, a: DVec3, b: DVec3) -> u64 {
    let doc = e.document();
    let mesh = doc.model.mesh();
    doc.model
        .order()
        .iter()
        .find_map(|&id| {
            let EntityId::Edge(edge) = id else {
                return None;
            };
            let s = mesh.edge_segment(edge).ok()?;
            let same = |p: DVec3, q: DVec3| (p - q).length() < 1e-6;
            ((same(s.a, a) && same(s.b, b)) || (same(s.a, b) && same(s.b, a)))
                .then(|| doc.model.pid_of(id).unwrap().0)
        })
        .expect("the edge")
}

#[test]
fn dimensions_follow_their_edges_and_survive_saving() {
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [1000, 600, 0] }),
    );
    let out = run(
        &mut e,
        "dimension",
        json!({ "start": [0, 0, 0], "end": [1000, 0, 0], "offset": [0, -300, 0] }),
    );
    assert_eq!(out["value"], 1000.0);
    assert_eq!(out["text"], "1000.0mm");
    // Move the right edge: the dimension re-measures.
    let right = edge_between(
        &e,
        DVec3::new(1000.0, 0.0, 0.0),
        DVec3::new(1000.0, 600.0, 0.0),
    );
    run(
        &mut e,
        "move",
        json!({ "targets": format!("@{right}"), "delta": [500, 0, 0] }),
    );
    assert_eq!(measure(&e).0, 1500.0);
    // Saved and reopened, still attached and measuring.
    let back = reopen(&e);
    let (value, _, detached) = measure(&back);
    assert!(!detached);
    assert_eq!(value, 1500.0);
    // Undo the move: back to 1000.
    e.undo().unwrap();
    assert_eq!(measure(&e).0, 1000.0);
    let report = crate::diagnostics::validate(e.document());
    assert!(report.is_clean(), "{report:#?}");
}

#[test]
fn erasing_an_anchor_detaches_the_dimension() {
    let mut e = engine();
    run(
        &mut e,
        "line",
        json!({ "points": [[0, 0, 0], [2000, 0, 0]] }),
    );
    run(
        &mut e,
        "dimension",
        json!({ "start": [0, 0, 0], "end": [2000, 0, 0], "offset": [0, 500, 0],
                "text_override": "<> (typ.)" }),
    );
    assert_eq!(measure(&e).1, "2000.0mm (typ.)");
    run(&mut e, "erase", json!({ "targets": "edge" }));
    assert!(measure(&e).2, "detached");
    let report = crate::diagnostics::validate(e.document());
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.kind == crate::diagnostics::ProblemKind::DanglingAnchor),
        "{report:#?}"
    );
}

#[test]
fn linear_and_radial_dimensions() {
    let mut e = engine();
    run(
        &mut e,
        "line",
        json!({ "points": [[0, 0, 0], [3000, 4000, 0]] }),
    );
    let aligned = run(
        &mut e,
        "dimension",
        json!({ "start": [0, 0, 0], "end": [3000, 4000, 0] }),
    );
    assert_eq!(aligned["value"], 5000.0);
    let linear = run(
        &mut e,
        "dimension",
        json!({ "kind": "linear", "start": [0, 0, 0], "end": [3000, 4000, 0], "offset": [0, -500, 0] }),
    );
    assert_eq!(linear["value"], 3000.0, "along x, across the offset");
    let vertical = run(
        &mut e,
        "dimension",
        json!({ "kind": "linear", "start": [0, 0, 0], "end": [3000, 4000, 0], "direction": [0, 1, 0] }),
    );
    assert_eq!(vertical["value"], 4000.0);
    run(
        &mut e,
        "circle",
        json!({ "center": [10000, 0, 0], "radius": 500.0, "segments": 24 }),
    );
    let r = run(
        &mut e,
        "dimension",
        json!({ "kind": "radius", "curve": { "Query": "edge & last" }, "offset": [300, 300, 0] }),
    );
    assert_eq!(r["text"], "R500.0mm");
    let d = run(
        &mut e,
        "dimension",
        json!({ "kind": "diameter", "curve": { "Query": "edge & length<1000" } }),
    );
    assert_eq!(d["value"], 1000.0);
    assert!(
        e.execute_erased(
            "dimension",
            json!({ "kind": "radius", "curve": { "Query": "edge & length>4000" } }),
        )
        .is_err(),
        "a straight edge has no radius"
    );
    assert!(
        e.execute_erased("dimension", json!({ "start": [0, 0, 0] }))
            .is_err()
    );
}

#[test]
fn texts_default_to_what_they_point_at() {
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [2000, 1000, 0] }),
    );
    let vertex = run(&mut e, "text", json!({ "at": [2000, 1000, 0] }));
    assert_eq!(vertex["content"], "2000.0mm, 1000.0mm, 0.0mm");
    let edge = run(&mut e, "text", json!({ "at": [1000, 0, 0] }));
    assert_eq!(edge["content"], "2000.0mm");
    let face = run(&mut e, "text", json!({ "at": [500, 500, 0] }));
    assert_eq!(face["content"], "2000000.0mm²");
    let screen = run(&mut e, "text", json!({ "text": "Kitchen" }));
    assert_eq!(screen["content"], "Kitchen");
    assert!(
        e.execute_erased("text", json!({ "at": [9000, 9000, 0] }))
            .is_err()
    );
    assert!(e.execute_erased("text", json!({})).is_err());
}

#[test]
fn tape_measure_protractor_and_guides() {
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [2000, 1000, 0] }),
    );
    // From an edge: a parallel guide line.
    let line = run(
        &mut e,
        "tape-measure",
        json!({ "from": [500, 0, 0], "to": [500, 300, 0] }),
    );
    assert_eq!(line["distance"], 300.0);
    // From a vertex: a guide point.
    let point = run(
        &mut e,
        "tape-measure",
        json!({ "from": [0, 0, 0], "to": [100, 100, 0] }),
    );
    // From nowhere: only a measurement.
    let none = run(
        &mut e,
        "tape-measure",
        json!({ "from": [5000, 5000, 0], "to": [5000, 5100, 0] }),
    );
    assert_eq!(none["guide"], serde_json::Value::Null);
    let guides: Vec<Guide> = e
        .document()
        .model
        .order()
        .iter()
        .filter_map(|&id| match e.document().model.view(id) {
            Some(EntityView::Guide(g)) => Some(g.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(guides.len(), 2, "{line} {point}");
    assert!(matches!(guides[0], Guide::Line { dir, .. } if (dir.x.abs() - 1.0).abs() < 1e-9));
    assert!(
        matches!(guides[1], Guide::Point { pos } if (pos - DVec3::new(100.0, 100.0, 0.0)).length() < 1e-9)
    );

    let angle = run(
        &mut e,
        "protractor",
        json!({ "center": [0, 0, 0], "from_point": [1000, 0, 0], "to_point": [0, 500, 0] }),
    );
    assert!((angle["angle"].as_f64().unwrap() - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
    run(
        &mut e,
        "protractor",
        json!({ "center": [0, 0, 0], "from_point": [1000, 0, 0], "angle": 0.5 }),
    );
    let deleted = run(&mut e, "guide-delete-all", json!({}));
    assert_eq!(deleted["deleted"], 4);
}

#[test]
fn resize_model_scales_about_the_origin() {
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [1000, 0, 0], "corner_b": [3000, 1000, 0] }),
    );
    run(
        &mut e,
        "dimension",
        json!({ "start": [1000, 0, 0], "end": [3000, 0, 0], "offset": [0, -200, 0] }),
    );
    let out = run(
        &mut e,
        "resize-model",
        json!({ "measured": 2000.0, "target": 3000.0 }),
    );
    assert_eq!(out["factor"], 1.5);
    let b = e.document().aabb();
    assert!(
        (b.min - DVec3::new(1500.0, 0.0, 0.0)).length() < 1e-6,
        "{:?}",
        b.min
    );
    assert!((b.max - DVec3::new(4500.0, 1500.0, 0.0)).length() < 1e-6);
    assert_eq!(measure(&e).0, 3000.0);
    let report = crate::diagnostics::validate(e.document());
    assert!(report.is_clean(), "{report:#?}");
}

#[test]
fn dimensions_inside_components_keep_their_anchors_when_copied() {
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [800, 400, 0] }),
    );
    run(
        &mut e,
        "dimension",
        json!({ "start": [0, 0, 0], "end": [800, 0, 0], "offset": [0, -100, 0] }),
    );
    run(
        &mut e,
        "make-component",
        json!({ "targets": "all", "name": "Board" }),
    );
    let storage = Arc::new(crate::io::MemoryStorage::new());
    e.set_storage(storage.clone());
    run(
        &mut e,
        "component-save-as",
        json!({ "definition": "Board", "path": "board.plyrc" }),
    );
    let mut other = engine();
    other.set_storage(storage);
    run(
        &mut other,
        "component-insert",
        json!({ "path": "board.plyrc" }),
    );
    let doc = other.document();
    let def = doc.definition_by_name("Board").unwrap();
    let c = doc.container(ContainerRef::Definition(def)).unwrap();
    let (_, dim) = c.dimensions().next().expect("the dimension came along");
    let v = dimension_view(doc, ContainerRef::Definition(def), dim);
    assert!(!v.detached);
    assert_eq!(v.value, 800.0);
}

#[test]
fn text_3d_makes_letters_with_holes_and_solids() {
    let mut e = engine();
    let flat = run(&mut e, "text-3d", json!({ "text": "Ob", "height": 100.0 }));
    // "O" is one face with a hole, "b" one face with a hole.
    assert_eq!(flat["faces"], 2, "{flat}");
    let doc = e.document();
    let group = doc.definitions.iter().last().unwrap().0;
    let mesh = doc
        .container(ContainerRef::Definition(group))
        .unwrap()
        .mesh();
    let holes: usize = mesh
        .faces()
        .map(|(f, _)| mesh.face_cycles(f).unwrap().len() - 1)
        .sum();
    assert_eq!(holes, 2);
    // Capital letters are 100 mm high.
    let b = e.document().aabb();
    assert!((b.max.y - b.min.y - 100.0).abs() < 15.0, "{b:?}");

    let solid = run(
        &mut e,
        "text-3d",
        json!({ "text": "L", "height": 200.0, "extrude": 20.0, "align": "center" }),
    );
    let doc = e.document();
    let def = doc.definitions.iter().last().unwrap().0;
    let mesh = doc.container(ContainerRef::Definition(def)).unwrap().mesh();
    assert!(mesh.is_solid(), "{solid}");
    assert!(mesh.volume().unwrap() > 0.0, "outward faces");
    let outline = run(
        &mut e,
        "text-3d",
        json!({ "text": "A", "height": 100.0, "filled": false }),
    );
    assert_eq!(outline["faces"], 0);
    let report = crate::diagnostics::validate(e.document());
    assert!(report.is_clean(), "{report:#?}");
    assert!(
        e.execute_erased("text-3d", json!({ "text": " ", "height": 10.0 }))
            .is_err()
    );
}

#[test]
fn section_planes_activate_one_at_a_time_and_slice() {
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [1000, 1000, 0] }),
    );
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face" }, "distance": 1000.0 }),
    );
    run(&mut e, "make-group", json!({ "targets": "all" }));
    let a = run(
        &mut e,
        "section-plane-add",
        json!({ "plane": { "normal": [0, 0, 1], "d": -400.0 } }),
    );
    let b = run(
        &mut e,
        "section-plane-add",
        json!({ "plane": { "normal": [1, 0, 0], "d": -500.0 }, "name": "Long" }),
    );
    assert_eq!(a["name"], "Section 1");
    assert_eq!(b["name"], "Long");
    let active: Vec<bool> = e
        .document()
        .model
        .order()
        .iter()
        .filter_map(|&id| match e.document().model.view(id) {
            Some(EntityView::SectionPlane(s)) => Some(s.active),
            _ => None,
        })
        .collect();
    assert_eq!(active, vec![false, true], "one active plane per container");
    let first = a["section_plane"].as_u64().unwrap();
    let t = run(
        &mut e,
        "section-plane-toggle",
        json!({ "section": { "Pid": first } }),
    );
    assert_eq!(t["active"], true);
    run(
        &mut e,
        "section-plane-reverse",
        json!({ "section": { "Pid": first } }),
    );
    // The slice of the cube at z = 400: a closed square, so a face.
    let slice = run(
        &mut e,
        "section-create-group-from-slice",
        json!({ "section": { "Pid": first } }),
    );
    assert_eq!(slice["edges"], 4);
    assert_eq!(slice["faces"], 1);
    let none = run(
        &mut e,
        "section-create-group-from-slice",
        json!({ "plane": { "normal": [0, 0, 1], "d": -5000.0 } }),
    );
    assert_eq!(none["group"], serde_json::Value::Null);
    run(
        &mut e,
        "section-plane-align-view",
        json!({ "section": { "Pid": first } }),
    );
    let cam = e.document().camera;
    assert!(
        (cam.forward() - DVec3::Z).length() < 1e-9,
        "looking at the kept side"
    );
    let shown = run(&mut e, "section-display-set", json!({ "fills": false }));
    assert_eq!(shown["fills"], false);
    run(
        &mut e,
        "section-plane-delete",
        json!({ "section": { "Pid": first } }),
    );
    let report = crate::diagnostics::validate(e.document());
    assert!(report.is_clean(), "{report:#?}");
}

#[test]
fn axes_move_the_inference_origin() {
    use crate::inference::{Constraint, InferenceEngine, InferenceRequest, ViewMetrics};
    let mut e = engine();
    let out = run(
        &mut e,
        "axes-set",
        json!({ "origin": [1000, 2000, 0], "x": [0, 1, 0], "y": [-1, 0, 0] }),
    );
    assert_eq!(out["axes"][0], json!([0.0, 1.0, 0.0]));
    // Picking near the new origin snaps to it.
    let index = crate::spatial::DocumentIndex::build(e.document());
    let req = InferenceRequest {
        ray: crate::math::Ray::new(DVec3::new(1000.5, 2000.5, 5000.0), -DVec3::Z),
        constraint: Constraint::Free,
        view: ViewMetrics {
            perspective: false,
            mm_per_pixel_at_unit_depth: 1.0,
        },
        pixel_tolerance: 5.0,
        reference_point: None,
        last_point: None,
    };
    let hit = InferenceEngine::new().infer(e.document(), &index, &req);
    assert!(
        (hit.point - DVec3::new(1000.0, 2000.0, 0.0)).length() < 1e-6,
        "{hit:?}"
    );
    run(&mut e, "axes-reset", json!({}));
    assert_eq!(e.document().info.axes, glam::DAffine3::IDENTITY);
}

#[test]
fn shadows_follow_the_sun_settings() {
    let mut e = engine();
    let out = run(
        &mut e,
        "shadows-set",
        json!({ "enabled": true, "date": "2026-06-21", "time": "12:00",
                "latitude": 45.0, "longitude": 0.0, "utc_offset": 0.0 }),
    );
    assert_eq!(out["enabled"], true);
    let alt = out["sun_altitude_deg"].as_f64().unwrap();
    assert!((alt - 68.4).abs() < 0.5, "{alt}");
    let az = out["sun_azimuth_deg"].as_f64().unwrap();
    assert!((az - 180.0).abs() < 2.0, "{az}");
    let manual = run(
        &mut e,
        "shadows-set",
        json!({ "azimuth": 90f64.to_radians(), "altitude": 30f64.to_radians() }),
    );
    assert!((manual["sun_azimuth_deg"].as_f64().unwrap() - 90.0).abs() < 1e-9);
    assert!(
        e.execute_erased("shadows-set", json!({ "date": "2026-02-30" }))
            .is_err()
    );
    assert!(
        e.execute_erased("shadows-set", json!({ "azimuth": 0.1, "latitude": 3.0 }))
            .is_err()
    );
}
