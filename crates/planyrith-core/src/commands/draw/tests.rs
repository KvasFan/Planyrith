//! Acceptance tests of docs/05 §3 for the 0.1 drawing commands.

use std::sync::Arc as Shared;

use glam::DVec3;
use serde_json::json;

use crate::commands::{CommandRegistry, Engine};
use crate::document::Document;
use crate::ids::{EntityId, Pid};

fn engine() -> Engine {
    Engine::new(
        Document::new(),
        Shared::new(CommandRegistry::with_builtins()),
    )
}

fn run(e: &mut Engine, name: &str, params: serde_json::Value) -> serde_json::Value {
    e.execute_erased(name, params)
        .unwrap_or_else(|err| panic!("{name}: {err}"))
}

fn face_area(e: &Engine, pid: &serde_json::Value) -> f64 {
    let pid = Pid(pid.as_u64().unwrap());
    let Some(EntityId::Face(f)) = e.document().model.id_of(pid) else {
        panic!("{pid} is not a face");
    };
    e.document().model.mesh().face_area(f).unwrap()
}

fn assert_valid(e: &Engine) {
    let v = e
        .document()
        .model
        .mesh()
        .validate(e.document().tolerances());
    assert!(v.is_empty(), "{v:#?}");
}

#[test]
fn line_closing_on_the_ground_makes_one_face_and_a_diagonal_splits_it() {
    let mut e = engine();
    let out = run(
        &mut e,
        "line",
        json!({ "points": [[0,0,0],[1000,0,0],[1000,800,0],[0,800,0],[0,0,0]] }),
    );
    assert_eq!(out["edges"].as_array().unwrap().len(), 4);
    assert_eq!(out["faces_created"].as_array().unwrap().len(), 1);
    assert_eq!(e.document().model.mesh().face_count(), 1);
    let out = run(&mut e, "line", json!({ "points": [[0,0,0],[1000,800,0]] }));
    assert_eq!(out["faces_split"].as_array().unwrap().len(), 1);
    assert_eq!(e.document().model.mesh().face_count(), 2);
    assert_valid(&e);
    assert!(
        e.execute_erased("line", json!({ "points": [[0,0,0]] }))
            .is_err()
    );
}

#[test]
fn rectangle_on_the_ground_and_inside_an_existing_face() {
    let mut e = engine();
    let out = run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [8000,6000,0] }),
    );
    let floor = out["face"].clone();
    assert!((face_area(&e, &floor) - 48e6).abs() < 1e-3);
    let out = run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [1000,1000,0], "corner_b": [2000,3000,0] }),
    );
    assert!((face_area(&e, &out["face"]) - 2e6).abs() < 1e-3);
    assert!((face_area(&e, &floor) - 46e6).abs() < 1e-3);
    // A vertical rectangle (shared y): the XZ plane.
    let out = run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [3000,0,2500] }),
    );
    assert!((face_area(&e, &out["face"]) - 7.5e6).abs() < 1e-3);
    assert_valid(&e);
    let err = e
        .execute_erased(
            "rectangle",
            json!({ "corner_a": [0,0,0], "corner_b": [1,1,1] }),
        )
        .unwrap_err();
    assert_eq!(err.code(), "E1001");
    let out = run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,5000], "corner_b": [300,100,5000], "aspect_lock": "square" }),
    );
    assert!((face_area(&e, &out["face"]) - 90000.0).abs() < 1e-6);
}

