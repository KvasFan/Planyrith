//! Headless snapshot tests (docs/06 §11, docs/12): small scenes rendered in
//! every face style, with a selection and in each projection, compared with
//! golden images at a per-pixel tolerance of 2/255 with at most 0.1 % of the
//! pixels differing. `PLANYRITH_UPDATE_GOLDEN=1` rewrites the goldens. The
//! tests are skipped when no graphics adapter exists, unless
//! `PLANYRITH_REQUIRE_GPU=1` (CI) makes that a failure.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use glam::DVec3;
use planyrith_core::document::{
    Camera, EditContext, EntityPath, FaceMode, InstancePath, Projection, Selection, Style,
};
use planyrith_core::io::render_mesh::draw_list;
use planyrith_core::{CommandRegistry, Document, Engine};
use planyrith_render::{FrameRequest, Gpu, PickKind, Renderer, RendererOptions};
use serde_json::{Value, json};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 200;

fn gpu() -> Option<Gpu> {
    match Gpu::headless() {
        Ok(gpu) => Some(gpu),
        Err(e) if std::env::var_os("PLANYRITH_REQUIRE_GPU").is_none() => {
            eprintln!("skipping GPU snapshot tests: {e}");
            None
        }
        Err(e) => panic!("PLANYRITH_REQUIRE_GPU is set but {e}"),
    }
}

fn run(e: &mut Engine, name: &str, params: Value) -> Value {
    e.execute_erased(name, params)
        .unwrap_or_else(|err| panic!("{name}: {err}"))
}

fn rect(e: &mut Engine, a: [f64; 3], b: [f64; 3]) {
    run(e, "rectangle", json!({ "corner_a": a, "corner_b": b }));
}

fn pull(e: &mut Engine, d: f64) {
    run(
        e,
        "push-pull",
        json!({ "face": { "Query": "face & last" }, "distance": d }),
    );
}

/// Walls with a door and a window, an oak floor, a table and two chairs
/// (instances of one component): the 0.1 acceptance room, smaller.
fn room() -> Engine {
    let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    rect(&mut e, [1000.0, 1000.0, 420.0], [1450.0, 1450.0, 420.0]);
    pull(&mut e, 30.0);
    for (x, y) in [
        (1010.0, 1010.0),
        (1400.0, 1010.0),
        (1010.0, 1400.0),
        (1400.0, 1400.0),
    ] {
        rect(&mut e, [x, y, 420.0], [x + 40.0, y + 40.0, 420.0]);
        pull(&mut e, 420.0);
    }
    rect(&mut e, [1000.0, 1420.0, 450.0], [1450.0, 1450.0, 450.0]);
    pull(&mut e, 450.0);
    run(
        &mut e,
        "make-component",
        json!({ "targets": "all", "name": "Chair" }),
    );
    run(
        &mut e,
        "move",
        json!({ "targets": "instance", "delta": [700, 0, 0], "copy": true }),
    );
    rect(&mut e, [1000.0, 1800.0, 690.0], [2600.0, 2600.0, 690.0]);
    pull(&mut e, 30.0);
    for (x, y) in [
        (1020.0, 1820.0),
        (2520.0, 1820.0),
        (1020.0, 2520.0),
        (2520.0, 2520.0),
    ] {
        rect(&mut e, [x, y, 690.0], [x + 60.0, y + 60.0, 690.0]);
        pull(&mut e, 690.0);
    }
    run(
        &mut e,
        "make-group",
        json!({ "targets": "face | edge", "name": "Table" }),
    );
    rect(&mut e, [-200.0, -200.0, 0.0], [4200.0, 3200.0, 0.0]);
    run(
        &mut e,
        "offset",
        json!({ "face": { "Query": "face & last" }, "distance": -200.0 }),
    );
    run(
        &mut e,
        "push-pull",
        json!({ "face": { "Query": "face & area<5m2" }, "distance": 2400.0 }),
    );
    rect(&mut e, [600.0, -200.0, 0.0], [1500.0, -200.0, 2100.0]);
    pull(&mut e, -200.0);
    rect(&mut e, [1800.0, 3200.0, 900.0], [3000.0, 3200.0, 2000.0]);
    pull(&mut e, -200.0);
    run(
        &mut e,
        "material-create",
        json!({ "name": "Oak", "color": "#A0522D" }),
    );
    run(
        &mut e,
        "material-apply",
        json!({ "targets": "face & plane:xy@0 & area>10m2", "material": "Oak" }),
    );
    run(
        &mut e,
        "material-create",
        json!({ "name": "Glass", "color": "#88CCEE", "opacity": 0.4 }),
    );
    run(
        &mut e,
        "material-apply",
        json!({ "targets": "instance", "material": "Glass" }),
    );
    run(
        &mut e,
        "camera-standard",
        json!({ "view": "iso", "extents": true }),
    );
    e
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Compares with the golden image (docs/06 §11 tolerance); writes it when
/// missing or when updating, and leaves the actual image next to the
/// target directory on failure.
fn check(name: &str, image: &image::RgbaImage) {
    let path = golden_dir().join(format!("{name}.png"));
    let update = std::env::var_os("PLANYRITH_UPDATE_GOLDEN").is_some();
    if update || !path.exists() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        image.save(&path).unwrap();
        return;
    }
    let golden = image::open(&path).unwrap().to_rgba8();
    assert_eq!(golden.dimensions(), image.dimensions(), "{name}: size");
    let differing = golden
        .pixels()
        .zip(image.pixels())
        .filter(|(a, b)| a.0.iter().zip(b.0.iter()).any(|(x, y)| x.abs_diff(*y) > 2))
        .count();
    let allowed = (golden.width() * golden.height()) as usize / 1000;
    if differing > allowed {
        let actual = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.actual.png"));
        image.save(&actual).unwrap();
        panic!(
            "{name}: {differing} pixels differ (allowed {allowed}); actual image: {}",
            actual.display()
        );
    }
}

