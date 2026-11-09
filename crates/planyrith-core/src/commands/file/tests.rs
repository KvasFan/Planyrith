//! Tests of `import` and `export` (docs/09 §5–§6): formats read through
//! the engine's storage, round trips, undo.

use std::sync::Arc;

use glam::DVec3;
use serde_json::json;

use crate::commands::{CommandRegistry, Engine};
use crate::document::{ContainerRef, Document, EntityView, Projection};
use crate::io::import::stl::read_triangles;
use crate::io::{MemoryStorage, Storage, StorageKey};

fn engine() -> (Engine, Arc<MemoryStorage>) {
    let storage = Arc::new(MemoryStorage::new());
    let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    e.set_storage(storage.clone());
    (e, storage)
}

fn run(e: &mut Engine, name: &str, params: serde_json::Value) -> serde_json::Value {
    e.execute_erased(name, params)
        .unwrap_or_else(|err| panic!("{name}: {err}"))
}

/// A 10-unit cube as ASCII STL, outward triangles.
pub(crate) fn cube_stl() -> String {
    let c = |i: u8| {
        DVec3::new(
            f64::from(i & 1) * 10.0,
            f64::from((i >> 1) & 1) * 10.0,
            f64::from((i >> 2) & 1) * 10.0,
        )
    };
    let quads: [[u8; 4]; 6] = [
        [0, 2, 3, 1],
        [4, 5, 7, 6],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 4, 6, 2],
        [1, 3, 7, 5],
    ];
    let mut text = String::from("solid cube\n");
    for q in quads {
        for t in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
            text.push_str("facet normal 0 0 0\n outer loop\n");
            for i in t {
                let p = c(i);
                text.push_str(&format!("  vertex {} {} {}\n", p.x, p.y, p.z));
            }
            text.push_str(" endloop\nendfacet\n");
        }
    }
    text.push_str("endsolid cube\n");
    text
}

/// Two 10-unit cubes overlapping by half, as one ASCII STL solid.
fn overlapping_cubes_stl() -> String {
    let one = cube_stl();
    let body: String = one
        .lines()
        .filter(|l| !l.starts_with("solid") && !l.starts_with("endsolid"))
        .map(|l| format!("{l}\n"))
        .collect();
    let shifted: String = body
        .lines()
        .map(|l| match l.trim().strip_prefix("vertex ") {
            Some(rest) => {
                let v: Vec<f64> = rest.split(' ').map(|x| x.parse().unwrap()).collect();
                format!("  vertex {} {} {}\n", v[0] + 5.0, v[1], v[2])
            }
            None => format!("{l}\n"),
        })
        .collect();
    format!("solid two\n{body}{shifted}endsolid two\n")
}

/// Faces and edges of the only definition, and its instance's bounds.
fn imported(e: &Engine) -> (usize, usize, DVec3, DVec3) {
    let doc = e.document();
    let model = doc.container(ContainerRef::Model).unwrap();
    let (def, transform) = model
        .order()
        .iter()
        .find_map(|&id| match model.view(id) {
            Some(EntityView::Instance(i)) => Some((i.definition, i.transform)),
            Some(EntityView::Group(g)) => Some((g.definition, g.transform)),
            _ => None,
        })
        .expect("a group or instance");
    let entities = doc.container(ContainerRef::Definition(def)).unwrap();
    let mesh = entities.mesh();
    let (mut lo, mut hi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
    for (_, v) in mesh.vertices() {
        let p = transform.transform_point3(v.position);
        lo = lo.min(p);
        hi = hi.max(p);
    }
    (mesh.faces().count(), mesh.edges().count(), lo, hi)
}

fn close(a: DVec3, b: DVec3) -> bool {
    (a - b).abs().max_element() < 1e-6
}

#[test]
fn stl_cube_imports_as_a_group_of_six_faces_y_up() {
    let (mut e, storage) = engine();
    storage
        .write(&StorageKey::new("cube.stl"), cube_stl().as_bytes())
        .unwrap();
    let out = run(
        &mut e,
        "import",
        json!({ "path": "cube.stl", "file_units": "cm" }),
    );
    assert_eq!(out["format"], "stl");
    assert_eq!(out["faces"], 6);
    assert_eq!(out["edges"], 12);
    let (faces, edges, lo, hi) = imported(&e);
    assert_eq!((faces, edges), (6, 12));
    // 10 cm, the file's Y up becomes Z up: y ∈ [−100, 0].
    assert!(close(lo, DVec3::new(0.0, -100.0, 0.0)), "{lo}");
    assert!(close(hi, DVec3::new(100.0, 0.0, 100.0)), "{hi}");
    let report = crate::diagnostics::validate(e.document());
    assert!(report.is_clean(), "{report:#?}");

    // Kept Z up, scaled, placed.
    let (mut e, storage) = engine();
    storage
        .write(&StorageKey::new("cube.stl"), cube_stl().as_bytes())
        .unwrap();
    run(
        &mut e,
        "import",
        json!({ "path": "cube.stl", "swap_yz": false, "scale": 2.0, "at": [100, 0, 0] }),
    );
    let (_, _, lo, hi) = imported(&e);
    assert!(close(lo, DVec3::new(100.0, 0.0, 0.0)), "{lo}");
    assert!(close(hi, DVec3::new(120.0, 20.0, 20.0)), "{hi}");
}

#[test]
fn overlapping_shells_import_as_separate_groups() {
    let (mut e, storage) = engine();
    storage
        .write(
            &StorageKey::new("two.stl"),
            overlapping_cubes_stl().as_bytes(),
        )
        .unwrap();
    let out = run(&mut e, "import", json!({ "path": "two.stl" }));
    assert_eq!(out["faces"], 12, "{out}");
    assert_eq!(out["warnings"], json!([]));
    let report = crate::diagnostics::validate(e.document());
    assert!(report.is_clean(), "{report:#?}");
    let shells = e
        .document()
        .definitions
        .iter()
        .filter(|(id, _)| {
            e.document()
                .container(ContainerRef::Definition(*id))
                .is_some_and(|c| c.mesh().faces().count() == 6)
        })
        .count();
    assert_eq!(shells, 2);
}

#[test]
fn stl_export_round_trips_binary_and_ascii() {
    let (mut e, storage) = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [1000, 600, 0] }),
    );
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face" }, "distance": 300.0 }),
    );
    let generation = e.generation();
    let out = run(&mut e, "export", json!({ "path": "box.stl" }));
    assert_eq!(out["entities"], 12);
    assert_eq!(e.generation(), generation, "export changes nothing");
    let binary = storage.read(&StorageKey::new("box.stl")).unwrap();
    assert_eq!(binary.len(), 84 + 12 * 50);
    run(
        &mut e,
        "export",
        json!({ "path": "box-ascii.stl", "ascii": true, "file_units": "m", "z_up": true }),
    );
    let ascii = storage.read(&StorageKey::new("box-ascii.stl")).unwrap();
    assert!(ascii.starts_with(b"solid planyrith"));
    let tris = read_triangles(&ascii).unwrap();
    assert_eq!(tris.len(), 12);
    let top = tris.iter().flatten().map(|p| p.z).fold(f64::MIN, f64::max);
    assert!((top - 0.3).abs() < 1e-6, "meters, Z up: {top}");

    // Binary (Y-up) back in: the same box.
    let (mut back, back_storage) = engine();
    back_storage
        .write(&StorageKey::new("box.stl"), &binary)
        .unwrap();
    let out = run(&mut back, "import", json!({ "path": "box.stl" }));
    assert_eq!(out["faces"], 6);
    let (_, _, lo, hi) = imported(&back);
    assert!(close(lo, DVec3::ZERO), "{lo}");
    assert!(close(hi, DVec3::new(1000.0, 600.0, 300.0)), "{hi}");
}

#[test]
fn import_errors_and_undo() {
    let (mut e, storage) = engine();
    let err = e
        .execute_erased("import", json!({ "path": "missing.stl" }))
        .unwrap_err();
    assert!(err.to_string().contains("missing.stl"), "{err}");
    storage
        .write(&StorageKey::new("notes.txt"), b"hello")
        .unwrap();
    assert!(
        e.execute_erased("import", json!({ "path": "notes.txt" }))
            .is_err()
    );
    assert!(
        e.execute_erased("import", json!({ "path": "a.stl", "as": "component:" }))
            .is_err()
    );
    storage
        .write(
            &StorageKey::new("bad.stl"),
            b"solid x\nfacet\n outer loop\n vertex 1 2\n",
        )
        .unwrap();
    assert!(
        e.execute_erased("import", json!({ "path": "bad.stl" }))
            .is_err()
    );
    assert_eq!(e.generation(), 0);

    storage
        .write(&StorageKey::new("cube.stl"), cube_stl().as_bytes())
        .unwrap();
    let out = run(
        &mut e,
        "import",
        json!({ "path": "cube.stl", "as": "component:Cube" }),
    );
    assert_eq!(out["created"].as_array().map(Vec::len), Some(1));
    assert!(e.document().definition_by_name("Cube").is_some());
    e.undo().unwrap();
    assert!(e.document().definition_by_name("Cube").is_none());
    assert_eq!(
        e.document()
            .container(ContainerRef::Model)
            .unwrap()
            .order()
            .len(),
        0
    );
}

#[test]
fn random_bytes_never_panic_the_stl_reader() {
    let mut state = 0x2545_f491_u32;
    for len in [0usize, 1, 5, 83, 84, 134, 200, 1000] {
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        let _ = read_triangles(&bytes);
        let mut solid = b"solid ".to_vec();
        solid.extend_from_slice(&bytes);
        let _ = read_triangles(&solid);
    }
}

fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb(rgb)))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

/// Faces of each definition, by definition name.
fn faces_by_definition(e: &Engine) -> Vec<(String, usize)> {
    let doc = e.document();
    let mut out: Vec<(String, usize)> = doc
        .definitions
        .iter()
        .map(|(id, d)| {
            let faces = doc
                .container(ContainerRef::Definition(id))
                .map_or(0, |c| c.mesh().faces().count());
            (d.name.clone(), faces)
        })
        .collect();
    out.sort();
    out
}

const OBJ: &str = "# two objects
mtllib parts.mtl
o Plate
v 0 0 0
v 4 0 0
v 4 0 4
v 2 0 4
v 2 0 2
v 0 0 2
vt 0 0
vt 1 0
vt 1 1
vt 0.5 1
vt 0.5 0.5
vt 0 0.5
usemtl Brick
f 1/1 6/6 5/5 4/4 3/3 2/2
o Card
v 0 1 0
v 1 1 0
v 1 2 0
v 0 2 0
usemtl Paint
f 7 8 9 10
l 7 9
";

const MTL: &str = "newmtl Brick
Kd 0.6 0.3 0.2
map_Kd -s 1 1 1 textures/brick.png
newmtl Paint
Kd 0 0 1
d 0.5
";

#[test]
fn obj_imports_objects_materials_textures_and_concave_polygons() {
    let (mut e, storage) = engine();
    storage
        .write(&StorageKey::new("in/parts.obj"), OBJ.as_bytes())
        .unwrap();
    storage
        .write(&StorageKey::new("in/parts.mtl"), MTL.as_bytes())
        .unwrap();
    storage
        .write(
            &StorageKey::new("in/textures/brick.png"),
            &png(4, 4, [150, 70, 50]),
        )
        .unwrap();
    let out = run(
        &mut e,
        "import",
        json!({ "path": "in/parts.obj", "file_units": "m" }),
    );
    assert_eq!(out["format"], "obj");
    // The L-shaped hexagon is one face, the quad another.
    assert_eq!(out["faces"], 2);
    assert_eq!(out["skipped"], 1, "{out}");
    let doc = e.document();
    let brick = doc
        .materials
        .by_name("Brick")
        .and_then(|m| doc.materials.get(m))
        .unwrap();
    assert_eq!(brick.color, crate::document::Rgba8::rgb(153, 77, 51));
    assert!(brick.texture.is_some());
    let paint = doc
        .materials
        .by_name("Paint")
        .and_then(|m| doc.materials.get(m))
        .unwrap();
    assert!((paint.opacity - 0.5).abs() < 1e-6);
    let faces = faces_by_definition(&e);
    assert!(faces.contains(&("Group 1".into(), 0)), "{faces:?}");
    assert_eq!(faces.iter().map(|(_, n)| n).sum::<usize>(), 2, "{faces:?}");
    let report = crate::diagnostics::validate(e.document());
    assert!(report.is_clean(), "{report:#?}");

    // Without its MTL the geometry still comes in, with a warning.
    let (mut e, storage) = engine();
    storage
        .write(&StorageKey::new("parts.obj"), OBJ.as_bytes())
        .unwrap();
    let out = run(&mut e, "import", json!({ "path": "parts.obj" }));
    assert_eq!(out["faces"], 2);
    assert!(
        out["warnings"].to_string().contains("parts.mtl"),
        "{}",
        out["warnings"]
    );
}