#[test]
fn circle_and_polygon_carry_curve_descriptors() {
    let mut e = engine();
    let out = run(
        &mut e,
        "circle",
        json!({ "center": [0,0,0], "radius": 500.0 }),
    );
    assert_eq!(out["edges"].as_array().unwrap().len(), 24);
    assert!(out["face"].is_u64());
    let mesh = e.document().model.mesh();
    assert_eq!(mesh.curve_count(), 1);
    let (c, _) = mesh.curves().next().unwrap();
    assert!(mesh.curve_is_intact(c).unwrap());
    let out = run(
        &mut e,
        "polygon",
        json!({ "center": [2000,0,0], "radius": 100.0, "sides": 4, "inscribed": false }),
    );
    // Circumscribed square of apothem 100: side 200.
    assert!((face_area(&e, &out["face"]) - 40000.0).abs() < 1e-6);
    assert_eq!(e.document().model.mesh().curve_count(), 2);
    assert_valid(&e);
    e.undo().unwrap();
    e.undo().unwrap();
    assert!(e.document().model.mesh().is_empty());
    e.redo().unwrap();
    assert_eq!(e.document().model.mesh().curve_count(), 1);
}

#[test]
fn arcs_and_pies() {
    let mut e = engine();
    // Quarter arc, radius 1000.
    let out = run(
        &mut e,
        "arc-2point",
        json!({ "center": [0,0,0], "start": [1000,0,0], "end": [0,1000,0], "segments": 6 }),
    );
    assert_eq!(out["edges"].as_array().unwrap().len(), 6);
    assert!(out["face"].is_null());
    let mesh = e.document().model.mesh();
    for (_, v) in mesh.vertices() {
        assert!((v.position.length() - 1000.0).abs() < 1e-9);
    }
    // Semicircle by bulge, closed by its chord: one face with N+1 edges.
    let out = run(
        &mut e,
        "arc-bulge",
        json!({ "start": [3000,0,0], "end": [5000,0,0], "bulge": 1000.0, "segments": 8 }),
    );
    assert_eq!(out["edges"].as_array().unwrap().len(), 8);
    let out = run(&mut e, "line", json!({ "points": [[3000,0,0],[5000,0,0]] }));
    let face = out["faces_created"][0].clone();
    let half_disc = std::f64::consts::PI * 1e6 / 2.0;
    assert!((face_area(&e, &face) - half_disc).abs() / half_disc < 0.03);
    // Pie of 90°, 12 segments: arc + 2 radii, one face.
    let out = run(
        &mut e,
        "pie",
        json!({ "center": [0,5000,0], "start": [1000,5000,0], "end_angle": std::f64::consts::FRAC_PI_2 }),
    );
    assert_eq!(out["edges"].as_array().unwrap().len(), 14);
    assert!(out["face"].is_u64());
    // Three-point arc through the top of a circle.
    let out = run(
        &mut e,
        "arc-3point",
        json!({ "a": [-1000,9000,0], "b": [0,10000,0], "c": [1000,9000,0], "segments": 4 }),
    );
    assert_eq!(out["edges"].as_array().unwrap().len(), 4);
    assert!(
        e.document()
            .model
            .mesh()
            .vertices()
            .any(|(_, v)| v.position.distance(DVec3::new(0.0, 10000.0, 0.0)) < 1e-6)
    );
    assert_valid(&e);
    let err = e
        .execute_erased(
            "arc-3point",
            json!({ "a": [0,0,0], "b": [1,0,0], "c": [2,0,0] }),
        )
        .unwrap_err();
    assert_eq!(err.code(), "E1001");
}

#[test]
fn builtins_expose_schemas_with_semantic_types() {
    let reg = CommandRegistry::with_builtins();
    for name in [
        "line",
        "rectangle",
        "circle",
        "polygon",
        "arc-2point",
        "arc-3point",
        "arc-bulge",
        "pie",
    ] {
        assert!(reg.contains(name), "{name}");
    }
    let schema = serde_json::to_value(reg.get("circle").unwrap().params_schema()).unwrap();
    assert_eq!(
        schema["properties"]["center"]["$ref"],
        json!("#/$defs/Point")
    );
    assert_eq!(
        schema["properties"]["radius"]["$ref"],
        json!("#/$defs/Length")
    );
    assert_eq!(schema["properties"]["segments"]["default"], json!(24));
}
