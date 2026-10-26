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

fn tag_count(e: &mut Engine, tag: &str) -> u64 {
    run(e, "select", json!({ "query": format!("tag:{tag}") }))["count"]
        .as_u64()
        .unwrap()
}

#[test]
fn tags_create_assign_rename_hide_and_delete_with_undo() {
    let mut e = engine();
    boxed(&mut e, 0.0, 0.0, 100.0);
    run(&mut e, "tag-folder-create", json!({ "name": "Building" }));
    run(
        &mut e,
        "tag-create",
        json!({ "name": "Walls", "color": "#A0522D", "folder": "Building" }),
    );
    run(&mut e, "tag-create", json!({ "name": "Floors" }));
    let out = run(
        &mut e,
        "tag-assign",
        json!({ "targets": "face & !normal:z & !normal:-z", "tag": "Walls" }),
    );
    assert_eq!(out["assigned"], json!(4));
    run(
        &mut e,
        "tag-assign",
        json!({ "targets": "face & normal:-z", "tag": "Floors" }),
    );
    assert_eq!(tag_count(&mut e, "Walls"), 4);
    run(
        &mut e,
        "tag-rename",
        json!({ "tag": "Walls", "new_name": "Exterior" }),
    );
    assert_eq!(tag_count(&mut e, "Exterior"), 4);
    run(
        &mut e,
        "tag-set-visible",
        json!({ "tag": "Exterior", "visible": false }),
    );
    let hidden = run(&mut e, "select", json!({ "query": "hidden" }))["count"]
        .as_u64()
        .unwrap();
    assert_eq!(hidden, 4);
    let out = run(
        &mut e,
        "tag-delete",
        json!({ "tag": "Exterior", "reassign_to": "Floors" }),
    );
    assert_eq!(out["reassigned"], json!(4));
    assert_eq!(tag_count(&mut e, "Floors"), 5);
    e.undo().unwrap();
    assert_eq!(tag_count(&mut e, "Exterior"), 4);
    assert_eq!(tag_count(&mut e, "Floors"), 1);
    let err = e
        .execute_erased(
            "tag-set-visible",
            json!({ "tag": "Untagged", "visible": false }),
        )
        .unwrap_err();
    assert_eq!(err.code(), "E1201");
    let err = e
        .execute_erased("tag-assign", json!({ "targets": "face", "tag": "Nope" }))
        .unwrap_err();
    assert_eq!(err.code(), "E1001");
    assert_eq!(
        e.execute_erased("tag-folder-delete", json!({ "name": "Building" }))
            .unwrap_err()
            .code(),
        "E1201",
        "a folder with tags is not empty"
    );
    assert_valid(&e);
}

#[test]
fn materials_create_apply_fill_delete_and_purge() {
    let mut e = engine();
    boxed(&mut e, 0.0, 0.0, 100.0);
    run(
        &mut e,
        "material-create",
        json!({ "name": "Oak", "color": "#A0522D" }),
    );
    run(
        &mut e,
        "material-create",
        json!({ "name": "Glass", "color": "#88CCEE", "opacity": 0.4 }),
    );
    let out = run(
        &mut e,
        "material-apply",
        json!({ "targets": "face & normal:z", "material": "Oak" }),
    );
    assert_eq!(out["faces"], json!(1));
    assert_eq!(
        run(&mut e, "select", json!({ "query": "material:Oak" }))["count"],
        json!(1)
    );
    // Connected spreads over the default-material neighbors only.
    let out = run(
        &mut e,
        "material-apply",
        json!({ "targets": "face & normal:x", "material": "Glass", "fill": "connected" }),
    );
    assert_eq!(out["faces"], json!(5));
    let out = run(
        &mut e,
        "material-apply",
        json!({ "targets": "face & normal:x", "material": "Oak", "fill": "same-material" }),
    );
    assert_eq!(out["faces"], json!(5));
    assert_eq!(
        run(&mut e, "select", json!({ "query": "material:Oak" }))["count"],
        json!(6)
    );
    run(
        &mut e,
        "material-edit",
        json!({ "material": "Oak", "color": "#000000", "name": "Ebony" }),
    );
    assert!(e.document().materials.by_name("Ebony").is_some());
    run(&mut e, "material-delete", json!({ "material": "Ebony" }));
    assert_eq!(
        run(&mut e, "select", json!({ "query": "material:Default" }))["count"],
        json!(6)
    );
    let out = run(&mut e, "material-purge", json!({}));
    assert_eq!(out["removed"], json!(["Glass"]));
    e.undo().unwrap();
    e.undo().unwrap();
    assert_eq!(
        run(&mut e, "select", json!({ "query": "material:Ebony" }))["count"],
        json!(6)
    );
    let err = e
        .execute_erased("material-delete", json!({ "material": "Default" }))
        .unwrap_err();
    assert_eq!(err.code(), "E1201");
    assert_valid(&e);
}