#[test]
fn obj_export_round_trips_with_materials_and_textures() {
    let (mut e, storage) = engine();
    storage
        .write(&StorageKey::new("oak.png"), &png(8, 8, [160, 110, 60]))
        .unwrap();
    run(
        &mut e,
        "material-create",
        json!({ "name": "Light Oak", "color": "#A06E3C", "texture": "oak.png" }),
    );
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [1000, 600, 0] }),
    );
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face" }, "distance": 300.0 }),
    );
    run(
        &mut e,
        "material-apply",
        json!({ "targets": "face & normal:z", "material": "Light Oak" }),
    );
    run(&mut e, "make-group", json!({ "targets": "all" }));
    let out = run(&mut e, "export", json!({ "path": "out/box.obj" }));
    assert_eq!(out["format"], "obj");
    assert_eq!(out["entities"], 12);
    let obj = String::from_utf8(storage.read(&StorageKey::new("out/box.obj")).unwrap()).unwrap();
    assert!(obj.contains("mtllib box.mtl"));
    assert!(obj.contains("usemtl Light_Oak"));
    let mtl = String::from_utf8(storage.read(&StorageKey::new("out/box.mtl")).unwrap()).unwrap();
    assert!(mtl.contains("map_Kd box_textures/Light_Oak.png"), "{mtl}");
    assert!(
        storage
            .read(&StorageKey::new("out/box_textures/Light_Oak.png"))
            .is_ok()
    );
    // Same bytes again: deterministic.
    run(&mut e, "export", json!({ "path": "out/again.obj" }));
    let again =
        String::from_utf8(storage.read(&StorageKey::new("out/again.obj")).unwrap()).unwrap();
    assert_eq!(
        obj.replace("box.mtl", "again.mtl"),
        again,
        "deterministic output"
    );

    let (mut back, back_storage) = engine();
    for key in [
        "out/box.obj",
        "out/box.mtl",
        "out/box_textures/Light_Oak.png",
    ] {
        back_storage
            .write(
                &StorageKey::new(key),
                &storage.read(&StorageKey::new(key)).unwrap(),
            )
            .unwrap();
    }
    let out = run(&mut back, "import", json!({ "path": "out/box.obj" }));
    assert_eq!(out["faces"], 6, "{out}");
    let (_, _, lo, hi) = imported(&back);
    assert!(close(lo, DVec3::ZERO), "{lo}");
    assert!(close(hi, DVec3::new(1000.0, 600.0, 300.0)), "{hi}");
    let doc = back.document();
    let oak = doc
        .materials
        .by_name("Light_Oak")
        .and_then(|m| doc.materials.get(m))
        .unwrap();
    assert_eq!(oak.color, crate::document::Rgba8::rgb(160, 110, 60));
    assert!(oak.texture.is_some());
}

#[test]
fn random_obj_and_stl_files_never_panic_the_importers() {
    let mut state = 0x9e37_79b9_u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let words = [
        "v",
        "vt",
        "vn",
        "f",
        "o",
        "g",
        "usemtl",
        "mtllib",
        "l",
        "p",
        "s",
        "1",
        "-1",
        "0",
        "2/3",
        "1//2",
        "-2/-1/-3",
        "nan",
        "inf",
        "1e308",
        "x",
        "/",
        "#",
        "3/",
        "999999999",
    ];
    for round in 0..300 {
        let mut text = String::new();
        for _ in 0..(next() % 40) {
            for _ in 0..(next() % 6) {
                text.push_str(words[next() as usize % words.len()]);
                text.push(' ');
            }
            text.push('\n');
        }
        let (mut e, storage) = engine();
        storage
            .write(&StorageKey::new("r.obj"), text.as_bytes())
            .unwrap();
        let raw: Vec<u8> = (0..(next() % 300)).map(|_| next() as u8).collect();
        storage.write(&StorageKey::new("r.stl"), &raw).unwrap();
        for path in ["r.obj", "r.stl"] {
            if e.execute_erased("import", json!({ "path": path })).is_ok() {
                let report = crate::diagnostics::validate(e.document());
                assert!(
                    !report.has_errors(),
                    "round {round} {path}: {report:#?}\n{text}"
                );
            }
        }
    }
}

/// Two instances of a textured component and a plain group.
fn chairs_and_box(e: &mut Engine, storage: &MemoryStorage) {
    storage
        .write(&StorageKey::new("oak.png"), &png(8, 8, [160, 110, 60]))
        .unwrap();
    run(
        e,
        "material-create",
        json!({ "name": "Oak", "color": "#A06E3C", "texture": "oak.png" }),
    );
    run(
        e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [400, 400, 0] }),
    );
    run(
        e,
        "push-pull",
        json!({ "face": { "Query": "face & last" }, "distance": 450.0 }),
    );
    run(
        e,
        "material-apply",
        json!({ "targets": "face", "material": "Oak" }),
    );
    run(
        e,
        "make-component",
        json!({ "targets": "all", "name": "Seat" }),
    );
    run(
        e,
        "move",
        json!({ "targets": "all", "delta": [1000, 0, 0], "copy": true }),
    );
    run(
        e,
        "rectangle",
        json!({ "corner_a": [0, 1000, 0], "corner_b": [2000, 1600, 0] }),
    );
    run(
        e,
        "push-pull",
        json!({ "face": { "Query": "face & last" }, "distance": 750.0 }),
    );
    run(e, "make-group", json!({ "targets": "face | edge" }));
}

fn bounds_of_model(e: &Engine) -> (DVec3, DVec3) {
    let b = e.document().aabb();
    (b.min, b.max)
}

#[test]
fn glb_and_gltf_round_trip_components_materials_and_textures() {
    let (mut e, storage) = engine();
    chairs_and_box(&mut e, &storage);
    let (lo, hi) = bounds_of_model(&e);
    let out = run(&mut e, "export", json!({ "path": "out/scene.glb" }));
    assert_eq!(out["format"], "glb");
    let glb = storage.read(&StorageKey::new("out/scene.glb")).unwrap();
    assert!(glb.starts_with(b"glTF"));
    let parsed = gltf::Gltf::from_slice(&glb).expect("a valid GLB");
    assert_eq!(
        parsed.document.meshes().count(),
        2,
        "the seat mesh is shared"
    );
    assert_eq!(parsed.document.nodes().count(), 3);
    run(&mut e, "export", json!({ "path": "out/scene.glb" }));
    assert_eq!(
        storage.read(&StorageKey::new("out/scene.glb")).unwrap(),
        glb,
        "deterministic"
    );

    for (file, extra) in [
        ("out/scene.glb", vec![]),
        ("out/scene.gltf", vec!["out/scene.bin"]),
    ] {
        if file.ends_with(".gltf") {
            run(&mut e, "export", json!({ "path": file }));
        }
        let (mut back, back_storage) = engine();
        for key in std::iter::once(file).chain(extra) {
            back_storage
                .write(
                    &StorageKey::new(key),
                    &storage.read(&StorageKey::new(key)).unwrap(),
                )
                .unwrap();
        }
        let out = run(&mut back, "import", json!({ "path": file }));
        assert_eq!(out["format"], "gltf", "{file}");
        assert_eq!(out["faces"], 12, "{file}: {out}");
        let (blo, bhi) = bounds_of_model(&back);
        // glTF stores f32 meters.
        let near = |a: DVec3, b: DVec3| (a - b).abs().max_element() < 1e-3;
        assert!(
            near(blo, lo) && near(bhi, hi),
            "{file}: {blo} {bhi} vs {lo} {hi}"
        );
        let doc = back.document();
        let seat = doc
            .definition_by_name("Seat")
            .expect("the shared mesh is a component");
        let model = doc.container(ContainerRef::Model).unwrap();
        let top = model
            .order()
            .iter()
            .find_map(|&id| match model.view(id) {
                Some(EntityView::Group(g)) => Some(g.definition),
                _ => None,
            })
            .unwrap();
        let uses = doc.container(ContainerRef::Definition(top)).map(|c| {
            c.order()
                .iter()
                .filter(|&&id| {
                    matches!(c.view(id), Some(EntityView::Instance(i)) if i.definition == seat)
                })
                .count()
        });
        assert_eq!(uses, Some(2), "{file}");
        let oak = doc
            .materials
            .by_name("Oak")
            .and_then(|m| doc.materials.get(m))
            .unwrap();
        assert_eq!(
            oak.color,
            crate::document::Rgba8::rgb(160, 110, 60),
            "{file}"
        );
        assert!(oak.texture.is_some(), "{file}");
        let report = crate::diagnostics::validate(doc);
        assert!(report.is_clean(), "{file}: {report:#?}");
    }
}

