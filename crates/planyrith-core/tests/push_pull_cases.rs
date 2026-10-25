//! docs/03 §4.9 and Decisions (Extrude cases): push/pull where the drawn
//! shape touches the boundary of its face (notch, step, groove, corner, door
//! recess), joins onto a coplanar face, or would invert the solid. Every
//! result is a closed solid of the expected volume, or a clean refusal.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use planyrith_core::{CommandRegistry, Document, Engine};
use serde_json::{Value, json};

fn engine() -> Engine {
    Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
}

fn run(e: &mut Engine, name: &str, params: Value) -> Value {
    e.execute_erased(name, params)
        .unwrap_or_else(|err| panic!("{name}: {err}"))
}

fn rect(e: &mut Engine, a: [f64; 3], b: [f64; 3]) {
    run(e, "rectangle", json!({ "corner_a": a, "corner_b": b }));
}

fn pull_last(e: &mut Engine, d: f64) -> Value {
    run(
        e,
        "push-pull",
        json!({ "face": { "Query": "face & last" }, "distance": d }),
    )
}

fn cube(e: &mut Engine, size: f64) {
    rect(e, [0.0, 0.0, 0.0], [size, size, 0.0]);
    pull_last(e, size);
}

/// The model is one closed solid of `volume` mm³ (relative tolerance 1e-9)
/// and validates clean.
fn assert_solid(e: &Engine, volume: f64, what: &str) {
    let mesh = e.document().model.mesh();
    let problems = mesh.validate(e.document().tolerances());
    assert!(problems.is_empty(), "{what}: {problems:?}");
    assert!(mesh.is_solid(), "{what}: the result is not closed");
    let v = mesh.volume().unwrap();
    assert!(
        (v - volume).abs() <= 1e-9 * volume,
        "{what}: volume {v}, expected {volume}"
    );
}

#[test]
fn a_step_cut_along_a_whole_edge() {
    let mut e = engine();
    cube(&mut e, 1000.0);
    rect(&mut e, [0.0, 0.0, 1000.0], [1000.0, 200.0, 1000.0]);
    pull_last(&mut e, -300.0);
    assert_solid(&e, 1e9 - 1000.0 * 200.0 * 300.0, "step");
}

#[test]
fn notches_grooves_and_corners_at_the_face_boundary() {
    let cases: [(&str, [f64; 2], [f64; 2], f64); 5] = [
        ("front notch", [30.0, 0.0], [60.0, 20.0], -30.0),
        ("groove", [30.0, 0.0], [60.0, 100.0], -30.0),
        ("corner", [0.0, 0.0], [30.0, 20.0], -30.0),
        ("corner through", [0.0, 0.0], [30.0, 20.0], -100.0),
        ("pulled notch", [30.0, 0.0], [60.0, 20.0], 30.0),
    ];
    for (what, a, b, d) in cases {
        let mut e = engine();
        cube(&mut e, 100.0);
        rect(&mut e, [a[0], a[1], 100.0], [b[0], b[1], 100.0]);
        pull_last(&mut e, d);
        let prism = (b[0] - a[0]) * (b[1] - a[1]) * d;
        assert_solid(&e, 1e6 + prism, what);
    }
}

#[test]
fn two_adjacent_rectangles_pushed_in_turn() {
    let mut e = engine();
    cube(&mut e, 100.0);
    let first = run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [20.0, 20.0, 100.0], "corner_b": [50.0, 60.0, 100.0] }),
    )["face"]
        .clone();
    let second = run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [50.0, 20.0, 100.0], "corner_b": [80.0, 60.0, 100.0] }),
    )["face"]
        .clone();
    for face in [first, second] {
        run(
            &mut e,
            "push-pull",
            json!({ "face": { "Pid": face }, "distance": -30.0 }),
        );
    }
    assert_solid(&e, 1e6 - 2.0 * 30.0 * 40.0 * 30.0, "two pockets");
}

#[test]
fn a_door_recess_at_the_floor() {
    let mut e = engine();
    rect(&mut e, [0.0, 0.0, 0.0], [4000.0, 200.0, 0.0]);
    pull_last(&mut e, 2600.0);
    rect(&mut e, [1000.0, 0.0, 0.0], [1900.0, 0.0, 2100.0]);
    pull_last(&mut e, -100.0);
    assert_solid(
        &e,
        4000.0 * 200.0 * 2600.0 - 900.0 * 2100.0 * 100.0,
        "door recess",
    );
}

#[test]
fn stretching_onto_a_coplanar_face_joins_the_volumes() {
    // Same outline: the two boxes become one.
    let mut e = engine();
    cube(&mut e, 1000.0);
    rect(&mut e, [0.0, 0.0, 1500.0], [1000.0, 1000.0, 1500.0]);
    pull_last(&mut e, 1000.0);
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face & normal:z & plane:xy@1000" }, "distance": 500.0 }),
    );
    assert_solid(&e, 1000.0 * 1000.0 * 2500.0, "stacked boxes");
    assert_eq!(e.document().model.mesh().face_count(), 6);

    // Smaller outline landing inside a larger face.
    let mut e = engine();
    rect(&mut e, [250.0, 250.0, 0.0], [750.0, 750.0, 0.0]);
    pull_last(&mut e, 1000.0);
    rect(&mut e, [0.0, 0.0, 1500.0], [1000.0, 1000.0, 1500.0]);
    pull_last(&mut e, 1000.0);
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face & normal:z & plane:xy@1000" }, "distance": 500.0 }),
    );
    assert_solid(&e, 500.0 * 500.0 * 1500.0 + 1e9, "post into slab");
}

#[test]
fn pushing_a_box_top_past_its_bottom_is_refused() {
    for d in [-1000.0, -1500.0] {
        let mut e = engine();
        cube(&mut e, 1000.0);
        let err = e
            .execute_erased(
                "push-pull",
                json!({ "face": { "Query": "face & normal:z & plane:xy@1000" }, "distance": d }),
            )
            .expect_err("an inverting push must be refused");
        assert_eq!(err.code(), "E1301", "{d}: {err}");
        assert_solid(&e, 1e9, "unchanged box");
    }
}

#[test]
fn a_landing_face_with_a_gap_under_the_cap_does_not_join() {
    // A U-shaped face above the box: its notch lies under the cap outline,
    // so it does not cover the cap and the pull is an ordinary extrusion.
    let mut e = engine();
    rect(&mut e, [0.0, 0.0, 0.0], [300.0, 300.0, 0.0]);
    pull_last(&mut e, 100.0);
    rect(&mut e, [50.0, 50.0, 100.0], [250.0, 250.0, 100.0]);
    let points = [
        [0.0, 0.0],
        [100.0, 0.0],
        [100.0, 250.0],
        [200.0, 250.0],
        [200.0, 0.0],
        [300.0, 0.0],
        [300.0, 300.0],
        [0.0, 300.0],
    ];
    let points: Vec<[f64; 3]> = points.iter().map(|p| [p[0], p[1], 200.0]).collect();
    run(&mut e, "line", json!({ "points": points, "close": true }));
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face & normal:z & plane:xy@100 & area<50000" }, "distance": 50.0 }),
    );
    let mesh = e.document().model.mesh();
    assert!(mesh.validate(e.document().tolerances()).is_empty());
    // The box with its 50 mm column stays closed: the only border edges are
    // the eight of the untouched U face.
    let border = mesh
        .edges()
        .filter(|(edge, _)| {
            mesh.edge_faces(*edge)
                .unwrap()
                .iter()
                .filter(|f| f.is_some())
                .count()
                == 1
        })
        .count();
    assert_eq!(border, 8);
}
