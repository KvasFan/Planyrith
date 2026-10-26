//! # planyrith-render
//!
//! Implements `docs/06-rendering-and-viewport.md`: the `wgpu` renderer that
//! draws Planyrith documents without any user-interface dependency.
//!
//! The renderer always draws into its own offscreen targets (multisampled
//! color, reversed-Z depth and, on request, the id buffer used for hover
//! picking, docs/06 §8). The graphical frontend (`planyrith-gui`) shows the
//! resolved color texture in its viewport; the command-line frontend
//! (`planyrith render`, docs/06 §11) reads it back and encodes an image. Both
//! therefore draw exactly the same pixels.
//!
//! Render data comes from `planyrith-core::io::render_mesh` (`docs/06 §2`) in
//! `f32`, relative to each container's local origin, and the draw list of the
//! document's occurrences; the camera math (camera-relative, reversed-Z
//! projections) is `planyrith_core::document::camera`. Milestone 0.2 draws
//! faces (shaded, textured, monochrome, hidden-line, X-ray, wireframe),
//! edges with profiles and dashed back edges, transparency, selection and
//! edit-context fading; later milestones add fog and watermarks (0.4), and
//! section cuts with caps, sun shadows and overlays — guides, dimensions,
//! texts and section plane widgets, with CPU-drawn labels headless (0.6).

#![forbid(unsafe_code)]

mod gpu;
mod headless;
mod labels;
mod renderer;

pub use gpu::Gpu;
pub use headless::{HeadlessOptions, render_image};
pub use labels::draw_labels;
pub use renderer::{FrameRequest, PickHit, PickKind, Renderer, RendererOptions};

/// Rendering failures.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// No GPU adapter (hardware or software) is available.
    #[error("no graphics adapter is available: {0}")]
    NoAdapter(String),
    /// The device could not be created.
    #[error("the graphics device could not be created: {0}")]
    Device(String),
    /// Reading the rendered image back failed.
    #[error("reading the image back failed: {0}")]
    Readback(String),
    /// Render data could not be extracted from the document.
    #[error("render data: {0}")]
    Extract(#[from] planyrith_core::GeometryError),
    /// The requested size is zero or above the device limit.
    #[error("invalid image size {width}×{height} (the limit is {max})")]
    Size {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
        /// Largest side the device supports.
        max: u32,
    },
}