#[test]
fn scenes_capture_and_restore_camera_tags_and_style() {
    let mut e = engine();
    boxed(&mut e, 0.0, 0.0, 1000.0);
    run(&mut e, "tag-create", json!({ "name": "Walls" }));
    run(
        &mut e,
        "camera-standard",
        json!({ "view": "top", "extents": true }),
    );
    let top = e.document().camera;
    run(&mut e, "scene-add", json!({ "name": "Plan" }));
    // View commands neither enter the history nor start a new generation.
    let (generation, undo) = (e.generation(), e.history().applied());
    run(&mut e, "camera-standard", json!({ "view": "iso" }));
    run(&mut e, "orbit", json!({ "yaw": 0.5, "pitch": 0.2 }));
    assert_eq!((e.generation(), e.history().applied()), (generation, undo));
    assert_ne!(e.document().camera, top);
    run(
        &mut e,
        "tag-set-visible",
        json!({ "tag": "Walls", "visible": false }),
    );

    run(&mut e, "scene-activate", json!({ "scene": "Plan" }));
    assert_eq!(e.document().camera, top);
    let walls = e.document().tags.by_name("Walls").unwrap();
    assert!(e.document().tags.get(walls).unwrap().visible);
    // Activation is not an undo step: undo reverts `tag-set-visible`'s own change.
    assert_eq!(e.history().undo_label(), Some("Tag Visibility"));

    // A scene without the camera leaves it alone.
    run(
        &mut e,
        "scene-add",
        json!({ "name": "Tags only", "capture": { "camera": false, "style": false } }),
    );
    run(&mut e, "zoom", json!({ "factor": 2.0 }));
    let zoomed = e.document().camera;
    run(&mut e, "scene-activate", json!({ "scene": "Tags only" }));
    assert_eq!(e.document().camera, zoomed);

    let err = e
        .execute_erased("scene-add", json!({ "name": "Plan" }))
        .unwrap_err();
    assert_eq!(err.code(), "E1001");
    let err = e
        .execute_erased("zoom-extents", json!({ "targets": "group" }))
        .unwrap_err();
    assert_eq!(err.code(), "E1201");
}

/// A small PNG of `w`×`h` pixels.
fn png(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(w, h, image::Rgba(color));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

fn engine_with_storage() -> (Engine, Arc<crate::io::MemoryStorage>) {
    use crate::io::{Storage, StorageKey};
    let storage = Arc::new(crate::io::MemoryStorage::new());
    storage
        .write(
            &StorageKey::new("textures/oak.png"),
            &png(64, 32, [160, 82, 45, 255]),
        )
        .unwrap();
    storage
        .write(
            &StorageKey::new("textures/tile.png"),
            &png(16, 16, [200, 200, 210, 255]),
        )
        .unwrap();
    let mut e = engine();
    e.set_storage(storage.clone());
    (e, storage)
}

#[test]
fn textures_load_size_colorize_sample_and_undo() {
    let (mut e, _storage) = engine_with_storage();
    run(
        &mut e,
        "material-create",
        json!({ "name": "Oak", "color": "#A0522D", "texture": "textures/oak.png" }),
    );
    let doc = e.document();
    assert_eq!(doc.images.len(), 1);
    let oak = doc
        .materials
        .get(doc.materials.by_name("Oak").unwrap())
        .unwrap();
    let t = oak.texture.as_ref().unwrap();
    // One meter wide, the height following the 64×32 image.
    assert_eq!(t.size_mm, glam::DVec2::new(1000.0, 500.0));

    // Width only: the tile keeps its proportions.
    run(
        &mut e,
        "material-edit",
        json!({ "material": "Oak", "texture_width": 300.0, "colorize": "tint" }),
    );
    let doc = e.document();
    let t = doc
        .materials
        .get(doc.materials.by_name("Oak").unwrap())
        .unwrap()
        .texture
        .clone()
        .unwrap();
    assert_eq!(t.size_mm, glam::DVec2::new(300.0, 150.0));
    assert_eq!(t.colorize, Some(crate::document::ColorizeMode::Tint));

    // Paint a face and sample it back.
    let r = run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [1000, 1000, 0] }),
    );
    run(
        &mut e,
        "material-apply",
        json!({ "targets": format!("@{}", r["face"]), "material": "Oak" }),
    );
    let sampled = run(
        &mut e,
        "material-sample",
        json!({ "face": { "Pid": r["face"] } }),
    );
    assert_eq!(sampled["material"], "Oak");
    let back = run(
        &mut e,
        "material-sample",
        json!({ "face": { "Pid": r["face"] }, "side": "back" }),
    );
    assert_eq!(back["material"], "Default Back");

    // A missing file is an I/O error; a material without a texture cannot
    // be sized.
    let err = e
        .execute_erased(
            "material-create",
            json!({ "name": "Gone", "color": "#000000", "texture": "missing.png" }),
        )
        .unwrap_err();
    assert!(matches!(err, crate::commands::CommandError::Io(_)), "{err}");
    run(
        &mut e,
        "material-create",
        json!({ "name": "Plain", "color": "#808080" }),
    );
    assert!(
        e.execute_erased(
            "material-edit",
            json!({ "material": "Plain", "texture_width": 10.0 })
        )
        .is_err()
    );

    // Undo back to the start removes the image again.
    while e.history().can_undo() {
        e.undo().unwrap();
    }
    assert_eq!(e.document().images.len(), 0);
    assert!(e.document().materials.by_name("Oak").is_none());
    e.redo().unwrap();
    assert_eq!(e.document().images.len(), 1);
    assert_valid(&e);
}

