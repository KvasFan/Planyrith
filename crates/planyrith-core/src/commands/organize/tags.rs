//! Implements docs/05-modeling-tools.md §5.6: `tag-create`, `tag-rename`,
//! `tag-set-visible`, `tag-delete`, `tag-assign`, `tag-folder-create`,
//! `tag-folder-delete`. Tags and folders are named by their (unique) names.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::targets_in;
use crate::commands::params::target_container;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Document, Query, Rgba8, Tag, TagFolder};
use crate::ids::{TagFolderId, TagId};

fn meta(name: &str, label: &str) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        category: CommandCategory::Organize,
        label: label.into(),
        view_only: false,
        steps: Vec::new(),
    }
}

/// A tag by name (`E1001` when unknown).
pub(crate) fn tag_named(doc: &Document, name: &str) -> Result<TagId, CommandError> {
    doc.tags
        .by_name(name)
        .ok_or_else(|| CommandError::invalid_param("tag", format!("no tag named `{name}`")))
}

fn folder_named(doc: &Document, name: &str) -> Result<TagFolderId, CommandError> {
    doc.tags
        .folders()
        .find(|(_, f)| f.name == name)
        .map(|(id, _)| id)
        .ok_or_else(|| {
            CommandError::invalid_param("folder", format!("no tag folder named `{name}`"))
        })
}

/// A `#RRGGBB[AA]` color parameter.
pub(crate) fn parse_color(param: &str, text: &str) -> Result<Rgba8, CommandError> {
    Rgba8::from_hex(text).ok_or_else(|| {
        CommandError::invalid_param(param, format!("`{text}` is not #RRGGBB or #RRGGBBAA"))
    })
}

fn untagged() -> String {
    Tag::UNTAGGED_NAME.into()
}

/// Output naming a tag or folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TagOutput {
    /// The tag or folder name.
    pub name: String,
}

// ----- tag-create ---------------------------------------------------------------------------

/// `tag-create` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TagCreateParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Unique name.
    pub name: String,
    /// Color `#RRGGBB` (default mid gray).
    #[serde(default)]
    pub color: Option<String>,
    /// Folder name.
    #[serde(default)]
    pub folder: Option<String>,
}

/// The `tag-create` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct TagCreate;

impl Command for TagCreate {
    type Params = TagCreateParams;
    type Output = TagOutput;

    fn name(&self) -> &'static str {
        "tag-create"
    }

    fn validate(&self, doc: &Document, p: &TagCreateParams) -> Result<(), CommandError> {
        if p.name.trim().is_empty() {
            return Err(CommandError::invalid_param("name", "must not be empty"));
        }
        if doc.tags.by_name(&p.name).is_some() {
            return Err(CommandError::invalid_param(
                "name",
                format!("tag `{}` exists", p.name),
            ));
        }
        if let Some(c) = &p.color {
            parse_color("color", c)?;
        }
        if let Some(f) = &p.folder {
            folder_named(doc, f)?;
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: TagCreateParams,
    ) -> Result<TagOutput, CommandError> {
        let color = match &p.color {
            Some(c) => parse_color("color", c)?,
            None => Rgba8::rgb(0x80, 0x80, 0x80),
        };
        let mut tag = Tag::new(p.name.clone(), color);
        tag.folder = p
            .folder
            .as_deref()
            .map(|f| folder_named(tx.document(), f))
            .transpose()?;
        tx.insert_tag(tag)?;
        Ok(TagOutput { name: p.name })
    }

    fn meta(&self) -> CommandMeta {
        meta("tag-create", "Create Tag")
    }
}

// ----- tag-rename / tag-set-visible -------------------------------------------------------

/// `tag-rename` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TagRenameParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Current name.
    pub tag: String,
    /// New unique name.
    pub new_name: String,
}

/// The `tag-rename` command (`Untagged` cannot be renamed).
#[derive(Debug, Default, Clone, Copy)]
pub struct TagRename;

impl Command for TagRename {
    type Params = TagRenameParams;
    type Output = TagOutput;

