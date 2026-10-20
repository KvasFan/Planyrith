//! Implements docs/05-modeling-tools.md §5. Organization tools (category `organize`): `make-group`, `make-component`, `explode`, `make-unique`, `tag-*`, `material-*`, `purge-unused`, …
//!
//! Each tool is one `Command` implementation in its own file with a serde
//! `Params` struct (flattening `commands::CommonParams`) and an `Output`
//! struct, registered in `CommandRegistry::with_builtins` under its stable
//! kebab-case name (AGENTS.md §4). Empty in the skeleton contract.
//!
//! Owner key: `later`.

use crate::commands::CommandRegistry;

/// Registers this category's commands (called by
/// [`CommandRegistry::with_builtins`]).
pub fn register(registry: &mut CommandRegistry) {
    let _ = registry;
}
