//! Implements docs/05-modeling-tools.md §8. View commands (category `view`, not undoable): `activate-scene`, `scene-add`, camera and style switching.
//!
//! Each tool is one `Command` implementation in its own file with a serde
//! `Params` struct (flattening `commands::CommonParams`) and an `Output`
//! struct, registered in `CommandRegistry::with_builtins` under its stable
//! kebab-case name (AGENTS.md §4). Empty in the skeleton contract.
//!
//! Owner key: `later`.
