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
