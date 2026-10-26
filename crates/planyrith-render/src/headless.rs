//! Headless rendering (docs/06 §11, `planyrith render`): a device without a
//! window, one frame of the document's draw list, read back as an image.

use planyrith_core::document::{Camera, Document, EditContext, Selection, Style};
use planyrith_core::io::render_mesh::draw_list;

use crate::{FrameRequest, Gpu, RenderError, Renderer, RendererOptions};

/// What to render headless.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadlessOptions {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// MSAA samples (1 or 4).
    pub msaa: u32,
    /// The camera (default: the document's).
    pub camera: Option<Camera>,
    /// The style (default: the document's active style).
    pub style: Option<Style>,
    /// Draw hidden geometry.
    pub show_hidden: bool,
    /// Transparent background.
    pub transparent_background: bool,
}

impl Default for HeadlessOptions {
    fn default() -> Self {
        HeadlessOptions {
            width: 1920,
            height: 1080,
            msaa: 4,
            camera: None,
            style: None,
            show_hidden: false,
            transparent_background: false,
        }
    }
}

/// Renders `doc` to an sRGB image on `gpu` (a new headless device when
/// `None`).
pub fn render_image(
    doc: &Document,
    options: &HeadlessOptions,
    gpu: Option<&Gpu>,
) -> Result<image::RgbaImage, RenderError> {
    let owned;
    let gpu = match gpu {
        Some(g) => g,
        None => {
            owned = Gpu::headless()?;
            &owned
        }
    };
    let mut renderer = Renderer::new(
        gpu.device.clone(),
        gpu.queue.clone(),
        RendererOptions {
            msaa: options.msaa,
            pixels_per_point: 1.0,
        },
    );
    let items = draw_list(
        doc,
        &Selection::new(),
        &EditContext::model(),
        options.show_hidden,
    );
    let overlay = planyrith_core::io::overlay::overlay(doc, &items);
    let camera = options.camera.unwrap_or(doc.camera);
    let style = options
        .style
        .clone()
        .or_else(|| doc.styles.active_style().cloned())
        .unwrap_or_default();
    renderer.render(&FrameRequest {
        doc,
        items: &items,
        camera: &camera,
        style: &style,
        width: options.width,
        height: options.height,
        show_hidden: options.show_hidden,
        ids: false,
        transparent_background: options.transparent_background,
        axes: false,
        overlay: Some(&overlay),
    })?;
    let mut image = renderer.read_color()?;
    crate::labels::draw_labels(&mut image, &overlay, &camera);
    Ok(image)
}
