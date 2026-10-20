//! Acceptance tests of docs/05 §5 for the 0.1 organization commands.

use std::sync::Arc;

use serde_json::json;

use crate::commands::{CommandRegistry, Engine};
use crate::document::{ContainerRef, Document};

fn engine() -> Engine {
    Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
}

fn run(e: &mut Engine, name: &str, params: serde_json::Value) -> serde_json::Value {
    e.execute_erased(name, params)
        .unwrap_or_else(|err| panic!("{name}: {err}"))
}

fn boxed(e: &mut Engine, x: f64, y: f64, size: f64) {
    let r = run(
        e,
        "rectangle",
        json!({ "corner_a": [x, y, 0], "corner_b": [x + size, y + size, 0] }),
    );
    run(
        e,
        "push-pull",
        json!({ "face": { "Pid": r["face"] }, "distance": size }),
    );
}

fn assert_valid(e: &Engine) {
    let report = crate::diagnostics::validate(e.document());
    assert!(report.is_clean(), "{report:#?}");
}

#[test]
fn make_group_and_explode_round_trip() {
    let mut e = engine();
    boxed(&mut e, 100.0, 200.0, 50.0);
    let out = run(
        &mut e,
        "make-group",
        json!({ "targets": "all", "name": "Box" }),
    );
    let doc = e.document();
    assert_eq!(doc.model.mesh().face_count(), 0);
    assert_eq!(doc.model.mesh().edge_count(), 0);
    assert_eq!(doc.model.counts().groups, 1);
    assert_eq!(doc.definitions.len(), 1);
    let (d, def) = doc.definitions.iter().next().unwrap();
    assert!(def.is_internal_group);
    assert!(def.entities.mesh().is_solid());
    assert!((def.entities.mesh().volume().unwrap() - 125_000.0).abs() < 1e-6);
    // Definition coordinates start at the selection's minimum corner.
    assert_eq!(def.entities.mesh().aabb().min, glam::DVec3::ZERO);
    assert_eq!(e.selection().len(), 1);
    assert_eq!(doc.aabb().min, glam::DVec3::new(100.0, 200.0, 0.0));
    assert!(out["group"].is_u64());
    assert_valid(&e);
    let _ = d;

    let out = run(&mut e, "explode", json!({ "targets": "group" }));
    assert_eq!(out["exploded"], json!(1));
    let doc = e.document();
    assert_eq!(doc.model.mesh().face_count(), 6);
    assert_eq!(doc.model.counts().groups, 0);
    assert_eq!(doc.definitions.len(), 0, "the private definition is purged");
    assert!((doc.model.mesh().volume().unwrap() - 125_000.0).abs() < 1e-6);
    assert_valid(&e);

    e.undo().unwrap();
    assert_eq!(e.document().model.counts().groups, 1);
    e.undo().unwrap();
    assert_eq!(e.document().model.mesh().face_count(), 6);
    assert_eq!(e.document().definitions.len(), 0);
    e.redo().unwrap();
    e.redo().unwrap();
    assert_eq!(e.document().model.mesh().face_count(), 6);
    assert_valid(&e);
}

#[test]
fn components_copy_into_arrays_sharing_one_definition() {
    let mut e = engine();
    boxed(&mut e, 0.0, 0.0, 400.0);
    let out = run(
        &mut e,
        "make-component",
        json!({ "targets": "all", "name": "Chair" }),
    );
    assert!(out["instance"].is_u64());
    let copies = run(
        &mut e,
        "move",
        json!({ "targets": "instance", "delta": [600, 0, 0], "copy": true, "copies": 3 }),
    );
    let first_copy = copies["copies"][0].as_u64().unwrap();
    let doc = e.document();
    assert_eq!(doc.model.counts().instances, 4);
    assert_eq!(doc.definitions.len(), 1);
    assert!(doc.definition_by_name("Chair").is_some());
    assert!((doc.aabb().max.x - 2200.0).abs() < 1e-9);
    let out = run(&mut e, "select", json!({ "query": "definition:Chair" }));
    assert_eq!(out["count"], json!(4));
    // Exploding one instance leaves the others and the definition.
    run(
        &mut e,
        "explode",
        json!({ "targets": format!("@{first_copy}") }),
    );
    assert_eq!(e.document().model.counts().instances, 3);
    assert_eq!(e.document().definitions.len(), 1);
    assert_eq!(e.document().model.mesh().face_count(), 6);
    let err = e
        .execute_erased(
            "make-component",
            json!({ "targets": "face", "name": "Chair" }),
        )
        .unwrap_err();
    assert_eq!(err.code(), "E1001");
    assert_valid(&e);
}

#[test]
fn grouping_one_face_keeps_the_shared_edge_on_both_sides() {
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [100,50,0] }),
    );
    run(&mut e, "line", json!({ "points": [[40,0,0],[40,50,0]] }));
    run(
        &mut e,
        "make-group",
        json!({ "targets": "face & area<2500" }),
    );
    let doc = e.document();
    assert_eq!(doc.model.mesh().face_count(), 1);
    // The remaining face keeps all its edges, the shared chord included.
    let f = doc.model.mesh().faces().next().unwrap().0;
    assert_eq!(doc.model.mesh().face_edges(f).unwrap().len(), 4);
    assert_eq!(doc.model.mesh().edge_count(), 4);
    let def = doc.definitions.iter().next().unwrap().1;
    assert_eq!(def.entities.mesh().face_count(), 1);
    assert_eq!(def.entities.mesh().edge_count(), 4);
    let _ = ContainerRef::Model;
    assert_valid(&e);
}