#[test]
fn gltf_with_data_uris_strips_and_texture_transforms_imports() {
    // A unit quad as a triangle strip in a data URI buffer, in the XZ
    // plane (Y-up: the ground).
    let mut data = Vec::new();
    for p in [
        [0.0f32, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 0.0, -1.0],
        [1.0, 0.0, -1.0],
    ] {
        for v in p {
            data.extend_from_slice(&v.to_le_bytes());
        }
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b64 = String::new();
    for chunk in data.chunks(3) {
        let n =
            chunk.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for k in 0..=chunk.len() {
            b64.push(alphabet[((n >> (18 - 6 * k)) & 63) as usize] as char);
        }
        for _ in chunk.len()..3 {
            b64.push('=');
        }
    }
    let gltf = json!({
        "asset": { "version": "2.0" },
        "buffers": [{ "byteLength": data.len(), "uri": format!("data:application/octet-stream;base64,{b64}") }],
        "bufferViews": [{ "buffer": 0, "byteLength": data.len() }],
        "accessors": [{ "bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3",
                        "min": [0, 0, -1], "max": [1, 0, 0] }],
        "materials": [{ "name": "Red", "pbrMetallicRoughness": { "baseColorFactor": [1, 0, 0, 1] } }],
        "meshes": [{ "name": "Quad", "primitives": [{ "attributes": { "POSITION": 0 }, "mode": 5, "material": 0 }] }],
        "nodes": [{ "name": "Floor", "mesh": 0, "translation": [2, 0, 0] }],
        "scenes": [{ "nodes": [0] }],
        "scene": 0
    });
    let (mut e, storage) = engine();
    storage
        .write(&StorageKey::new("quad.gltf"), gltf.to_string().as_bytes())
        .unwrap();
    let out = run(&mut e, "import", json!({ "path": "quad.gltf" }));
    assert_eq!(out["faces"], 1, "{out}");
    let (lo, hi) = bounds_of_model(&e);
    assert!(close(lo, DVec3::new(2000.0, 0.0, 0.0)), "{lo}");
    assert!(close(hi, DVec3::new(3000.0, 1000.0, 0.0)), "{hi}");
    let doc = e.document();
    let red = doc
        .materials
        .by_name("Red")
        .and_then(|m| doc.materials.get(m))
        .unwrap();
    assert_eq!(red.color, crate::document::Rgba8::rgb(255, 0, 0));
}

#[test]
fn damaged_glb_files_never_panic_the_importer() {
    let (mut e, storage) = engine();
    chairs_and_box(&mut e, &storage);
    run(&mut e, "export", json!({ "path": "s.glb" }));
    let glb = storage.read(&StorageKey::new("s.glb")).unwrap();
    let mut state = 0x1234_5678_u32;
    for round in 0..200 {
        let mut bytes = glb.clone();
        for _ in 0..(1 + round % 8) {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let at = state as usize % bytes.len();
            bytes[at] = (state >> 8) as u8;
        }
        if round % 10 == 0 {
            bytes.truncate(state as usize % bytes.len());
        }
        let (mut back, back_storage) = engine();
        back_storage
            .write(&StorageKey::new("d.glb"), &bytes)
            .unwrap();
        if back
            .execute_erased("import", json!({ "path": "d.glb" }))
            .is_ok()
        {
            let report = crate::diagnostics::validate(back.document());
            assert!(!report.has_errors(), "round {round}: {report:#?}");
        }
    }
}

/// A height-field GLB of `n`×`n` cells (2n² triangles, positions, normals
/// and texture coordinates, like a scanned terrain).
pub(crate) fn terrain_glb(n: u32) -> Vec<u8> {
    let mut bin = Vec::new();
    let side = n + 1;
    for j in 0..side {
        for i in 0..side {
            let h = libm::sin(f64::from(i) * 0.37) * libm::cos(f64::from(j) * 0.23) * 0.5
                + f64::from((i * 7919 + j * 104_729) % 97) / 970.0;
            for v in [f64::from(i) * 0.1, h, -f64::from(j) * 0.1] {
                bin.extend_from_slice(&(v as f32).to_le_bytes());
            }
        }
    }
    let positions = bin.len();
    for _ in 0..side * side {
        for v in [0.0f32, 1.0, 0.0] {
            bin.extend_from_slice(&v.to_le_bytes());
        }
    }
    let normals = bin.len() - positions;
    for j in 0..side {
        for i in 0..side {
            for v in [i as f32 / n as f32, j as f32 / n as f32] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    let uvs = bin.len() - positions - normals;
    for j in 0..n {
        for i in 0..n {
            let a = j * side + i;
            let (b, c) = (a + 1, a + side);
            for v in [a, b, c + 1, a, c + 1, c] {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    let indices = bin.len() - positions - normals - uvs;
    let count = side * side;
    let mut text = json!({
        "asset": { "version": "2.0" },
        "buffers": [{ "byteLength": bin.len() }],
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": positions },
            { "buffer": 0, "byteOffset": positions, "byteLength": normals },
            { "buffer": 0, "byteOffset": positions + normals, "byteLength": uvs },
            { "buffer": 0, "byteOffset": positions + normals + uvs, "byteLength": indices }
        ],
        "accessors": [
            { "bufferView": 0, "componentType": 5126, "count": count, "type": "VEC3",
              "min": [0, -1, -(f64::from(n) * 0.1)], "max": [f64::from(n) * 0.1, 1, 0] },
            { "bufferView": 1, "componentType": 5126, "count": count, "type": "VEC3" },
            { "bufferView": 2, "componentType": 5126, "count": count, "type": "VEC2" },
            { "bufferView": 3, "componentType": 5125, "count": n * n * 6, "type": "SCALAR" }
        ],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2 }, "indices": 3 }] }],
        "nodes": [{ "mesh": 0 }],
        "scenes": [{ "nodes": [0] }],
        "scene": 0
    })
    .to_string()
    .into_bytes();
    while !text.len().is_multiple_of(4) {
        text.push(b' ');
    }
    let total = 12 + 8 + text.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&text);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

