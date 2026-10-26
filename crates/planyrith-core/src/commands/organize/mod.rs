//! Implements docs/05-modeling-tools.md §5. Organization tools (category `organize`): `make-group`, `make-component`, `explode`, `make-unique`, `tag-*`, `material-*`, `scene-*`, `purge-unused`, …
//!
//! Each tool is one `Command` implementation in its own file with a serde
//! `Params` struct (flattening `commands::CommonParams`) and an `Output`
//! struct, registered in `CommandRegistry::with_builtins` under its stable
//! kebab-case name (AGENTS.md §4).
//!
//! Owner key: `later`.

#![warn(unused_variables, dead_code)]

mod axes;
pub(crate) mod components;
mod groups;
pub(crate) mod images;
pub(crate) mod libraries;
mod materials;
mod scenes;
pub(crate) mod sections;
mod styles;
mod tags;

pub use components::{
    ComponentInsert, ComponentInsertOutput, ComponentInsertParams, ComponentOutput,
    ComponentReload, ComponentReloadParams, ComponentReplace, ComponentReplaceOutput,
    ComponentReplaceParams, ComponentSaveAs, ComponentSaveAsParams, MakeUnique, MakeUniqueOutput,
    MakeUniqueParams,
};
pub use groups::{
    Explode, ExplodeOutput, ExplodeParams, MakeComponent, MakeComponentOutput, MakeComponentParams,
    MakeGroup, MakeGroupOutput, MakeGroupParams,
};
pub use images::{ImageInsert, ImageInsertOutput, ImageInsertParams};
pub use libraries::{
    MaterialLibraryLoad, MaterialLibraryLoadParams, MaterialLibraryOutput, MaterialLibrarySave,
    MaterialLibrarySaveParams,
};
pub use materials::{
    Fill, MaterialApply, MaterialApplyOutput, MaterialApplyParams, MaterialCreate,
    MaterialCreateParams, MaterialDelete, MaterialDeleteParams, MaterialEdit, MaterialEditParams,
    MaterialOutput, MaterialPurge, MaterialPurgeOutput, MaterialPurgeParams, MaterialSample,
    MaterialSampleParams, Side, TexturePosition, TexturePositionParams,
};
pub use scenes::{
    SceneActivate, SceneActivateParams, SceneAdd, SceneAddParams, SceneCapture, SceneDelete,
    SceneDeleteParams, SceneOutput, SceneRename, SceneRenameParams, SceneReorder,
    SceneReorderParams, SceneUpdate, SceneUpdateParams,
};
pub use styles::{
    PlacementChoice, StyleCreate, StyleCreateParams, StyleDelete, StyleDeleteParams, StyleEdit,
    StyleEditParams, StyleLibraryLoad, StyleLibraryLoadParams, StyleLibraryOutput,
    StyleLibrarySave, StyleLibrarySaveParams, StyleOutput, StyleWatermarkAdd,
    StyleWatermarkAddParams, StyleWatermarkOutput, StyleWatermarkRemove,
    StyleWatermarkRemoveParams,
};
pub(crate) use tags::parse_color as parse_color_param;
pub use tags::{
    TagAssign, TagAssignOutput, TagAssignParams, TagCreate, TagCreateParams, TagDelete,
    TagDeleteOutput, TagDeleteParams, TagFolderCreate, TagFolderCreateParams, TagFolderDelete,
    TagFolderDeleteParams, TagFolderSetVisible, TagFolderSetVisibleParams, TagMove, TagMoveParams,
    TagOutput, TagRename, TagRenameParams, TagSetVisible, TagSetVisibleParams,
};

use crate::commands::CommandRegistry;

/// Registers this category's commands (called by
/// [`CommandRegistry::with_builtins`]).
pub fn register(registry: &mut CommandRegistry) {
    registry.register(MakeGroup);
    registry.register(MakeComponent);
    registry.register(Explode);
    registry.register(MakeUnique);
    registry.register(ImageInsert);
    registry.register(axes::AxesSet);
    registry.register(axes::AxesReset);
    registry.register(sections::SectionPlaneAdd);
    registry.register(sections::SectionPlaneToggle);
    registry.register(sections::SectionPlaneReverse);
    registry.register(sections::SectionPlaneDelete);
    registry.register(sections::SectionPlaneAlignView);
    registry.register(sections::SectionSlice);
    registry.register(sections::SectionDisplaySet);
    registry.register(ComponentInsert);
    registry.register(ComponentSaveAs);
    registry.register(ComponentReload);
    registry.register(ComponentReplace);
    registry.register(TagCreate);
    registry.register(TagRename);
    registry.register(TagSetVisible);
    registry.register(TagDelete);
    registry.register(TagAssign);
    registry.register(TagFolderCreate);
    registry.register(TagFolderDelete);
    registry.register(TagMove);
    registry.register(TagFolderSetVisible);
    registry.register(MaterialCreate);
    registry.register(MaterialEdit);
    registry.register(MaterialDelete);
    registry.register(MaterialApply);
    registry.register(MaterialPurge);
    registry.register(MaterialSample);
    registry.register(TexturePosition);
    registry.register(MaterialLibrarySave);
    registry.register(MaterialLibraryLoad);
    registry.register(SceneAdd);
    registry.register(SceneUpdate);
    registry.register(SceneDelete);
    registry.register(SceneRename);
    registry.register(SceneReorder);
    registry.register(SceneActivate);
    registry.register(StyleCreate);
    registry.register(StyleDelete);
    registry.register(StyleEdit);
    registry.register(StyleWatermarkAdd);
    registry.register(StyleWatermarkRemove);
    registry.register(StyleLibrarySave);
    registry.register(StyleLibraryLoad);
}

#[cfg(test)]
mod tests;
