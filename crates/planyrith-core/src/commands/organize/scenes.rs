//! Implements docs/05-modeling-tools.md §5.7 (scenes): `scene-add`,
//! `scene-update`, `scene-delete`, `scene-rename`, `scene-reorder` (undoable)
//! and `scene-activate` (a view command, not undoable). Milestone 0.2 scenes
//! capture and restore the camera, tag visibility and the active style; the
//! other properties of docs/04 §6 arrive with their features (docs/05
//! Decisions).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Camera, Document, Scene};
use crate::ids::SceneId;

fn meta(name: &str, label: &str) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        category: CommandCategory::Organize,
        label: label.into(),
        view_only: false,
        steps: Vec::new(),
    }
}

fn scene_named(doc: &Document, name: &str) -> Result<SceneId, CommandError> {
    doc.scenes
        .by_name(name)
        .ok_or_else(|| CommandError::invalid_param("scene", format!("no scene named `{name}`")))
}

fn check_new_name(doc: &Document, param: &str, name: &str) -> Result<(), CommandError> {
    if name.trim().is_empty() {
        return Err(CommandError::invalid_param(param, "must not be empty"));
    }
    if doc.scenes.by_name(name).is_some() {
        return Err(CommandError::invalid_param(
            param,
            format!("scene `{name}` exists"),
        ));
    }
    Ok(())
}

/// What a scene captures (docs/05 §5.7 `SceneCaptureFlags`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SceneCapture {
    /// The camera.
    pub camera: bool,
    /// The visibility of every tag.
    pub tags: bool,
    /// The active style.
    pub style: bool,
}

impl Default for SceneCapture {
    fn default() -> Self {
        SceneCapture {
            camera: true,
            tags: true,
            style: true,
        }
    }
}

/// Fills the captured properties of `scene` from the document (and `camera`).
fn capture(doc: &Document, camera: Camera, flags: SceneCapture, scene: &mut Scene) {
    scene.camera = flags.camera.then_some(camera);
    scene.tag_visibility = flags.tags.then(|| {
        doc.tags
            .iter()
            .map(|(id, t)| (id, t.visible))
            .collect::<BTreeMap<_, _>>()
    });
    scene.style = flags.style.then(|| doc.styles.active());
}

/// A scene command's result.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SceneOutput {
    /// The scene's name.
    pub name: String,
}

// ----- scene-add / scene-update ---------------------------------------------------------------

/// `scene-add` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SceneAddParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Unique name.
    pub name: String,
    /// What to capture (all by default).
    #[serde(default)]
    pub capture: SceneCapture,
}

/// The `scene-add` command: a new last scene capturing the current view.
#[derive(Debug, Default, Clone, Copy)]
pub struct SceneAdd;

impl Command for SceneAdd {
    type Params = SceneAddParams;
    type Output = SceneOutput;

    fn name(&self) -> &'static str {
        "scene-add"
    }

    fn validate(&self, doc: &Document, p: &SceneAddParams) -> Result<(), CommandError> {
        check_new_name(doc, "name", &p.name)
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SceneAddParams,
    ) -> Result<SceneOutput, CommandError> {
        let mut scene = Scene::named(p.name.clone());
        capture(tx.document(), tx.camera(), p.capture, &mut scene);
        tx.insert_scene(scene)?;
        Ok(SceneOutput { name: p.name })
    }

    fn meta(&self) -> CommandMeta {
        meta("scene-add", "Add Scene")
    }
}

/// `scene-update` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SceneUpdateParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The scene.
    pub scene: String,
    /// What to capture again (all by default; the rest is cleared).
    #[serde(default)]
    pub capture: SceneCapture,
}

/// The `scene-update` command: re-captures the current view into a scene.
#[derive(Debug, Default, Clone, Copy)]
pub struct SceneUpdate;

impl Command for SceneUpdate {
    type Params = SceneUpdateParams;
    type Output = SceneOutput;

    fn name(&self) -> &'static str {
        "scene-update"
    }

    fn validate(&self, doc: &Document, p: &SceneUpdateParams) -> Result<(), CommandError> {
        scene_named(doc, &p.scene).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SceneUpdateParams,
    ) -> Result<SceneOutput, CommandError> {
        let id = scene_named(tx.document(), &p.scene)?;
        let mut scene = tx
            .document()
            .scenes
            .get(id)
            .cloned()
            .ok_or_else(|| CommandError::Internal("scene vanished".into()))?;
        capture(tx.document(), tx.camera(), p.capture, &mut scene);
        tx.replace_scene(id, scene)?;
        Ok(SceneOutput { name: p.scene })
    }

    fn meta(&self) -> CommandMeta {
        meta("scene-update", "Update Scene")
    }
}

// ----- scene-delete / scene-rename / scene-reorder ---------------------------------------------