/// docs/13 0.5 acceptance: a 100 MB glTF imports in under 20 s. Run with
/// `cargo test --release -p planyrith-core import_benchmark -- --ignored
/// --nocapture` (`PLANYRITH_BENCH_CELLS` sets the grid; 1150 ≈ 100 MB).
#[test]
#[ignore = "benchmark"]
fn import_benchmark() {
    let n = std::env::var("PLANYRITH_BENCH_CELLS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1150);
    let glb = terrain_glb(n);
    eprintln!("{:.1} MB, {} triangles", glb.len() as f64 / 1e6, 2 * n * n);
    let mut best = f64::MAX;
    for _ in 0..3 {
        let (mut e, storage) = engine();
        storage.write(&StorageKey::new("t.glb"), &glb).unwrap();
        let started = std::time::Instant::now();
        let out = run(&mut e, "import", json!({ "path": "t.glb" }));
        let secs = started.elapsed().as_secs_f64();
        eprintln!("import: {secs:.2} s, {} faces", out["faces"]);
        best = best.min(secs);
    }
    eprintln!("best: {best:.2} s");
}

#[test]
fn images_import_as_entities_or_textures() {
    let (mut e, storage) = engine();
    storage
        .write(
            &StorageKey::new("pics/plan.png"),
            &png(40, 20, [10, 200, 30]),
        )
        .unwrap();
    let out = run(
        &mut e,
        "import",
        json!({ "path": "pics/plan.png", "at": [100, 200, 0], "scale": 2.0 }),
    );
    assert_eq!(out["format"], "image");
    assert_eq!(out["created"].as_array().map(Vec::len), Some(1));
    let (lo, hi) = bounds_of_model(&e);
    assert!(close(lo, DVec3::new(100.0, 200.0, 0.0)), "{lo}");
    assert!(close(hi, DVec3::new(2100.0, 1200.0, 0.0)), "{hi}");
    let out = run(
        &mut e,
        "import",
        json!({ "path": "pics/plan.png", "as": "texture" }),
    );
    assert_eq!(out["created"], json!([]));
    let doc = e.document();
    let tex = doc
        .materials
        .by_name("plan #2")
        .and_then(|m| doc.materials.get(m))
        .and_then(|m| m.texture.clone())
        .expect("a textured material");
    assert!(close(
        tex.size_mm.extend(0.0),
        DVec3::new(1000.0, 500.0, 0.0)
    ));
    assert!(
        e.execute_erased(
            "import",
            json!({ "path": "pics/plan.png", "as": "component:X" })
        )
        .is_err()
    );
}

#[test]
fn csv_points_import_as_guide_points() {
    let (mut e, storage) = engine();
    storage
        .write(
            &StorageKey::new("survey.csv"),
            b"x,y,z,name\n0,0,0,A\n1.5;2;0.25;B\n3\t4\n\nbad,row\n# note\n",
        )
        .unwrap();
    let out = run(
        &mut e,
        "import",
        json!({ "path": "survey.csv", "file_units": "m" }),
    );
    assert_eq!(out["format"], "csv");
    assert_eq!(out["skipped"], 2, "{out}");
    let doc = e.document();
    let points: Vec<DVec3> = doc
        .definitions
        .iter()
        .flat_map(|(id, _)| {
            let c = doc.container(ContainerRef::Definition(id)).unwrap();
            c.order()
                .iter()
                .filter_map(|&g| match c.view(g) {
                    Some(EntityView::Guide(crate::document::Guide::Point { pos })) => Some(*pos),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(points.len(), 3);
    assert!(close(points[1], DVec3::new(1500.0, 2000.0, 250.0)));
    assert!(close(points[2], DVec3::new(3000.0, 4000.0, 0.0)));
}

#[test]
fn plyr_documents_merge_with_components_and_materials() {
    let (mut other, storage) = engine();
    chairs_and_box(&mut other, &storage);
    run(&mut other, "export", json!({ "path": "room.plyr" }));
    let (lo, hi) = bounds_of_model(&other);

    let (mut e, _) = engine();
    e.set_storage(storage.clone());
    let out = run(
        &mut e,
        "import",
        json!({ "path": "room.plyr", "at": [0, 5000, 0] }),
    );
    assert_eq!(out["format"], "plyr");
    assert_eq!(out["faces"], 12, "{out}");
    let (mlo, mhi) = bounds_of_model(&e);
    let shift = DVec3::new(0.0, 5000.0, 0.0);
    assert!(
        close(mlo, lo + shift) && close(mhi, hi + shift),
        "{mlo} {mhi}"
    );
    let doc = e.document();
    assert!(doc.definition_by_name("Seat").is_some());
    let oak = doc
        .materials
        .by_name("Oak")
        .and_then(|m| doc.materials.get(m))
        .unwrap();
    assert!(oak.texture.is_some());
    // Merged twice: the component and the box's group definition (same
    // UUIDs) are reused, only the new top-level component is added.
    let before = doc.definitions.iter().count();
    run(
        &mut e,
        "import",
        json!({ "path": "room.plyr", "as": "component:Room" }),
    );
    assert_eq!(e.document().definitions.iter().count(), before + 1);
    let report = crate::diagnostics::validate(e.document());
    assert!(report.is_clean(), "{report:#?}");
    e.undo().unwrap();
    e.undo().unwrap();
    assert!(e.document().definition_by_name("Seat").is_none());
}

#[test]
fn csv_component_report_counts_and_measures() {
    let (mut e, storage) = engine();
    chairs_and_box(&mut e, &storage);
    run(
        &mut e,
        "export",
        json!({ "path": "report.csv", "file_units": "cm" }),
    );
    let text = String::from_utf8(storage.read(&StorageKey::new("report.csv")).unwrap()).unwrap();
    assert_eq!(
        text,
        "name,count,width_cm,depth_cm,height_cm,volume_cm3,description\n\
         Seat,2,40,40,45,72000,\n"
    );
}

/// Group code / value lines of a DXF from `(code, value)` pairs.
fn dxf_text(pairs: &[(i32, &str)]) -> String {
    pairs.iter().map(|(c, v)| format!("{c}\n{v}\n")).collect()
}

pub(crate) fn sample_dxf() -> String {
    let mut p: Vec<(i32, &str)> = vec![
        (0, "SECTION"),
        (2, "HEADER"),
        (9, "$INSUNITS"),
        (70, "6"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "TABLES"),
        (0, "TABLE"),
        (2, "LAYER"),
        (0, "LAYER"),
        (2, "Walls"),
        (62, "1"),
        (0, "LAYER"),
        (2, "Furniture"),
        (62, "3"),
        (0, "ENDTAB"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "BLOCKS"),
        (0, "BLOCK"),
        (2, "Stool"),
        (10, "1"),
        (20, "1"),
        (30, "0"),
        // A 0.4 m square seat (closed LWPOLYLINE) around the base point.
        (0, "LWPOLYLINE"),
        (8, "0"),
        (90, "4"),
        (70, "1"),
        (10, "0.8"),
        (20, "0.8"),
        (10, "1.2"),
        (20, "0.8"),
        (10, "1.2"),
        (20, "1.2"),
        (10, "0.8"),
        (20, "1.2"),
        (0, "ENDBLK"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "ENTITIES"),
    ];
    // Walls: a closed rectangle of lines (a face) and a rounded slot.
    for (a, b) in [
        ((0, 0), (4, 0)),
        ((4, 0), (4, 3)),
        ((4, 3), (0, 3)),
        ((0, 3), (0, 0)),
    ] {
        let coords = [a.0, a.1, b.0, b.1].map(|v: i32| v.to_string());
        let leaked: Vec<&'static str> = coords
            .iter()
            .map(|s| &*Box::leak(s.clone().into_boxed_str()))
            .collect();
        p.extend([
            (0, "LINE"),
            (8, "Walls"),
            (10, leaked[0]),
            (20, leaked[1]),
            (30, "0"),
            (11, leaked[2]),
            (21, leaked[3]),
            (31, "0"),
        ]);
    }
    p.extend([
        (0, "LWPOLYLINE"),
        (8, "Walls"),
        (90, "4"),
        (70, "1"),
        (38, "1"),
        (10, "5"),
        (20, "0"),
        (42, "0"),
        (10, "6"),
        (20, "0"),
        (42, "1"),
        (10, "6"),
        (20, "1"),
        (42, "0"),
        (10, "5"),
        (20, "1"),
        (42, "1"),
        (0, "CIRCLE"),
        (8, "Furniture"),
        (10, "2"),
        (20, "1.5"),
        (30, "0"),
        (40, "0.5"),
        (0, "3DFACE"),
        (8, "Furniture"),
        (10, "0"),
        (20, "0"),
        (30, "2"),
        (11, "1"),
        (21, "0"),
        (31, "2"),
        (12, "1"),
        (22, "1"),
        (32, "2"),
        (13, "0"),
        (23, "1"),
        (33, "2"),
        (0, "INSERT"),
        (8, "Furniture"),
        (2, "Stool"),
        (10, "10"),
        (20, "0"),
        (30, "0"),
        (0, "INSERT"),
        (8, "Furniture"),
        (2, "Stool"),
        (10, "10"),
        (20, "5"),
        (30, "0"),
        (50, "90"),
        (0, "TEXT"),
        (8, "Walls"),
        (10, "0"),
        (20, "0"),
        (40, "0.2"),
        (1, "Kitchen"),
        (0, "ENDSEC"),
        (0, "EOF"),
    ]);
    dxf_text(&p)
}

#[test]
fn dxf_imports_layers_blocks_arcs_and_faces() {
    let (mut e, storage) = engine();
    storage
        .write(&StorageKey::new("plan.dxf"), sample_dxf().as_bytes())
        .unwrap();
    let out = run(&mut e, "import", json!({ "path": "plan.dxf" }));
    assert_eq!(out["format"], "dxf");
    assert_eq!(out["skipped"], 1, "{out}");
    assert!(out["warnings"].to_string().contains("TEXT"), "{out}");
    let doc = e.document();
    let report = crate::diagnostics::validate(doc);
    assert!(report.is_clean(), "{report:#?}");
    // Layers became tags; meters from $INSUNITS.
    assert!(doc.tags.by_name("Walls").is_some());
    assert!(doc.tags.by_name("Furniture").is_some());
    let (lo, hi) = bounds_of_model(&e);
    // The stool at (10, 0) reaches 0.2 m below the walls.
    assert!(close(lo, DVec3::new(0.0, -200.0, 0.0)), "{lo}");
    assert!(close(hi, DVec3::new(10200.0, 5200.0, 2000.0)), "{hi}");
    // The block is a component used twice, its seat a face.
    let stool = doc.definition_by_name("Stool").expect("the block");
    let seat = doc.container(ContainerRef::Definition(stool)).unwrap();
    assert_eq!(seat.mesh().faces().count(), 1);
    // Faces: the walls rectangle, the slot (two arcs), the circle, the
    // 3DFACE quad and the two stool seats (one definition).
    let faces: usize = doc
        .definitions
        .iter()
        .map(|(id, _)| {
            doc.container(ContainerRef::Definition(id))
                .unwrap()
                .mesh()
                .faces()
                .count()
        })
        .sum();
    assert_eq!(faces, 5, "{out}");
}

#[test]
fn dxf_export_round_trips_faces_edges_and_layers() {
    let (mut e, storage) = engine();
    chairs_and_box(&mut e, &storage);
    run(&mut e, "tag-create", json!({ "name": "Seats/Main" }));
    run(
        &mut e,
        "tag-assign",
        json!({ "targets": "instance", "tag": "Seats/Main" }),
    );
    let (lo, hi) = bounds_of_model(&e);
    let out = run(
        &mut e,
        "export",
        json!({ "path": "out/room.dxf", "file_units": "cm" }),
    );
    assert_eq!(out["format"], "dxf");
    let text = String::from_utf8(storage.read(&StorageKey::new("out/room.dxf")).unwrap()).unwrap();
    assert!(text.contains("$INSUNITS\n70\n5\n"));
    assert!(
        text.contains("LAYER\n2\nSeats_Main\n"),
        "layer names are sanitized"
    );
    run(
        &mut e,
        "export",
        json!({ "path": "out/again.dxf", "file_units": "cm" }),
    );
    assert_eq!(
        storage.read(&StorageKey::new("out/again.dxf")).unwrap(),
        text.as_bytes(),
        "deterministic"
    );
    let (mut back, back_storage) = engine();
    back_storage
        .write(&StorageKey::new("room.dxf"), text.as_bytes())
        .unwrap();
    let out = run(&mut back, "import", json!({ "path": "room.dxf" }));
    assert_eq!(out["faces"], 18, "{out}");
    let (blo, bhi) = bounds_of_model(&back);
    assert!(close(blo, lo) && close(bhi, hi), "{blo} {bhi}");
    assert!(back.document().tags.by_name("Seats_Main").is_some());
    let report = crate::diagnostics::validate(back.document());
    assert!(report.is_clean(), "{report:#?}");
}

#[test]
fn random_dxf_files_never_panic_the_importer() {
    let base = sample_dxf();
    let lines: Vec<&str> = base.lines().collect();
    let mut state = 0x0bad_5eed_u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let values = [
        "0", "-1", "1e300", "nan", "x", "LINE", "SEQEND", "VERTEX", "999", "",
    ];
    for round in 0..300 {
        let mut damaged: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        for _ in 0..(1 + round % 6) {
            let at = next() as usize % damaged.len();
            damaged[at] = values[next() as usize % values.len()].to_string();
        }
        if round % 7 == 0 {
            damaged.truncate(next() as usize % damaged.len());
        }
        let (mut e, storage) = engine();
        storage
            .write(&StorageKey::new("d.dxf"), damaged.join("\n").as_bytes())
            .unwrap();
        if e.execute_erased("import", json!({ "path": "d.dxf" }))
            .is_ok()
        {
            let report = crate::diagnostics::validate(e.document());
            assert!(!report.has_errors(), "round {round}: {report:#?}");
        }
    }
}

/// A 1 m cube in front of a taller box half hidden behind it, seen from
/// the front with a parallel camera 3 m high centered on x 750, z 1000.
fn boxes_front_view() -> (Engine, Arc<MemoryStorage>) {
    let (mut e, storage) = engine();
    for (a, b, h) in [
        ([0, 0, 0], [1000, 1000, 0], 1000.0),
        ([500, 2000, 0], [1500, 3000, 0], 2000.0),
    ] {
        run(&mut e, "rectangle", json!({ "corner_a": a, "corner_b": b }));
        run(
            &mut e,
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": h }),
        );
        run(&mut e, "make-group", json!({ "targets": "face | edge" }));
    }
    e.set_camera(crate::document::Camera {
        eye: DVec3::new(750.0, -5000.0, 1000.0),
        target: DVec3::new(750.0, 0.0, 1000.0),
        up: DVec3::Z,
        projection: crate::document::Projection::Parallel { height_mm: 3000.0 },
        aspect: 1.0,
    });
    (e, storage)
}

#[test]
fn view_drawings_remove_hidden_lines() {
    use crate::io::export::view2d::{LineKind, Paper, draw_view};
    let (e, _) = boxes_front_view();
    let doc = e.document();
    let d = draw_view(
        doc,
        &crate::io::export::ExportScope::default(),
        &doc.camera,
        Paper {
            scale: Some(0.1),
            width_mm: 297.0,
        },
    );
    assert!((d.width_mm - 300.0).abs() < 1e-9 && (d.height_mm - 300.0).abs() < 1e-9);
    // Model length of the visible lines: the cube's square (4 m) and the
    // box's outline but the part behind the cube (1 + 2 + 1 + 0.5 m).
    let total: f64 = d
        .lines
        .iter()
        .map(|(a, b, _)| (*b - *a).length() * 10.0)
        .sum();
    assert!((total - 8500.0).abs() < 1.0, "{total}: {:?}", d.lines);
    assert!(d.lines.iter().all(|(_, _, k)| *k == LineKind::Profile));
    // Nothing of the box shows inside the cube's square (paper x 75–175,
    // y 50–150 mm).
    for (a, b, _) in &d.lines {
        let m = (*a + *b) / 2.0;
        let inside = m.x > 75.5 && m.x < 174.5 && m.y > 50.5 && m.y < 149.5;
        assert!(!inside, "{a} {b}");
    }
}

#[test]
fn views_export_as_svg_pdf_and_2d_dxf() {
    let (mut e, storage) = boxes_front_view();
    let out = run(
        &mut e,
        "export",
        json!({ "path": "v/front.svg", "scale": "1:10" }),
    );
    assert_eq!(out["format"], "svg");
    let svg = String::from_utf8(storage.read(&StorageKey::new("v/front.svg")).unwrap()).unwrap();
    assert!(svg.contains("width=\"300mm\" height=\"300mm\""), "{svg}");
    assert!(svg.contains("stroke-width=\"0.5\""));
    run(&mut e, "export", json!({ "path": "v/front.pdf" }));
    let pdf = storage.read(&StorageKey::new("v/front.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    run(&mut e, "export", json!({ "path": "v/again.pdf" }));
    assert_eq!(
        storage.read(&StorageKey::new("v/again.pdf")).unwrap(),
        pdf,
        "deterministic"
    );
    let out = run(
        &mut e,
        "export",
        json!({ "path": "v/front.dxf", "two_d": true, "scale": "1:10" }),
    );
    assert_eq!(out["format"], "dxf");
    let dxf = String::from_utf8(storage.read(&StorageKey::new("v/front.dxf")).unwrap()).unwrap();
    assert!(dxf.contains("LINE\n8\nProfiles\n"));
    assert!(!dxf.contains("3DFACE"));
    // The 2D DXF is in model units of the view plane: re-imported, its
    // lines span the 3 m frame's drawing (2.25 m wide, 2 m high).
    let (mut back, back_storage) = engine();
    back_storage
        .write(&StorageKey::new("front.dxf"), dxf.as_bytes())
        .unwrap();
    run(&mut back, "import", json!({ "path": "front.dxf" }));
    let (lo, hi) = bounds_of_model(&back);
    assert!(close(hi - lo, DVec3::new(1500.0, 2000.0, 0.0)), "{lo} {hi}");

    // A scene's camera: from above, the cube and the box do not overlap.
    e.set_camera(crate::document::Camera {
        eye: DVec3::new(750.0, 1500.0, 9000.0),
        target: DVec3::new(750.0, 1500.0, 0.0),
        up: DVec3::Y,
        projection: crate::document::Projection::Parallel { height_mm: 4000.0 },
        aspect: 1.0,
    });
    run(&mut e, "scene-add", json!({ "name": "Plan" }));
    e.set_camera(crate::document::Camera::default());
    let out = run(
        &mut e,
        "export",
        json!({ "path": "v/plan.svg", "scene": "Plan", "scale": "1:20" }),
    );
    assert_eq!(out["entities"], 8, "{out}");
    assert!(
        e.execute_erased("export", json!({ "path": "v/x.svg", "scene": "Nope" }))
            .is_err()
    );
    assert!(
        e.execute_erased("export", json!({ "path": "v/x.svg", "scale": "1:0" }))
            .is_err()
    );
}

#[test]
fn perspective_views_draw_what_the_eye_sees() {
    let (mut e, storage) = boxes_front_view();
    e.set_camera(crate::document::Camera::default());
    let out = run(&mut e, "export", json!({ "path": "p.svg" }));
    let svg = String::from_utf8(storage.read(&StorageKey::new("p.svg")).unwrap()).unwrap();
    assert!(svg.contains("width=\"297mm\""), "{svg}");
    // Of the 24 edges, the hidden ones (at least the three behind each box)
    // are gone.
    let lines = out["entities"].as_u64().unwrap();
    assert!((9..=24).contains(&lines), "{out}");
}

#[test]
fn shared_shading_normals_make_soft_edges() {
    // Two quads meeting at 60° along x (Z up, kept with `swap_yz: false`).
    let geometry = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 1 1.5 0.866\nv 0 1.5 0.866\n";
    let smooth = format!(
        "{geometry}vn 0 0 1\nvn 0 -0.5 0.866\nvn 0 -0.866 0.5\n\
         f 1//1 2//1 3//2 4//2\nf 4//2 3//2 5//3 6//3\n"
    );
    let flat = format!(
        "{geometry}vn 0 0 1\nvn 0 -0.866 0.5\n\
         f 1//1 2//1 3//1 4//1\nf 4//2 3//2 5//2 6//2\n"
    );
    for (text, soft) in [(smooth, true), (flat, false)] {
        let (mut e, storage) = engine();
        storage
            .write(&StorageKey::new("s.obj"), text.as_bytes())
            .unwrap();
        let out = run(
            &mut e,
            "import",
            json!({ "path": "s.obj", "swap_yz": false }),
        );
        assert_eq!(out["faces"], 2, "{out}");
        let doc = e.document();
        let shared = doc
            .definitions
            .iter()
            .flat_map(|(id, _)| {
                let mesh = doc.container(ContainerRef::Definition(id)).unwrap().mesh();
                mesh.edges()
                    .filter(|(e, _)| matches!(mesh.edge_faces(*e), Ok([Some(_), Some(_)])))
                    .map(|(_, edge)| edge.flags.contains(crate::geom::EdgeFlags::SOFT))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(shared, vec![soft], "smooth source: {soft}");
    }
}

/// Scenes export as glTF cameras and import back as scenes with the same
/// views (docs/09 §6); a second import gets unique names.
#[test]
fn gltf_cameras_round_trip_as_scenes() {
    let (mut e, storage) = engine();
    chairs_and_box(&mut e, &storage);
    run(
        &mut e,
        "camera-set",
        json!({ "projection": "perspective", "eye": [3000, -2500, 1800],
                "target": [500, 400, 300], "up": [0, 0, 1], "fov": 0.6981317 }),
    );
    run(&mut e, "scene-add", json!({ "name": "Corner" }));
    run(
        &mut e,
        "camera-set",
        json!({ "projection": "parallel", "eye": [400, 300, 5000],
                "target": [400, 300, 0], "up": [0, 1, 0], "height": 2400.0 }),
    );
    run(&mut e, "scene-add", json!({ "name": "Plan" }));
    let views: Vec<(String, crate::document::Camera)> = e
        .document()
        .scenes
        .iter()
        .map(|(_, s)| (s.name.clone(), s.camera.unwrap()))
        .collect();
    assert_eq!(views.len(), 2);

    run(&mut e, "export", json!({ "path": "out/scene.glb" }));
    let glb = storage.read(&StorageKey::new("out/scene.glb")).unwrap();
    let parsed = gltf::Gltf::from_slice(&glb).expect("a valid GLB");
    assert_eq!(parsed.document.cameras().count(), 2);

    let (mut back, back_storage) = engine();
    back_storage
        .write(&StorageKey::new("scene.glb"), &glb)
        .unwrap();
    let out = run(&mut back, "import", json!({ "path": "scene.glb" }));
    assert_eq!(out["scenes"], 2, "{out}");
    let doc = back.document();
    let frame = |c: &crate::document::Camera| {
        let forward = (c.target - c.eye).normalize();
        let right = forward.cross(c.up).normalize();
        (forward, right.cross(forward))
    };
    for (name, view) in &views {
        let scene = doc
            .scenes
            .by_name(name)
            .and_then(|id| doc.scenes.get(id))
            .unwrap_or_else(|| panic!("scene {name}"));
        let camera = scene.camera.unwrap();
        // glTF stores f32 meters.
        assert!(
            camera.eye.distance(view.eye) < 1.0,
            "{name}: {}",
            camera.eye
        );
        let ((f0, u0), (f1, u1)) = (frame(view), frame(&camera));
        assert!(f0.dot(f1) > 0.99999 && u0.dot(u1) > 0.99999, "{name}");
        match (view.projection, camera.projection) {
            (
                Projection::Perspective { fov_y_deg: a },
                Projection::Perspective { fov_y_deg: b },
            ) => {
                assert!((a - b).abs() < 1e-3, "{name}: {a} {b}");
            }
            (Projection::Parallel { height_mm: a }, Projection::Parallel { height_mm: b }) => {
                assert!((a - b).abs() < 1e-2, "{name}: {a} {b}");
            }
            other => panic!("{name}: {other:?}"),
        }
    }
    let out = run(&mut back, "import", json!({ "path": "scene.glb" }));
    assert_eq!(out["scenes"], 2);
    assert!(back.document().scenes.by_name("Corner (2)").is_some());
}

/// docs/00 §6 quality targets that the core alone decides: native save and
/// load, undo and redo of a small change, and inference latency, on a
/// terrain of `PLANYRITH_BENCH_CELLS`² cells (two triangles each; the
/// default 707 gives one million). Run with `cargo test --release -p
/// planyrith-core quality_targets_benchmark -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark"]
fn quality_targets_benchmark() {
    use crate::inference::{Constraint, InferenceEngine, InferenceRequest, ViewMetrics};
    use crate::io::NoProgress;
    use crate::io::native::{WriteOptions, read_document, write_document};
    use crate::spatial::DocumentIndex;
    use std::time::Instant;
    let n = std::env::var("PLANYRITH_BENCH_CELLS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(707);
    let best = |runs: usize, mut f: Box<dyn FnMut() -> f64 + '_>| {
        (0..runs).map(|_| f()).fold(f64::MAX, f64::min)
    };
    let (mut e, storage) = engine();
    // `PLANYRITH_BENCH_FILE` measures a native file instead of the terrain.
    if let Some(path) = std::env::var_os("PLANYRITH_BENCH_FILE") {
        let bytes = crate::io::storage::NativeFs::new()
            .read(&StorageKey::new(path.to_string_lossy()))
            .unwrap();
        let loaded = read_document(&bytes, &NoProgress).unwrap();
        e = Engine::new(loaded.document, Arc::new(CommandRegistry::with_builtins()));
        let counts = e.document().model.counts();
        eprintln!(
            "model: {} ({} definitions, model faces {})",
            path.to_string_lossy(),
            e.document().definitions.len(),
            counts.faces
        );
    } else {
        storage
            .write(&StorageKey::new("t.glb"), &terrain_glb(n))
            .unwrap();
        let started = Instant::now();
        let out = run(&mut e, "import", json!({ "path": "t.glb" }));
        eprintln!(
            "model: {} faces, {} edges ({} triangles); import {:.2} s",
            out["faces"],
            out["edges"],
            2 * n * n,
            started.elapsed().as_secs_f64()
        );
    }

    // Save and load (target: 100 MB in < 3 s).
    let mut bytes = Vec::new();
    let save = best(
        3,
        Box::new(|| {
            let t = Instant::now();
            bytes =
                write_document(e.document(), &[], &WriteOptions::default(), &NoProgress).unwrap();
            t.elapsed().as_secs_f64()
        }),
    );
    let load = best(
        3,
        Box::new(|| {
            let t = Instant::now();
            read_document(&bytes, &NoProgress).unwrap();
            t.elapsed().as_secs_f64()
        }),
    );
    eprintln!(
        "native file: {:.1} MB; save {save:.2} s, load {load:.2} s (target < 3 s per 100 MB)",
        bytes.len() as f64 / 1e6
    );

    // Undo and redo of one line (target: proportional to the change).
    let mut undo = f64::MAX;
    let mut redo = f64::MAX;
    for k in 0..5 {
        let x = f64::from(k) * 10.0;
        run(
            &mut e,
            "line",
            json!({ "points": [[x, -500.0, 0.0], [x, -400.0, 0.0]] }),
        );
        let t = Instant::now();
        e.undo().unwrap();
        undo = undo.min(t.elapsed().as_secs_f64());
        let t = Instant::now();
        e.redo().unwrap();
        redo = redo.min(t.elapsed().as_secs_f64());
    }
    eprintln!(
        "one-line change: undo {:.1} µs, redo {:.1} µs",
        undo * 1e6,
        redo * 1e6
    );

    // Inference over a sweep of the cursor across the terrain (target: < 4 ms).
    let doc = e.document();
    let t = Instant::now();
    let index = DocumentIndex::build(doc);
    let build = t.elapsed().as_secs_f64();
    let engine = InferenceEngine::default();
    let mut camera = doc.camera;
    camera.zoom_extents(&doc.aabb(), 0.05);
    let view = ViewMetrics::for_camera(&camera, 1080.0);
    let mut slowest: Vec<(f64, usize, String)> = Vec::new();
    let mut times: Vec<f64> = (0..400)
        .filter_map(|i| {
            let (x, y) = (
                f64::from(i % 20) / 10.0 - 0.95,
                f64::from(i / 20) / 10.0 - 0.95,
            );
            let req = InferenceRequest {
                ray: camera.ray_through_ndc(glam::DVec2::new(x, y)),
                pixel_tolerance: InferenceRequest::DEFAULT_PIXEL_TOLERANCE,
                view,
                constraint: Constraint::Free,
                reference_point: None,
                last_point: None,
            };
            let t = Instant::now();
            let found = engine.infer(doc, &index, &req);
            let secs = t.elapsed().as_secs_f64();
            if std::env::var_os("PLANYRITH_BENCH_PARTS").is_some() {
                let all = engine.candidates(doc, &index, &req);
                let n = all.len();
                let mut points: Vec<[i64; 3]> = all
                    .iter()
                    .map(|c| (c.point * 10.0).round().as_i64vec3().to_array())
                    .collect();
                points.sort_unstable();
                points.dedup();
                let mut paths: Vec<String> = all
                    .iter()
                    .filter_map(|c| c.on.as_ref().map(|p| format!("{:?}", p.instances)))
                    .collect();
                paths.sort_unstable();
                paths.dedup();
                slowest.push((
                    secs,
                    n,
                    format!(
                        "{:?}; {} distinct points, {} occurrences",
                        found.kind,
                        points.len(),
                        paths.len()
                    ),
                ));
            }
            // Moves over the model only: a ray past it costs nothing.
            (found.kind != crate::inference::InferenceKind::Free).then_some(secs)
        })
        .collect();
    times.sort_by(f64::total_cmp);
    slowest.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (secs, n, kind) in slowest.iter().take(3) {
        eprintln!("  slow move: {:.2} ms, {n} candidates, {kind}", secs * 1e3);
    }
    if std::env::var_os("PLANYRITH_BENCH_PARTS").is_some() {
        let t = Instant::now();
        for _ in 0..20 {
            std::hint::black_box(doc.aabb());
        }
        eprintln!(
            "  doc.aabb: {:.2} ms",
            t.elapsed().as_secs_f64() * 1e3 / 20.0
        );
        for (d, _) in doc.definitions.iter() {
            let c = crate::document::ContainerRef::Definition(d);
            let Some(entities) = doc.container(c) else {
                continue;
            };
            let mesh = entities.mesh();
            let t = Instant::now();
            let mut n = 0;
            for (f, _) in mesh.faces() {
                if crate::geom::tessellate::tessellate_face(mesh, f, doc.tolerances()).is_ok() {
                    n += 1;
                }
            }
            let tess = t.elapsed().as_secs_f64();
            let t = Instant::now();
            let _ = crate::io::render_mesh::extract(doc, c, Default::default());
            eprintln!(
                "  {n} faces: tessellation {:.0} ms, whole extraction {:.0} ms",
                tess * 1e3,
                t.elapsed().as_secs_f64() * 1e3
            );
        }
        let ray = camera.ray_through_ndc(glam::DVec2::ZERO);
        for (d, _) in doc.definitions.iter() {
            let c = crate::document::ContainerRef::Definition(d);
            let (Some(ci), Some(entities)) = (index.container(c), doc.container(c)) else {
                continue;
            };
            for (faces, edges, vertices) in [
                (true, false, false),
                (false, true, false),
                (false, false, true),
            ] {
                let filter = crate::spatial::HitFilter {
                    proximity: 1.0,
                    faces,
                    edges,
                    vertices,
                    ..Default::default()
                };
                let t = Instant::now();
                let hits = ci.ray_cast_local(entities.mesh(), &ray, &filter, doc.tolerances());
                eprintln!(
                    "  local f{faces} e{edges} v{vertices}: {:.3} ms, {} hits",
                    t.elapsed().as_secs_f64() * 1e3,
                    hits.len()
                );
            }
        }
        for proximity in [1.0, 10.0, 100.0] {
            let filter = crate::spatial::HitFilter {
                proximity,
                ..Default::default()
            };
            let t = Instant::now();
            let hits = index.ray_cast(doc, &ray, &filter);
            eprintln!(
                "  ray_cast proximity {proximity} mm: {:.2} ms, {} hits",
                t.elapsed().as_secs_f64() * 1e3,
                hits.len()
            );
        }
    }
    eprintln!(
        "inference: index {build:.2} s; {} moves over the model: median {:.2} ms, 95th percentile {:.2} ms, \
         worst {:.2} ms (target < 4 ms)",
        times.len(),
        times[times.len() / 2] * 1e3,
        times[times.len() * 95 / 100] * 1e3,
        times[times.len() - 1] * 1e3
    );
}

/// docs/00 §6 "core document memory ≤ 2× the on-disk uncompressed size":
/// the resident memory a loaded document adds, against the uncompressed
/// size of its native file's parts. Linux (reads `/proc/self/statm`); the
/// same model variables as [`quality_targets_benchmark`]. Run alone:
/// `cargo test --release -p planyrith-core memory_benchmark -- --ignored
/// --nocapture --test-threads 1`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "benchmark"]
fn memory_benchmark() {
    use crate::io::NoProgress;
    use crate::io::native::{WriteOptions, read_document, write_document};
    let resident = || {
        let statm = crate::io::storage::NativeFs::new()
            .read(&StorageKey::new("/proc/self/statm"))
            .unwrap();
        let pages: u64 = String::from_utf8_lossy(&statm)
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse().ok())
            .unwrap();
        pages * 4096
    };
    let bytes = match std::env::var_os("PLANYRITH_BENCH_FILE") {
        Some(path) => crate::io::storage::NativeFs::new()
            .read(&StorageKey::new(path.to_string_lossy()))
            .unwrap(),
        None => {
            let n = std::env::var("PLANYRITH_BENCH_CELLS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(707);
            let (mut e, storage) = engine();
            storage
                .write(&StorageKey::new("t.glb"), &terrain_glb(n))
                .unwrap();
            run(&mut e, "import", json!({ "path": "t.glb" }));
            write_document(e.document(), &[], &WriteOptions::default(), &NoProgress).unwrap()
        }
    };
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes[..])).unwrap();
    let mut uncompressed = 0u64;
    let mut parts = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).unwrap();
        uncompressed += entry.size();
        parts.push((
            entry.name().map(|n| n.into_owned()).unwrap_or_default(),
            entry.size(),
        ));
    }
    parts.sort_by_key(|p| std::cmp::Reverse(p.1));
    let before = resident();
    let loaded = read_document(&bytes, &NoProgress).unwrap();
    let after = resident();
    let used = after.saturating_sub(before);
    eprintln!(
        "file {:.1} MB, uncompressed {:.1} MB; document adds {:.1} MB resident = {:.2}× \
         (target ≤ 2×); largest parts: {}",
        bytes.len() as f64 / 1e6,
        uncompressed as f64 / 1e6,
        used as f64 / 1e6,
        used as f64 / uncompressed as f64,
        parts
            .iter()
            .take(3)
            .map(|(n, s)| format!("{n} {:.1} MB", *s as f64 / 1e6))
            .collect::<Vec<_>>()
            .join(", ")
    );
    std::hint::black_box(&loaded);
}

/// docs/09 §5 3MF: a colored box goes out as one welded, closed object in
/// centimeters and comes back as a solid of the same size and color; an
/// open tray is written with `W2007`.
#[test]
fn three_mf_round_trips_a_colored_solid() {
    let (mut e, storage) = engine();
    run(
        &mut e,
        "material-create",
        json!({ "name": "Oak", "color": "#A0522D" }),
    );
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [600, 400, 0] }),
    );
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face & last" }, "distance": 300 }),
    );
    run(
        &mut e,
        "material-apply",
        json!({ "targets": "face", "material": "Oak" }),
    );
    run(
        &mut e,
        "make-group",
        json!({ "targets": "all", "name": "Crate" }),
    );
    let out = run(
        &mut e,
        "export",
        json!({ "path": "crate.3mf", "file_units": "cm" }),
    );
    assert_eq!(out["format"], "3mf");
    assert_eq!(out["entities"], 1);
    assert_eq!(out["warnings"], json!([]));
    let bytes = storage.read(&StorageKey::new("crate.3mf")).unwrap();
    let (mut e, storage) = engine();
    storage
        .write(&StorageKey::new("crate.3mf"), &bytes)
        .unwrap();
    let out = run(&mut e, "import", json!({ "path": "crate.3mf" }));
    assert_eq!(out["format"], "3mf");
    assert_eq!(out["faces"], 6, "coplanar triangles merge back into faces");
    let (faces, _, lo, hi) = imported(&e);
    assert_eq!(faces, 6);
    assert!(close(lo, DVec3::ZERO), "{lo}");
    assert!(close(hi, DVec3::new(600.0, 400.0, 300.0)), "{hi}");
    let check = run(&mut e, "solid-check", json!({}));
    assert_eq!(check["printable"], true, "{check}");
    let volume = check["objects"][0]["volume_mm3"].as_f64().unwrap();
    assert!((volume - 72_000_000.0).abs() < 1e-3, "{volume}");
    let doc = e.document();
    let oak = doc
        .materials
        .iter()
        .find(|(_, m)| m.name == "Oak")
        .map(|(_, m)| m.color);
    assert_eq!(oak, Some(crate::document::Rgba8::rgb(0xA0, 0x52, 0x2D)));

    // An open tray is still written, with W2007.
    run(&mut e, "delete", json!({ "targets": "all" }));
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [100, 50, 0] }),
    );
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face & last" }, "distance": 20 }),
    );
    run(
        &mut e,
        "delete",
        json!({ "targets": "face & normal:z & plane:xy@20" }),
    );
    let out = run(&mut e, "export", json!({ "path": "tray.3mf" }));
    assert_eq!(out["warnings"][0]["code"], "W2007", "{out}");
}