    fn name(&self) -> &'static str {
        "tag-rename"
    }

    fn validate(&self, doc: &Document, p: &TagRenameParams) -> Result<(), CommandError> {
        let id = tag_named(doc, &p.tag)?;
        if id == doc.tags.untagged() {
            return Err(CommandError::NotApplicable(
                "the Untagged tag cannot be renamed".into(),
            ));
        }
        if p.new_name.trim().is_empty() {
            return Err(CommandError::invalid_param("new_name", "must not be empty"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: TagRenameParams,
    ) -> Result<TagOutput, CommandError> {
        let id = tag_named(tx.document(), &p.tag)?;
        let mut tag = tx
            .document()
            .tags
            .get(id)
            .cloned()
            .ok_or_else(|| CommandError::Internal("tag vanished".into()))?;
        tag.name = p.new_name.clone();
        tx.replace_tag(id, tag)?;
        Ok(TagOutput { name: p.new_name })
    }

    fn meta(&self) -> CommandMeta {
        meta("tag-rename", "Rename Tag")
    }
}

/// `tag-set-visible` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TagSetVisibleParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Tag name.
    pub tag: String,
    /// Visibility.
    pub visible: bool,
}

/// The `tag-set-visible` command (`Untagged` cannot be hidden: `E1201`).
#[derive(Debug, Default, Clone, Copy)]
pub struct TagSetVisible;

impl Command for TagSetVisible {
    type Params = TagSetVisibleParams;
    type Output = TagOutput;

    fn name(&self) -> &'static str {
        "tag-set-visible"
    }

    fn validate(&self, doc: &Document, p: &TagSetVisibleParams) -> Result<(), CommandError> {
        let id = tag_named(doc, &p.tag)?;
        if id == doc.tags.untagged() && !p.visible {
            return Err(CommandError::NotApplicable(
                "the Untagged tag cannot be hidden".into(),
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: TagSetVisibleParams,
    ) -> Result<TagOutput, CommandError> {
        let id = tag_named(tx.document(), &p.tag)?;
        let mut tag = tx
            .document()
            .tags
            .get(id)
            .cloned()
            .ok_or_else(|| CommandError::Internal("tag vanished".into()))?;
        tag.visible = p.visible;
        tx.replace_tag(id, tag)?;
        Ok(TagOutput { name: p.tag })
    }

    fn meta(&self) -> CommandMeta {
        meta("tag-set-visible", "Tag Visibility")
    }
}

// ----- tag-delete ---------------------------------------------------------------------------

/// `tag-delete` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TagDeleteParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Tag to delete.
    pub tag: String,
    /// Tag its entities move to.
    #[serde(default = "untagged")]
    pub reassign_to: String,
}

/// `tag-delete` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TagDeleteOutput {
    /// Entities re-tagged.
    pub reassigned: usize,
}

/// The `tag-delete` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct TagDelete;

impl Command for TagDelete {
    type Params = TagDeleteParams;
    type Output = TagDeleteOutput;

    fn name(&self) -> &'static str {
        "tag-delete"
    }

    fn validate(&self, doc: &Document, p: &TagDeleteParams) -> Result<(), CommandError> {
        let id = tag_named(doc, &p.tag)?;
        if id == doc.tags.untagged() {
            return Err(CommandError::NotApplicable(
                "the Untagged tag cannot be deleted".into(),
            ));
        }
        let to = tag_named(doc, &p.reassign_to)?;
        if to == id {
            return Err(CommandError::invalid_param(
                "reassign_to",
                "must differ from the deleted tag",
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: TagDeleteParams,
    ) -> Result<TagDeleteOutput, CommandError> {
        let id = tag_named(tx.document(), &p.tag)?;
        let to = tag_named(tx.document(), &p.reassign_to)?;
        let mut reassigned = 0;
        for c in tx.document().containers() {
            let tagged = tx
                .document()
                .container(c)
                .map(|e| e.with_tag(id))
                .unwrap_or_default();
            for e in tagged {
                tx.set_tag(c, e, Some(to))?;
                reassigned += 1;
            }
        }
        tx.remove_tag(id)?;
        Ok(TagDeleteOutput { reassigned })
    }

    fn meta(&self) -> CommandMeta {
        meta("tag-delete", "Delete Tag")
    }
}

// ----- tag-assign ---------------------------------------------------------------------------

/// `tag-assign` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TagAssignParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Entities to tag.
    pub targets: Query,
    /// Tag name (`Untagged` clears).
    pub tag: String,
}

/// `tag-assign` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TagAssignOutput {
    /// Entities tagged.
    pub assigned: usize,
}

/// The `tag-assign` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct TagAssign;

impl Command for TagAssign {
    type Params = TagAssignParams;
    type Output = TagAssignOutput;

    fn name(&self) -> &'static str {
        "tag-assign"
    }

    fn validate(&self, doc: &Document, p: &TagAssignParams) -> Result<(), CommandError> {
        p.targets
            .parse()
            .map_err(|e| CommandError::invalid_param("targets", e.to_string()))?;
        tag_named(doc, &p.tag).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: TagAssignParams,
    ) -> Result<TagAssignOutput, CommandError> {
        let tag = tag_named(tx.document(), &p.tag)?;
        let target = target_container(tx, &p.common)?;
        let ids = targets_in(tx, &target, &p.targets)?;
        for &id in &ids {
            tx.set_tag(target.container, id, Some(tag))?;
        }
        Ok(TagAssignOutput {
            assigned: ids.len(),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("tag-assign", "Assign Tag")
    }
}

// ----- folders ------------------------------------------------------------------------------

/// `tag-folder-create` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TagFolderCreateParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Folder name (unique among folders).
    pub name: String,
    /// Parent folder name.
    #[serde(default)]
    pub parent: Option<String>,
}

/// The `tag-folder-create` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct TagFolderCreate;

impl Command for TagFolderCreate {
    type Params = TagFolderCreateParams;
    type Output = TagOutput;

    fn name(&self) -> &'static str {
        "tag-folder-create"
    }

    fn validate(&self, doc: &Document, p: &TagFolderCreateParams) -> Result<(), CommandError> {
        if p.name.trim().is_empty() || folder_named(doc, &p.name).is_ok() {
            return Err(CommandError::invalid_param(
                "name",
                "must be a new, non-empty folder name",
            ));
        }
        if let Some(parent) = &p.parent {
            folder_named(doc, parent)?;
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: TagFolderCreateParams,
    ) -> Result<TagOutput, CommandError> {
        let parent = p
            .parent
            .as_deref()
            .map(|f| folder_named(tx.document(), f))
            .transpose()?;
        tx.insert_tag_folder(TagFolder {
            name: p.name.clone(),
            visible: true,
            parent,
        })?;
        Ok(TagOutput { name: p.name })
    }

    fn meta(&self) -> CommandMeta {
        meta("tag-folder-create", "Create Tag Folder")
    }
}

/// `tag-folder-delete` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TagFolderDeleteParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Folder name (must be empty).
    pub name: String,
}

