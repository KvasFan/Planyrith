//! docs/12 §2 undo/redo determinism: for every 0.1 command, apply → undo →
//! the saved `.plyr` bytes equal the original's, redo → equal the
//! post-apply bytes. The pid counter only moves forward (docs/04 §2), so
//! both sides are written with the same counter (docs/12 Decisions).

use std::sync::Arc;

use serde_json::{Value, json};

use crate::commands::{CommandRegistry, Engine};
use crate::document::Document;
use crate::io::NoProgress;
use crate::io::native::{WriteOptions, write_document};

fn bytes(doc: &Document, next_pid: crate::ids::PidAllocator) -> Vec<u8> {
    let mut doc = doc.clone();
    doc.info.next_pid = next_pid;
    let opts = WriteOptions {
        deterministic: true,
        include_history: false,
        ..WriteOptions::default()
    };
    write_document(&doc, &[], &opts, &NoProgress).unwrap()
}

/// Runs `steps` in order; checks undo/redo byte equality around each one.
fn check(steps: &[(&str, Value)]) {
    check_in(
        Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins())),
        steps,
    );
}

fn check_in(mut e: Engine, steps: &[(&str, Value)]) {
    for (name, params) in steps {
        let before_doc = e.document().clone();
        let applied = e.history().applied();
        e.execute_erased(name, params.clone())
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        let counter = e.document().info.next_pid;
        assert!(
            e.history().applied() == applied + 1,
            "`{name}` changed nothing; the test step is meaningless"
        );
        let after = bytes(e.document(), counter);
        e.undo().unwrap_or_else(|err| panic!("undo {name}: {err}"));
        assert!(
            bytes(e.document(), counter) == bytes(&before_doc, counter),
            "undo of `{name}` does not restore the document"
        );
        e.redo().unwrap_or_else(|err| panic!("redo {name}: {err}"));
        assert!(
            bytes(e.document(), counter) == after,
            "redo of `{name}` differs"
        );
    }
}

#[test]
fn drawing_and_push_pull() {
    check(&[
        (
            "rectangle",
            json!({ "corner_a": [0,0,0], "corner_b": [1000,600,0] }),
        ),
        ("line", json!({ "points": [[400,0,0],[400,600,0]] })),
        (
            "push-pull",
            json!({ "face": { "Query": "face & area<300000" }, "distance": 300.0 }),
        ),
        (
            "push-pull",
            json!({ "face": { "Query": "face & plane:xy@300" }, "distance": 100.0 }),
        ),
        (
            "circle",
            json!({ "center": [2000,0,0], "radius": 200.0, "segments": 12 }),
        ),
        (
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 500.0 }),
        ),
        (
            "arc-bulge",
            json!({ "start": [3000,0,0], "end": [3500,0,0], "bulge": 100.0 }),
        ),
        (
            "polygon",
            json!({ "center": [5000,0,0], "radius": 300.0, "sides": 5 }),
        ),
        (
            "offset",
            json!({ "face": { "Query": "face & last" }, "distance": -50.0 }),
        ),
    ]);
}

/// docs/05 §8.18–§8.19: mold groups and hollow routing undo as one action.
#[test]
fn molds_and_routing() {
    check(&[
        (
            "rectangle",
            json!({"corner_a": [0,0,0], "corner_b": [100,60,0]}),
        ),
        (
            "push-pull",
            json!({"face": {"Query": "face & last"}, "distance": 40}),
        ),
        (
            "make-group",
            json!({"targets": "edge|face", "name": "Part"}),
        ),
        (
            "mold-split",
            json!({"body": "name:Part", "point": [50,30,20], "normal": [0,0,1], "shrink_factor": 1.02}),
        ),
        (
            "route",
            json!({"points": [[200,0,0],[300,0,0],[300,100,0]], "segments": 12, "bend_segments": 4}),
        ),
        (
            "route-fitting",
            json!({"kind": "reducer", "size": "32x2", "outlet_diameter": 20, "position": [400,0,0]}),
        ),
    ]);
}