/// docs/05 §10 printing options: each top-level object in its own file,
/// each lifted onto the build plate; refused where they make no sense.
#[test]
fn per_object_export_puts_each_object_on_the_plate() {
    let (mut e, storage) = engine();
    for (z, name) in [(500, "Raised Box"), (-100, "Sunk/Box")] {
        run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [0, 0, z], "corner_b": [100, 50, z] }),
        );
        run(
            &mut e,
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 20 }),
        );
        run(
            &mut e,
            "make-group",
            json!({ "targets": "edge | face", "name": name }),
        );
    }
    let out = run(
        &mut e,
        "export",
        json!({ "path": "out/parts.3mf", "per_object": true, "on_plate": true }),
    );
    assert_eq!(
        out["files"],
        json!(["out/parts-Raised-Box.3mf", "out/parts-Sunk-Box.3mf"])
    );
    assert_eq!(out["entities"], 2);
    for file in ["out/parts-Raised-Box.3mf", "out/parts-Sunk-Box.3mf"] {
        let bytes = storage.read(&StorageKey::new(file)).unwrap();
        let (mut fresh, store) = engine();
        store.write(&StorageKey::new("part.3mf"), &bytes).unwrap();
        run(&mut fresh, "import", json!({ "path": "part.3mf" }));
        let (faces, _, lo, hi) = imported(&fresh);
        assert_eq!(faces, 6, "{file}");
        assert!(
            lo.z.abs() < 1e-9 && (hi.z - 20.0).abs() < 1e-9,
            "{file}: {lo} {hi}"
        );
    }
    // Without the options one file holds both, where they are.
    let out = run(&mut e, "export", json!({ "path": "all.stl" }));
    assert!(out.get("files").is_none());
    for (params, bad) in [
        (json!({ "path": "a.dxf", "on_plate": true }), "on_plate"),
        (
            json!({ "path": "a.stl", "per_object": true, "targets": "all" }),
            "per_object",
        ),
    ] {
        let err = e.execute_erased("export", params).unwrap_err();
        assert!(err.to_string().contains(bad), "{err}");
    }
}

