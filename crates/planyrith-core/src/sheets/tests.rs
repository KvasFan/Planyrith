//! Acceptance tests for docs/15 §8: measured scale, live anchors, persistence
//! and item-sized inverse deltas.
use super::*;
use crate::commands::{CommandRegistry, Engine};
use crate::document::{Camera, Document, Projection};
use crate::io::NoProgress;
use crate::io::native::{self, WriteOptions};
use glam::DVec3;
use serde_json::{Value, json};
use std::sync::Arc;

fn engine() -> Engine {
    Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
}
fn run(e: &mut Engine, name: &str, p: Value) -> Value {
    e.execute_erased(name, p)
        .unwrap_or_else(|err| panic!("{name}: {err}"))
}
fn bytes(e: &Engine) -> Vec<u8> {
    native::write_document(e.document(), &[], &WriteOptions::default(), &NoProgress).unwrap()
}
fn camera() -> Camera {
    Camera {
        eye: DVec3::new(500.0, 500.0, 5000.0),
        target: DVec3::new(500.0, 500.0, 0.0),
        up: DVec3::Y,
        projection: Projection::Parallel { height_mm: 2000.0 },
        aspect: 1.0,
    }
}
fn cube(e: &mut Engine) {
    run(
        e,
        "rectangle",
        json!({"corner_a":[0,0,0],"corner_b":[1000,1000,0]}),
    );
    run(
        e,
        "push-pull",
        json!({"face":{"Query":"face"},"distance":1000}),
    );
}
fn page(e: &mut Engine) {
    run(e, "sheet-add-page", json!({"page":"A101"}));
}
fn view(e: &mut Engine) {
    run(
        e,
        "sheet-add-view",
        json!({"page":"A101","id":"plan","camera":camera(),"scale":"1:20","frame":{"x":20,"y":20,"width":100,"height":100}}),
    );
}
#[test]
fn cube_export_has_measured_fifty_mm_edges_and_multiple_pdf_pages() {
    let mut e = engine();
    cube(&mut e);
    page(&mut e);
    view(&mut e);
    let drawing =
        layout::layout_page(e.document(), &e.document().sheets.pages[0], 300, None).unwrap();
    let pdf = export::pdf(
        &[drawing.clone(), drawing.clone(), drawing],
        "House",
        "Author",
    )
    .unwrap();
    let source = String::from_utf8_lossy(&pdf);
    assert!(source.contains("/Count 3"));
    assert!(source.contains("/PageLabels"));
    assert!(source.contains("/OCProperties"));
    // Independently measure actual PDF path operands, then account for the
    // content stream's mm-to-point CTM (PDF points are 1/72 inch).
    let mut spans = Vec::new();
    let mut pending = Vec::new();
    let mut a = None;
    for line in source.lines() {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts.len() == 3 && matches!(parts[2], "m" | "l") {
            if let (Ok(x), Ok(y)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>()) {
                if parts[2] == "m" {
                    a = Some((x, y));
                } else if let Some((ax, ay)) = a {
                    pending.push(libm::sqrt((x - ax).powi(2) + (y - ay).powi(2)));
                    a = Some((x, y));
                }
            }
        } else if matches!(line, "S" | "B" | "b") {
            spans.append(&mut pending);
            a = None;
        } else if line == "n" {
            pending.clear();
            a = None;
        }
    }
    assert!(!spans.is_empty());
    assert!(
        spans.iter().all(|x| (x - 50.0).abs() < 0.01),
        "PDF edge lengths {spans:?}"
    );
    let ctm = source.lines().find(|l| l.ends_with(" cm")).unwrap();
    let factor: f64 = ctm.split_whitespace().next().unwrap().parse().unwrap();
    assert!((factor * 50.0 * 25.4 / 72.0 - 50.0).abs() < 0.01);
}
#[test]
fn live_model_dimensions_remeasure_and_survive_reopening() {
    let mut e = engine();
    cube(&mut e);
    page(&mut e);
    view(&mut e);
    run(
        &mut e,
        "sheet-add-dimension",
        json!({"page":"A101","id":"width","view":"plan","model_start":[0,0,0],"model_end":[1000,0,0]}),
    );
    let read = |e: &mut Engine| {
        run(e, "sheet-refresh", json!({}))["dimensions"][0]["value_mm"]
            .as_f64()
            .unwrap()
    };
    assert!((read(&mut e) - 1000.0).abs() < 1e-6);
    run(
        &mut e,
        "scale",
        json!({"targets":"all","center":[0,0,0],"factors":[2,1,1]}),
    );
    assert!((read(&mut e) - 2000.0).abs() < 1e-6);
    let loaded = native::read_document(&bytes(&e), &NoProgress).unwrap();
    let mut again = Engine::new(loaded.document, Arc::new(CommandRegistry::with_builtins()));
    assert!((read(&mut again) - 2000.0).abs() < 1e-6);
}
#[test]
fn sheet_edits_undo_to_exact_native_bytes() {
    let mut e = engine();
    page(&mut e);
    let edits = [
        ("sheet-set-paper", json!({"page":"A101","paper":"A3"})),
        (
            "sheet-add-text",
            json!({"page":"A101","id":"text","text":"Piantà Ø 1:20"}),
        ),
        (
            "sheet-add-shape",
            json!({"page":"A101","id":"shape","shape":"rectangle","points":[[10,10],[30,40]]}),
        ),
        (
            "sheet-add-table",
            json!({"page":"A101","rows":[["Name","Count"],["Wall","4"]]}),
        ),
        ("sheet-add-label", json!({"page":"A101","binding":"title"})),
        (
            "sheet-add-title-block",
            json!({"page":"A101","fields":{"Revision":"A"}}),
        ),
        ("sheet-add-dimension", json!({"page":"A101"})),
        (
            "sheet-layer-set",
            json!({"page":"A101","layer":"Notes","locked":true}),
        ),
        (
            "sheet-edit-item",
            json!({"page":"A101","id":"text","item":{"kind":"label","frame":Rect::default(),"binding":"author","prefix":"By "}}),
        ),
        ("sheet-remove-item", json!({"page":"A101","id":"shape"})),
        ("sheet-remove-page", json!({"page":"A101"})),
    ];
    for (name, p) in edits {
        let before = bytes(&e);
        run(&mut e, name, p);
        let after = bytes(&e);
        e.undo().unwrap();
        assert_eq!(bytes(&e), before, "undo {name}");
        e.redo().unwrap();
        assert_eq!(bytes(&e), after, "redo {name}");
    }
}
#[test]
fn invalid_or_locked_edits_leave_the_document_unchanged() {
    let mut e = engine();
    page(&mut e);
    let before = bytes(&e);
    assert!(
        e.execute_erased("sheet-add-view", json!({"page":"A101","scale":"1:0"}))
            .is_err()
    );
    assert!(
        e.execute_erased("sheet-add-text", json!({"page":"A101","height":-1}))
            .is_err()
    );
    assert!(
        e.execute_erased("sheet-add-shape", json!({"page":"A101","points":[[0,0]]}))
            .is_err()
    );
    assert_eq!(bytes(&e), before);
    run(
        &mut e,
        "sheet-layer-set",
        json!({"page":"A101","layer":"Drawing","locked":true}),
    );
    let before = bytes(&e);
    assert!(
        e.execute_erased("sheet-add-text", json!({"page":"A101","text":"locked"}))
            .is_err()
    );
    assert_eq!(bytes(&e), before);
}
#[test]
fn sheets_use_a_separate_optional_native_part() {
    let mut e = engine();
    let old = bytes(&e);
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&old)).unwrap();
    assert!(archive.by_name("sheets.msgpack").is_err());
    page(&mut e);
    view(&mut e);
    let saved = bytes(&e);
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&saved)).unwrap();
    assert!(archive.by_name("sheets.msgpack").is_ok());
    let loaded = native::read_document(&saved, &NoProgress).unwrap();
    assert_eq!(loaded.document.sheets, e.document().sheets);
    assert!(loaded.extra_parts.is_empty());
    assert_eq!(
        native::write_document(&loaded.document, &[], &WriteOptions::default(), &NoProgress)
            .unwrap(),
        saved
    );
}
#[test]
fn frozen_views_remain_stable_and_image_lifetimes_undo() {
    let mut e = engine();
    cube(&mut e);
    page(&mut e);
    view(&mut e);
    run(
        &mut e,
        "sheet-add-dimension",
        json!({"page":"A101","view":"plan","model_start":[0,0,0],"model_end":[1000,0,0]}),
    );
    let before = bytes(&e);
    run(
        &mut e,
        "sheet-freeze-view",
        json!({"page":"A101","id":"plan","dpi":36}),
    );
    let frozen = bytes(&e);
    assert!(crate::diagnostics::validate(e.document()).is_clean());
    let zip = zip::ZipArchive::new(std::io::Cursor::new(&frozen)).unwrap();
    assert!(
        zip.file_names()
            .any(|n| n.unwrap().starts_with("sheets/frozen/"))
    );
    e.undo().unwrap();
    assert_eq!(bytes(&e), before);
    e.redo().unwrap();
    assert_eq!(bytes(&e), frozen);
    let draw = |e: &Engine| {
        layout::layout_page(e.document(), &e.document().sheets.pages[0], 36, None).unwrap()
    };
    let first = draw(&e);
    run(&mut e, "erase", json!({"targets":"all"}));
    assert_eq!(draw(&e), first);
}
#[test]
fn unicode_text_raster_and_formats_export_headlessly() {
    let mut e = engine();
    cube(&mut e);
    page(&mut e);
    view(&mut e);
    run(
        &mut e,
        "sheet-add-text",
        json!({"page":"A101","text":"Pianta — Ø 25 mm","frame":{"x":20,"y":125,"width":100,"height":20}}),
    );
    for mode in [RenderMode::Vector, RenderMode::Raster, RenderMode::Hybrid] {
        let page = layout::layout_page(e.document(), &e.document().sheets.pages[0], 36, Some(mode))
            .unwrap();
        let pdf = export::pdf(std::slice::from_ref(&page), "House", "Me").unwrap();
        assert!(pdf.starts_with(b"%PDF"));
        assert!(String::from_utf8_lossy(&pdf).contains("/FontFile2"));
        let svg = export::svg(&page);
        assert!(svg.contains("width=\"297mm\""));
        let (dxf, images) = export::dxf(&page, "house").unwrap();
        assert!(dxf.contains("AC1015"));
        if mode != RenderMode::Vector {
            assert!(!images.is_empty());
        }
    }
}
#[test]
fn sectioned_hlr_does_not_emit_removed_geometry() {
    let mut e = engine();
    cube(&mut e);
    page(&mut e);
    run(
        &mut e,
        "section-plane-add",
        json!({"plane":{"d":-500,"normal":[1,0,0]}}),
    );
    let mut cam = camera();
    cam.eye = DVec3::new(500.0, -5000.0, 500.0);
    cam.target = DVec3::splat(500.0);
    cam.up = DVec3::Z;
    run(
        &mut e,
        "sheet-add-view",
        json!({"page":"A101","id":"section","camera":cam,"scale":"1:20","frame":{"x":20,"y":20,"width":100,"height":100}}),
    );
    let d = layout::layout_page(e.document(), &e.document().sheets.pages[0], 36, None).unwrap();
    let points = d
        .groups
        .iter()
        .flat_map(|g| &g.graphics)
        .flat_map(|p| match p {
            layout::Graphic::Path { points, .. } => points.as_slice(),
            _ => &[],
        })
        .collect::<Vec<_>>();
    assert!(!points.is_empty());
    assert!(
        points.iter().all(|p| p[0] <= 70.001),
        "removed half of cube leaked into view"
    );
}
proptest::proptest! {
    #[test]
    fn rational_scale_preserves_projected_distances(length in 0.01f64..1e6,num in 1u32..100,den in 1u32..1000){let cam=camera();let scale=Scale{numerator:num,denominator:den};let rect=Rect::default();let a=layout::project(&cam,rect,scale,DVec3::ZERO);let b=layout::project(&cam,rect,scale,DVec3::X*length);proptest::prop_assert!(((b[0]-a[0])-length*scale.factor()).abs()<1e-5);}
}