#[test]
fn freehand_and_rotated_rectangle_restore_exact_bytes() {
    check(&[
        (
            "freehand",
            json!({"points":[[0,0,0],[500,0.1,0],[1000,0,0],[1000,1000,0],[0,1000,0],[0,0,0]]}),
        ),
        (
            "rotated-rectangle",
            json!({"origin":[3000,0,0],"edge_end":[3500,500,0],"width":1000}),
        ),
    ]);
}

#[test]
fn solid_tools_restore_exact_bytes() {
    for name in [
        "solid-union",
        "solid-subtract",
        "solid-intersect",
        "solid-trim",
        "solid-split",
        "outer-shell",
    ] {
        check(&[
            (
                "rectangle",
                json!({"corner_a":[0,0,0],"corner_b":[1000,1000,0]}),
            ),
            (
                "push-pull",
                json!({"face":{"Query":"face"},"distance":1000}),
            ),
            ("make-group", json!({"targets":"all","name":"A"})),
            (
                "rectangle",
                json!({"corner_a":[500,500,500],"corner_b":[1500,1500,500]}),
            ),
            (
                "push-pull",
                json!({"face":{"Query":"face"},"distance":1000}),
            ),
            ("make-group", json!({"targets":"edge | face","name":"B"})),
            (name, json!({"a":"name:A","b":"name:B"})),
        ]);
    }
}

#[test]
fn sweep_intersection_and_softening_restore_exact_bytes() {
    check(&[
        (
            "circle",
            json!({"center":[0,0,0],"normal":[1,0,0],"radius":100,"segments":12}),
        ),
        ("line", json!({"points":[[0,0,0],[1000,0,0]]})),
        (
            "sweep",
            json!({"profile":{"Query":"face"},"path":"edge & last"}),
        ),
        (
            "soften-smooth",
            json!({"targets":"edge","angle":2.0,"smooth":true}),
        ),
    ]);
    check(&[
        (
            "rectangle",
            json!({"corner_a":[0,0,0],"corner_b":[1000,1000,0]}),
        ),
        (
            "rectangle",
            json!({"corner_a":[500,200,-500],"corner_b":[500,800,500]}),
        ),
        (
            "intersect-faces",
            json!({"targets":"face","scope":"selection"}),
        ),
    ]);
}

#[cfg(feature = "scripting")]
#[test]
fn scripts_and_attributes_restore_exact_bytes() {
    check(&[
        (
            "script-eval",
            json!({"source":"rectangle(#{corner_a:pt(0,0,0),corner_b:pt(100,100,0)});push_pull(#{face:sel(\"face & last\"),distance:50});"}),
        ),
        (
            "attribute-set",
            json!({"targets":"face","dictionary":"ext.test","key":"kind","value":"panel"}),
        ),
        (
            "attribute-set",
            json!({"targets":"face","dictionary":"ext.test","key":"kind","value":null}),
        ),
    ]);
}

#[test]
fn editing() {
    check(&[
        (
            "rectangle",
            json!({ "corner_a": [0,0,0], "corner_b": [1000,600,0] }),
        ),
        (
            "push-pull",
            json!({ "face": { "Query": "face" }, "distance": 300.0 }),
        ),
        (
            "rectangle",
            json!({ "corner_a": [0,0,300], "corner_b": [200,600,300] }),
        ),
        ("erase", json!({ "targets": "edge & last & length=600" })),
        (
            "move",
            json!({ "targets": "face & plane:xy@300", "delta": [0,0,50] }),
        ),
        (
            "move",
            json!({ "targets": "all", "delta": [2000,0,0], "copy": true, "copies": 2 }),
        ),
        (
            "rotate",
            json!({ "targets": "all", "center": [0,0,0], "angle": 0.5 }),
        ),
        (
            "scale",
            json!({ "targets": "all", "factors": [1.5, 1.0, 0.5] }),
        ),
        ("hide", json!({ "targets": "face & normal:z" })),
        ("delete", json!({ "targets": "face & hidden" })),
        ("erase", json!({ "targets": "edge", "mode": "soften" })),
    ]);
}

