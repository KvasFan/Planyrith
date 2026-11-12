//! Implements docs/05 §8.18–§8.19 acceptance: source preservation, context
//! coordinates, native round trips and failure rollback through the registry.

#![allow(clippy::unwrap_used)]

use glam::DVec3;
use planyrith_core::commands::{CommandRegistry, Engine};
use planyrith_core::document::{Document, EditContext, EntityKey, InstancePath};
use planyrith_core::ids::Pid;
use planyrith_core::io::NoProgress;
use planyrith_core::io::native::{WriteOptions, read_document, write_document};
use serde_json::{Value, json};
use std::sync::Arc;

fn engine() -> Engine {
    Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
}
fn run(e: &mut Engine, name: &str, p: Value) -> Value {
    e.execute_erased(name, p).unwrap()
}
fn bytes(doc: &Document) -> Vec<u8> {
    write_document(
        doc,
        &[],
        &WriteOptions {
            deterministic: true,
            include_history: false,
            ..Default::default()
        },
        &NoProgress,
    )
    .unwrap()
}
fn block(e: &mut Engine) {
    run(
        e,
        "rectangle",
        json!({"corner_a": [0,0,0], "corner_b": [100,60,0]}),
    );
    run(
        e,
        "push-pull",
        json!({"face": {"Query": "face & last"}, "distance": 40}),
    );
    run(
        e,
        "make-group",
        json!({"targets": "edge|face", "name": "Part"}),
    );
}

#[test]
fn mold_source_and_generated_groups_survive_native_round_trip() {
    let mut e = engine();
    block(&mut e);
    let (_, source) = e.document().model.groups().next().unwrap();
    let d = source.definition;
    let before = e
        .document()
        .definition(d)
        .unwrap()
        .entities
        .mesh()
        .to_indexed()
        .0;
    let result = run(
        &mut e,
        "mold-split",
        json!({"body": "name:Part", "point": [50,30,20], "normal": [0,0,1]}),
    );
    assert_eq!(e.document().model.groups().count(), 5);
    assert_eq!(
        before,
        e.document()
            .definition(d)
            .unwrap()
            .entities
            .mesh()
            .to_indexed()
            .0
    );
    assert!((result["parting_length_mm"].as_f64().unwrap() - 320.0).abs() < 1e-6);
    let original = bytes(e.document());
    let loaded = read_document(&original, &NoProgress).unwrap();
    assert_eq!(original, bytes(&loaded.document));
    assert!(planyrith_core::diagnostics::validate(&loaded.document).is_clean());
}

#[test]
fn invalid_requests_leave_no_partial_groups_or_history() {
    let mut e = engine();
    block(&mut e);
    for (name, params) in [
        (
            "mold-split",
            json!({"body": "name:Part", "point": [0,0,100], "normal": [0,0,1]}),
        ),
        (
            "mold-split",
            json!({"body": "name:Part", "point": [0,0,20], "normal": [0,0,0]}),
        ),
        ("route", json!({"points": [[0,0,0],[10,0,0],[10,10,0]]})),
        (
            "route",
            json!({"points": [[0,0,0],[100,0,0]], "path": "edge"}),
        ),
        ("route", json!({"points": [[0,0,0],[100,0,0]], "wall": 2})),
        (
            "route-fitting",
            json!({"kind": "reducer", "size": "32x2", "outlet_diameter": 3}),
        ),
        (
            "route-fitting",
            json!({"kind": "coupling", "axis": [0,0,0]}),
        ),
    ] {
        let before = bytes(e.document());
        let history = e.history().applied();
        assert!(e.execute_erased(name, params).is_err(), "{name}");
        assert_eq!(before, bytes(e.document()), "{name}");
        assert_eq!(history, e.history().applied());
    }
}

#[test]
fn selected_paths_keep_edges_and_reject_branches() {
    let mut e = engine();
    run(
        &mut e,
        "line",
        json!({"points": [[0,0,0],[100,0,0],[100,100,0]]}),
    );
    let result = run(
        &mut e,
        "route",
        json!({"path": "edge", "segments": 12, "bend_segments": 4}),
    );
    assert_eq!(result["elbows"], 1);
    assert_eq!(e.document().model.mesh().edges().count(), 2);
    run(&mut e, "line", json!({"points": [[100,0,0],[100,0,100]]}));
    assert!(e.execute_erased("route", json!({"path": "edge"})).is_err());
    let data = bytes(e.document());
    assert_eq!(
        data,
        bytes(&read_document(&data, &NoProgress).unwrap().document)
    );
}

#[test]
fn world_dimensions_inside_a_transformed_context() {
    let mut e = engine();
    block(&mut e);
    run(
        &mut e,
        "scale",
        json!({"targets": "group", "factors": [2,2,2]}),
    );
    run(
        &mut e,
        "move",
        json!({"targets": "group", "delta": [1000,0,0]}),
    );
    let (g, _) = e.document().model.groups().next().unwrap();
    let pid = e.document().model.pid_of(g.into()).unwrap();
    e.set_context(EditContext {
        path: InstancePath::default().child(EntityKey::Pid(pid)),
    })
    .unwrap();
    let result = run(
        &mut e,
        "route",
        json!({"points": [[1200,0,0],[1300,0,0]], "size": "20x1.5"}),
    );
    assert!((result["length_mm"].as_f64().unwrap() - 100.0).abs() < 1e-6);
    let pid = Pid(result["group"].as_u64().unwrap());
    let (c, id) = e.document().locate_pid(pid).unwrap();
    let definition = e
        .document()
        .container(c)
        .unwrap()
        .definition_of(id)
        .unwrap();
    let mesh = e.document().definition(definition).unwrap().entities.mesh();
    let local = mesh.aabb();
    let (_, world) = e.document().resolve_container(&e.context().path).unwrap();
    assert!((world.transform_point3(local.min).x - 1200.0).abs() < 1e-6);
    assert!(
        (world
            .transform_vector3(DVec3::X * (local.max.x - local.min.x))
            .length()
            - 100.0)
            .abs()
            < 1e-6
    );
    run(
        &mut e,
        "make-group",
        json!({"targets": "edge|face", "name": "Inner"}),
    );
    let point = world.transform_point3(DVec3::new(50.0, 30.0, 20.0));
    let result = run(
        &mut e,
        "mold-split",
        json!({"body": "name:Inner", "point": point.to_array(), "normal": [0,0,1]}),
    );
    assert!(
        (result["compensated_volume_mm3"].as_f64().unwrap() - 100.0 * 60.0 * 40.0 * 8.0).abs()
            < 1e-4
    );
}
