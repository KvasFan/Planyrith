//! Implements docs/04-document-model.md §6 (scenes): saved view property sets
//! where every `None` means "do not change on activation".

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use slotmap::SlotMap;

use crate::document::camera::Camera;
use crate::document::shadows::ShadowSettings;
use crate::ids::{Pid, SceneId, StyleId, TagId};

/// A scene (docs/04 §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    /// Unique name.
    pub name: String,
    /// Free text.
    pub description: String,
    /// Camera to restore.
    pub camera: Option<Camera>,
    /// Tag visibility to restore.
    #[serde(default, with = "crate::document::serde_seq::option")]
    pub tag_visibility: Option<BTreeMap<TagId, bool>>,
    /// Entities hidden in this scene (by pid).
    pub hidden_entities: Option<BTreeSet<Pid>>,
    /// Section plane active flags (by pid).
    pub section_planes: Option<BTreeMap<Pid, bool>>,
    /// Style to activate.
    pub style: Option<StyleId>,
    /// Shadow settings to restore.
    pub shadows: Option<ShadowSettings>,
    /// Axes visibility.
    pub axes_visible: Option<bool>,
    /// Transition time in seconds.
    pub transition_s: f32,
    /// Included when playing the scene animation.
    pub include_in_animation: bool,
}

impl Scene {
    /// A scene capturing nothing but a name.
    pub fn named(name: impl Into<String>) -> Self {
        Scene {
            name: name.into(),
            description: String::new(),
            camera: None,
            tag_visibility: None,
            hidden_entities: None,
            section_planes: None,
            style: None,
            shadows: None,
            axes_visible: None,
            transition_s: 1.0,
            include_in_animation: true,
        }
    }
}

/// The ordered scene list (docs/04 §1 "Scenes: ordered list of Scene").
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scenes {
    items: SlotMap<SceneId, Scene>,
    order: Vec<SceneId>,
}

impl Scenes {
    /// Hard limit on the number of scenes (docs/04 §14).
    pub const MAX: usize = 65_535;

    /// Empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of scenes.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// `true` when there are no scenes.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Scene by id.
    pub fn get(&self, id: SceneId) -> Option<&Scene> {
        self.items.get(id)
    }

    /// Scene by name.
    pub fn by_name(&self, name: &str) -> Option<SceneId> {
        self.order
            .iter()
            .copied()
            .find(|id| self.items.get(*id).is_some_and(|s| s.name == name))
    }

    /// Scenes in list order.
    pub fn iter(&self) -> impl Iterator<Item = (SceneId, &Scene)> + '_ {
        self.order
            .iter()
            .filter_map(|id| self.items.get(*id).map(|s| (*id, s)))
    }

    /// Position of a scene in the list.
    pub fn index_of(&self, id: SceneId) -> Option<usize> {
        self.order.iter().position(|s| *s == id)
    }

    /// Appends a scene.
    pub(crate) fn insert(&mut self, scene: Scene) -> SceneId {
        let id = self.items.insert(scene);
        self.order.push(id);
        id
    }

    /// Inserts at `index` (undo of a removal).
    pub(crate) fn insert_at(&mut self, index: usize, scene: Scene) -> SceneId {
        let id = self.items.insert(scene);
        self.order.insert(index.min(self.order.len()), id);
        id
    }

    /// Removes a scene, returning it and its former index.
    pub(crate) fn remove(&mut self, id: SceneId) -> Option<(Scene, usize)> {
        let index = self.index_of(id)?;
        let scene = self.items.remove(id)?;
        self.order.remove(index);
        Some((scene, index))
    }

    /// Replaces a scene's data.
    pub(crate) fn replace(&mut self, id: SceneId, scene: Scene) -> Option<Scene> {
        self.items
            .get_mut(id)
            .map(|slot| std::mem::replace(slot, scene))
    }

    /// Moves a scene to `index`.
    pub(crate) fn reorder(&mut self, id: SceneId, index: usize) -> Option<()> {
        let from = self.index_of(id)?;
        let id = self.order.remove(from);
        self.order.insert(index.min(self.order.len()), id);
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_remove_reorder_keep_order() {
        let mut scenes = Scenes::new();
        let a = scenes.insert(Scene::named("a"));
        let b = scenes.insert(Scene::named("b"));
        let c = scenes.insert(Scene::named("c"));
        assert_eq!(
            scenes
                .iter()
                .map(|(_, s)| s.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        scenes.reorder(c, 0).unwrap();
        assert_eq!(scenes.index_of(c), Some(0));
        let (removed, idx) = scenes.remove(b).unwrap();
        assert_eq!((removed.name.as_str(), idx), ("b", 2));
        scenes.insert_at(idx, removed);
        assert_eq!(scenes.len(), 3);
        assert_eq!(scenes.by_name("a"), Some(a));
    }
}
