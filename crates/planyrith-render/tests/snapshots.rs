//! Headless snapshot tests (docs/06 §11, docs/12): small scenes rendered in
//! every face style, with a selection and in each projection, compared with
//! golden images at a per-pixel tolerance of 2/255 with at most 0.1 % of the
//! pixels differing. `PLANYRITH_UPDATE_GOLDEN=1` rewrites the goldens. The
//! tests are skipped when no graphics adapter exists, unless
//! `PLANYRITH_REQUIRE_GPU=1` (CI) makes that a failure.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

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

/// The device all tests share, as the application has one. A test that
/// destroyed its own Vulkan instance could crash another that was using the
/// Vulkan loader (docs/12 Decisions, 2026-11-02).
fn gpu() -> Option<Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| match Gpu::headless() {
        Ok(gpu) => Some(gpu),
        Err(e) if std::env::var_os("PLANYRITH_REQUIRE_GPU").is_none() => {
            eprintln!("skipping GPU snapshot tests: {e}");
            None
        }
        Err(e) => panic!("PLANYRITH_REQUIRE_GPU is set but {e}"),
    })
    .clone()
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
    check_within(name, image, 2, 1000);
}

/// [`check`] with a per-channel tolerance and an allowed share of differing
/// pixels (`1 / per`).
fn check_within(name: &str, image: &image::RgbaImage, channel: u8, per: usize) {
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
        .filter(|(a, b)| {
            a.0.iter()
                .zip(b.0.iter())
                .any(|(x, y)| x.abs_diff(*y) > channel)
        })
        .count();
    let allowed = (golden.width() * golden.height()) as usize / per;
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
    frame_with_axes(renderer, doc, selection, context, camera, style, ids, false)
}