#[test]
fn organizing() {
    check(&[
        (
            "rectangle",
            json!({ "corner_a": [0,0,0], "corner_b": [400,400,0] }),
        ),
        (
            "push-pull",
            json!({ "face": { "Query": "face" }, "distance": 400.0 }),
        ),
        ("tag-create", json!({ "name": "Boxes", "color": "#336699" })),
        ("tag-assign", json!({ "targets": "face", "tag": "Boxes" })),
        (
            "material-create",
            json!({ "name": "Oak", "color": "#A0522D" }),
        ),
        (
            "material-apply",
            json!({ "targets": "face & normal:z", "material": "Oak", "fill": "all" }),
        ),
        (
            "make-component",
            json!({ "targets": "all", "name": "Crate" }),
        ),
        (
            "move",
            json!({ "targets": "instance", "delta": [600,0,0], "copy": true, "copies": 3 }),
        ),
        (
            "rectangle",
            json!({ "corner_a": [0,1000,0], "corner_b": [300,1300,0] }),
        ),
        ("make-group", json!({ "targets": "face | edge" })),
        ("explode", json!({ "targets": "group" })),
        (
            "tag-rename",
            json!({ "tag": "Boxes", "new_name": "Crates" }),
        ),
        ("tag-delete", json!({ "tag": "Crates" })),
        ("material-delete", json!({ "material": "Oak" })),
        ("lock", json!({ "targets": "instance" })),
        ("unlock", json!({ "targets": "instance" })),
    ]);
}

#[test]
fn scenes() {
    check(&[
        (
            "rectangle",
            json!({ "corner_a": [0,0,0], "corner_b": [400,400,0] }),
        ),
        ("scene-add", json!({ "name": "Plan" })),
        (
            "tag-create",
            json!({ "name": "Hidden", "color": "#336699" }),
        ),
        (
            "tag-set-visible",
            json!({ "tag": "Hidden", "visible": false }),
        ),
        (
            "scene-add",
            json!({ "name": "Detail", "capture": { "camera": false } }),
        ),
        ("scene-update", json!({ "scene": "Plan" })),
        (
            "scene-rename",
            json!({ "scene": "Plan", "new_name": "Overview" }),
        ),
        ("scene-reorder", json!({ "scene": "Detail", "index": 0 })),
        ("scene-delete", json!({ "scene": "Overview" })),
        ("tag-folder-create", json!({ "name": "Layers" })),
        ("tag-move", json!({ "tag": "Hidden", "folder": "Layers" })),
        (
            "tag-folder-set-visible",
            json!({ "name": "Layers", "visible": false }),
        ),
        ("tag-move", json!({ "tag": "Hidden" })),
    ]);
}

/// Storage with a texture, a component file and a material library, made
/// by another engine (saving is a view command, so it is no step here).
fn prepared_storage() -> Arc<crate::io::MemoryStorage> {
    use crate::io::{Storage, StorageKey};
    let storage = Arc::new(crate::io::MemoryStorage::new());
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        8,
        4,
        image::Rgba([90, 60, 30, 255]),
    ))
    .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
    .unwrap();
    storage.write(&StorageKey::new("wood.png"), &png).unwrap();
    let mut other = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    other.set_storage(storage.clone());
    for (name, params) in [
        (
            "material-create",
            json!({ "name": "Walnut", "color": "#5C4033", "texture": "wood.png" }),
        ),
        (
            "rectangle",
            json!({ "corner_a": [0,0,0], "corner_b": [900,900,0] }),
        ),
        (
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 700.0 }),
        ),
        (
            "material-apply",
            json!({ "targets": "face", "material": "Walnut" }),
        ),
        (
            "make-component",
            json!({ "targets": "all", "name": "Table" }),
        ),
        (
            "component-save-as",
            json!({ "definition": "Table", "path": "table.plyrc" }),
        ),
        ("material-library-save", json!({ "path": "woods.plyrm" })),
    ] {
        other.execute_erased(name, params).unwrap();
    }
    storage
}

