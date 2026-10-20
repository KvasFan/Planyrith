//! Implements docs/05-modeling-tools.md §5. Organization tools (category `organize`): `make-group`, `make-component`, `explode`, `make-unique`, `tag-*`, `material-*`, `purge-unused`, …
//!
//! Each tool is one `Command` implementation in its own file with a serde
//! `Params` struct (flattening `commands::CommonParams`) and an `Output`
//! struct, registered in `CommandRegistry::with_builtins` under its stable
//! kebab-case name (AGENTS.md §4).
//!
//! Owner key: `later`.

#![warn(unused_variables, dead_code)]

mod groups;

pub use groups::{
    Explode, ExplodeOutput, ExplodeParams, MakeComponent, MakeComponentOutput, MakeComponentParams,
    MakeGroup, MakeGroupOutput, MakeGroupParams,
};

use crate::commands::CommandRegistry;

/// Registers this category's commands (called by
/// [`CommandRegistry::with_builtins`]).
pub fn register(registry: &mut CommandRegistry) {
    registry.register(MakeGroup);
    registry.register(MakeComponent);
    registry.register(Explode);
}

#[cfg(test)]
mod tests;
