//! File › Export › Rendered Image… (docs/06 §11.1, desktop): a path-traced
//! image of the current view, traced on a background thread and saved as
//! PNG or JPEG.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use planyrith_core::document::{Camera, Document, Style};
use planyrith_render::{PathTraceOptions, path_trace};

use crate::i18n::{t, t_args};

/// The window and the job in progress.
pub struct RenderJob {
    /// The window is shown.
    pub open: bool,
    width: u32,
    height: u32,
    samples: u32,
    /// The save dialog was asked for.
    pub pick: bool,
    running: Option<(PathBuf, Receiver<Result<(), String>>)>,
}

impl Default for RenderJob {
    fn default() -> Self {
        RenderJob {
            open: false,
            width: 1920,
            height: 1080,
            samples: 64,
            pick: false,
            running: None,
        }
    }
}

impl RenderJob {
    /// Starts tracing `doc` from `camera` into `path`.
    pub fn start(&mut self, path: PathBuf, doc: Document, camera: Camera, style: Option<Style>) {
        let (send, receive) = channel();
        let options = PathTraceOptions {
            width: self.width,
            height: self.height,
            samples: self.samples,
            camera: Some(camera),
            style,
            ..PathTraceOptions::default()
        };
        let target = path.clone();
        std::thread::spawn(move || {
            let image = path_trace(&doc, &options);
            let saved = image::DynamicImage::ImageRgba8(image)
                .to_rgb8()
                .save(&target)
                .map_err(|e| e.to_string());
            let _ = send.send(saved);
        });
        self.running = Some((path, receive));
    }

    /// The finished job's message, if one finished.
    pub fn poll(&mut self) -> Option<Result<String, String>> {
        let (path, receive) = self.running.as_ref()?;
        let outcome = receive.try_recv().ok()?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.running = None;
        Some(outcome.map(|()| t_args("Rendered { $file }", &[("file", name.into())])))
    }

    /// `true` while an image is being traced.
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// Draws the window.
    pub fn window(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Rendered Image"))
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(t(
                    "A path-traced image of the current view: sun, sky and reflections.",
                ));
                egui::Grid::new("render job").num_columns(2).show(ui, |ui| {
                    ui.label(t("Width"));
                    ui.add(
                        egui::DragValue::new(&mut self.width)
                            .range(16..=8192)
                            .suffix(" px"),
                    );
                    ui.end_row();
                    ui.label(t("Height"));
                    ui.add(
                        egui::DragValue::new(&mut self.height)
                            .range(16..=8192)
                            .suffix(" px"),
                    );
                    ui.end_row();
                    ui.label(t("Quality"));
                    egui::ComboBox::from_id_salt("render quality")
                        .selected_text(format!("{}", self.samples))
                        .show_ui(ui, |ui| {
                            for (n, label) in [(16, t("Draft")), (64, t("Good")), (256, t("Best"))]
                            {
                                ui.selectable_value(&mut self.samples, n, format!("{label} ({n})"));
                            }
                        });
                    ui.end_row();
                });
                if self.busy() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(t("Rendering…"));
                    });
                } else if ui.button(t("Render…")).clicked() {
                    self.pick = true;
                }
            });
        self.open = open || self.busy();
        if self.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_job_traces_and_saves_in_the_background() {
        let dir = std::env::temp_dir().join(format!("planyrith-render-job-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("image.png");
        let mut job = RenderJob {
            width: 32,
            height: 20,
            samples: 2,
            ..RenderJob::default()
        };
        let doc = Document::new();
        job.start(path.clone(), doc.clone(), doc.camera, None);
        assert!(job.busy());
        let outcome = loop {
            if let Some(o) = job.poll() {
                break o;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(outcome.unwrap().contains("image.png"));
        assert_eq!(image::open(&path).unwrap().width(), 32);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
