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
mod materials;
mod tags;

pub use groups::{
    Explode, ExplodeOutput, ExplodeParams, MakeComponent, MakeComponentOutput, MakeComponentParams,
    MakeGroup, MakeGroupOutput, MakeGroupParams,
};
pub use materials::{
    Fill, MaterialApply, MaterialApplyOutput, MaterialApplyParams, MaterialCreate,
    MaterialCreateParams, MaterialDelete, MaterialDeleteParams, MaterialEdit, MaterialEditParams,
    MaterialOutput, MaterialPurge, MaterialPurgeOutput, MaterialPurgeParams, Side,
};
pub use tags::{
    TagAssign, TagAssignOutput, TagAssignParams, TagCreate, TagCreateParams, TagDelete,
    TagDeleteOutput, TagDeleteParams, TagFolderCreate, TagFolderCreateParams, TagFolderDelete,
    TagFolderDeleteParams, TagOutput, TagRename, TagRenameParams, TagSetVisible,
    TagSetVisibleParams,
};

use crate::commands::CommandRegistry;

/// Registers this category's commands (called by
/// [`CommandRegistry::with_builtins`]).
pub fn register(registry: &mut CommandRegistry) {
    registry.register(MakeGroup);
    registry.register(MakeComponent);
    registry.register(Explode);
    registry.register(TagCreate);
    registry.register(TagRename);
    registry.register(TagSetVisible);
    registry.register(TagDelete);
    registry.register(TagAssign);
    registry.register(TagFolderCreate);
    registry.register(TagFolderDelete);
    registry.register(MaterialCreate);
    registry.register(MaterialEdit);
    registry.register(MaterialDelete);
    registry.register(MaterialApply);
    registry.register(MaterialPurge);
}

#[cfg(test)]
mod tests;
