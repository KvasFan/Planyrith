//! Implements docs/04-document-model.md §5 (tags): visibility/style categories
//! organized in folders, with the undeletable `Untagged` tag.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use slotmap::SlotMap;

use crate::document::materials::Rgba8;
use crate::ids::{TagFolderId, TagId};

/// Line style used by sheets (docs/04 §5, docs/15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum LineStyle {
    /// Continuous.
    #[default]
    Solid,
    /// Dashed.
    Dashed,
    /// Dotted.
    Dotted,
    /// Dash-dot.
    DashDot,
}

/// A tag (docs/04 §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    /// Unique name.
    pub name: String,
    /// Visibility toggle.
    pub visible: bool,
    /// Color used by "color by tag" styles.
    pub color: Rgba8,
    /// Line style for sheets.
    pub line_style: LineStyle,
    /// Containing folder.
    pub folder: Option<TagFolderId>,
}

impl Tag {
    /// Name of the always-present default tag.
    pub const UNTAGGED_NAME: &'static str = "Untagged";

    /// A visible tag with the given name and color.
    pub fn new(name: impl Into<String>, color: Rgba8) -> Self {
        Tag {
            name: name.into(),
            visible: true,
            color,
            line_style: LineStyle::Solid,
            folder: None,
        }
    }
}

/// A folder in the tag tree (docs/04 §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TagFolder {
    /// Folder name.
    pub name: String,
    /// Folder-level visibility (ANDed with its tags).
    pub visible: bool,
    /// Parent folder.
    pub parent: Option<TagFolderId>,
}

/// Errors from tag bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TagError {
    /// A tag with this name exists (docs/04 §13 forbids duplicates).
    #[error("tag name already used: {0}")]
    DuplicateName(String),
    /// `Untagged` cannot be deleted or hidden.
    #[error("the Untagged tag cannot be deleted or hidden")]
    UntaggedProtected,
    /// Unknown tag id.
    #[error("tag not found: {0}")]
    NotFound(TagId),
    /// Unknown folder id.
    #[error("tag folder not found: {0}")]
    FolderNotFound(TagFolderId),
    /// Folder parenting would form a cycle.
    #[error("tag folder cycle")]
    Cycle,
    /// Only empty folders can be removed.
    #[error("tag folder is not empty: {0}")]
    FolderNotEmpty(TagFolderId),
}

/// The document's tag tree (docs/04 §1 "Tags").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tags {
    tags: SlotMap<TagId, Tag>,
    folders: SlotMap<TagFolderId, TagFolder>,
    order: Vec<TagId>,
    folder_order: Vec<TagFolderId>,
    untagged: TagId,
}

impl Default for Tags {
    fn default() -> Self {
        let mut tags = SlotMap::with_key();
        let untagged = tags.insert(Tag::new(Tag::UNTAGGED_NAME, Rgba8::rgb(0x80, 0x80, 0x80)));
        Tags {
            tags,
            folders: SlotMap::with_key(),
            order: vec![untagged],
            folder_order: Vec::new(),
            untagged,
        }
    }
}

impl Tags {
    /// Only the `Untagged` tag.
    pub fn new() -> Self {
        Self::default()
    }

    /// The always-present default tag.
    pub fn untagged(&self) -> TagId {
        self.untagged
    }

    /// Number of tags (≥ 1).
    pub fn len(&self) -> usize {
        self.tags.len()
    }

    /// Never true.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// Tag by id.
    pub fn get(&self, id: TagId) -> Option<&Tag> {
        self.tags.get(id)
    }

    /// Tag by name.
    pub fn by_name(&self, name: &str) -> Option<TagId> {
        self.order
            .iter()
            .copied()
            .find(|id| self.tags.get(*id).is_some_and(|t| t.name == name))
    }

