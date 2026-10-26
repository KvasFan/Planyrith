//! Implements docs/05-modeling-tools.md §5.7 (scenes): `scene-add`,
//! `scene-update`, `scene-delete`, `scene-rename`, `scene-reorder` (undoable)
//! and `scene-activate` (a view command, not undoable). Scenes capture and
//! restore the camera, tag visibility, the active style, hidden entities,
//! section plane states and shadow settings; the axes visibility is the
//! frontend's to give and to apply (docs/05 Decisions).

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Camera, Document, EntityRecord, EntityView, Scene};
use crate::ids::{EntityId, Pid, SceneId};

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
    /// Which entities are hidden.
    pub hidden: bool,
    /// Which section planes are active.
    pub sections: bool,
    /// The shadow settings.
    pub shadows: bool,
}

impl Default for SceneCapture {
    fn default() -> Self {
        SceneCapture {
            camera: true,
            tags: true,
            style: true,
            hidden: true,
            sections: true,
            shadows: true,
        }
    }
}

/// Scene properties that are not captured from the view.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SceneProperties {
    /// Free text.
    pub description: Option<String>,
    /// Seconds the transition to this scene takes (0 jumps).
    pub transition_s: Option<f32>,
    /// Whether the scene animation plays this scene.
    pub include_in_animation: Option<bool>,
    /// Axes visibility to store (the frontend's setting; `None` keeps it).
    pub axes_visible: Option<bool>,
}

fn check_properties(p: &SceneProperties) -> Result<(), CommandError> {
    if let Some(t) = p.transition_s
        && !(t.is_finite() && (0.0..=600.0).contains(&t))
    {
        return Err(CommandError::OutOfRange {
            name: "transition_s".into(),
            reason: "must be between 0 and 600 seconds".into(),
        });
    }
    Ok(())
}

fn set_properties(p: &SceneProperties, scene: &mut Scene) {
    if let Some(d) = &p.description {
        scene.description = d.clone();
    }
    if let Some(t) = p.transition_s {
        scene.transition_s = t;
    }
    if let Some(i) = p.include_in_animation {
        scene.include_in_animation = i;
    }
    if p.axes_visible.is_some() {
        scene.axes_visible = p.axes_visible;
    }
}

/// Every entity occurrence's pid with its container and id, in container
/// order (scenes refer to entities by pid).
fn all_pids(doc: &Document) -> Vec<(crate::document::ContainerRef, EntityId, Pid)> {
    let mut out = Vec::new();
    for c in doc.containers() {
        if let Some(entities) = doc.container(c) {
            for id in entities.iter() {
                if let Some(pid) = entities.pid_of(id) {
                    out.push((c, id, pid));
                }
            }
        }
    }
    out
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
    scene.hidden_entities = flags.hidden.then(|| {
        all_pids(doc)
            .into_iter()
            .filter(|(c, id, _)| doc.container(*c).is_some_and(|e| e.is_hidden(*id)))
            .map(|(_, _, pid)| pid)
            .collect::<BTreeSet<_>>()
    });
    scene.section_planes = flags.sections.then(|| {
        all_pids(doc)
            .into_iter()
            .filter_map(|(c, id, pid)| match doc.container(c)?.view(id)? {
                EntityView::SectionPlane(s) => Some((pid, s.active)),
                _ => None,
            })
            .collect::<BTreeMap<_, _>>()
    });
    scene.shadows = flags.shadows.then_some(doc.rendering.shadows);
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
    /// Description, transition, animation and axes.
    #[serde(flatten)]
    pub properties: SceneProperties,
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
        check_new_name(doc, "name", &p.name)?;
        check_properties(&p.properties)
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SceneAddParams,
    ) -> Result<SceneOutput, CommandError> {
        let mut scene = Scene::named(p.name.clone());
        capture(tx.document(), tx.camera(), p.capture, &mut scene);
        set_properties(&p.properties, &mut scene);
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
    /// Keep the captured view and change only the properties.
    #[serde(default)]
    pub properties_only: bool,
    /// Description, transition, animation and axes.
    #[serde(flatten)]
    pub properties: SceneProperties,
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
        scene_named(doc, &p.scene)?;
        check_properties(&p.properties)
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
        if !p.properties_only {
            capture(tx.document(), tx.camera(), p.capture, &mut scene);
        }
        set_properties(&p.properties, &mut scene);
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
        if let Some(hidden) = &scene.hidden_entities {
            for (c, id, pid) in all_pids(tx.document()) {
                // Hidden applies to entities that can be hidden; unknown
                // pids (deleted since) are ignored.
                let want = hidden.contains(&pid);
                let is = tx.document().container(c).is_some_and(|e| e.is_hidden(id));
                if want != is {
                    tx.set_hidden(c, id, want)?;
                }
            }
        }
        if let Some(states) = &scene.section_planes {
            for (c, id, pid) in all_pids(tx.document()) {
                let Some(&active) = states.get(&pid) else {
                    continue;
                };
                let Some(EntityView::SectionPlane(plane)) =
                    tx.document().container(c).and_then(|e| e.view(id))
                else {
                    continue;
                };
                if plane.active != active {
                    let mut plane = plane.clone();
                    plane.active = active;
                    tx.replace_entity(c, id, EntityRecord::SectionPlane(plane))?;
                }
            }
        }
        if let Some(shadows) = &scene.shadows
            && tx.document().rendering.shadows != *shadows
        {
            let mut rendering = tx.document().rendering.clone();
            rendering.shadows = *shadows;
            tx.set_rendering(rendering)?;
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