#[test]
fn page_order_defaults_template_and_pen_table_undo() {
    let mut e = engine();
    let before = bytes(&e);
    run(
        &mut e,
        "sheet-settings",
        json!({"paper_defaults":{"width":420,"height":297,"margin":12},"title_block":{"fields":{"Project":"Demo"}}}),
    );
    page(&mut e);
    run(&mut e, "sheet-add-page", json!({"page":"A201"}));
    assert!((e.document().sheets.pages[0].paper.width - 420.0).abs() < 1e-9);
    let old = bytes(&e);
    run(&mut e, "sheet-move-page", json!({"page":"A201","index":0}));
    assert_eq!(e.document().sheets.pages[0].name, "A201");
    e.undo().unwrap();
    assert_eq!(bytes(&e), old);
    e.redo().unwrap();
    run(&mut e, "sheet-add-title-block", json!({"page":"A101"}));
    assert!(
        matches!(&e.document().sheets.page("A101").unwrap().items[0].item,SheetItem::TitleBlock{fields,..} if fields.get("Project").is_some_and(|v|v=="Demo"))
    );
    while e.history().can_undo() {
        e.undo().unwrap();
    }
    assert_eq!(bytes(&e), before);
}
#[test]
fn house_plan_section_elevation_match_layout_goldens() {
    let loaded = native::read_document(
        include_bytes!("../../tests/fixtures/sheets/house.plyr"),
        &NoProgress,
    )
    .unwrap();
    let golden: Vec<layout::Drawing> = serde_json::from_str(include_str!(
        "../../tests/fixtures/sheets/house-drawings.json"
    ))
    .unwrap();
    let actual = loaded
        .document
        .sheets
        .pages
        .iter()
        .map(|p| layout::layout_page(&loaded.document, p, 300, None).unwrap())
        .collect::<Vec<_>>();
    fn compare(a: &Value, b: &Value, path: &str) {
        match (a, b) {
            (Value::Number(a), Value::Number(b)) => assert!(
                (a.as_f64().unwrap() - b.as_f64().unwrap()).abs() < 1e-7,
                "{path}: {a} != {b}"
            ),
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(a.len(), b.len(), "{path}");
                for (i, (a, b)) in a.iter().zip(b).enumerate() {
                    compare(a, b, &format!("{path}/{i}"));
                }
            }
            (Value::Object(a), Value::Object(b)) => {
                assert_eq!(a.len(), b.len(), "{path}");
                for (k, a) in a {
                    compare(
                        a,
                        b.get(k).unwrap_or_else(|| panic!("{path}: missing {k}")),
                        &format!("{path}/{k}"),
                    );
                }
            }
            _ => assert_eq!(a, b, "{path}"),
        }
    }
    compare(
        &serde_json::to_value(&actual).unwrap(),
        &serde_json::to_value(&golden).unwrap(),
        "pages",
    );
    let pdf = export::pdf(&actual, "House", "Planyrith").unwrap();
    let source = String::from_utf8_lossy(&pdf);
    assert_eq!(
        source.matches("/W [").count(),
        1,
        "each font must have one widths array"
    );
    assert!(source.contains("/FontFile2"));
    assert!(source.contains("/Count 3"));
}

