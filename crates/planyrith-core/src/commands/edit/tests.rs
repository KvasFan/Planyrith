//! Acceptance tests of docs/05 §4 for the 0.1 editing commands.

use std::sync::Arc;

use serde_json::json;

use crate::commands::{CommandRegistry, Engine};
use crate::document::Document;

fn engine() -> Engine {
    Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
}

fn run(e: &mut Engine, name: &str, params: serde_json::Value) -> serde_json::Value {
    e.execute_erased(name, params)
        .unwrap_or_else(|err| panic!("{name}: {err}"))
}

fn at(pid: &serde_json::Value) -> String {
    format!("@{}", pid.as_u64().unwrap())
}

#[test]
fn wall_with_a_door_opening_headless() {
    let mut e = engine();
    // Wall footprint 4 m × 200 mm, pulled to 2.6 m.
    let r = run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [4000,200,0] }),
    );
    let out = run(
        &mut e,
        "push-pull",
        json!({ "face": { "Pid": r["face"] }, "distance": 2600.0 }),
    );
    assert_eq!(out["solid_after"], json!(true));
    assert_eq!(out["new_faces"].as_array().unwrap().len(), 5);
    // Door outline on the front face (y = 0), pushed through the wall.
    let door = run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [1000,0,0], "corner_b": [1900,0,2100] }),
    );
    let out = run(
        &mut e,
        "push-pull",
        json!({ "face": { "Pid": door["face"] }, "distance": -200.0 }),
    );
    assert_eq!(out["through_hole"], json!(true));
    assert_eq!(out["solid_after"], json!(true));
    let mesh = e.document().model.mesh();
    let volume = mesh.volume().unwrap();
    let expected = 4000.0 * 200.0 * 2600.0 - 900.0 * 200.0 * 2100.0;
    assert!((volume - expected).abs() < 1e-3, "{volume} vs {expected}");
    assert!(mesh.validate(e.document().tolerances()).is_empty());

    // Undo the door push, then the wall, and redo both.
    e.undo().unwrap();
    assert!(!e.document().model.mesh().is_solid() || e.document().model.mesh().volume().is_ok());
    e.undo().unwrap();
    e.undo().unwrap();
    e.redo().unwrap();
    e.redo().unwrap();
    e.redo().unwrap();
    let mesh = e.document().model.mesh();
    assert!((mesh.volume().unwrap() - expected).abs() < 1e-3);
    let _ = at;
}

#[test]
fn push_pull_needs_a_face_and_a_distance() {
    let mut e = engine();
    let r = run(&mut e, "line", json!({ "points": [[0,0,0],[100,0,0]] }));
    let edge = r["edges"][0].clone();
    let err = e
        .execute_erased(
            "push-pull",
            json!({ "face": { "Pid": edge }, "distance": 10.0 }),
        )
        .unwrap_err();
    assert_eq!(err.code(), "E1201");
    let err = e
        .execute_erased("push-pull", json!({ "face": { "Pid": 999 } }))
        .unwrap_err();
    assert_eq!(err.code(), "E1001");
    let err = e
        .execute_erased(
            "push-pull",
            json!({ "face": { "Pid": 999 }, "distance": 5.0 }),
        )
        .unwrap_err();
    assert_eq!(err.code(), "E1101");
}

#[test]
fn push_pull_to_a_point_measures_along_the_normal() {
    let mut e = engine();
    let r = run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [100,100,0] }),
    );
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Pid": r["face"] }, "to": [500, 500, 750] }),
    );
    let mesh = e.document().model.mesh();
    assert!((mesh.volume().unwrap() - 100.0 * 100.0 * 750.0).abs() < 1e-6);
}

#[test]
fn push_pull_accepts_queries() {
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [1200,800,0] }),
    );
    let out = run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face & last" }, "distance": 30.0 }),
    );
    assert_eq!(out["solid_after"], json!(true));
    let err = e
        .execute_erased(
            "push-pull",
            json!({ "face": { "Query": "face" }, "distance": 10.0 }),
        )
        .unwrap_err();
    assert_eq!(err.code(), "E1102");
}

#[test]
fn select_sets_the_engine_selection_without_history() {
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [100,100,0] }),
    );
    let before = e.history().applied();
    let out = run(&mut e, "select", json!({ "query": "face" }));
    assert_eq!(out["count"], json!(1));
    assert_eq!(out["kinds"]["face"], json!(1));
    assert_eq!(e.selection().len(), 1);
    let out = run(&mut e, "select", json!({ "query": "edge", "mode": "add" }));
    assert_eq!(out["count"], json!(5));
    assert_eq!(e.history().applied(), before);
    // `selection` in a later query refers to it.
    let out = run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "selection & face" }, "distance": 10.0 }),
    );
    assert_eq!(out["solid_after"], json!(true));
}

fn faces(e: &Engine) -> usize {
    e.document().model.mesh().face_count()
}

#[test]
fn erase_merges_and_deletes_by_the_edge_rules() {
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [100,50,0] }),
    );
    run(&mut e, "line", json!({ "points": [[40,0,0],[40,50,0]] }));
    assert_eq!(faces(&e), 2);
    // The chord: the only edge with x = 40 at both ends.
    let out = run(
        &mut e,
        "erase",
        json!({ "targets": "edge & last & length=50" }),
    );
    assert_eq!(out["faces_merged"], json!(1));
    assert_eq!(faces(&e), 1);
    // Faces are not erased by the eraser.
    let out = run(&mut e, "erase", json!({ "targets": "face" }));
    assert_eq!(out["erased"], json!(0));
    assert_eq!(faces(&e), 1);
    // Soften mode changes flags only.
    run(
        &mut e,
        "erase",
        json!({ "targets": "edge", "mode": "hide" }),
    );
    assert!(
        e.document()
            .model
            .mesh()
            .edges()
            .all(|(_, x)| x.flags.contains(crate::geom::EdgeFlags::HIDDEN))
    );
    e.undo().unwrap();
    assert!(
        e.document()
            .model
            .mesh()
            .edges()
            .all(|(_, x)| x.flags.bits() == 0)
    );
}

#[test]
fn delete_removes_faces_but_keeps_their_edges() {
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [100,50,0] }),
    );
    let out = run(&mut e, "delete", json!({ "targets": "face" }));
    assert_eq!(out["deleted"], json!(1));
    assert_eq!(faces(&e), 0);
    assert_eq!(e.document().model.mesh().edge_count(), 4);
    let out = run(&mut e, "delete", json!({ "targets": "all" }));
    assert_eq!(out["deleted"], json!(4));
    assert!(e.document().model.is_empty());
    e.undo().unwrap();
    e.undo().unwrap();
    assert_eq!(faces(&e), 1);
}

#[test]
fn hide_and_unhide_round_trip() {
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [100,50,0] }),
    );
    let out = run(&mut e, "hide", json!({ "targets": "face" }));
    assert_eq!(out["changed"], json!(1));
    let out = run(&mut e, "select", json!({ "query": "hidden" }));
    assert_eq!(out["count"], json!(1));
    run(&mut e, "unhide", json!({ "targets": "hidden" }));
    let out = run(&mut e, "select", json!({ "query": "hidden" }));
    assert_eq!(out["count"], json!(0));
}
