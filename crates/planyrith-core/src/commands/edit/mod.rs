//! Implements docs/05-modeling-tools.md §4. Editing tools (category `edit`): `select`, `erase`, `push-pull`, `move`, `rotate`, `scale`, `offset`, `follow-me`, `intersect`, `soften`, `flip`, `delete`, …
//!
//! Each tool is one `Command` implementation in its own file with a serde
//! `Params` struct (flattening `commands::CommonParams`) and an `Output`
//! struct, registered in `CommandRegistry::with_builtins` under its stable
//! kebab-case name (AGENTS.md §4).
//!
//! Owner key: `later`.

#![warn(unused_variables, dead_code)]

mod erase;
mod offset;
mod push_pull;
mod select;
mod transform;

pub use erase::{
    Delete, DeleteOutput, DeleteParams, Erase, EraseMode, EraseOutput, EraseParams, Hide, Lock,
    StateOutput, StateParams, Unhide, Unlock,
};
pub use offset::{Offset, OffsetOutput, OffsetParams};
pub use push_pull::{PushPull, PushPullOutput, PushPullParams};
pub use select::{Select, SelectOutput, SelectParams};
pub use transform::{
    Move, MoveParams, Rotate, RotateParams, Scale, ScaleAbout, ScaleOutput, ScaleParams, Spacing,
    TransformOutput,
};

use crate::commands::CommandRegistry;

/// Registers this category's commands (called by
/// [`CommandRegistry::with_builtins`]).
pub fn register(registry: &mut CommandRegistry) {
    registry.register(Erase);
    registry.register(Delete);
    registry.register(Hide);
    registry.register(Unhide);
    registry.register(Lock);
    registry.register(Unlock);
    registry.register(PushPull);
    registry.register(Offset);
    registry.register(Select);
    registry.register(Move);
    registry.register(Rotate);
    registry.register(Scale);
}

#[cfg(test)]
mod tests;