/// docs/09 Decisions 2026-11-08: glTF carries roughness and metalness; the
/// format's defaults stay unset on import.
#[test]
fn gltf_keeps_surface_factors() {
    let (mut e, storage) = engine();
    for (name, extra) in [
        ("Brass", json!({ "metallic": 0.9, "roughness": 0.25 })),
        ("Paint", json!({})),
    ] {
        let mut params = json!({ "name": name, "color": "#C8A050" });
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        run(&mut e, "material-create", params);
    }
    for (x, material) in [(0, "Brass"), (500, "Paint")] {
        run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [x, 0, 0], "corner_b": [x + 100, 100, 0] }),
        );
        run(
            &mut e,
            "material-apply",
            json!({ "targets": "face & last", "material": material }),
        );
    }
    run(&mut e, "export", json!({ "path": "m.glb" }));
    let bytes = storage.read(&StorageKey::new("m.glb")).unwrap();
    let (mut e, storage) = engine();
    storage.write(&StorageKey::new("m.glb"), &bytes).unwrap();
    run(&mut e, "import", json!({ "path": "m.glb" }));
    let doc = e.document();
    let find = |n: &str| {
        doc.materials
            .iter()
            .find(|(_, m)| m.name == n)
            .map(|(_, m)| m.clone())
    };
    let brass = find("Brass").unwrap();
    assert!((brass.metallic.unwrap() - 0.9).abs() < 1e-6);
    assert!((brass.roughness.unwrap() - 0.25).abs() < 1e-6);
    let paint = find("Paint").unwrap();
    assert_eq!((paint.metallic, paint.roughness), (None, None));
}

/// docs/06 §4.1: `style-edit` turns the realistic look on with its
/// defaults, tunes it, and off again; out-of-range values are refused.
#[test]
fn style_edit_switches_the_realistic_look() {
    let (mut e, _) = engine();
    let realism = |e: &Engine| e.document().styles.active_style().and_then(|s| s.realism);
    run(&mut e, "style-edit", json!({ "exposure": 1.0 }));
    let r = realism(&e).unwrap();
    assert!((r.exposure - 1.0).abs() < 1e-6 && (r.sky_light - 0.6).abs() < 1e-6);
    run(
        &mut e,
        "style-edit",
        json!({ "realistic": false, "sky_light": 0.2 }),
    );
    assert!(realism(&e).is_none());
    for bad in [
        json!({ "exposure": 4.0 }),
        json!({ "ambient_occlusion": -0.1 }),
    ] {
        assert!(e.execute_erased("style-edit", bad).is_err());
    }
    assert!(
        e.execute_erased(
            "material-create",
            json!({ "name": "X", "color": "#FFFFFF", "metallic": 2.0 })
        )
        .is_err()
    );
}

/// docs/09 §5 PLY: a painted box goes out (binary and ASCII) and comes back
/// with its size, faces and color.
#[test]
fn ply_round_trips_a_painted_box() {
    for ascii in [false, true] {
        let (mut e, storage) = engine();
        run(
            &mut e,
            "material-create",
            json!({ "name": "Teal", "color": "#108080" }),
        );
        run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [300, 200, 0] }),
        );
        run(
            &mut e,
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 100 }),
        );
        run(
            &mut e,
            "material-apply",
            json!({ "targets": "face", "material": "Teal" }),
        );
        let out = run(
            &mut e,
            "export",
            json!({ "path": "box.ply", "ascii": ascii }),
        );
        assert_eq!(out["format"], "ply");
        assert_eq!(out["entities"], 12, "triangles");
        let bytes = storage.read(&StorageKey::new("box.ply")).unwrap();
        assert_eq!(bytes.starts_with(b"ply\nformat ascii"), ascii);
        let (mut e, storage) = engine();
        storage.write(&StorageKey::new("box.ply"), &bytes).unwrap();
        let out = run(&mut e, "import", json!({ "path": "box.ply" }));
        assert_eq!(out["format"], "ply");
        let (faces, _, lo, hi) = imported(&e);
        assert_eq!(faces, 6);
        assert!(
            close(lo, DVec3::ZERO) && close(hi, DVec3::new(300.0, 200.0, 100.0)),
            "{lo} {hi}"
        );
        let teal = e
            .document()
            .materials
            .iter()
            .any(|(_, m)| m.color == crate::document::Rgba8::rgb(0x10, 0x80, 0x80));
        assert!(teal);
    }
}

