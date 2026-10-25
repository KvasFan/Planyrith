//! `planyrith-gui` graphical frontend (docs/07): an eframe shell with docks
//! (`egui_tiles`), the wgpu viewport of `planyrith-render`, navigation,
//! selection, Entity Info, Tags, Scenes and Outliner panels, undo/redo and
//! open/save. Every document change is a registered command (docs/01 §3.3);
//! interactive modeling tools arrive in milestone 0.3 (docs/13).
//!
//! Usage: `planyrith-gui [model.plyr]`.

#![forbid(unsafe_code)]

mod app;
mod panels;
mod session;
mod viewport;

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