#[allow(clippy::too_many_arguments)]
fn frame_with_axes(
    renderer: &mut Renderer,
    doc: &Document,
    selection: &Selection,
    context: &EditContext,
    camera: &Camera,
    style: &Style,
    ids: bool,
    axes: bool,
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
            axes,
            overlay: None,
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

/// Pixels darker than mid-gray: the edges of a hidden-line frame.
fn ink(image: &image::RgbaImage) -> usize {
    image
        .pixels()
        .filter(|p| u32::from(p.0[0]) + u32::from(p.0[1]) + u32::from(p.0[2]) < 384)
        .count()
}

/// Extension, endpoints, depth cue and jitter (docs/06 §5) each add ink to
/// a hidden-line frame; jitter moves edges the same way in every frame.
#[test]
fn edge_effects() {
    let Some(gpu) = gpu() else { return };
    let e = room();
    let doc = e.document();
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions::default(),
    );
    let mut draw = |edit: &dyn Fn(&mut Style)| {
        let mut style = Style::default();
        style.faces.mode = FaceMode::HiddenLine;
        style.edges.profiles = None;
        edit(&mut style);
        frame(
            &mut renderer,
            doc,
            &Selection::new(),
            &EditContext::model(),
            &doc.camera,
            &style,
            false,
        )
    };
    let plain = ink(&draw(&|_| {}));
    for (name, edit) in [
        (
            "room-edges-extension",
            &(|s: &mut Style| s.edges.extension = Some(8)) as &dyn Fn(&mut Style),
        ),
        ("room-edges-endpoints", &|s: &mut Style| {
            s.edges.endpoints = Some(10)
        }),
        ("room-edges-depth-cue", &|s: &mut Style| {
            s.edges.depth_cue = Some(5)
        }),
        ("room-edges-jitter", &|s: &mut Style| s.edges.jitter = true),
    ] {
        let image = draw(edit);
        let more = ink(&image);
        assert!(
            more > plain + plain / 20,
            "{name}: {more} ink pixels, plain {plain}"
        );
        // Software, Metal and Direct3D adapters rasterize thin lines a
        // little differently; the ink counts above carry the exact check.
        check_within(name, &image, 24, 100);
        assert_eq!(draw(edit), image, "{name}: the same frame twice");
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
    // The drawing axes: depth-tested, dashed on the negative side; the sky
    // shows above the horizon in perspective (docs/06 §3.1).
    let image = frame_with_axes(
        &mut renderer,
        doc,
        &Selection::new(),
        &EditContext::model(),
        &two_point,
        &style,
        false,
        true,
    );
    check("room-axes", &image);

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

#[test]
fn fog_ground_and_watermarks() {
    let Some(gpu) = gpu() else { return };
    let mut e = room();
    // A watermark image: a 16×8 two-color badge.
    use planyrith_core::io::{MemoryStorage, Storage, StorageKey};
    let badge = image::RgbaImage::from_fn(16, 8, |x, _| {
        if x < 8 {
            image::Rgba([200, 40, 40, 255])
        } else {
            image::Rgba([40, 40, 200, 255])
        }
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(badge)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let storage = Arc::new(MemoryStorage::new());
    storage.write(&StorageKey::new("badge.png"), &png).unwrap();
    e.set_storage(storage);
    run(
        &mut e,
        "style-edit",
        json!({ "ground": "#B8C4A8", "ground_opacity": 1.0 }),
    );
    run(
        &mut e,
        "style-watermark-add",
        json!({ "image": "badge.png", "placement": "anchored", "anchor_x": 1.0, "anchor_y": 1.0, "scale": 0.15 }),
    );
    run(
        &mut e,
        "fog-set",
        json!({ "enabled": true, "start": 6000.0, "end": 14000.0 }),
    );
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions::default(),
    );
    let doc = e.document();
    let style = doc.styles.active_style().unwrap().clone();
    let camera = Camera {
        projection: Projection::Perspective { fov_y_deg: 40.0 },
        eye: DVec3::new(9000.0, -7000.0, 1700.0),
        target: DVec3::new(2000.0, 1500.0, 1000.0),
        ..doc.camera
    };
    let image = frame(
        &mut renderer,
        doc,
        &Selection::new(),
        &EditContext::model(),
        &camera,
        &style,
        false,
    );
    check("room-fog-ground-watermark", &image);
}

/// Distant textures blend their mip levels (docs/06 §13): a floor of
/// 2 cm black and white squares seen from far away is an even gray, not
/// noise, and keeps its brightness.
#[test]
fn distant_textures_blend_to_their_average() {
    use planyrith_core::io::{MemoryStorage, Storage, StorageKey};
    let Some(gpu) = gpu() else { return };
    let checker = image::RgbaImage::from_fn(64, 64, |x, y| {
        if (x + y) % 2 == 0 {
            image::Rgba([0, 0, 0, 255])
        } else {
            image::Rgba([255, 255, 255, 255])
        }
    });
    let mut png = Vec::new();
    checker
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let storage = Arc::new(MemoryStorage::new());
    storage
        .write(&StorageKey::new("checker.png"), &png)
        .unwrap();
    let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    e.set_storage(storage);
    rect(&mut e, [0.0, 0.0, 0.0], [20000.0, 20000.0, 0.0]);
    run(
        &mut e,
        "material-create",
        json!({ "name": "Checker", "color": "#FFFFFF", "texture": "checker.png",
                "texture_width": 1280.0, "texture_height": 1280.0 }),
    );
    run(
        &mut e,
        "material-apply",
        json!({ "targets": "face", "material": "Checker" }),
    );
    run(
        &mut e,
        "camera-standard",
        json!({ "view": "iso", "extents": true }),
    );
    let doc = e.document();
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions::default(),
    );
    let image = frame(
        &mut renderer,
        doc,
        &Selection::new(),
        &EditContext::model(),
        &doc.camera,
        &Style::default(),
        false,
    );
    // The middle of the frame is all floor.
    let values: Vec<f64> = (WIDTH * 2 / 5..WIDTH * 3 / 5)
        .flat_map(|x| (HEIGHT * 2 / 5..HEIGHT * 3 / 5).map(move |y| (x, y)))
        .map(|(x, y)| f64::from(image.get_pixel(x, y).0[1]))
        .collect();
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let spread =
        (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64).sqrt();
    assert!(spread < 12.0, "noise: standard deviation {spread:.1}");
    // Lit linear 50 % gray, well above an sRGB average's darkening.
    assert!((90.0..230.0).contains(&mean), "mean {mean:.1}");
}

/// The axes take the renderer's palette (docs/07 §12): with vermilion
/// for the red axis, the axes show vermilion and no longer the usual red.
/// Without multisampling (Preferences › Graphics) the same scene draws,
/// with hard instead of blended edges.
#[test]
fn single_sample_draws_the_same_scene() {
    let Some(gpu) = gpu() else { return };
    let e = room();
    let doc = e.document();
    let draw = |msaa: u32| {
        let mut renderer = Renderer::new(
            gpu.device.clone(),
            gpu.queue.clone(),
            RendererOptions {
                msaa,
                ..RendererOptions::default()
            },
        );
        frame(
            &mut renderer,
            doc,
            &Selection::new(),
            &EditContext::model(),
            &doc.camera,
            &Style::default(),
            true,
        )
    };
    let (smooth, hard) = (draw(4), draw(1));
    // Mostly the same pixels; they differ only along edges.
    let differing = smooth
        .pixels()
        .zip(hard.pixels())
        .filter(|(a, b)| a.0.iter().zip(b.0).any(|(x, y)| x.abs_diff(y) > 24))
        .count();
    let total = (WIDTH * HEIGHT) as usize;
    let drawn = |image: &image::RgbaImage| {
        image
            .pixels()
            .filter(|p| p.0[..3].iter().any(|c| *c < 235))
            .count()
    };
    assert!(
        drawn(&hard) > total / 5,
        "the room is drawn: {}",
        drawn(&hard)
    );
    assert!(drawn(&hard).abs_diff(drawn(&smooth)) < total / 50);
    assert!(
        differing < total / 10,
        "{differing} of {total} pixels differ"
    );
}

#[test]
fn axes_follow_the_palette() {
    let Some(gpu) = gpu() else { return };
    let e = room();
    let doc = e.document();
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions::default(),
    );
    let near =
        |p: &image::Rgba<u8>, c: [u8; 3]| p.0[..3].iter().zip(c).all(|(a, b)| a.abs_diff(b) <= 12);
    let count =
        |image: &image::RgbaImage, c: [u8; 3]| image.pixels().filter(|p| near(p, c)).count();
    let draw = |renderer: &mut Renderer| {
        frame_with_axes(
            renderer,
            doc,
            &Selection::new(),
            &EditContext::model(),
            &doc.camera,
            &Style::default(),
            false,
            true,
        )
    };
    // The default red axis, 0.8 linear, is sRGB 231.
    let red = [231, 63, 63];
    let standard = draw(&mut renderer);
    let vermilion = [0xD5, 0x5E, 0x00];
    let lin = |v: u8| {
        let c = f32::from(v) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let mut colors = planyrith_render::AXIS_COLORS;
    colors[0] = [lin(vermilion[0]), lin(vermilion[1]), lin(vermilion[2]), 1.0];
    renderer.set_axis_colors(colors);
    let safe = draw(&mut renderer);
    assert!(
        count(&safe, vermilion) > 20,
        "vermilion pixels: {}",
        count(&safe, vermilion)
    );
    assert!(count(&safe, vermilion) > count(&standard, vermilion));
    assert!(count(&safe, red) < count(&standard, red).max(1));
}

/// Edges that bound no face cast shadows when the settings ask for it
/// (docs/06 §6): a wire above the ground darkens it only then.
#[test]
fn edges_cast_shadows() {
    let Some(gpu) = gpu() else { return };
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions::default(),
    );
    let mut frames = Vec::new();
    for from_edges in [false, true] {
        let mut e = room();
        // A wire outside the window wall, 30 cm above the ground.
        run(
            &mut e,
            "line",
            json!({ "points": [[500, 4500, 300], [3500, 4500, 300]] }),
        );
        run(
            &mut e,
            "camera-standard",
            json!({ "view": "iso", "extents": true }),
        );
        run(
            &mut e,
            "shadows-set",
            json!({ "enabled": true, "date": "2026-06-21", "time": "12:00",
                    "latitude": 45.0, "longitude": 9.0, "utc_offset": 2.0,
                    "from_edges": from_edges }),
        );
        let mut style = Style::default();
        style.edges.show = false;
        let doc = e.document();
        frames.push(frame(
            &mut renderer,
            doc,
            &Selection::new(),
            &EditContext::model(),
            &doc.camera,
            &style,
            false,
        ));
    }
    // Only the wire's shadow differs: a darker line on the ground.
    let changed = |darker: bool| {
        frames[0]
            .pixels()
            .zip(frames[1].pixels())
            .filter(|(a, b)| {
                let (a, b) = (i32::from(a.0[1]), i32::from(b.0[1]));
                if darker { a - b > 20 } else { b - a > 20 }
            })
            .count()
    };
    let (darker, lighter) = (changed(true), changed(false));
    assert!(
        darker > 40 && lighter == 0,
        "darker {darker}, lighter {lighter}"
    );
}

/// docs/06 §3.1: in a large model a nearby thin post still casts a clear
/// shadow, from the near cascade; one map over the whole 200 m ground would
/// make each of its texels wider than the post.
#[test]
fn nearby_shadows_stay_sharp_in_large_models() {
    let Some(gpu) = gpu() else { return };
    let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    rect(
        &mut e,
        [-100_000.0, -100_000.0, 0.0],
        [100_000.0, 100_000.0, 0.0],
    );
    rect(&mut e, [-30.0, -30.0, 0.0], [30.0, 30.0, 0.0]);
    pull(&mut e, 2000.0);
    run(
        &mut e,
        "shadows-set",
        json!({ "enabled": true, "date": "2026-06-21", "time": "15:00",
                "latitude": 45.0, "longitude": 9.0, "utc_offset": 2.0 }),
    );
    run(
        &mut e,
        "position-camera",
        json!({ "eye": [2500, -3500, 2200], "target": [0, 300, 300] }),
    );
    let doc = e.document();
    let mut camera = doc.camera;
    camera.aspect = f64::from(WIDTH) / f64::from(HEIGHT);
    let mut style = Style::default();
    style.edges.show = false;
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions::default(),
    );
    let image = frame(
        &mut renderer,
        doc,
        &Selection::new(),
        &EditContext::model(),
        &camera,
        &style,
        false,
    );
    if let Some(dir) = std::env::var_os("PLANYRITH_SCREENSHOTS") {
        image
            .save(Path::new(&dir).join("near-cascade.png"))
            .unwrap();
    }
    let depth = camera.fit_depth(&doc.aabb());
    let pixel = |p: DVec3| {
        let ndc = camera.project(p, depth)?;
        let x = ((ndc.x * 0.5 + 0.5) * f64::from(WIDTH)) as u32;
        let y = ((0.5 - ndc.y * 0.5) * f64::from(HEIGHT)) as u32;
        (x < WIDTH && y < HEIGHT).then(|| i32::from(image.get_pixel(x, y).0[1]))
    };
    // Points along the post's shadow, from its foot to near its tip, each
    // against the ground 600 mm to the side of it.
    let sun = doc.rendering.shadows.sun.direction();
    let shadow = |t: f64| DVec3::new(0.0, 0.0, 2000.0 * t) - sun * (2000.0 * t / sun.z);
    let side = sun.cross(DVec3::Z).normalize() * 600.0;
    let mut dark = 0;
    for i in 2..=8 {
        let at = shadow(f64::from(i) / 10.0);
        let at = DVec3::new(at.x, at.y, 0.0);
        if let (Some(here), Some(beside)) = (pixel(at), pixel(at + side))
            && beside - here > 25
        {
            dark += 1;
        }
    }
    assert!(
        dark >= 5,
        "only {dark} of 7 points along the post's shadow are dark"
    );
}