/// docs/09 §5 USD: a painted box as a text stage with its unit, Z up, a
/// mesh and a preview-surface material, and packed as USDZ.
#[test]
fn usd_writes_a_stage_and_a_package() {
    let (mut e, storage) = engine();
    run(
        &mut e,
        "material-create",
        json!({ "name": "Brass rod", "color": "#C8A050", "metallic": 1.0, "roughness": 0.3 }),
    );
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [300, 200, 0] }),
    );
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face & last" }, "distance": 100 }),
    );
    run(
        &mut e,
        "material-apply",
        json!({ "targets": "face & normal:z & plane:xy@100", "material": "Brass rod" }),
    );
    let out = run(
        &mut e,
        "export",
        json!({ "path": "box.usda", "file_units": "cm" }),
    );
    assert_eq!(out["format"], "usda");
    assert_eq!(out["entities"], 12);
    let text = String::from_utf8(storage.read(&StorageKey::new("box.usda")).unwrap()).unwrap();
    for needle in [
        "#usda 1.0",
        "metersPerUnit = 0.01",
        "upAxis = \"Z\"",
        "def Mesh \"Model_1\"",
        "def Mesh \"Model_2\"",
        "def Material \"Brass_rod\"",
        "float inputs:metallic = 1",
        "float inputs:roughness = 0.3",
        "(30, 20, 10)",
    ] {
        assert!(text.contains(needle), "missing {needle}");
    }
    let out = run(&mut e, "export", json!({ "path": "box.usdz" }));
    assert_eq!(out["format"], "usdz");
    let bytes = storage.read(&StorageKey::new("box.usdz")).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    assert_eq!(archive.by_index(0).unwrap().name().unwrap(), "model.usda");
}

/// docs/09 §5 IFC: two groups become two building element proxies in one
/// storey, with their geometry and colors; the same model gives the same
/// file.
#[test]
fn ifc_writes_one_element_per_top_level_object() {
    let (mut e, storage) = engine();
    run(
        &mut e,
        "material-create",
        json!({ "name": "Concrete", "color": "#A0A0A0" }),
    );
    for (x, name) in [(0, "Wall"), (2000, "Slab")] {
        run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [x, 0, 0], "corner_b": [x + 1000, 200, 0] }),
        );
        run(
            &mut e,
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 300 }),
        );
        run(
            &mut e,
            "make-group",
            json!({ "targets": "edge | face", "name": name }),
        );
    }
    run(
        &mut e,
        "material-apply",
        json!({ "targets": "group & name:Wall", "material": "Concrete" }),
    );
    // The wall's class from its tag, the slab's from an attribute.
    run(&mut e, "tag-create", json!({ "name": "Muri" }));
    run(
        &mut e,
        "tag-assign",
        json!({ "targets": "group & name:Wall", "tag": "Muri" }),
    );
    #[cfg(feature = "scripting")]
    run(
        &mut e,
        "attribute-set",
        json!({ "targets": "group & name:Slab", "dictionary": "ifc", "key": "class", "value": "IfcSlab" }),
    );
    let out = run(&mut e, "export", json!({ "path": "site.ifc" }));
    assert_eq!(out["format"], "ifc");
    assert_eq!(out["entities"], 24);
    let text = String::from_utf8(storage.read(&StorageKey::new("site.ifc")).unwrap()).unwrap();
    assert!(text.starts_with("ISO-10303-21;"));
    assert!(text.contains("FILE_SCHEMA(('IFC4'));"));
    assert_eq!(text.matches("IFCWALL(").count(), 1, "{text}");
    #[cfg(feature = "scripting")]
    assert_eq!(text.matches("IFCSLAB(").count(), 1);
    assert_eq!(text.matches("IFCTRIANGULATEDFACESET(").count(), 2);
    assert!(text.contains("'Wall'") && text.contains("'Slab'"));
    assert!(text.contains("IFCSURFACESTYLE('Concrete'"));
    run(&mut e, "export", json!({ "path": "copy/site.ifc" }));
    assert_eq!(
        storage.read(&StorageKey::new("copy/site.ifc")).unwrap(),
        text.as_bytes(),
        "the same model gives the same file"
    );
}

/// docs/09 Decisions 2026-11-08: a textured material's image goes into
/// the package, or into `<stem>_textures/` beside a text stage.
#[test]
fn usd_carries_textures() {
    let (mut e, storage) = engine();
    let png = {
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(4, 4, image::Rgb([160, 82, 45]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    };
    storage.write(&StorageKey::new("oak.png"), &png).unwrap();
    run(
        &mut e,
        "material-create",
        json!({ "name": "Oak", "color": "#A0522D", "texture": "oak.png" }),
    );
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [300, 200, 0] }),
    );
    run(
        &mut e,
        "material-apply",
        json!({ "targets": "face", "material": "Oak" }),
    );
    run(&mut e, "export", json!({ "path": "out/desk.usda" }));
    let text = String::from_utf8(storage.read(&StorageKey::new("out/desk.usda")).unwrap()).unwrap();
    assert!(
        text.contains("asset inputs:file = @desk_textures/Oak.png@"),
        "{text}"
    );
    assert!(text.contains("texCoord2f[] primvars:st"));
    assert_eq!(
        storage
            .read(&StorageKey::new("out/desk_textures/Oak.png"))
            .unwrap(),
        png
    );
    run(&mut e, "export", json!({ "path": "desk.usdz" }));
    let bytes = storage.read(&StorageKey::new("desk.usdz")).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    assert_eq!(
        archive.by_index(1).unwrap().name().unwrap(),
        "textures/Oak.png"
    );
}

/// docs/09 Decisions 2026-11-08: an `ifc`/`storey` attribute puts an
/// element on a storey of its own, at the element's lowest point.
#[cfg(feature = "scripting")]
#[test]
fn ifc_storeys_come_from_an_attribute() {
    let (mut e, storage) = engine();
    for (z, name) in [(0, "Ground"), (3000, "Upper")] {
        run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [0, 0, z], "corner_b": [4000, 3000, z] }),
        );
        run(
            &mut e,
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 200 }),
        );
        run(
            &mut e,
            "make-group",
            json!({ "targets": "edge | face", "name": name }),
        );
    }
    run(
        &mut e,
        "attribute-set",
        json!({ "targets": "group & name:Upper", "dictionary": "ifc", "key": "storey", "value": "Piano 1" }),
    );
    run(&mut e, "export", json!({ "path": "two.ifc" }));
    let text = String::from_utf8(storage.read(&StorageKey::new("two.ifc")).unwrap()).unwrap();
    assert_eq!(text.matches("IFCBUILDINGSTOREY(").count(), 2);
    assert!(
        text.contains(",'Piano 1',$,$,") && text.contains(",.ELEMENT.,3000.)"),
        "{text}"
    );
    assert_eq!(
        text.matches("IFCRELCONTAINEDINSPATIALSTRUCTURE(").count(),
        2
    );
    // Closed boxes carry their volume (4 × 3 × 0.2 m) and a property set.
    assert_eq!(
        text.matches("IFCQUANTITYVOLUME('GrossVolume',$,$,2.4,$)")
            .count(),
        2,
        "{text}"
    );
    assert!(text.contains("IFCPROPERTYSINGLEVALUE('Material',$,IFCLABEL('Default'),$)"));
}

/// docs/09 §5 COLLADA: a component placed twice and a textured box go out
/// with shared geometry and come back with their sizes, placements and
/// texture.
#[test]
fn collada_round_trips_components_and_textures() {
    let (mut e, storage) = engine();
    let png = {
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(4, 4, image::Rgb([160, 82, 45]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    };
    storage.write(&StorageKey::new("oak.png"), &png).unwrap();
    run(
        &mut e,
        "material-create",
        json!({ "name": "Oak", "color": "#A0522D", "texture": "oak.png" }),
    );
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [300, 200, 0] }),
    );
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face & last" }, "distance": 100 }),
    );
    run(
        &mut e,
        "material-apply",
        json!({ "targets": "face", "material": "Oak" }),
    );
    run(
        &mut e,
        "make-component",
        json!({ "targets": "all", "name": "Block" }),
    );
    run(
        &mut e,
        "move",
        json!({ "targets": "instance", "delta": [1000, 0, 0], "copy": true }),
    );
    let out = run(&mut e, "export", json!({ "path": "out/blocks.dae" }));
    assert_eq!(out["format"], "dae");
    let text =
        String::from_utf8(storage.read(&StorageKey::new("out/blocks.dae")).unwrap()).unwrap();
    assert_eq!(
        text.matches("<geometry ").count(),
        1,
        "the component's geometry is shared"
    );
    assert_eq!(text.matches("<node ").count(), 2);
    assert!(
        storage
            .read(&StorageKey::new("out/blocks_textures/Oak.png"))
            .is_ok()
    );

    let (mut e2, storage2) = engine();
    let bytes = storage.read(&StorageKey::new("out/blocks.dae")).unwrap();
    storage2
        .write(&StorageKey::new("in/blocks.dae"), &bytes)
        .unwrap();
    storage2
        .write(&StorageKey::new("in/blocks_textures/Oak.png"), &png)
        .unwrap();
    let out = run(&mut e2, "import", json!({ "path": "in/blocks.dae" }));
    assert_eq!(out["format"], "dae");
    assert_eq!(out["faces"], 6, "faces counted once per definition");
    let doc = e2.document();
    let oak = doc
        .materials
        .iter()
        .find(|(_, m)| m.name == "Oak")
        .unwrap()
        .1;
    assert!(oak.texture.is_some(), "the texture came back");
    let check = run(&mut e2, "solid-check", json!({}));
    let objects = check["objects"].as_array().unwrap();
    assert_eq!(objects.len(), 2);
    for o in objects {
        assert_eq!(o["size_mm"], json!([300.0, 200.0, 100.0]));
        assert_eq!(o["printable"], true);
    }
}

/// docs/09 §5 IFC import: elements come back as groups tagged with their
/// class and carrying `ifc` attributes, so a second export keeps classes
/// and storeys; a multi-part element stays one group.
#[test]
fn ifc_round_trips_classes_storeys_and_parts() {
    let (mut e, storage) = engine();
    for (x, name, tag) in [(0, "Wall", "Walls"), (2000, "Table", "Furniture")] {
        run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [x, 0, 0], "corner_b": [x + 1000, 200, 0] }),
        );
        run(
            &mut e,
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 500 }),
        );
        run(
            &mut e,
            "make-group",
            json!({ "targets": "edge | face", "name": name }),
        );
        run(&mut e, "tag-create", json!({ "name": tag }));
        run(
            &mut e,
            "tag-assign",
            json!({ "targets": format!("group & name:{name}"), "tag": tag }),
        );
    }
    run(&mut e, "export", json!({ "path": "first.ifc" }));
    let bytes = storage.read(&StorageKey::new("first.ifc")).unwrap();
    let (mut e, storage) = engine();
    storage
        .write(&StorageKey::new("first.ifc"), &bytes)
        .unwrap();
    let out = run(&mut e, "import", json!({ "path": "first.ifc" }));
    assert_eq!(out["format"], "ifc");
    assert!(crate::diagnostics::validate(e.document()).is_clean());
    for tag in ["Wall", "Furniture"] {
        assert!(e.document().tags.by_name(tag).is_some(), "tag {tag}");
    }
    run(&mut e, "export", json!({ "path": "second.ifc" }));
    let text = String::from_utf8(storage.read(&StorageKey::new("second.ifc")).unwrap()).unwrap();
    assert_eq!(text.matches("IFCWALL(").count(), 1, "{text}");
    assert_eq!(text.matches("IFCFURNITURE(").count(), 1);
    assert!(text.contains("'Level 0'"));
}

