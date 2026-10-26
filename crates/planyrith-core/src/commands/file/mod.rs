//! Implements docs/05-modeling-tools.md §9. File commands (category `file`, docs/08 §6): `save`, `save-as`, `revert`, `purge-unused`, `compact-history`.
//!
//! Each tool is one `Command` implementation in its own file with a serde
//! `Params` struct (flattening `commands::CommonParams`) and an `Output`
//! struct, registered in `CommandRegistry::with_builtins` under its stable
//! kebab-case name (AGENTS.md §4). Empty in the skeleton contract.
//!
//! `import` and `export` (docs/05 §10) read and write other formats.
//!
//! Owner key: `later`.

pub mod export;
pub mod import;

use crate::commands::CommandRegistry;

pub use export::{Export, ExportOutput, ExportParams};
pub use import::{Import, ImportOutput, ImportParams};

/// Registers this category's commands (called by
/// [`CommandRegistry::with_builtins`]).
pub fn register(registry: &mut CommandRegistry) {
    registry.register(Import);
    registry.register(Export);
}

#[cfg(test)]
pub(crate) mod tests;