fn frame(
    renderer: &mut Renderer,
    doc: &Document,
    selection: &Selection,
    context: &EditContext,
    camera: &Camera,
    style: &Style,
    ids: bool,
) -> image::RgbaImage {
    let items = draw_list(doc, selection, context, false);
    renderer
        .render(&FrameRequest {
            doc,
            items: &items,
            camera,
            style,
            width: WIDTH,
            height: HEIGHT,
            show_hidden: false,
            ids,
            transparent_background: false,
        })
        .unwrap();
    renderer.read_color().unwrap()
}

#[test]
fn face_styles() {
    let Some(gpu) = gpu() else { return };
    let e = room();
    let doc = e.document();
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions::default(),
    );
    for (name, mode) in [
        ("room-shaded", FaceMode::ShadedTextured),
        ("room-hidden-line", FaceMode::HiddenLine),
        ("room-wireframe", FaceMode::Wireframe),
        ("room-monochrome", FaceMode::Monochrome),
        ("room-xray", FaceMode::XRay),
    ] {
        let mut style = Style::default();
        style.faces.mode = mode;
        let image = frame(
            &mut renderer,
            doc,
            &Selection::new(),
            &EditContext::model(),
            &doc.camera,
            &style,
            false,
        );
        check(name, &image);
    }
}

#[test]
fn projections_selection_and_picking() {
    let Some(gpu) = gpu() else { return };
    let mut e = room();
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions::default(),
    );
    let style = Style::default();

    let doc = e.document();
    let parallel = Camera {
        projection: Projection::Parallel { height_mm: 4000.0 },
        ..doc.camera
    };
    let mut top = parallel;
    top.standard_view(planyrith_core::document::StandardView::Top);
    top.zoom_extents(&doc.aabb(), 0.05);
    let two_point = Camera {
        projection: Projection::TwoPoint { fov_y_deg: 40.0 },
        eye: DVec3::new(5500.0, -3500.0, 1600.0),
        target: DVec3::new(2000.0, 1500.0, 1000.0),
        ..doc.camera
    };
    for (name, camera) in [("room-top-parallel", top), ("room-two-point", two_point)] {
        let image = frame(
            &mut renderer,
            doc,
            &Selection::new(),
            &EditContext::model(),
            &camera,
            &style,
            false,
        );
        check(name, &image);
    }

    // Select the table and the floor; open the first chair for editing.
    run(
        &mut e,
        "select",
        json!({ "query": "group | face & material:Oak" }),
    );
    let doc = e.document();
    let chair = doc
        .model
        .order()
        .iter()
        .find(|id| matches!(id, planyrith_core::EntityId::Instance(_)))
        .map(|id| doc.model.pid_of(*id).unwrap())
        .unwrap();
    let context = EditContext {
        path: InstancePath::root().child(planyrith_core::document::EntityKey::Pid(chair)),
    };
    let image = frame(
        &mut renderer,
        doc,
        e.selection(),
        &context,
        &doc.camera,
        &style,
        true,
    );
    check("room-selection-context", &image);

    // The id buffer finds the floor at the center of the view and the
    // window wall's outer face near the right; nothing in a corner.
    let center = renderer.pick(WIDTH / 2, HEIGHT / 2 + 25, 0).unwrap();
    assert!(
        matches!(center, Some(hit) if matches!(hit.kind, PickKind::Face(_))),
        "{center:?}"
    );
    assert_eq!(renderer.pick(2, 2, 1).unwrap(), None);
    // Near an edge, edges win within the radius.
    let items = draw_list(doc, e.selection(), &context, false);
    let near = (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .find_map(|(x, y)| {
            renderer
                .pick(x, y, 0)
                .unwrap()
                .filter(|h| matches!(h.kind, PickKind::Edge(_)))
                .map(|h| (x, y, h))
        });
    let (x, y, hit) = near.expect("some edge is visible");
    assert!(hit.item < items.len());
    let path = EntityPath::new(
        items[hit.item].path.clone(),
        match hit.kind {
            PickKind::Edge(edge) => {
                planyrith_core::document::EntityKey::Id(planyrith_core::EntityId::Edge(edge))
            }
            PickKind::Face(face) => {
                planyrith_core::document::EntityKey::Id(planyrith_core::EntityId::Face(face))
            }
        },
    );
    assert!(doc.resolve(&path).is_ok(), "pick at ({x}, {y}) resolves");
}