#[test]
fn textures_libraries_and_components() {
    let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    e.set_storage(prepared_storage());
    check_in(
        e,
        &[
            (
                "material-create",
                json!({ "name": "Oak", "color": "#A0522D", "texture": "wood.png" }),
            ),
            (
                "material-edit",
                json!({ "material": "Oak", "texture_width": 300.0, "colorize": "shift" }),
            ),
            ("material-library-load", json!({ "path": "woods.plyrm" })),
            (
                "rectangle",
                json!({ "corner_a": [0,0,0], "corner_b": [400,400,0] }),
            ),
            (
                "push-pull",
                json!({ "face": { "Query": "face" }, "distance": 400.0 }),
            ),
            (
                "make-component",
                json!({ "targets": "all", "name": "Stool" }),
            ),
            (
                "move",
                json!({ "targets": "instance", "delta": [600,0,0], "copy": true, "copies": 2 }),
            ),
            ("make-unique", json!({ "targets": "last" })),
            (
                "component-insert",
                json!({ "path": "table.plyrc", "position": [0, 2000, 0] }),
            ),
            (
                "component-replace",
                json!({ "instances": "definition:Stool", "with": "Table" }),
            ),
            (
                "component-reload",
                json!({ "definition": "Table", "path": "table.plyrc" }),
            ),
            (
                "component-rename",
                json!({ "definition": "Table", "new_name": "Desk" }),
            ),
        ],
    );
}

#[test]
fn styles_and_watermarks() {
    let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    e.set_storage(prepared_storage());
    check_in(
        e,
        &[
            ("style-create", json!({ "name": "Sketch" })),
            (
                "style-edit",
                json!({ "profiles": 4, "faces": "monochrome", "sky": "none", "ground": "#808060" }),
            ),
            (
                "style-watermark-add",
                json!({ "image": "wood.png", "placement": "tiled", "behind_model": true }),
            ),
            ("style-watermark-remove", json!({ "index": 0 })),
            (
                "material-create",
                json!({ "name": "Oak", "color": "#A0522D", "texture": "wood.png" }),
            ),
            ("material-delete", json!({ "material": "Oak" })),
            (
                "image-insert",
                json!({ "path": "wood.png", "width": 800.0 }),
            ),
            ("explode", json!({ "targets": "image" })),
            (
                "texture-position",
                json!({ "face": { "Query": "face" }, "side": "both", "rotation": 45.0 }),
            ),
            (
                "style-create",
                json!({ "name": "Other", "activate": false }),
            ),
            ("style-delete", json!({ "style": "Other" })),
        ],
    );
}

#[test]
fn annotations_sections_and_guides() {
    check(&[
        (
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [2000, 1000, 0] }),
        ),
        (
            "push-pull",
            json!({ "face": { "Query": "face" }, "distance": 1000.0 }),
        ),
        (
            "dimension",
            json!({ "start": [0, 0, 0], "end": [2000, 0, 0], "offset": [0, -300, 0] }),
        ),
        ("text", json!({ "at": [2000, 1000, 0] })),
        (
            "label-style",
            json!({"targets":"text | dimension","font":{"family":"Inter","size_pt":16,"bold":true},"arrow":"closed","color":"#318ac0","leader_visible":true}),
        ),
        (
            "tape-measure",
            json!({ "from": [500, 0, 0], "to": [500, -300, 0] }),
        ),
        (
            "protractor",
            json!({ "center": [0, 0, 0], "from_point": [1000, 0, 0], "angle": 0.5 }),
        ),
        ("guide-delete-all", json!({})),
        (
            "section-plane-add",
            json!({ "plane": { "normal": [0, 0, 1], "d": -400.0 } }),
        ),
        (
            "section-plane-reverse",
            json!({ "section": { "Query": "section & last" } }),
        ),
        (
            "section-plane-add",
            json!({ "plane": { "normal": [1, 0, 0], "d": -500.0 } }),
        ),
        (
            "section-plane-toggle",
            json!({ "section": { "Query": "section & last" } }),
        ),
        (
            "section-create-group-from-slice",
            json!({ "plane": { "normal": [0, 0, 1], "d": -400.0 } }),
        ),
        (
            "axes-set",
            json!({ "origin": [100, 0, 0], "x": [0, 1, 0], "y": [-1, 0, 0] }),
        ),
        ("axes-reset", json!({})),
        (
            "text-3d",
            json!({ "text": "Ab", "height": 100.0, "extrude": 10.0 }),
        ),
        (
            "resize-model",
            json!({ "measured": 2000.0, "target": 3000.0 }),
        ),
    ]);
}

