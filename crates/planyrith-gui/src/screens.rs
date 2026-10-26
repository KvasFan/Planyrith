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
}