/// docs/13 0.6: a section cut with caps, sun shadows, a dimension, a guide
/// and the section plane's widget.
#[test]
fn sections_shadows_and_overlays() {
    let Some(gpu) = gpu() else { return };
    let mut e = room();
    run(&mut e, "make-group", json!({ "targets": "face | edge" }));
    run(
        &mut e,
        "section-plane-add",
        json!({ "plane": { "normal": [0, 0, 1], "d": -1200.0 } }),
    );
    run(
        &mut e,
        "shadows-set",
        json!({ "enabled": true, "date": "2026-06-21", "time": "16:00",
                "latitude": 45.0, "longitude": 9.0, "utc_offset": 2.0 }),
    );
    run(
        &mut e,
        "dimension",
        json!({ "start": [-200, -200, 0], "end": [4200, -200, 0], "offset": [0, -600, 0] }),
    );
    run(
        &mut e,
        "tape-measure",
        json!({ "from": [2000, 1000, 0], "to": [2000, 1300, 0], "guide": "line" }),
    );
    let doc = e.document();
    let items = draw_list(doc, &Selection::new(), &EditContext::model(), false);
    let overlay = planyrith_core::io::overlay::overlay(doc, &items);
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions::default(),
    );
    renderer
        .render(&FrameRequest {
            doc,
            items: &items,
            camera: &doc.camera,
            style: &Style::default(),
            width: WIDTH,
            height: HEIGHT,
            show_hidden: false,
            ids: false,
            transparent_background: false,
            axes: false,
            overlay: Some(&overlay),
        })
        .unwrap();
    let mut image = renderer.read_color().unwrap();
    // Caps: the cut walls show the section fill color.
    let fill = [0x40u8, 0x40, 0x40];
    let capped = image
        .pixels()
        .filter(|p| p.0[..3].iter().zip(fill).all(|(a, b)| a.abs_diff(b) <= 8))
        .count();
    assert!(capped > 200, "{capped} cap pixels");
    planyrith_render::draw_labels(&mut image, &overlay, &doc.camera);
    // Shadow-map edges fall on different pixels with each adapter's depth
    // precision (docs/12 Decisions): 1 % of the pixels may differ.
    check_within("room-section-shadows", &image, 24, 100);
}

