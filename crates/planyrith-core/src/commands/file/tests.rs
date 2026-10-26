//! Tests of `import` and `export` (docs/09 §5–§6): formats read through
//! the engine's storage, round trips, undo.

use std::sync::Arc;

use glam::DVec3;
use serde_json::json;

use crate::commands::{CommandRegistry, Engine};
use crate::document::{ContainerRef, Document, EntityView};
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
