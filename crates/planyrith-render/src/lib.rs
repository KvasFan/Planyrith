//! # planyrith-render
//!
//! Implements `docs/06-rendering-and-viewport.md`: the `wgpu` renderer that
//! draws Planyrith documents without any user-interface dependency.
//!
//! The graphical frontend (`planyrith-gui`) embeds this renderer through an
//! `egui-wgpu` paint callback; the command-line frontend (`planyrith-cli`)
//! drives it headless for the `render` subcommand (`docs/08 §7`). Render data
//! is extracted by `planyrith-core::io::render_mesh` (`docs/06 §2`) in `f32`,
//! relative to each container's local origin; this crate owns the GPU side:
//! passes (`docs/06 §4`), styles and edges (`§5`, `§6`), shadows (`§7`),
//! section cuts (`§9`), the id buffer used for picking acceleration (`§8`) and
//! deterministic headless output (`§11`).
//!
//! In 0.1 this crate is a scaffold; the first renderer lands at milestone 0.2
//! (`docs/13-roadmap.md`).

#![forbid(unsafe_code)]