#[test]
fn annotation_styles_keep_separate_line_colors_on_the_gpu() {
    let Some(gpu) = gpu() else { return };
    let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    rect(&mut e, [0.0, 0.0, 0.0], [2000.0, 1000.0, 0.0]);
    for (y, offset, color) in [(0, -200, "#cc2000"), (1000, 200, "#0066cc")] {
        let result = run(
            &mut e,
            "dimension",
            json!({"start":[0,y,0],"end":[2000,y,0],"offset":[0,offset,0]}),
        );
        run(
            &mut e,
            "label-style",
            json!({"targets":format!("@{}",result["dimension"].as_u64().unwrap()),"color":color,"arrow":"closed"}),
        );
    }
    let doc = e.document();
    let items = draw_list(doc, &Selection::new(), &EditContext::model(), false);
    let overlay = planyrith_core::io::overlay::overlay(doc, &items);
    let camera = Camera {
        eye: DVec3::new(1000.0, 500.0, 5000.0),
        target: DVec3::new(1000.0, 500.0, 0.0),
        up: DVec3::Y,
        projection: Projection::Parallel { height_mm: 2000.0 },
        aspect: f64::from(WIDTH) / f64::from(HEIGHT),
    };
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions::default(),
    );
    renderer
        .render(&FrameRequest {
            doc,
            items: &items,
            camera: &camera,
            style: &Style::default(),
            width: WIDTH,
            height: HEIGHT,
            show_hidden: false,
            ids: false,
            transparent_background: false,
            axes: false,
            overlay: Some(&overlay),
        })
        .unwrap();
    // Count the GPU line pass before CPU label text could conceal a grouping bug.
    let image = renderer.read_color().unwrap();
    let red = image
        .pixels()
        .filter(|p| {
            i16::from(p[0]) - i16::from(p[1]) > 40 && i16::from(p[0]) - i16::from(p[2]) > 40
        })
        .count();
    let blue = image
        .pixels()
        .filter(|p| {
            i16::from(p[2]) - i16::from(p[0]) > 40 && i16::from(p[2]) - i16::from(p[1]) > 20
        })
        .count();
    assert!(
        red > 100 && blue > 100,
        "expected both annotation colors: red={red}, blue={blue}"
    );
}

