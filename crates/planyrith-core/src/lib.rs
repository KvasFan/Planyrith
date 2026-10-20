//! # planyrith-core
//!
//! Headless heart of Planyrith: the document model, the geometry kernel, the
//! command system that every tool is built on, and native/interchange file I/O.
//!
//! This crate must never depend on a GUI toolkit or a GPU API. Both the
//! command-line frontend (`planyrith-cli`) and the graphical frontend
//! (`planyrith-gui`) drive it through the same command registry, which is
//! what guarantees that every tool is available from the CLI first and from
//! the GUI second.
//!
//! Version 0.0.0 is specification-only. The complete design lives in the
//! repository's `docs/` directory, starting with `docs/00-overview.md` and
//! `docs/01-architecture.md`. Implementation starts at milestone 0.1
//! (`docs/13-roadmap.md`).

#![forbid(unsafe_code)]
