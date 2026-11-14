//! Acceptance for docs/13 milestones 1.23–1.29 and 1.32.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use glam::DVec3;
use planyrith_core::{
    commands::{CommandRegistry, Engine},
    document::{Document, EditContext, Selection},
    io::{
        NoProgress,
        native::{WriteOptions, read_document, write_document},
    },
};
use serde_json::{Value, json};
use std::sync::Arc;

fn engine() -> Engine {
    Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
}
fn run(e: &mut Engine, name: &str, params: Value) -> Value {
    e.execute_erased(name, params).unwrap()
}
fn bytes(e: &Engine) -> Vec<u8> {
    write_document(
        e.document(),
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
fn cube(e: &mut Engine, name: &str, x: f64) {
    run(
        e,
        "rectangle",
        json!({"corner_a":[x,0,0],"corner_b":[x+100.0,100,0]}),
    );
    run(
        e,
        "push-pull",
        json!({"face":{"Query":"face & last"},"distance":100}),
    );
    run(e, "make-group", json!({"targets":"edge|face","name":name}));
}
fn overlay(e: &Engine) -> planyrith_core::io::overlay::Overlay {
    let items = planyrith_core::io::render_mesh::draw_list(
        e.document(),
        &Selection::new(),
        &EditContext::model(),
        false,
    );
    planyrith_core::io::overlay::overlay(e.document(), &items)
}

#[test]
fn rotate_and_scale_check_only_changed_occurrences_and_undo() {
    let mut e = engine();
    cube(&mut e, "A", 0.0);
    cube(&mut e, "B", 200.0);
    let initial = bytes(&e);
    for (name, params) in [
        (
            "rotate",
            json!({"targets":"name:B & group","center":[125,50,0],"angle":std::f64::consts::PI,"check_collisions":true}),
        ),
        (
            "scale",
            json!({"targets":"name:B & group","pivot":[300,0,0],"factors":[2.5,1,1],"check_collisions":true}),
        ),
        (
            "scale",
            json!({"targets":"name:B & group","pivot":[175,0,0],"factors":[-1,1,1],"check_collisions":true}),
        ),
    ] {
        let output = run(&mut e, name, params);
        let pairs = output["interference"]["interferences"].as_array().unwrap();
        assert_eq!(pairs.len(), 1, "{output}");
        assert!(pairs[0]["volume_mm3"].as_f64().unwrap() > 499_999.0);
        let full = run(&mut e, "interference-check", json!({}));
        assert_eq!(output["interference"], full);
        e.undo().unwrap();
        assert_eq!(bytes(&e), initial);
    }
    let output = run(
        &mut e,
        "rotate",
        json!({"targets":"name:B & group","center":[125,50,0],"angle":std::f64::consts::PI,"copy":true,"check_collisions":true}),
    );
    assert_eq!(output["copies"].as_array().unwrap().len(), 1);
    assert_eq!(
        output["interference"]["interferences"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn scenes_restore_exploded_placements_with_native_roundtrip_and_undo() {
    let mut e = engine();
    cube(&mut e, "A", 0.0);
    cube(&mut e, "B", 200.0);
    run(
        &mut e,
        "scene-add",
        json!({"name":"Collapsed","capture":{"placements":true}}),
    );
    run(&mut e, "explode-view", json!({"factor":2}));
    run(
        &mut e,
        "scene-add",
        json!({"name":"Exploded","capture":{"placements":true}}),
    );
    let exploded = bytes(&e);
    run(&mut e, "scene-activate", json!({"scene":"Collapsed"}));
    let bounds = e.document().aabb();
    assert!((bounds.min.x).abs() < 1e-6 && (bounds.max.x - 300.0).abs() < 1e-6);
    e.undo().unwrap();
    assert_eq!(bytes(&e), exploded);
    // Typed commands also work without registering the built-ins first.
    let mut typed = Engine::new(e.document().clone(), Arc::new(CommandRegistry::new()));
    typed
        .execute::<planyrith_core::commands::organize::SceneActivate>(
            planyrith_core::commands::organize::SceneActivateParams {
                common: Default::default(),
                scene: "Collapsed".into(),
                no_transition: true,
            },
        )
        .unwrap();
    typed.undo().unwrap();
    assert_eq!(bytes(&typed), exploded);
    let loaded = read_document(&exploded, &NoProgress).unwrap();
    let mut loaded = Engine::new(loaded.document, Arc::new(CommandRegistry::with_builtins()));
    assert!(!loaded.command_is_view_only("scene-activate", &json!({"scene":"Collapsed"})));
    run(&mut loaded, "scene-activate", json!({"scene":"Collapsed"}));
    assert!((loaded.document().aabb().max.x - 300.0).abs() < 1e-6);
    run(&mut loaded, "lock", json!({"targets":"name:B & group"}));
    let locked = bytes(&loaded);
    assert!(
        loaded
            .execute_erased("scene-activate", json!({"scene":"Exploded"}))
            .is_err()
    );
    assert_eq!(bytes(&loaded), locked, "a failed restore is atomic");
}

#[test]
fn dimension_extension_style_changes_lines_without_changing_measurement() {
    let mut e = engine();
    run(
        &mut e,
        "dimension",
        json!({"start":[0,0,0],"end":[100,0,0],"offset":[0,20,0],"kind":"aligned"}),
    );
    run(
        &mut e,
        "label-style",
        json!({"targets":"dimension","arrow":"none","extension_gap":5,"extension_overshoot":3}),
    );
    let out = overlay(&e);
    assert_eq!(out.lines.len(), 3);
    assert!(out.lines[0].a.distance(DVec3::new(0.0, 5.0, 0.0)) < 1e-6);
    assert!(out.lines[0].b.distance(DVec3::new(0.0, 23.0, 0.0)) < 1e-6);
    let before = bytes(&e);
    let loaded = read_document(&before, &NoProgress).unwrap();
    let loaded = Engine::new(loaded.document, Arc::new(CommandRegistry::with_builtins()));
    assert_eq!(overlay(&loaded), out);
    assert!(
        e.execute_erased(
            "label-style",
            json!({"targets":"dimension","extension_gap":-1})
        )
        .is_err()
    );
    assert_eq!(before, bytes(&e));
    run(
        &mut e,
        "label-style",
        json!({"targets":"dimension","extension_visible":false}),
    );
    let hidden = overlay(&e);
    assert_eq!(hidden.lines.len(), 1);
    assert_eq!(hidden.labels[0].text, out.labels[0].text);
    e.undo().unwrap();
    assert_eq!(bytes(&e), before);
}

#[test]
fn text_leader_modes_preserve_anchors_and_validate_pushpin_offsets() {
    let mut e = engine();
    run(&mut e, "line", json!({"points":[[0,0,0],[100,0,0]]}));
    run(
        &mut e,
        "text",
        json!({"at":[0,0,0],"text":"Anchor","screen_offset":[40,40]}),
    );
    let initial = bytes(&e);
    assert!(
        e.execute_erased(
            "label-style",
            json!({"targets":"text","leader_mode":"pushpin"})
        )
        .is_err()
    );
    assert_eq!(initial, bytes(&e));
    run(
        &mut e,
        "label-style",
        json!({"targets":"text","leader_mode":"pushpin","leader_offset":[10,20,30]}),
    );
    let pinned = overlay(&e);
    assert_eq!(
        pinned.labels[0].layout.as_ref().unwrap().world_offset,
        Some(DVec3::new(10.0, 20.0, 30.0))
    );
    run(
        &mut e,
        "label-style",
        json!({"targets":"text","leader_mode":"view"}),
    );
    let view = overlay(&e);
    assert!(
        view.labels[0]
            .layout
            .as_ref()
            .unwrap()
            .world_offset
            .is_none()
    );
    assert_eq!(view.labels[0].at, pinned.labels[0].at);
    assert_eq!(view.labels[0].text, pinned.labels[0].text);
    e.undo().unwrap();
    assert_eq!(overlay(&e), pinned);
}

#[test]
fn hollow_lofts_keep_open_cavities_and_reject_mismatches() {
    use planyrith_core::geom::{FaceSides, loft::loft_with_holes};
    let square = |z: f64, r: f64| {
        vec![
            DVec3::new(-r, -r, z),
            DVec3::new(r, -r, z),
            DVec3::new(r, r, z),
            DVec3::new(-r, r, z),
        ]
    };
    let profiles = vec![
        vec![square(0.0, 10.0), square(0.0, 5.0)],
        vec![square(20.0, 10.0), square(20.0, 5.0)],
    ];
    let tol = Default::default();
    let solid = loft_with_holes(&profiles, FaceSides::default(), &tol).unwrap();
    assert!(solid.is_solid());
    assert!((solid.volume().unwrap() - 6000.0).abs() < 1e-6);
    let mut bad = profiles.clone();
    bad[1].pop();
    assert!(loft_with_holes(&bad, FaceSides::default(), &tol).is_err());
    let mut bad = profiles;
    for p in &mut bad {
        for v in &mut p[1] {
            v.x += 15.0;
        }
    }
    assert!(loft_with_holes(&bad, FaceSides::default(), &tol).is_err());
    let mut e = engine();
    for z in [0, 200] {
        run(
            &mut e,
            "rectangle",
            json!({"corner_a":[0,0,z],"corner_b":[100,100,z]}),
        );
        run(
            &mut e,
            "rectangle",
            json!({"corner_a":[25,25,z],"corner_b":[75,75,z]}),
        );
        run(&mut e, "delete", json!({"targets":"face & area<3000"}));
    }
    let output = run(
        &mut e,
        "loft",
        json!({"profiles":["face & plane:xy@0","face & plane:xy@200"]}),
    );
    assert!((output["volume_mm3"].as_f64().unwrap() - 1_500_000.0).abs() < 1e-3);
    e.undo().unwrap();
    assert_eq!(e.document().model.mesh().face_count(), 2);
}

#[test]
fn camera_direction_and_height_preserve_view_only_history() {
    let mut e = engine();
    run(&mut e, "line", json!({"points":[[0,0,0],[100,0,0]]}));
    let output = run(
        &mut e,
        "position-camera",
        json!({"eye":[10,20,0],"eye_height":1600,"look_dir":[0,2,0],"target_distance":500}),
    );
    assert_eq!(output["camera"]["eye"], json!([10.0, 20.0, 1600.0]));
    assert_eq!(output["camera"]["target"], json!([10.0, 520.0, 1600.0]));
    for bad in [
        json!({"eye":[0,0,0],"look_dir":[0,0,0]}),
        json!({"eye":[0,0,0],"look_dir":[1,0,0],"target":[1,0,0]}),
        json!({"eye":[0,0,0],"look_dir":[1,0,0],"target_distance":-1}),
    ] {
        let before = bytes(&e);
        assert!(e.execute_erased("position-camera", bad).is_err());
        assert_eq!(bytes(&e), before);
    }
    e.undo().unwrap();
    assert_eq!(
        e.document().model.mesh().edge_count(),
        0,
        "camera adds no undo step"
    );
}

#[test]
fn autosave_interval_changes_keep_pending_rows_and_recovery() {
    use planyrith_core::io::storage::Storage;
    use planyrith_core::io::{
        autosave::{Autosave, RowState},
        storage::{MemoryStorage, StorageKey},
    };
    let storage = Arc::new(MemoryStorage::new());
    let key = StorageKey::new("work.plyr");
    storage.write(&key, &bytes(&engine())).unwrap();
    let mut save = Autosave::new(key.clone(), 0.0);
    save.record(
        storage.as_ref(),
        "line",
        &json!({"points":[[0,0,0],[100,0,0]]}),
        None,
        RowState::default(),
    )
    .unwrap();
    assert!(!save.due(119.0));
    assert!(save.due(120.0));
    save.set_interval_seconds(300.0).unwrap();
    assert!(!save.due(120.0));
    assert_eq!(save.journaled(), 1);
    assert!(save.set_interval_seconds(f64::NAN).is_err());
    assert!(save.set_interval_seconds(0.0).is_err());
    assert!(!save.due(299.0));
    assert!(save.due(300.0));
    save.set_interval_seconds(10.0).unwrap();
    assert!(save.due(10.0));
    assert_eq!(save.journaled(), 1);
    let recovered = planyrith_core::io::autosave::recover(
        storage,
        &key,
        Arc::new(CommandRegistry::with_builtins()),
    )
    .unwrap();
    assert_eq!(recovered.replayed, 1);
    assert!(recovered.skipped.is_empty());
    assert_eq!(recovered.engine.document().model.mesh().edge_count(), 1);
}

#[test]
fn preview_marks_only_changed_faces_in_a_shared_root_container() {
    use planyrith_core::io::render_mesh::preview_changed_faces;
    let mut e = engine();
    run(
        &mut e,
        "rectangle",
        json!({"corner_a":[-200,-100,0],"corner_b":[-20,100,0]}),
    );
    run(
        &mut e,
        "rectangle",
        json!({"corner_a":[20,-100,0],"corner_b":[200,100,0]}),
    );
    let before = e.document().clone();
    assert!(preview_changed_faces(&before, &before).is_empty());
    run(
        &mut e,
        "move",
        json!({"targets":"face & last","delta":[0,0,50]}),
    );
    let changed = preview_changed_faces(&before, e.document());
    assert_eq!(changed.len(), 1);
    assert_eq!(
        changed.values().next().unwrap().len(),
        1,
        "only the moved face is ghosted"
    );
    let saved = bytes(&e);
    let loaded = read_document(&saved, &NoProgress).unwrap();
    assert!(
        preview_changed_faces(e.document(), &loaded.document).is_empty(),
        "native id rebuilding does not create ghost differences"
    );
}

#[test]
fn captured_nested_shared_occurrences_restore_and_deleted_paths_are_ignored() {
    use planyrith_core::document::{EntityKey, InstancePath};
    use planyrith_core::ids::Pid;
    let mut e = engine();
    cube(&mut e, "Inner", 0.0);
    let assembly = run(
        &mut e,
        "make-component",
        json!({"targets":"group","name":"Assembly"}),
    );
    let pid = Pid(assembly["instance"].as_u64().unwrap());
    run(
        &mut e,
        "move",
        json!({"targets":"instance","delta":[300,0,0],"copy":true}),
    );
    run(
        &mut e,
        "scene-add",
        json!({"name":"Nested","capture":{"placements":true}}),
    );
    e.set_context(EditContext {
        path: InstancePath {
            steps: vec![EntityKey::Pid(pid)],
        },
    })
    .unwrap();
    run(&mut e, "move", json!({"targets":"group","delta":[100,0,0]}));
    e.set_context(EditContext::model()).unwrap();
    let displaced = bytes(&e);
    run(&mut e, "scene-activate", json!({"scene":"Nested"}));
    assert!(e.document().aabb().min.x.abs() < 1e-6);
    assert!((e.document().aabb().max.x - 400.0).abs() < 1e-6);
    e.undo().unwrap();
    assert_eq!(bytes(&e), displaced);
    run(&mut e, "delete", json!({"targets":format!("@{}",pid.0)}));
    run(&mut e, "scene-activate", json!({"scene":"Nested"}));
    assert_eq!(e.document().model.instances().count(), 1);
    assert!((e.document().aabb().min.x - 300.0).abs() < 1e-6);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(24))]
    #[test]
    fn hollow_prismatic_loft_volume_is_outer_minus_inner(o in 5u32..100, fraction in 1u32..9, h in 10u32..100) {
        use planyrith_core::geom::{loft::loft_with_holes,FaceSides};
        let outer=f64::from(o);let inner=outer*f64::from(fraction)/10.0;let height=f64::from(h);
        let square=|z:f64,r:f64|vec![DVec3::new(-r,-r,z),DVec3::new(r,-r,z),DVec3::new(r,r,z),DVec3::new(-r,r,z)];
        let profiles=vec![vec![square(0.0,outer),square(0.0,inner)],vec![square(height,outer),square(height,inner)]];
        let solid=loft_with_holes(&profiles,FaceSides::default(),&Default::default()).unwrap();
        proptest::prop_assert!(solid.is_solid());
        proptest::prop_assert!((solid.volume().unwrap()-4.0*(outer*outer-inner*inner)*height).abs()<1e-5);
    }
}
