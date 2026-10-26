//! Whole-window screenshots, headless (docs/12 §7): with
//! `PLANYRITH_SCREENSHOTS=<dir>` (and optionally `PLANYRITH_SCREENSHOT_DOC=<file.plyr>`),
//! `cargo test -p planyrith-gui screenshots` writes PNGs of the GUI rendered
//! through wgpu, the 3D viewport included. Without the variable the test does
//! nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::App;
use crate::theme::{self, ThemeChoice};

fn save(harness: &mut Harness<'_, App>, dir: &std::path::Path, name: &str) {
    harness.run_steps(4);
    let image = harness.render().expect("rendered");
    image.save(dir.join(format!("{name}.png"))).expect("saved");
}

#[test]
fn screenshots() {
    let Some(dir) = std::env::var_os("PLANYRITH_SCREENSHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("output directory");
    let doc = std::env::var_os("PLANYRITH_SCREENSHOT_DOC").map(PathBuf::from);
    for (name, choice) in [("light", ThemeChoice::Light), ("dark", ThemeChoice::Dark)] {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1400.0, 900.0))
            .wgpu()
            .build_eframe(|cc| App::new(cc, doc.clone()));
        theme::apply(&harness.ctx, choice);
        save(&mut harness, &dir, &format!("window-{name}"));

        // The Line tool hovering the model: inference marks and the label.
        harness.event(egui::Event::Text("l".into()));
        harness.hover_at(egui::pos2(700.0, 450.0));
        harness.run_steps(30);
        save(&mut harness, &dir, &format!("line-tool-{name}"));
    }

    // The context menu over a wall, and Push/Pull's options.
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .wgpu()
        .build_eframe(|cc| App::new(cc, doc.clone()));
    theme::apply(&harness.ctx, ThemeChoice::Light);
    harness.event(egui::Event::Text("p".into()));
    let at = egui::pos2(700.0, 660.0);
    harness.hover_at(at);
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
    }
    save(&mut harness, &dir, "context-menu-light");

    // The 0.4 panels.
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .wgpu()
        .build_eframe(|cc| App::new(cc, doc.clone()));
    theme::apply(&harness.ctx, ThemeChoice::Light);
    harness.run_steps(2);
    for tab in ["Materials", "Components", "Styles"] {
        harness.get_by_label(tab).click();
        harness.run_steps(2);
        if tab == "Materials" {
            harness.get_by_label("Oak").click();
        }
        if tab == "Components" {
            harness.get_by_label_contains("in model").click();
        }
        save(&mut harness, &dir, &format!("panel-{}", tab.to_lowercase()));
    }

    // A texture applied through the GUI's path: material-create, then
    // material-edit with a file, then Paint.
    if let Some(texture) = std::env::var_os("PLANYRITH_SCREENSHOT_TEXTURE") {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1400.0, 900.0))
            .wgpu()
            .build_eframe(|cc| App::new(cc, None));
        theme::apply(&harness.ctx, ThemeChoice::Light);
        harness.state_mut().run_for_screens(&[
            (
                "rectangle",
                serde_json::json!({ "corner_a": [0, 0, 0], "corner_b": [2000, 1500, 0] }),
            ),
            (
                "push-pull",
                serde_json::json!({ "face": { "Query": "face & last" }, "distance": 1000.0 }),
            ),
            (
                "material-create",
                serde_json::json!({ "name": "Wood", "color": "#FFFFFF" }),
            ),
            (
                "material-apply",
                serde_json::json!({ "targets": "face", "material": "Wood", "side": "both" }),
            ),
            (
                "camera-standard",
                serde_json::json!({ "view": "iso", "extents": true }),
            ),
        ]);
        harness.run_steps(3);
        harness.state_mut().run_for_screens(&[(
            "material-edit",
            serde_json::json!({ "material": "Wood", "texture": texture.to_string_lossy() }),
        )]);
        save(&mut harness, &dir, "texture");
    }

    // The bundled materials, one block each, with the Materials panel open.
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .wgpu()
        .build_eframe(|cc| App::new(cc, None));
    theme::apply(&harness.ctx, ThemeChoice::Light);
    let mut commands: Vec<(String, serde_json::Value)> = Vec::new();
    for (i, b) in crate::bundled::catalogue().iter().enumerate() {
        let x = (i % 5) as f64 * 1600.0;
        let y = (i / 5) as f64 * 1600.0;
        let path = crate::bundled::texture_path(b).expect("bundled texture");
        commands.push((
            "material-create".into(),
            serde_json::json!({
            "name": b.name, "color": b.color, "texture": path.to_string_lossy(),
            "texture_width": b.size_mm.0, "texture_height": b.size_mm.1 }),
        ));
        commands.push((
            "rectangle".into(),
            serde_json::json!({
            "corner_a": [x, y, 0], "corner_b": [x + 1200.0, y + 1200.0, 0] }),
        ));
        commands.push((
            "push-pull".into(),
            serde_json::json!({
            "face": { "Query": "face & last" }, "distance": 800.0 }),
        ));
        commands.push((
            "material-apply".into(),
            serde_json::json!({
            "targets": "face & last", "material": b.name, "side": "both" }),
        ));
    }
    commands.push((
        "camera-standard".into(),
        serde_json::json!({ "view": "iso", "extents": true }),
    ));
    let refs: Vec<(&str, serde_json::Value)> = commands
        .iter()
        .map(|(n, p)| (n.as_str(), p.clone()))
        .collect();
    harness.state_mut().run_for_screens(&refs);
    harness.run_steps(2);
    harness.get_by_label("Materials").click();
    save(&mut harness, &dir, "bundled-materials");
}
