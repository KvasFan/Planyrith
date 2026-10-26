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
                "style-create",
                json!({ "name": "Other", "activate": false }),
            ),
            ("style-delete", json!({ "style": "Other" })),
        ],
    );
}
