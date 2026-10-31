//! The `planyrith-gui` executable (docs/07): the desktop application, or the
//! web application when built for `wasm32-unknown-unknown` (docs/11 §4).
//!
//! Usage: `planyrith-gui [model.plyr]`.

#![forbid(unsafe_code)]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    planyrith_gui::run_native(std::env::args_os().nth(1).map(std::path::PathBuf::from))
}

#[cfg(target_arch = "wasm32")]
fn main() {
    planyrith_gui::run_web();
}