/// The browser build falls back to WebGL2 (docs/11 §4.2), which lacks some
/// downlevel features (a second view format of a texture, for one): the
/// renderer draws, hands the UI its display view and picks on the GL backend
/// with WebGL2's limits. Skipped where no GL adapter exists.
#[test]
fn renders_within_webgl2_limits() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::GL,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: None,
        apply_limit_buckets: false,
    })) else {
        eprintln!("skipping the WebGL2-like test: no GL adapter");
        return;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("webgl2-like"),
        required_features: wgpu::Features::empty(),
        required_limits:
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::default(),
    }))
    .unwrap();
    let mut renderer = Renderer::new(device, queue, RendererOptions::default());
    let e = room();
    let doc = e.document();
    let style = Style::default();
    let image = frame(
        &mut renderer,
        doc,
        &Selection::new(),
        &EditContext::model(),
        &doc.camera,
        &style,
        true,
    );
    assert!(image.pixels().any(|p| p.0 != image.get_pixel(0, 0).0));
    assert!(renderer.display_view().is_some());
    assert!(renderer.pick(WIDTH / 2, HEIGHT / 2, 4).unwrap().is_some());
    // The realistic look, ambient occlusion included, within the same limits.
    let style = Style {
        realism: Some(planyrith_core::document::Realism::default()),
        ..Style::default()
    };
    let realistic = frame(
        &mut renderer,
        doc,
        &Selection::new(),
        &EditContext::model(),
        &doc.camera,
        &style,
        false,
    );
    assert_ne!(realistic, image);
}