#[test]
fn raster_texture_and_tag_pen_overrides_are_observable() {
    use crate::io::{Storage, StorageKey};
    let storage = Arc::new(crate::io::storage::MemoryStorage::new());
    let mut pixels = image::RgbImage::new(2, 2);
    for (x, _, p) in pixels.enumerate_pixels_mut() {
        *p = if x == 0 {
            image::Rgb([255, 0, 0])
        } else {
            image::Rgb([0, 0, 255])
        };
    }
    let mut png = std::io::Cursor::new(Vec::new());
    pixels.write_to(&mut png, image::ImageFormat::Png).unwrap();
    storage
        .write(&StorageKey::new("tile.png"), png.get_ref())
        .unwrap();
    let mut e = engine();
    e.set_storage(storage);
    cube(&mut e);
    run(
        &mut e,
        "material-create",
        json!({"name":"Tile","color":"#FFFFFF","texture":"tile.png","texture_width":1000}),
    );
    run(
        &mut e,
        "material-apply",
        json!({"targets":"face","material":"Tile"}),
    );
    run(&mut e, "style-set", json!({"faces":"shaded-textured"}));
    page(&mut e);
    view(&mut e);
    let drawing = layout::layout_page(
        e.document(),
        &e.document().sheets.pages[0],
        36,
        Some(RenderMode::Raster),
    )
    .unwrap();
    let image = drawing
        .groups
        .iter()
        .flat_map(|g| &g.graphics)
        .find_map(|p| {
            if let layout::Graphic::Image { bytes, .. } = p {
                Some(image::load_from_memory(bytes).unwrap().to_rgb8())
            } else {
                None
            }
        })
        .unwrap();
    assert!(image.pixels().any(|p| p[0] > 100 && p[2] < 10));
    assert!(image.pixels().any(|p| p[2] > 100 && p[0] < 10));
    run(&mut e, "make-group", json!({"targets":"all","name":"Box"}));
    run(
        &mut e,
        "tag-create",
        json!({"name":"Structure","color":"#0080FF"}),
    );
    run(
        &mut e,
        "tag-assign",
        json!({"targets":"group","tag":"Structure"}),
    );
    let mut styles = e.document().sheets.styles.clone();
    styles.pens.insert(
        "Structure".into(),
        Stroke {
            width: 0.8,
            color: [255, 0, 0, 255],
            dash: vec![4.0, 2.0],
        },
    );
    run(&mut e, "sheet-settings", json!({"styles":styles}));
    let drawing = layout::layout_page(
        e.document(),
        &e.document().sheets.pages[0],
        36,
        Some(RenderMode::Vector),
    )
    .unwrap();
    assert!(drawing.groups.iter().flat_map(|g|&g.graphics).any(|g|matches!(g,layout::Graphic::Path{stroke,tag:Some(tag),..} if tag=="Structure"&&(stroke.width-0.8).abs()<1e-9&&stroke.color==[255,0,0,255])));
    assert!(export::svg(&drawing).contains("data-tag=\"Structure\""));
    let (dxf, _) = export::dxf(&drawing, "pens").unwrap();
    assert!(dxf.contains("Drawing_Structure"));
    assert!(dxf.contains("LTYPE"));
    assert!(dxf.contains("370\n80\n"));
}