/// `scene-delete` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SceneDeleteParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The scene.
    pub scene: String,
}

/// The `scene-delete` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct SceneDelete;

impl Command for SceneDelete {
    type Params = SceneDeleteParams;
    type Output = SceneOutput;

    fn name(&self) -> &'static str {
        "scene-delete"
    }

    fn validate(&self, doc: &Document, p: &SceneDeleteParams) -> Result<(), CommandError> {
        scene_named(doc, &p.scene).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SceneDeleteParams,
    ) -> Result<SceneOutput, CommandError> {
        let id = scene_named(tx.document(), &p.scene)?;
        tx.remove_scene(id)?;
        Ok(SceneOutput { name: p.scene })
    }

    fn meta(&self) -> CommandMeta {
        meta("scene-delete", "Delete Scene")
    }
}

/// `scene-rename` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SceneRenameParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The scene.
    pub scene: String,
    /// New unique name.
    pub new_name: String,
}

/// The `scene-rename` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct SceneRename;

impl Command for SceneRename {
    type Params = SceneRenameParams;
    type Output = SceneOutput;

    fn name(&self) -> &'static str {
        "scene-rename"
    }

    fn validate(&self, doc: &Document, p: &SceneRenameParams) -> Result<(), CommandError> {
        scene_named(doc, &p.scene)?;
        if p.new_name != p.scene {
            check_new_name(doc, "new_name", &p.new_name)?;
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SceneRenameParams,
    ) -> Result<SceneOutput, CommandError> {
        let id = scene_named(tx.document(), &p.scene)?;
        let mut scene = tx
            .document()
            .scenes
            .get(id)
            .cloned()
            .ok_or_else(|| CommandError::Internal("scene vanished".into()))?;
        scene.name = p.new_name.clone();
        tx.replace_scene(id, scene)?;
        Ok(SceneOutput { name: p.new_name })
    }

    fn meta(&self) -> CommandMeta {
        meta("scene-rename", "Rename Scene")
    }
}

/// `scene-reorder` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SceneReorderParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The scene.
    pub scene: String,
    /// New position (0 = first; past the end = last).
    pub index: u32,
}

/// The `scene-reorder` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct SceneReorder;

impl Command for SceneReorder {
    type Params = SceneReorderParams;
    type Output = SceneOutput;

    fn name(&self) -> &'static str {
        "scene-reorder"
    }

    fn validate(&self, doc: &Document, p: &SceneReorderParams) -> Result<(), CommandError> {
        scene_named(doc, &p.scene).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SceneReorderParams,
    ) -> Result<SceneOutput, CommandError> {
        let id = scene_named(tx.document(), &p.scene)?;
        let last = tx.document().scenes.len().saturating_sub(1);
        tx.move_scene(id, (p.index as usize).min(last))?;
        Ok(SceneOutput { name: p.scene })
    }

    fn meta(&self) -> CommandMeta {
        meta("scene-reorder", "Reorder Scenes")
    }
}

// ----- scene-activate -----------------------------------------------------------------------

/// `scene-activate` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SceneActivateParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The scene.
    pub scene: String,
    /// Jump without the animated transition (frontends animate otherwise).
    #[serde(default)]
    pub no_transition: bool,
}

/// The `scene-activate` command: restores what the scene captured (a view
/// command: not undoable).
#[derive(Debug, Default, Clone, Copy)]
pub struct SceneActivate;

impl Command for SceneActivate {
    type Params = SceneActivateParams;
    type Output = SceneOutput;

    fn name(&self) -> &'static str {
        "scene-activate"
    }

    fn validate(&self, doc: &Document, p: &SceneActivateParams) -> Result<(), CommandError> {
        scene_named(doc, &p.scene).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SceneActivateParams,
    ) -> Result<SceneOutput, CommandError> {
        let id = scene_named(tx.document(), &p.scene)?;
        let scene = tx
            .document()
            .scenes
            .get(id)
            .cloned()
            .ok_or_else(|| CommandError::Internal("scene vanished".into()))?;
        if let Some(camera) = scene.camera {
            tx.set_camera(camera);
        }
        if let Some(visibility) = &scene.tag_visibility {
            for (&tag_id, &visible) in visibility {
                let Some(tag) = tx.document().tags.get(tag_id) else {
                    continue;
                };
                if tag.visible != visible {
                    let mut tag = tag.clone();
                    tag.visible = visible;
                    tx.replace_tag(tag_id, tag)?;
                }
            }
        }
        if let Some(style) = scene.style
            && tx.document().styles.get(style).is_some()
            && tx.document().styles.active() != style
        {
            tx.set_active_style(style)?;
        }
        Ok(SceneOutput { name: p.scene })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            category: CommandCategory::View,
            view_only: true,
            ..meta("scene-activate", "Activate Scene")
        }
    }
}