/// The `tag-folder-delete` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct TagFolderDelete;

impl Command for TagFolderDelete {
    type Params = TagFolderDeleteParams;
    type Output = TagOutput;

    fn name(&self) -> &'static str {
        "tag-folder-delete"
    }

    fn validate(&self, doc: &Document, p: &TagFolderDeleteParams) -> Result<(), CommandError> {
        folder_named(doc, &p.name).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: TagFolderDeleteParams,
    ) -> Result<TagOutput, CommandError> {
        let id = folder_named(tx.document(), &p.name)?;
        tx.remove_tag_folder(id)?;
        Ok(TagOutput { name: p.name })
    }

    fn meta(&self) -> CommandMeta {
        meta("tag-folder-delete", "Delete Tag Folder")
    }
}

/// `tag-move` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TagMoveParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The tag.
    pub tag: String,
    /// Folder to put it in (none: the top level).
    #[serde(default)]
    pub folder: Option<String>,
}

/// The `tag-move` command: puts a tag into a folder or back at the top
/// level.
#[derive(Debug, Default, Clone, Copy)]
pub struct TagMove;

impl Command for TagMove {
    type Params = TagMoveParams;
    type Output = TagOutput;

    fn name(&self) -> &'static str {
        "tag-move"
    }

    fn validate(&self, doc: &Document, p: &TagMoveParams) -> Result<(), CommandError> {
        let id = tag_named(doc, &p.tag)?;
        if id == doc.tags.untagged() {
            return Err(CommandError::NotApplicable(
                "the Untagged tag stays at the top level".into(),
            ));
        }
        if let Some(f) = &p.folder {
            folder_named(doc, f)?;
        }
        Ok(())
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: TagMoveParams) -> Result<TagOutput, CommandError> {
        let id = tag_named(tx.document(), &p.tag)?;
        let folder = p
            .folder
            .as_deref()
            .map(|f| folder_named(tx.document(), f))
            .transpose()?;
        let mut tag = tx
            .document()
            .tags
            .get(id)
            .cloned()
            .ok_or_else(|| CommandError::Internal("tag vanished".into()))?;
        tag.folder = folder;
        tx.replace_tag(id, tag)?;
        Ok(TagOutput { name: p.tag })
    }

    fn meta(&self) -> CommandMeta {
        meta("tag-move", "Move Tag")
    }
}

/// `tag-folder-set-visible` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TagFolderSetVisibleParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Folder name.
    pub name: String,
    /// Visible (its tags show only when it and its parents are visible).
    pub visible: bool,
}

/// The `tag-folder-set-visible` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct TagFolderSetVisible;

impl Command for TagFolderSetVisible {
    type Params = TagFolderSetVisibleParams;
    type Output = TagOutput;

    fn name(&self) -> &'static str {
        "tag-folder-set-visible"
    }

    fn validate(&self, doc: &Document, p: &TagFolderSetVisibleParams) -> Result<(), CommandError> {
        folder_named(doc, &p.name).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: TagFolderSetVisibleParams,
    ) -> Result<TagOutput, CommandError> {
        let id = folder_named(tx.document(), &p.name)?;
        let mut folder = tx
            .document()
            .tags
            .folder(id)
            .cloned()
            .ok_or_else(|| CommandError::Internal("folder vanished".into()))?;
        folder.visible = p.visible;
        tx.replace_tag_folder(id, folder)?;
        Ok(TagOutput { name: p.name })
    }

    fn meta(&self) -> CommandMeta {
        meta("tag-folder-set-visible", "Tag Folder Visibility")
    }
}
