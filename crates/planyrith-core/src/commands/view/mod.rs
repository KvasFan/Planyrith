//! Implements docs/05-modeling-tools.md §9. View commands (category `view`,
//! not undoable, do not dirty the document): camera projection and standard
//! views, orbit, pan, zoom, zoom extents and the active style's display
//! settings (`scene-activate` lives with the
//! scene commands in `organize`).
//!
//! Each tool is one `Command` implementation with a serde `Params` struct
//! (flattening `commands::CommonParams`) and an `Output` struct, registered
//! in `CommandRegistry::with_builtins` under its stable kebab-case name
//! (AGENTS.md §4).

mod camera;
pub mod shadows;
mod style;

pub use camera::{
    CameraOutput, CameraSet, CameraSetParams, CameraStandard, CameraStandardParams, Orbit,
    OrbitParams, Pan, PanParams, PositionCamera, PositionCameraParams, ProjectionKind, Zoom,
    ZoomExtents, ZoomExtentsParams, ZoomParams,
};
pub use style::{
    FogSet, FogSetOutput, FogSetParams, StyleActivate, StyleActivateParams, StyleSet,
    StyleSetOutput, StyleSetParams,
};

use crate::commands::CommandRegistry;

/// Registers this category's commands (called by
/// [`CommandRegistry::with_builtins`]).
pub fn register(registry: &mut CommandRegistry) {
    registry.register(CameraSet);
    registry.register(CameraStandard);
    registry.register(PositionCamera);
    registry.register(Orbit);
    registry.register(Pan);
    registry.register(Zoom);
    registry.register(ZoomExtents);
    registry.register(StyleSet);
    registry.register(StyleActivate);
    registry.register(FogSet);
    registry.register(shadows::ShadowsSet);
}