/// docs/06 §4.1: the realistic look. The sky lights faces the sun does not
/// reach, so shadows keep their color; the filmic curve keeps lit faces
/// below white; a polished metal reflects its own color. Compared with a
/// golden at the shadow tolerance.
#[test]
fn realistic_style_lights_from_the_sky() {
    let Some(gpu) = gpu() else { return };
    let mut e = room();
    run(
        &mut e,
        "shadows-set",
        json!({ "enabled": true, "date": "2026-06-21", "time": "16:00",
                "latitude": 45.0, "longitude": 9.0, "utc_offset": 2.0 }),
    );
    run(
        &mut e,
        "material-create",
        json!({ "name": "Brass", "color": "#C8A050", "metallic": 1.0, "roughness": 0.25 }),
    );
    run(
        &mut e,
        "material-apply",
        json!({ "targets": "instance", "material": "Brass" }),
    );
    let doc = e.document().clone();
    let technical = doc.styles.active_style().cloned().unwrap_or_default();
    run(&mut e, "style-edit", json!({ "realistic": true }));
    let realistic = e.document().styles.active_style().cloned().unwrap();
    assert!(realistic.realism.is_some());
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions::default(),
    );
    let doc = e.document();
    let context = EditContext::model();
    let plain = frame(
        &mut renderer,
        doc,
        &Selection::new(),
        &context,
        &doc.camera,
        &technical,
        false,
    );
    let image = frame(
        &mut renderer,
        doc,
        &Selection::new(),
        &context,
        &doc.camera,
        &realistic,
        false,
    );
    if let Some(dir) = std::env::var_os("PLANYRITH_SCREENSHOTS") {
        image.save(Path::new(&dir).join("realistic.png")).unwrap();
        plain.save(Path::new(&dir).join("technical.png")).unwrap();
    }
    let differing = plain
        .pixels()
        .zip(image.pixels())
        .filter(|(a, b)| {
            a.0[..3]
                .iter()
                .zip(&b.0[..3])
                .any(|(x, y)| x.abs_diff(*y) > 8)
        })
        .count();
    assert!(
        differing > (WIDTH * HEIGHT) as usize / 4,
        "{differing} pixels changed"
    );
    // No face burns out to pure white under the filmic curve: only the
    // (white) background is.
    let white = |img: &image::RgbaImage| {
        img.pixels()
            .filter(|p| p.0[..3].iter().all(|&c| c == 255))
            .count()
    };
    assert!(
        white(&image) <= white(&plain) + (WIDTH * HEIGHT) as usize / 200,
        "{} white pixels, {} in the technical look",
        white(&image),
        white(&plain)
    );
    check_within("realistic", &image, 24, 100);

    // Ambient occlusion only darkens, and only some of the frame.
    run(&mut e, "style-edit", json!({ "ambient_occlusion": 0.0 }));
    let flat = e.document().styles.active_style().cloned().unwrap();
    let without = frame(
        &mut renderer,
        e.document(),
        &Selection::new(),
        &context,
        &e.document().camera,
        &flat,
        false,
    );
    let (mut darker, mut lighter) = (0, 0);
    for (a, b) in image.pixels().zip(without.pixels()) {
        let (x, y): (u32, u32) = (
            a.0[..3].iter().map(|&c| u32::from(c)).sum(),
            b.0[..3].iter().map(|&c| u32::from(c)).sum(),
        );
        if x + 6 < y {
            darker += 1;
        } else if x > y + 6 {
            lighter += 1;
        }
    }
    let total = (WIDTH * HEIGHT) as usize;
    assert!(
        darker > total / 200 && darker < total / 2,
        "{darker} darker pixels"
    );
    assert_eq!(lighter, 0, "occlusion never lightens");

    // Off again: the technical look comes back exactly.
    run(&mut e, "style-edit", json!({ "realistic": false }));
    let style = e.document().styles.active_style().cloned().unwrap();
    assert!(style.realism.is_none());
    let back = frame(
        &mut renderer,
        e.document(),
        &Selection::new(),
        &context,
        &e.document().camera,
        &style,
        false,
    );
    assert_eq!(back, plain);
}
