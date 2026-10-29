//! `planyrith-gui` graphical frontend (docs/07): an eframe shell with docks
//! (`egui_tiles`), the wgpu viewport of `planyrith-render`, navigation,
//! selection, the interactive tools with inference and the value field
//! (docs/07 §2–§5), Entity Info, Tags, Scenes, Outliner, Tool Options and
//! Tool Help panels, undo/redo, open/save and autosave. Every document
//! change is a registered command (docs/01 §3.3); tools only collect
//! parameters. The same code runs on the desktop and, built for
//! `wasm32-unknown-unknown`, in the browser (docs/11 §4); `platform` holds
//! what differs.

#![forbid(unsafe_code)]

mod app;
mod bundled;
mod extensions;
mod icons;
mod panels;
pub mod platform;
#[cfg(not(target_arch = "wasm32"))]
mod remote;
mod resources;
pub mod samples;
pub mod session;
mod theme;
mod tools;
mod viewport;

#[cfg(test)]
mod screens;

/// Runs the desktop application, opening `file` if given.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_native(file: Option<std::path::PathBuf>) -> eframe::Result {
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

/// Starts the web application on the page's `planyrith` canvas, once the
/// browser's files are loaded (docs/11 §4).
#[cfg(target_arch = "wasm32")]
pub fn run_web() {
    use wasm_bindgen::JsCast;

    wasm_bindgen_futures::spawn_local(async {
        let status = |text: &str| {
            if let Some(el) = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id("status"))
            {
                el.set_text_content(Some(text));
            }
        };
        if let Err(e) = platform::web::install().await {
            status(&format!(
                "This browser's storage is unavailable ({e}); files are kept until the page closes."
            ));
        }
        platform::web::guard_unload();
        let Some(canvas) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("planyrith"))
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        else {
            status("The page has no `planyrith` canvas.");
            return;
        };
        let started = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(app::App::new(cc, None)))),
            )
            .await;
        match started {
            Ok(()) => status(""),
            Err(e) => status(&format!("Planyrith could not start: {e:?}")),
        }
    });
}

pub mod sheets;
