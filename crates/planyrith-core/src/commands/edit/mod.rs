//! Implements docs/05-modeling-tools.md §4. Editing tools (category `edit`): `select`, `erase`, `push-pull`, `move`, `rotate`, `scale`, `offset`, `follow-me`, `intersect`, `soften`, `flip`, `delete`, …
//!
//! Each tool is one `Command` implementation in its own file with a serde
//! `Params` struct (flattening `commands::CommonParams`) and an `Output`
//! struct, registered in `CommandRegistry::with_builtins` under its stable
//! kebab-case name (AGENTS.md §4).
//!
//! Owner key: `later`.

#![warn(unused_variables, dead_code)]

mod push_pull;

pub use push_pull::{PushPull, PushPullOutput, PushPullParams};

use crate::commands::CommandRegistry;

/// Registers this category's commands (called by
/// [`CommandRegistry::with_builtins`]).
pub fn register(registry: &mut CommandRegistry) {
    registry.register(PushPull);
}

#[cfg(test)]
mod tests;