/// docs/09 §5 FBX: a component placed twice (once turned) and a textured
/// box go out with shared geometry and an embedded texture, and come back
/// with their sizes, placements, material and texture.
#[test]
fn fbx_round_trips_components_and_textures() {
    let (mut e, storage) = engine();
    let png = {
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(4, 4, image::Rgb([160, 82, 45]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    };
    storage.write(&StorageKey::new("oak.png"), &png).unwrap();
    run(
        &mut e,
        "material-create",
        json!({ "name": "Oak", "color": "#A0522D", "texture": "oak.png" }),
    );
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [300, 200, 0] }),
    );
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face & last" }, "distance": 100 }),
    );
    run(
        &mut e,
        "material-apply",
        json!({ "targets": "face", "material": "Oak" }),
    );
    run(
        &mut e,
        "make-component",
        json!({ "targets": "all", "name": "Block" }),
    );
    run(
        &mut e,
        "move",
        json!({ "targets": "instance", "delta": [1000, 0, 0], "copy": true }),
    );
    run(
        &mut e,
        "rotate",
        json!({ "targets": "instance & last", "center": [1000, 0, 0], "axis": [0, 0, 1], "angle": std::f64::consts::FRAC_PI_2 }),
    );
    for (file, opts) in [
        ("out/blocks.fbx", json!({})),
        ("out/blocks-z.fbx", json!({ "z_up": true, "units": "m" })),
    ] {
        let out = run(&mut e, "export", json!({ "path": file, "options": opts }));
        assert_eq!(out["format"], "fbx");
        let bytes = storage.read(&StorageKey::new(file)).unwrap();
        let tree = crate::io::fbx::parse(&bytes).unwrap();
        let objects = tree.node("Objects").unwrap();
        assert_eq!(objects.all("Geometry").count(), 1, "the geometry is shared");
        assert_eq!(objects.all("Model").count(), 2);
        assert_eq!(objects.all("Video").count(), 1, "the texture is embedded");

        let (mut e2, storage2) = engine();
        storage2
            .write(&StorageKey::new("in/blocks.fbx"), &bytes)
            .unwrap();
        let out = run(&mut e2, "import", json!({ "path": "in/blocks.fbx" }));
        assert_eq!(out["format"], "fbx");
        assert_eq!(out["faces"], 6, "faces counted once per definition");
        let doc = e2.document();
        let oak = doc
            .materials
            .iter()
            .find(|(_, m)| m.name == "Oak")
            .unwrap()
            .1;
        assert!(oak.texture.is_some(), "the texture came back");
        assert_eq!([oak.color.r, oak.color.g, oak.color.b], [0xA0, 0x52, 0x2D]);
        let check = run(&mut e2, "solid-check", json!({}));
        let objects = check["objects"].as_array().unwrap();
        assert_eq!(objects.len(), 2);
        let mut sizes: Vec<Vec<f64>> = objects
            .iter()
            .map(|o| {
                let s: Vec<f64> = serde_json::from_value(o["size_mm"].clone()).unwrap();
                s.iter().map(|v| (v * 1000.0).round() / 1000.0).collect()
            })
            .collect();
        sizes.sort_by(|a, b| a[0].total_cmp(&b[0]));
        assert_eq!(
            sizes,
            vec![vec![200.0, 300.0, 100.0], vec![300.0, 200.0, 100.0]],
            "{file}"
        );
        let bounds = e2.document().aabb();
        assert!(
            bounds.min.distance(glam::DVec3::new(0.0, 0.0, 0.0)) < 1e-6,
            "{file}: {bounds:?}"
        );
        assert!(
            bounds.max.distance(glam::DVec3::new(1000.0, 300.0, 100.0)) < 1e-6,
            "{file}: {bounds:?}"
        );
    }
}

/// A local survey (docs/09 Decisions, FBX): imports every FBX file of the
/// folder `PLANYRITH_FBX_CORPUS` and, where a reference OBJ of the same
/// model sits beside it, compares the proportions of the two bounding
/// boxes. The files are not part of the repository.
#[test]
#[ignore = "needs a local corpus"]
fn fbx_corpus_survey() {
    let Ok(dir) = std::env::var("PLANYRITH_FBX_CORPUS") else {
        return;
    };
    let import = |name: &str, bytes: &[u8]| -> Result<crate::math::Aabb, String> {
        let (mut e, storage) = engine();
        storage.write(&StorageKey::new(name), bytes).unwrap();
        e.execute_erased("import", json!({ "path": name }))
            .map_err(|err| err.to_string())?;
        // Exact bounds: the vertices as exported in world coordinates.
        e.execute_erased(
            "export",
            json!({ "path": "x.obj", "options": { "z_up": true } }),
        )
        .map_err(|err| err.to_string())?;
        let text = String::from_utf8(storage.read(&StorageKey::new("x.obj")).unwrap()).unwrap();
        let mut b = crate::math::Aabb::EMPTY;
        for line in text.lines().filter(|l| l.starts_with("v ")) {
            let v: Vec<f64> = line[2..]
                .split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect();
            if v.len() >= 3 {
                b = b.union(&crate::math::Aabb::from_point(glam::DVec3::new(
                    v[0], v[1], v[2],
                )));
            }
        }
        Ok(b)
    };
    let shape = |b: &crate::math::Aabb| {
        let mut d = [b.max.x - b.min.x, b.max.y - b.min.y, b.max.z - b.min.z];
        d.sort_by(f64::total_cmp);
        (d[2] > 0.0).then(|| [d[0] / d[2], d[1] / d[2]])
    };
    let (mut imported, mut failed, mut same, mut different) = (0, Vec::new(), 0, Vec::new());
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".fbx") && !n.starts_with("synthetic"))
        .collect();
    names.sort();
    for name in names {
        let bytes = std::fs::read(format!("{dir}/{name}")).unwrap();
        let bounds = match import(&name, &bytes) {
            Ok(b) => b,
            Err(e) => {
                failed.push(format!("{name}: {e}"));
                continue;
            }
        };
        imported += 1;
        let base = name
            .rsplit_once('_')
            .and_then(|(rest, _)| rest.rsplit_once('_'))
            .map_or(name.as_str(), |(b, _)| b);
        let Ok(obj) = std::fs::read(format!("{dir}/{base}.obj")) else {
            continue;
        };
        let Ok(reference) = import(&format!("{base}.obj"), &obj) else {
            continue;
        };
        if let (Some(a), Some(b)) = (shape(&bounds), shape(&reference)) {
            if (a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01 {
                same += 1;
            } else {
                different.push(format!("{name}: {a:?} vs {b:?}"));
            }
        }
    }
    println!(
        "imported {imported}, failed {}; same proportions as the reference {same}, different {}",
        failed.len(),
        different.len()
    );
    println!("{}\n{}", failed.join("\n"), different.join("\n"));
}

/// An image entity has bounds: its rectangle where it is placed (zoom to
/// selection, others' selections when working together, queries).
#[test]
fn image_entities_have_bounds() {
    let (mut e, storage) = engine();
    storage
        .write(&StorageKey::new("plan.png"), &png(40, 20, [90, 140, 200]))
        .unwrap();
    run(
        &mut e,
        "import",
        json!({ "path": "plan.png", "at": [100, 200, 0] }),
    );
    let doc = e.document();
    let id = *doc
        .model
        .order()
        .iter()
        .find(|id| matches!(id, crate::ids::EntityId::Image(_)))
        .expect("an image entity");
    let path = crate::document::EntityPath::in_model(crate::document::EntityKey::Id(id));
    let b = doc.occurrence_aabb(&path).expect("bounds");
    assert!(
        (b.min.x - 100.0).abs() < 1e-6 && (b.min.y - 200.0).abs() < 1e-6,
        "{b:?}"
    );
    let size = b.max - b.min;
    assert!(size.x > 0.0 && size.y > 0.0 && size.z.abs() < 1e-9, "{b:?}");
    assert!(
        (size.x / size.y - 2.0).abs() < 1e-6,
        "keeps the picture's proportions"
    );
}

/// docs/09 §6: a flat cut-out whose parts overlap in one plane (a shirt
/// over the skin, a decal on a wall) cannot be one mesh; each material
/// becomes a part of its own instead of the whole being skipped.
#[test]
fn overlapping_flat_parts_of_different_materials_all_import() {
    let (mut e, storage) = engine();
    let obj = "mtllib cut.mtl\no Figure\n\
        v 0 0 0\nv 1000 0 0\nv 1000 0 1000\nv 0 0 1000\n\
        v 250 0 250\nv 750 0 250\nv 750 0 1250\nv 250 0 1250\n\
        usemtl Skin\nf 1 2 3\nf 1 3 4\n\
        usemtl Shirt\nf 1 6 7\nf 1 7 8\n";
    let mtl = "newmtl Skin\nKd 0.9 0.7 0.6\nnewmtl Shirt\nKd 0.1 0.2 0.8\n";
    storage
        .write(&StorageKey::new("cut.obj"), obj.as_bytes())
        .unwrap();
    storage
        .write(&StorageKey::new("cut.mtl"), mtl.as_bytes())
        .unwrap();
    let out = run(&mut e, "import", json!({ "path": "cut.obj", "z_up": true }));
    assert_eq!(out["skipped"], 0, "{out}");
    assert_eq!(out["faces"], 2, "both squares: {out}");
    // The same material over itself: layers that do not cross.
    let same = obj.replace("usemtl Shirt", "usemtl Skin");
    storage
        .write(&StorageKey::new("same.obj"), same.as_bytes())
        .unwrap();
    storage
        .write(&StorageKey::new("same.mtl"), mtl.as_bytes())
        .unwrap();
    let same = same.replace("mtllib cut.mtl", "mtllib same.mtl");
    storage
        .write(&StorageKey::new("same.obj"), same.as_bytes())
        .unwrap();
    let out = run(
        &mut e,
        "import",
        json!({ "path": "same.obj", "z_up": true }),
    );
    assert_eq!(out["skipped"], 0, "{out}");
    assert_eq!(out["faces"], 2, "{out}");
}

/// docs/04 Decisions (2026-11-09): bounds follow the contents, not turned
/// boxes; zoom extents fits what is shown.
#[test]
fn bounds_follow_turned_contents_and_zoom_fits_what_is_shown() {
    let (mut e, _) = engine();
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [1000, 100, 0] }),
    );
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face & last" }, "distance": 100 }),
    );
    run(
        &mut e,
        "make-group",
        json!({ "targets": "all", "name": "Inner" }),
    );
    let quarter = std::f64::consts::FRAC_PI_4;
    run(
        &mut e,
        "rotate",
        json!({ "targets": "group", "center": [0, 0, 0], "axis": [0, 0, 1], "angle": quarter }),
    );
    run(
        &mut e,
        "make-group",
        json!({ "targets": "all", "name": "Outer" }),
    );
    run(
        &mut e,
        "rotate",
        json!({ "targets": "group", "center": [0, 0, 0], "axis": [0, 0, 1], "angle": -quarter }),
    );
    // Turned and turned back: the box is the beam's own.
    let b = e.document().aabb();
    assert!(b.min.distance(glam::DVec3::ZERO) < 1e-6, "{b:?}");
    assert!(
        b.max.distance(glam::DVec3::new(1000.0, 100.0, 100.0)) < 1e-6,
        "{b:?}"
    );

    // Hidden geometry far away does not count for zoom extents.
    run(
        &mut e,
        "rectangle",
        json!({ "corner_a": [50000, 0, 0], "corner_b": [51000, 1000, 0] }),
    );
    run(
        &mut e,
        "hide",
        json!({ "targets": "face & last | edge & last" }),
    );
    let shown = crate::io::render_mesh::visible_bounds(e.document());
    assert!(shown.max.x < 1001.0, "{shown:?}");
    let out = run(&mut e, "zoom-extents", json!({}));
    let target: Vec<f64> = serde_json::from_value(out["camera"]["target"].clone()).unwrap();
    assert!(
        target[0] < 1000.0,
        "the camera looks at the beam: {target:?}"
    );
}
