//! Implements docs/05-modeling-tools.md §6. Annotation tools (category `annotate`): dimensions, text, section planes, guides (tape measure, protractor, axes).
//!
//! Each tool is one `Command` implementation in its own file with a serde
//! `Params` struct (flattening `commands::CommonParams`) and an `Output`
//! struct, registered in `CommandRegistry::with_builtins` under its stable
//! kebab-case name (AGENTS.md §4).

pub mod dimension;
pub mod guides;
pub mod label_style;
pub mod text;

use crate::commands::CommandRegistry;

pub use dimension::{DimensionCommand, DimensionKind, DimensionOutput, DimensionParams};
pub use guides::{GuideDeleteAll, Protractor, ResizeModel, TapeMeasure};
pub use text::{TextCommand, TextOutput, TextParams};

/// Registers this category's commands (called by
/// [`CommandRegistry::with_builtins`]).
pub fn register(registry: &mut CommandRegistry) {
    registry.register(DimensionCommand);
    registry.register(TextCommand);
    registry.register(label_style::LabelStyle);
    registry.register(TapeMeasure);
    registry.register(Protractor);
    registry.register(GuideDeleteAll);
    registry.register(ResizeModel);
}

#[cfg(test)]
mod tests;