#[test]
fn paper_orientation_and_margin_preserve_unspecified_dimensions() {
    let mut e = engine();
    run(
        &mut e,
        "sheet-add-page",
        json!({"page":"Portrait","landscape":false}),
    );
    let p = &e.document().sheets.pages[0].paper;
    assert!((p.width - 210.0).abs() < 1e-9 && (p.height - 297.0).abs() < 1e-9);
    run(
        &mut e,
        "sheet-set-paper",
        json!({"page":"Portrait","paper":"A3","landscape":false}),
    );
    run(
        &mut e,
        "sheet-set-paper",
        json!({"page":"Portrait","margin":15}),
    );
    let p = &e.document().sheets.pages[0].paper;
    assert!(
        (p.width - 297.0).abs() < 1e-9
            && (p.height - 420.0).abs() < 1e-9
            && (p.margin - 15.0).abs() < 1e-9
    );
}

#[test]
fn custom_camera_keeps_scene_display_and_nested_section_overrides_are_local() {
    use crate::document::{EntityKey, EntityPath, EntityView};
    use crate::ids::EntityId;
    let mut e = engine();
    cube(&mut e);
    run(&mut e, "make-group", json!({"targets":"all","name":"Box"}));
    run(
        &mut e,
        "tag-create",
        json!({"name":"Structure","color":"#0080FF"}),
    );
    run(
        &mut e,
        "tag-assign",
        json!({"targets":"group","tag":"Structure"}),
    );
    run(
        &mut e,
        "camera-set",
        json!({"eye":camera().eye,"target":camera().target,"up":camera().up,"height":2000}),
    );
    run(&mut e, "scene-add", json!({"name":"Plan"}));
    run(
        &mut e,
        "tag-set-visible",
        json!({"tag":"Structure","visible":false}),
    );
    page(&mut e);
    let mut custom = camera();
    custom.eye = DVec3::new(500.0, -5000.0, 500.0);
    custom.target = DVec3::splat(500.0);
    custom.up = DVec3::Z;
    run(
        &mut e,
        "sheet-add-view",
        json!({"page":"A101","id":"custom","scene":"Plan","camera":custom,"camera_override":true}),
    );
    let entry = &e.document().sheets.pages[0].items[0];
    let (model, view) = layout::view_document(e.document(), &entry.item).unwrap();
    assert!(view.eye.abs_diff_eq(custom.eye, 1e-9));
    let tag = model.tags.by_name("Structure").unwrap();
    assert!(model.tags.is_visible(tag));
    assert!(!e.document().tags.is_visible(tag));
    run(
        &mut e,
        "tag-set-visible",
        json!({"tag":"Structure","visible":true}),
    );
    let group = e.document().model.groups().next().unwrap().0;
    let pid = e.document().model.pid_of(EntityId::Group(group)).unwrap();
    let context = EntityPath::in_model(EntityKey::Pid(pid));
    let section = run(
        &mut e,
        "section-plane-add",
        json!({"context":context,"plane":{"normal":[1,0,0],"d":-500}}),
    );
    let pid = crate::ids::Pid(section["section_plane"].as_u64().unwrap());
    run(&mut e, "lock", json!({"targets":"group"}));
    run(
        &mut e,
        "sheet-add-view",
        json!({"page":"A101","id":"override","section_override":{pid.0.to_string():false}}),
    );
    let (model, _) =
        layout::view_document(e.document(), &e.document().sheets.pages[0].items[1].item).unwrap();
    let (c, id) = model.locate_pid(pid).unwrap();
    assert!(
        matches!(model.container(c).unwrap().view(id),Some(EntityView::SectionPlane(s)) if !s.active)
    );
    assert!(
        matches!(e.document().container(c).unwrap().view(id),Some(EntityView::SectionPlane(s)) if s.active)
    );
}