    /// Tags in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (TagId, &Tag)> + '_ {
        self.order
            .iter()
            .filter_map(|id| self.tags.get(*id).map(|t| (*id, t)))
    }

    /// Folder by id.
    pub fn folder(&self, id: TagFolderId) -> Option<&TagFolder> {
        self.folders.get(id)
    }

    /// Folders in insertion order.
    pub fn folders(&self) -> impl Iterator<Item = (TagFolderId, &TagFolder)> + '_ {
        self.folder_order
            .iter()
            .filter_map(|id| self.folders.get(*id).map(|f| (*id, f)))
    }

    /// Effective visibility: the tag is visible and every ancestor folder is
    /// visible (docs/04 §5).
    pub fn is_visible(&self, id: TagId) -> bool {
        let Some(tag) = self.tags.get(id) else {
            return false;
        };
        if !tag.visible {
            return false;
        }
        let mut folder = tag.folder;
        // Bounded walk: a well-formed tree is at most `folders.len()` deep.
        for _ in 0..=self.folders.len() {
            let Some(f) = folder else {
                return true;
            };
            match self.folders.get(f) {
                Some(node) if node.visible => folder = node.parent,
                _ => return false,
            }
        }
        false
    }

    /// Position of a tag in the insertion order (captured before removal so
    /// undo can restore it with [`Tags::insert_at`]).
    pub fn position(&self, id: TagId) -> Option<usize> {
        self.order.iter().position(|x| *x == id)
    }

    /// Adds a tag with a unique name.
    pub(crate) fn insert(&mut self, tag: Tag) -> Result<TagId, TagError> {
        self.insert_at(tag, self.order.len())
    }

    /// Adds a tag at `index` in the order (undo of a removal; the id is new).
    pub(crate) fn insert_at(&mut self, tag: Tag, index: usize) -> Result<TagId, TagError> {
        if self.by_name(&tag.name).is_some() {
            return Err(TagError::DuplicateName(tag.name));
        }
        if let Some(f) = tag.folder
            && !self.folders.contains_key(f)
        {
            return Err(TagError::FolderNotFound(f));
        }
        let id = self.tags.insert(tag);
        self.order.insert(index.min(self.order.len()), id);
        Ok(id)
    }

    /// Replaces a tag's data; hiding `Untagged` is refused.
    pub(crate) fn replace(&mut self, id: TagId, tag: Tag) -> Result<Tag, TagError> {
        if !self.tags.contains_key(id) {
            return Err(TagError::NotFound(id));
        }
        if id == self.untagged && !tag.visible {
            return Err(TagError::UntaggedProtected);
        }
        if self.by_name(&tag.name).is_some_and(|other| other != id) {
            return Err(TagError::DuplicateName(tag.name));
        }
        if let Some(f) = tag.folder
            && !self.folders.contains_key(f)
        {
            return Err(TagError::FolderNotFound(f));
        }
        let slot = self.tags.get_mut(id).ok_or(TagError::NotFound(id))?;
        Ok(std::mem::replace(slot, tag))
    }

    /// Removes a tag (entities using it are re-tagged by the caller).
    pub(crate) fn remove(&mut self, id: TagId) -> Result<Tag, TagError> {
        if id == self.untagged {
            return Err(TagError::UntaggedProtected);
        }
        let tag = self.tags.remove(id).ok_or(TagError::NotFound(id))?;
        self.order.retain(|x| *x != id);
        Ok(tag)
    }

    /// Adds a folder; `parent` must exist and not create a cycle.
    pub(crate) fn insert_folder(&mut self, folder: TagFolder) -> Result<TagFolderId, TagError> {
        if let Some(parent) = folder.parent
            && !self.folders.contains_key(parent)
        {
            return Err(TagError::FolderNotFound(parent));
        }
        // A new folder has no children, so it cannot close a cycle.
        let id = self.folders.insert(folder);
        self.folder_order.push(id);
        Ok(id)
    }

    /// Removes an empty folder.
    pub(crate) fn remove_folder(&mut self, id: TagFolderId) -> Result<TagFolder, TagError> {
        if !self.folders.contains_key(id) {
            return Err(TagError::FolderNotFound(id));
        }
        let used = self.tags.values().any(|t| t.folder == Some(id))
            || self.folders.values().any(|f| f.parent == Some(id));
        if used {
            return Err(TagError::FolderNotEmpty(id));
        }
        self.folder_order.retain(|x| *x != id);
        self.folders.remove(id).ok_or(TagError::FolderNotFound(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untagged_always_exists() {
        let tags = Tags::new();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags.by_name(Tag::UNTAGGED_NAME), Some(tags.untagged()));
        assert!(tags.get(tags.untagged()).unwrap().visible);
    }

    #[test]
    fn tags_are_unique_and_visibility_follows_folders() {
        let mut tags = Tags::new();
        let folder = tags
            .insert_folder(TagFolder {
                name: "Furniture".into(),
                visible: true,
                parent: None,
            })
            .unwrap();
        let mut chairs = Tag::new("Chairs", Rgba8::BLACK);
        chairs.folder = Some(folder);
        let id = tags.insert(chairs.clone()).unwrap();
        assert_eq!(
            tags.insert(chairs.clone()),
            Err(TagError::DuplicateName("Chairs".into()))
        );
        assert!(tags.is_visible(id));
        let mut hidden_folder = tags.folder(folder).unwrap().clone();
        hidden_folder.visible = false;
        tags.folders[folder] = hidden_folder;
        assert!(!tags.is_visible(id));
        assert_eq!(
            tags.remove_folder(folder),
            Err(TagError::FolderNotEmpty(folder))
        );

        let untagged = tags.untagged();
        let mut u = tags.get(untagged).unwrap().clone();
        u.visible = false;
        assert_eq!(tags.replace(untagged, u), Err(TagError::UntaggedProtected));
        assert_eq!(tags.remove(untagged), Err(TagError::UntaggedProtected));

        let index = tags.position(id).unwrap();
        let removed = tags.remove(id).unwrap();
        let back = tags.insert_at(removed, index).unwrap();
        assert_eq!(tags.position(back), Some(index));
    }
}
