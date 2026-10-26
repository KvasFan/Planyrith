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
//! The module tree follows `docs/01-architecture.md` §2 exactly; every module
//! starts with a `//!` comment naming the specification section it implements.
//! The crate implements milestones 0.1–0.5 of `docs/13-roadmap.md`: the
//! kernel, the document model, the commands with undo/redo, native `.plyr`
//! read/write, the JSON export, validation/repair, the camera math, the
//! GPU-agnostic render extraction (`io::render_mesh`) and the inference
//! engine (`inference`). Modules of later
//! milestones keep the signatures of the skeleton contract
//! (`docs/dev/skeleton-contract.md`) with `todo!("docs/0X §Y")` bodies. The
//! complete design lives in the repository's `docs/` directory, starting with
//! `docs/00-overview.md` and `docs/01-architecture.md`.

#![forbid(unsafe_code)]
// TODO(skeleton): these allowances keep the signature-only draft quiet. Each
// module owner removes them from their module (by adding `#[warn(...)]` at the
// module level, then deleting this line once every module is implemented).
#![allow(unused_variables, dead_code)]
// Tests may unwrap (AGENTS.md §3); library code never does.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod commands;
pub mod diagnostics;
pub mod document;
pub(crate) mod fastmap;
pub mod fonts;
pub mod geom;
pub mod ids;
pub mod inference;
pub mod io;
pub mod math;
#[cfg(feature = "scripting")]
pub mod scripting;
pub mod spatial;
pub mod tolerance;
pub mod units;

pub use commands::{
    Command, CommandError, CommandMeta, CommandRegistry, Engine, ErasedCommand, Event, EventBus,
    Transaction,
};
pub use diagnostics::{Report, Warning, WarningCode};
pub use document::{
    AttributeDictionaries, Camera, ComponentDefinition, ComponentInstance, Document, EditContext,
    Entities, EntityPath, EntityRef, Group, Material, ModelInfo, Query, Scene, Selection, Style,
    Tag, Value,
};
pub use geom::{EdgeFlags, GeometryError, HalfEdgeMesh};
pub use ids::{
    CurveId, DefinitionId, DimensionId, EdgeId, EntityId, EntityKind, FaceId, GroupId, GuideId,
    HalfEdgeId, ImageId, InstanceId, LoopId, MaterialId, Pid, SceneId, SectionPlaneId, StyleId,
    TagFolderId, TagId, TextId, VertexId,
};
pub use io::{IoError, Storage};
pub use math::{Aabb, Plane, Ray, Segment};
pub use tolerance::Tolerances;
pub use units::{Angle, Length, LengthUnit};