#[test]
fn material_libraries_round_trip_with_textures() {
    let (mut e, storage) = engine_with_storage();
    run(
        &mut e,
        "material-create",
        json!({ "name": "Oak", "color": "#A0522D", "texture": "textures/oak.png", "texture_width": 600.0 }),
    );
    run(
        &mut e,
        "material-create",
        json!({ "name": "Tile", "color": "#C8C8D2", "texture": "textures/tile.png", "opacity": 0.5 }),
    );
    run(
        &mut e,
        "material-create",
        json!({ "name": "Paint", "color": "#336699" }),
    );
    let generation = e.generation();
    let saved = run(
        &mut e,
        "material-library-save",
        json!({ "path": "libs/woods.plyrm", "materials": ["Oak", "Tile"] }),
    );
    assert_eq!(saved["materials"], json!(["Oak", "Tile"]));
    assert_eq!(
        e.generation(),
        generation,
        "saving a library changes nothing"
    );

    // Another document loads it: same materials, same images.
    let mut other = engine();
    other.set_storage(storage.clone());
    run(
        &mut other,
        "material-create",
        json!({ "name": "Tile", "color": "#000000" }),
    );
    let loaded = run(
        &mut other,
        "material-library-load",
        json!({ "path": "libs/woods.plyrm" }),
    );
    assert_eq!(loaded["materials"], json!(["Oak"]));
    assert_eq!(loaded["skipped"], json!(["Tile"]));
    let replaced = run(
        &mut other,
        "material-library-load",
        json!({ "path": "libs/woods.plyrm", "replace": true }),
    );
    assert_eq!(replaced["materials"], json!(["Oak", "Tile"]));
    let (a, b) = (e.document(), other.document());
    for name in ["Oak", "Tile"] {
        let ma = a.materials.get(a.materials.by_name(name).unwrap()).unwrap();
        let mb = b.materials.get(b.materials.by_name(name).unwrap()).unwrap();
        assert_eq!(ma, mb, "{name}");
        let image = mb.texture.as_ref().unwrap().image;
        assert_eq!(a.images.get(image), b.images.get(image));
    }
    assert!(b.materials.by_name("Paint").is_none());
    // Saving the same library twice gives the same bytes.
    use crate::io::{Storage, StorageKey};
    let first = storage.read(&StorageKey::new("libs/woods.plyrm")).unwrap();
    run(
        &mut e,
        "material-library-save",
        json!({ "path": "libs/woods.plyrm", "materials": ["Oak", "Tile"] }),
    );
    assert_eq!(
        storage.read(&StorageKey::new("libs/woods.plyrm")).unwrap(),
        first
    );
    assert_valid(&other);
}