fn engine() -> Engine {
    Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
}

fn run(e: &mut Engine, name: &str, params: Value) -> u64 {
    e.execute_erased(name, params)
        .unwrap_or_else(|err| panic!("{name}: {err}"));
    e.history().newest_serial().unwrap()
}

/// docs/08 §3.1 per-person undo: an older step is taken back under a newer
/// one that does not touch it; the result is the document without that
/// step, and reverting the revert brings it back.
#[test]
fn revert_takes_back_an_older_step_that_nothing_newer_touched() {
    let mut e = engine();
    let a = run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [1000,600,0] }),
    );
    let only_a = e.document().clone();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [3000,0,0], "corner_b": [4000,600,0] }),
    );
    let both = e.document().clone();
    let counter = e.document().info.next_pid;

    // The same document drawn without the first rectangle.
    let mut expected = engine();
    run(
        &mut expected,
        "rectangle",
        json!({ "corner_a": [3000,0,0], "corner_b": [4000,600,0] }),
    );

    let undo_a = e.revert(a).unwrap();
    assert_eq!(e.history().undo_label(), Some("Undo Rectangle"));
    assert_eq!(e.document().model.mesh().faces().count(), 1);
    assert_eq!(e.document().model.mesh().edges().count(), 4);
    let geometry = |d: &Document| {
        let mut points: Vec<String> = d
            .model
            .mesh()
            .edges()
            .map(|(id, _)| {
                let s = d.model.mesh().edge_segment(id).unwrap();
                format!("{:?}", [s.a, s.b])
            })
            .collect();
        points.sort();
        points
    };
    assert_eq!(geometry(e.document()), geometry(expected.document()));
    assert!(!crate::diagnostics::validate(e.document()).has_errors());

    // Reverting the revert brings the rectangle back.
    e.revert(undo_a).unwrap();
    assert_eq!(geometry(e.document()), geometry(&both));
    // A shared undo of both reverts restores the document as it was.
    e.undo().unwrap();
    e.undo().unwrap();
    assert!(bytes(e.document(), counter) == bytes(&both, counter));
    e.undo().unwrap();
    assert!(bytes(e.document(), counter) == bytes(&only_a, counter));
}

/// A step that a newer one built on is refused, and nothing changes.
#[test]
fn revert_refuses_a_step_a_newer_one_touched() {
    let mut e = engine();
    let a = run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [1000,600,0] }),
    );
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face" }, "distance": 300.0 }),
    );
    let counter = e.document().info.next_pid;
    let before = bytes(e.document(), counter);
    let applied = e.history().applied();
    let err = e.revert(a).unwrap_err();
    assert!(err.to_string().contains("later change"), "{err}");
    assert!(bytes(e.document(), counter) == before);
    assert_eq!(e.history().applied(), applied);

    // The newest step itself can always be reverted, like an undo.
    let b = e.history().newest_serial().unwrap();
    e.revert(b).unwrap();
    assert_eq!(e.document().model.mesh().faces().count(), 1);

    // A step that was undone is no longer in the history.
    e.undo().unwrap();
    e.undo().unwrap();
    assert!(e.revert(b).is_err());
}

/// Painting two different faces with the same material does not overlap;
/// editing the material does.
#[test]
fn revert_of_painting_depends_on_what_was_painted() {
    let mut e = engine();
    run(
        &mut e,
        "material-create",
        json!({ "name": "Red", "color": "#FF0000" }),
    );
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0,0,0], "corner_b": [1000,600,0] }),
    );
    let paint_a = run(
        &mut e,
        "material-apply",
        json!({ "targets": "face & last", "material": "Red", "side": "both" }),
    );
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [3000,0,0], "corner_b": [4000,600,0] }),
    );
    let paint_b = run(
        &mut e,
        "material-apply",
        json!({ "targets": "face & last", "material": "Red", "side": "both" }),
    );
    e.revert(paint_a).unwrap();
    run(
        &mut e,
        "material-edit",
        json!({ "material": "Red", "color": "#00FF00" }),
    );
    assert!(e.revert(paint_b).is_err());
}
