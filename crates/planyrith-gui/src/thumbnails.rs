//! Thumbnails of component files in the library (docs/07 §6 Components).
//!
//! The Components panel asks for the files it lists; the application draws
//! at most one per frame with its own small renderer (an iso view fitted to
//! the component, on a transparent background) and keeps the result for the
//! session. A file that cannot be read or drawn is remembered as such and
//! listed without a picture.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use planyrith_core::document::{Camera, Document, EditContext, Selection, StandardView, Style};
use planyrith_core::io::NoProgress;
use planyrith_core::io::native::read_document;
use planyrith_core::io::render_mesh::draw_list;
use planyrith_core::spatial::DocumentIndex;
use planyrith_render::{FrameRequest, Renderer, RendererOptions};

/// Thumbnail size in pixels (shown at half size for sharpness on HiDPI).
const SIZE: u32 = 96;

/// Library thumbnails and the renderer that draws them.
#[derive(Default)]
pub struct Thumbnails {
    renderer: Option<Renderer>,
    /// Drawn thumbnails; `None` when the file could not be drawn.
    done: BTreeMap<PathBuf, Option<egui::TextureHandle>>,
    /// Files asked for since the last frame, in listing order.
    wanted: Vec<PathBuf>,
}

impl Thumbnails {
    /// The thumbnail of `path`, asking for it when it is not drawn yet.
    pub fn get(&mut self, path: &Path) -> Option<egui::TextureHandle> {
        match self.done.get(path) {
            Some(texture) => texture.clone(),
            None => {
                if !self.wanted.iter().any(|w| w == path) {
                    self.wanted.push(path.to_path_buf());
                }
                None
            }
        }
    }

    /// Draws the first file asked for (one per frame keeps the interface
    /// responsive), then asks for another frame if more wait.
    pub fn draw_next(&mut self, ctx: &egui::Context, render_state: &egui_wgpu::RenderState) {
        let Some(path) = self.wanted.first().cloned() else {
            return;
        };
        self.wanted.retain(|w| *w != path);
        let renderer = self.renderer.get_or_insert_with(|| {
            Renderer::new(
                render_state.device.clone(),
                render_state.queue.clone(),
                RendererOptions::default(),
            )
        });
        let texture = std::fs::read(&path)
            .ok()
            .and_then(|bytes| read_document(&bytes, &NoProgress).ok())
            .and_then(|loaded| picture(renderer, &loaded.document))
            .map(|image| {
                ctx.load_texture(
                    format!("library:{}", path.display()),
                    image,
                    egui::TextureOptions::LINEAR,
                )
            });
        self.done.insert(path, texture);
        if !self.wanted.is_empty() {
            ctx.request_repaint();
        }
    }

    /// Forgets every thumbnail (Refresh in the panel).
    pub fn clear(&mut self) {
        self.done.clear();
        self.wanted.clear();
    }
}

/// The camera for a thumbnail: an iso view fitting `doc`'s geometry.
fn camera_for(doc: &Document) -> Option<Camera> {
    let bounds = DocumentIndex::build(doc).bounds();
    if bounds.is_empty() {
        return None;
    }
    let mut camera = doc.camera;
    camera.aspect = 1.0;
    camera.standard_view(StandardView::Iso);
    camera.zoom_extents(&bounds, 0.08);
    Some(camera)
}

fn picture(renderer: &mut Renderer, doc: &Document) -> Option<egui::ColorImage> {
    let camera = camera_for(doc)?;
    let items = draw_list(doc, &Selection::new(), &EditContext::model(), false);
    renderer
        .render(&FrameRequest {
            doc,
            items: &items,
            camera: &camera,
            style: &Style::default(),
            width: SIZE,
            height: SIZE,
            show_hidden: false,
            ids: false,
            transparent_background: true,
            axes: false,
            overlay: None,
        })
        .ok()?;
    let image = renderer.read_color().ok()?;
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [SIZE as usize, SIZE as usize],
        image.as_raw(),
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use planyrith_core::commands::{CommandRegistry, Engine};
    use std::sync::Arc;

    #[test]
    fn the_camera_frames_the_component_and_asks_once() {
        assert!(camera_for(&Document::new()).is_none(), "nothing to frame");
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        e.execute_erased(
            "rectangle",
            serde_json::json!({ "corner_a": [0, 0, 0], "corner_b": [400, 400, 0] }),
        )
        .unwrap();
        let camera = camera_for(e.document()).unwrap();
        assert!((camera.aspect - 1.0).abs() < 1e-9);
        assert!(
            camera.eye.z > camera.target.z,
            "looks down from the iso corner"
        );

        let mut thumbnails = Thumbnails::default();
        assert!(thumbnails.get(Path::new("a.plyrc")).is_none());
        assert!(thumbnails.get(Path::new("a.plyrc")).is_none());
        assert_eq!(thumbnails.wanted.len(), 1);
    }

    /// Draws a box's thumbnail on a GPU when one is available: the box
    /// covers part of the picture and the corners stay transparent.
    #[test]
    fn a_box_draws_on_a_transparent_background() {
        let Ok(gpu) = planyrith_render::Gpu::headless() else {
            return;
        };
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        for (name, params) in [
            (
                "rectangle",
                serde_json::json!({ "corner_a": [0, 0, 0], "corner_b": [600, 400, 0] }),
            ),
            (
                "push-pull",
                serde_json::json!({ "face": { "Query": "face & last" }, "distance": 300 }),
            ),
        ] {
            e.execute_erased(name, params).unwrap();
        }
        let mut renderer = Renderer::new(
            gpu.device.clone(),
            gpu.queue.clone(),
            RendererOptions::default(),
        );
        let image = picture(&mut renderer, e.document()).unwrap();
        if let Some(dir) = std::env::var_os("PLANYRITH_SCREENSHOTS") {
            image::RgbaImage::from_raw(SIZE, SIZE, image.as_raw().to_vec())
                .unwrap()
                .save(std::path::Path::new(&dir).join("library-thumbnail.png"))
                .unwrap();
        }
        let opaque = image.pixels.iter().filter(|p| p.a() > 200).count();
        let total = (SIZE * SIZE) as usize;
        assert!(
            opaque > total / 8 && opaque < total * 3 / 4,
            "{opaque} of {total}"
        );
        assert_eq!(image.pixels[0].a(), 0, "the corner is transparent");
    }
}
