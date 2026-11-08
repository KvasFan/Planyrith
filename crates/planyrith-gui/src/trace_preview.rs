//! View › Path-Traced Preview (docs/06 §11.2, desktop): the path tracer of
//! docs/06 §11.1 refines an image of the current view on a background thread,
//! one sample per pixel at a time; a change of the model, the camera or the
//! window size starts it again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use planyrith_core::document::Camera;
use planyrith_render::{GpuPathTracer, PathTraceOptions, PathTracer, compute_device};

use crate::i18n::{t, t_args};
use crate::session::Session;

/// Samples per pixel after which the preview stops refining (on the
/// processor; the graphics card goes on to four times as many).
const MAX_SAMPLES: u32 = 256;
/// Paths per pixel the graphics card adds between two images.
const GPU_STEP: u32 = 4;

/// The graphics card's compute device, found once (`None`: trace on the
/// processor).
fn compute() -> Option<&'static (wgpu::Device, wgpu::Queue)> {
    static DEVICE: std::sync::OnceLock<Option<(wgpu::Device, wgpu::Queue)>> =
        std::sync::OnceLock::new();
    DEVICE
        .get_or_init(|| compute_device().ok().map(|(d, q, _)| (d, q)))
        .as_ref()
}
/// How long the view must stay still before tracing starts again (s).
const SETTLE_S: f64 = 0.25;

/// What the image shows: the document generation, the camera and the size.
type Key = (u64, Camera, u32, u32);

struct Worker {
    stop: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<(image::RgbaImage, u32)>>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The window's state.
#[derive(Default)]
pub struct TracePreview {
    /// The window is shown.
    pub open: bool,
    worker: Option<Worker>,
    key: Option<Key>,
    /// A key waiting for the view to settle, and since when.
    pending: Option<(Key, f64)>,
    texture: Option<egui::TextureHandle>,
    samples: u32,
}

/// The sample count at which the preview is finished.
fn limit() -> u32 {
    if compute().is_some() {
        4 * MAX_SAMPLES
    } else {
        MAX_SAMPLES
    }
}

fn start(
    doc: planyrith_core::document::Document,
    camera: Camera,
    width: u32,
    height: u32,
) -> Worker {
    let stop = Arc::new(AtomicBool::new(false));
    let latest = Arc::new(Mutex::new(None));
    let (flag, out) = (stop.clone(), latest.clone());
    std::thread::spawn(move || {
        let threads = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .saturating_sub(1)
            .max(1);
        let options = PathTraceOptions {
            width,
            height,
            samples: 1,
            bounces: 3,
            camera: Some(camera),
            threads,
            ..PathTraceOptions::default()
        };
        if let Some((device, queue)) = compute()
            && let Ok(mut tracer) = GpuPathTracer::new(device, queue, &doc, &options)
        {
            while tracer.samples() < 4 * MAX_SAMPLES && !flag.load(Ordering::Relaxed) {
                tracer.pass(GPU_STEP);
                let Ok(image) = tracer.image() else { break };
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(mut slot) = out.lock() {
                    *slot = Some((image, tracer.samples()));
                }
            }
            return;
        }
        let mut tracer = PathTracer::new(&doc, &options);
        while tracer.samples() < MAX_SAMPLES && !flag.load(Ordering::Relaxed) {
            tracer.pass(1);
            if flag.load(Ordering::Relaxed) {
                break;
            }
            if let Ok(mut slot) = out.lock() {
                *slot = Some((tracer.image(), tracer.samples()));
            }
        }
    });
    Worker { stop, latest }
}

impl TracePreview {
    /// Draws the window and keeps the tracing in step with the model.
    pub fn window(&mut self, ctx: &egui::Context, session: &Session) {
        if !self.open {
            self.worker = None;
            self.key = None;
            self.pending = None;
            return;
        }
        let mut open = true;
        egui::Window::new(t("Path-Traced Preview"))
            .open(&mut open)
            .default_size([520.0, 340.0])
            .resizable(true)
            .show(ctx, |ui| {
                let ppp = ctx.pixels_per_point();
                let size = ui.available_size() - egui::vec2(0.0, 24.0);
                // Half the window's pixels: refines four times as fast.
                let width = ((size.x * ppp * 0.5) as u32).clamp(16, 960);
                let height = ((size.y * ppp * 0.5) as u32).clamp(16, 720);
                let key = (
                    session.engine.generation(),
                    session.doc().camera,
                    width,
                    height,
                );
                let now = ctx.input(|i| i.time);
                if self.key != Some(key) {
                    match self.pending {
                        Some((pending, since)) if pending == key => {
                            if now - since >= SETTLE_S {
                                self.worker =
                                    Some(start(session.doc().clone(), key.1, width, height));
                                self.key = Some(key);
                                self.pending = None;
                            }
                        }
                        _ => self.pending = Some((key, now)),
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
                if let Some(worker) = &self.worker
                    && let Ok(mut slot) = worker.latest.lock()
                    && let Some((image, samples)) = slot.take()
                {
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [image.width() as usize, image.height() as usize],
                        image.as_raw(),
                    );
                    match &mut self.texture {
                        Some(texture) => texture.set(color, egui::TextureOptions::LINEAR),
                        None => {
                            self.texture = Some(ctx.load_texture(
                                "path-traced preview",
                                color,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                    }
                    self.samples = samples;
                }
                if let Some(texture) = &self.texture {
                    let fit = texture.size_vec2();
                    let scale = (size.x / fit.x).min(size.y / fit.y).max(0.01);
                    ui.image((texture.id(), fit * scale));
                }
                if self.samples < limit() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(t_args(
                            "{ $samples } samples",
                            &[("samples", self.samples.to_string().into())],
                        ));
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                } else {
                    ui.label(t_args(
                        "{ $samples } samples",
                        &[("samples", self.samples.to_string().into())],
                    ));
                }
            });
        self.open = open;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_refines_and_stops_when_dropped() {
        let doc = planyrith_core::document::Document::new();
        let worker = start(doc.clone(), doc.camera, 16, 12);
        let mut seen = 0;
        for _ in 0..500 {
            if let Some((image, samples)) = worker.latest.lock().unwrap().take() {
                assert_eq!(image.width(), 16);
                seen = samples;
                if samples >= 2 {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(seen >= 2, "{seen}");
        let stop = worker.stop.clone();
        drop(worker);
        assert!(stop.load(Ordering::Relaxed));
    }
}
