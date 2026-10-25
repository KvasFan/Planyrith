//! `planyrith-gui` graphical frontend (docs/07): an eframe shell with docks
//! (`egui_tiles`), the wgpu viewport of `planyrith-render`, navigation,
//! selection, the interactive tools with inference and the value field
//! (docs/07 §2–§5), Entity Info, Tags, Scenes, Outliner, Tool Options and
//! Tool Help panels, undo/redo and open/save. Every document change is a
//! registered command (docs/01 §3.3); tools only collect parameters.
//!
//! Usage: `planyrith-gui [model.plyr]`.

#![forbid(unsafe_code)]

mod app;
mod icons;
mod panels;
mod session;
mod theme;
mod tools;
mod viewport;

#[cfg(test)]
mod screens;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let file = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Planyrith")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Planyrith",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, file)))),
    )
}
