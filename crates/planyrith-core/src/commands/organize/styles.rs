//! Implements docs/07-interaction-and-ui.md §6 "Styles" (the style editor)
//! as commands: `style-create`, `style-delete`, `style-edit`,
//! `style-watermark-add`, `style-watermark-remove`, and `.plyrs` style
//! libraries (`style-library-save`, `style-library-load`, docs/09 §1).
//! `style-set` (quick switches) and `style-activate` / `fog-set` are view
//! commands in `commands::view`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::organize::libraries::{library_options, storage};
use crate::commands::organize::tags::parse_color;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{
    Document, EdgeColorMode, FaceMode, Rgba8, Style, Watermark, WatermarkPlacement,
};
use crate::ids::StyleId;
use crate::io::native::{read_document, write_document};
use crate::io::{IoError, NoProgress, StorageKey};

fn meta(name: &str, label: &str, view_only: bool) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        category: CommandCategory::Organize,
        label: label.into(),
        view_only,
        steps: Vec::new(),
    }
}

/// A style by name; the active one when `name` is `None`.
pub(crate) fn style_named(doc: &Document, name: Option<&str>) -> Result<StyleId, CommandError> {
    match name {
        None => Ok(doc.styles.active()),
        Some(n) => doc
            .styles
            .by_name(n)
            .ok_or_else(|| CommandError::invalid_param("style", format!("no style named `{n}`"))),
    }
}

fn style_of(tx: &Transaction<'_>, id: StyleId) -> Result<Style, CommandError> {
    tx.document()
        .styles
        .get(id)
        .cloned()
        .ok_or_else(|| CommandError::Internal("style vanished".into()))
}

/// `#RRGGBB`, or `none` (for optional colors).
fn optional_color(param: &str, text: &str) -> Result<Option<Rgba8>, CommandError> {
    if text.eq_ignore_ascii_case("none") {
        Ok(None)
    } else {
        parse_color(param, text).map(Some)
    }
}

fn check_unit(param: &str, v: Option<f32>) -> Result<(), CommandError> {
    match v {
        Some(x) if !(0.0..=1.0).contains(&x) => Err(CommandError::OutOfRange {
            name: param.into(),
            reason: "must be between 0 and 1".into(),
        }),
        _ => Ok(()),
    }
}

/// Output naming a style.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StyleOutput {
    /// Style name.
    pub style: String,
}

// ----- create / delete ----------------------------------------------------------------------

/// `style-create` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StyleCreateParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Unique name.
    pub name: String,
    /// Style to copy (the active one).
    #[serde(default)]
    pub from: Option<String>,
    /// Make it the active style (true).
    #[serde(default = "yes")]
    pub activate: bool,
}

fn yes() -> bool {
    true
}

/// The `style-create` command: a copy of a style under a new name.
#[derive(Debug, Default, Clone, Copy)]
pub struct StyleCreate;

impl Command for StyleCreate {
    type Params = StyleCreateParams;
    type Output = StyleOutput;

    fn name(&self) -> &'static str {
        "style-create"
    }

    fn validate(&self, doc: &Document, p: &StyleCreateParams) -> Result<(), CommandError> {
        if p.name.trim().is_empty() || doc.styles.by_name(&p.name).is_some() {
            return Err(CommandError::invalid_param(
                "name",
                "must be a new, non-empty style name",
            ));
        }
        style_named(doc, p.from.as_deref()).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: StyleCreateParams,
    ) -> Result<StyleOutput, CommandError> {
        let from = style_named(tx.document(), p.from.as_deref())?;
        let mut style = style_of(tx, from)?;
        style.name = p.name.clone();
        let id = tx.insert_style(style)?;
        if p.activate {
            tx.set_active_style(id)?;
        }
        Ok(StyleOutput { style: p.name })
    }

    fn meta(&self) -> CommandMeta {
        meta("style-create", "Create Style", false)
    }
}

/// `style-delete` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StyleDeleteParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The style (not the active one).
    pub style: String,
}

/// The `style-delete` command. Scenes that activated it keep their other
/// properties and no longer change the style.
#[derive(Debug, Default, Clone, Copy)]
pub struct StyleDelete;

impl Command for StyleDelete {
    type Params = StyleDeleteParams;
    type Output = StyleOutput;

    fn name(&self) -> &'static str {
        "style-delete"
    }

    fn validate(&self, doc: &Document, p: &StyleDeleteParams) -> Result<(), CommandError> {
        let id = style_named(doc, Some(&p.style))?;
        if id == doc.styles.active() {
            return Err(CommandError::NotApplicable(
                "the active style cannot be deleted; activate another one first".into(),
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: StyleDeleteParams,
    ) -> Result<StyleOutput, CommandError> {
        let id = style_named(tx.document(), Some(&p.style))?;
        // Scenes referring to it stop changing the style.
        let scenes: Vec<_> = tx
            .document()
            .scenes
            .iter()
            .filter(|(_, s)| s.style == Some(id))
            .map(|(sid, s)| (sid, s.clone()))
            .collect();
        for (sid, mut scene) in scenes {
            scene.style = None;
            tx.replace_scene(sid, scene)?;
        }
        let removed = tx.remove_style(id)?;
        let dropped: Vec<_> = removed.watermarks.iter().map(|w| w.image).collect();
        tx.release_images(&dropped);
        Ok(StyleOutput { style: p.style })
    }

    fn meta(&self) -> CommandMeta {
        meta("style-delete", "Delete Style", false)
    }
}

// ----- edit ---------------------------------------------------------------------------------

/// `style-edit` parameters: every style field (absent ones keep their value).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StyleEditParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The style (the active one).
    #[serde(default)]
    pub style: Option<String>,
    /// New name.
    #[serde(default)]
    pub name: Option<String>,
    /// Draw edges.
    #[serde(default)]
    pub edges: Option<bool>,
    /// Profile edge width in pixels (0 = none).
    #[serde(default)]
    pub profiles: Option<u8>,
    /// Depth cue width in pixels (0 = none).
    #[serde(default)]
    pub depth_cue: Option<u8>,
    /// Edge extension in pixels (0 = none).
    #[serde(default)]
    pub extension: Option<u8>,
    /// Endpoint emphasis in pixels (0 = none).
    #[serde(default)]
    pub endpoints: Option<u8>,
    /// Sketchy jitter.
    #[serde(default)]
    pub jitter: Option<bool>,
    /// Edge color mode.
    #[serde(default)]
    pub edge_color: Option<EdgeColorMode>,
    /// Dashed back edges.
    #[serde(default)]
    pub back_edges: Option<bool>,
    /// Face mode.
    #[serde(default)]
    pub faces: Option<FaceMode>,
    /// Default front color `#RRGGBB`.
    #[serde(default)]
    pub front_color: Option<String>,
    /// Default back color `#RRGGBB`.
    #[serde(default)]
    pub back_color: Option<String>,
    /// Background color `#RRGGBB`.
    #[serde(default)]
    pub background: Option<String>,
    /// Sky color `#RRGGBB` or `none`.
    #[serde(default)]
    pub sky: Option<String>,
    /// Ground color `#RRGGBB` or `none`.
    #[serde(default)]
    pub ground: Option<String>,
    /// Ground opacity 0–1.
    #[serde(default)]
    pub ground_opacity: Option<f32>,
    /// Show the ground from below.
    #[serde(default)]
    pub ground_from_below: Option<bool>,
    /// Selection color `#RRGGBB`.
    #[serde(default)]
    pub selection_color: Option<String>,
    /// Locked color `#RRGGBB`.
    #[serde(default)]
    pub locked_color: Option<String>,
    /// Guide color `#RRGGBB`.
    #[serde(default)]
    pub guide_color: Option<String>,
    /// Fade of what lies outside the open group, 0–1.
    #[serde(default)]
    pub inactive_fade: Option<f32>,
}

/// The `style-edit` command (undoable; the Styles panel's editor).
#[derive(Debug, Default, Clone, Copy)]
pub struct StyleEdit;

fn width(w: u8) -> Option<u8> {
    (w > 0).then_some(w)
}

impl Command for StyleEdit {
    type Params = StyleEditParams;
    type Output = StyleOutput;

    fn name(&self) -> &'static str {
        "style-edit"
    }

    fn validate(&self, doc: &Document, p: &StyleEditParams) -> Result<(), CommandError> {
        let id = style_named(doc, p.style.as_deref())?;
        if let Some(name) = &p.name
            && (name.trim().is_empty() || doc.styles.by_name(name).is_some_and(|o| o != id))
        {
            return Err(CommandError::invalid_param(
                "name",
                "must be a new, non-empty style name",
            ));
        }
        for (param, value) in [
            ("front_color", &p.front_color),
            ("back_color", &p.back_color),
            ("background", &p.background),
            ("selection_color", &p.selection_color),
            ("locked_color", &p.locked_color),
            ("guide_color", &p.guide_color),
        ] {
            if let Some(v) = value {
                parse_color(param, v)?;
            }
        }
        for (param, value) in [("sky", &p.sky), ("ground", &p.ground)] {
            if let Some(v) = value {
                optional_color(param, v)?;
            }
        }
        check_unit("ground_opacity", p.ground_opacity)?;
        check_unit("inactive_fade", p.inactive_fade)
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: StyleEditParams,
    ) -> Result<StyleOutput, CommandError> {
        let id = style_named(tx.document(), p.style.as_deref())?;
        let mut s = style_of(tx, id)?;
        if let Some(n) = &p.name {
            s.name = n.clone();
        }
        let e = &mut s.edges;
        if let Some(v) = p.edges {
            e.show = v;
        }
        if let Some(v) = p.profiles {
            e.profiles = width(v);
        }
        if let Some(v) = p.depth_cue {
            e.depth_cue = width(v);
        }
        if let Some(v) = p.extension {
            e.extension = width(v);
        }
        if let Some(v) = p.endpoints {
            e.endpoints = width(v);
        }
        if let Some(v) = p.jitter {
            e.jitter = v;
        }
        if let Some(v) = p.edge_color {
            e.color = v;
        }
        if let Some(v) = p.back_edges {
            e.back_edges = v;
        }
        if let Some(v) = p.faces {
            s.faces.mode = v;
        }
        if let Some(v) = &p.front_color {
            s.faces.front_default = parse_color("front_color", v)?;
        }
        if let Some(v) = &p.back_color {
            s.faces.back_default = parse_color("back_color", v)?;
        }
        if let Some(v) = &p.background {
            s.background.color = parse_color("background", v)?;
        }
        if let Some(v) = &p.sky {
            s.background.sky = optional_color("sky", v)?;
        }
        let opacity = p
            .ground_opacity
            .or_else(|| s.background.ground.map(|(_, o)| o))
            .unwrap_or(0.5);
        if let Some(v) = &p.ground {
            s.background.ground = optional_color("ground", v)?.map(|c| (c, opacity));
        } else if let Some((c, _)) = s.background.ground {
            s.background.ground = Some((c, opacity));
        }
        if let Some(v) = p.ground_from_below {
            s.background.show_ground_from_below = v;
        }
        if let Some(v) = &p.selection_color {
            s.modeling.selection_color = parse_color("selection_color", v)?;
        }
        if let Some(v) = &p.locked_color {
            s.modeling.locked_color = parse_color("locked_color", v)?;
        }
        if let Some(v) = &p.guide_color {
            s.modeling.guide_color = parse_color("guide_color", v)?;
        }
        if let Some(v) = p.inactive_fade {
            s.modeling.inactive_fade = v;
        }
        let name = s.name.clone();
        tx.replace_style(id, s)?;
        Ok(StyleOutput { style: name })
    }

    fn meta(&self) -> CommandMeta {
        meta("style-edit", "Edit Style", false)
    }
}

// ----- watermarks ---------------------------------------------------------------------------

/// Where a watermark goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PlacementChoice {
    /// Over the whole viewport.
    Stretched,
    /// Repeated.
    Tiled,
    /// In the middle.
    Centered,
    /// At `anchor_x`, `anchor_y` (0–1 from the top left).
    #[default]
    Anchored,
}

/// `style-watermark-add` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StyleWatermarkAddParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The style (the active one).
    #[serde(default)]
    pub style: Option<String>,
    /// Image file (PNG, JPEG, WebP).
    pub image: String,
    /// Placement (anchored).
    #[serde(default)]
    pub placement: PlacementChoice,
    /// Horizontal anchor 0–1 (1 = right).
    #[serde(default = "one")]
    pub anchor_x: f32,
    /// Vertical anchor 0–1 (1 = bottom).
    #[serde(default = "one")]
    pub anchor_y: f32,
    /// Opacity 0–1 (1).
    #[serde(default = "one")]
    pub opacity: f32,
    /// Size relative to the viewport height (0.2).
    #[serde(default = "fifth")]
    pub scale: f32,
    /// Behind the model instead of over it.
    #[serde(default)]
    pub behind_model: bool,
}

fn one() -> f32 {
    1.0
}

fn fifth() -> f32 {
    0.2
}

/// `style-watermark-add` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StyleWatermarkOutput {
    /// Style name.
    pub style: String,
    /// Watermarks the style now has.
    pub watermarks: usize,
}

/// The `style-watermark-add` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct StyleWatermarkAdd;

impl Command for StyleWatermarkAdd {
    type Params = StyleWatermarkAddParams;
    type Output = StyleWatermarkOutput;

    fn name(&self) -> &'static str {
        "style-watermark-add"
    }

    fn validate(&self, doc: &Document, p: &StyleWatermarkAddParams) -> Result<(), CommandError> {
        style_named(doc, p.style.as_deref())?;
        check_unit("anchor_x", Some(p.anchor_x))?;
        check_unit("anchor_y", Some(p.anchor_y))?;
        check_unit("opacity", Some(p.opacity))?;
        if !(p.scale > 0.0 && p.scale <= 10.0) {
            return Err(CommandError::OutOfRange {
                name: "scale".into(),
                reason: "must be above 0 and at most 10".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: StyleWatermarkAddParams,
    ) -> Result<StyleWatermarkOutput, CommandError> {
        let id = style_named(tx.document(), p.style.as_deref())?;
        let bytes = storage(tx, &p.image)?.read(&StorageKey::new(&p.image))?;
        let image = tx.insert_image(bytes)?;
        let mut s = style_of(tx, id)?;
        s.watermarks.push(Watermark {
            image,
            placement: match p.placement {
                PlacementChoice::Stretched => WatermarkPlacement::Stretched,
                PlacementChoice::Tiled => WatermarkPlacement::Tiled,
                PlacementChoice::Centered => WatermarkPlacement::Centered,
                PlacementChoice::Anchored => WatermarkPlacement::Anchored(p.anchor_x, p.anchor_y),
            },
            opacity: p.opacity,
            scale: p.scale,
            behind_model: p.behind_model,
        });
        let out = StyleWatermarkOutput {
            style: s.name.clone(),
            watermarks: s.watermarks.len(),
        };
        tx.replace_style(id, s)?;
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        meta("style-watermark-add", "Add Watermark", false)
    }
}

/// `style-watermark-remove` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StyleWatermarkRemoveParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The style (the active one).
    #[serde(default)]
    pub style: Option<String>,
    /// Position in the style's watermark list (0 = first).
    pub index: u32,
}

/// The `style-watermark-remove` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct StyleWatermarkRemove;

impl Command for StyleWatermarkRemove {
    type Params = StyleWatermarkRemoveParams;
    type Output = StyleWatermarkOutput;

    fn name(&self) -> &'static str {
        "style-watermark-remove"
    }

    fn validate(&self, doc: &Document, p: &StyleWatermarkRemoveParams) -> Result<(), CommandError> {
        let id = style_named(doc, p.style.as_deref())?;
        let count = doc.styles.get(id).map_or(0, |s| s.watermarks.len());
        if p.index as usize >= count {
            return Err(CommandError::OutOfRange {
                name: "index".into(),
                reason: format!("the style has {count} watermark(s)"),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: StyleWatermarkRemoveParams,
    ) -> Result<StyleWatermarkOutput, CommandError> {
        let id = style_named(tx.document(), p.style.as_deref())?;
        let mut s = style_of(tx, id)?;
        let mut dropped = Vec::new();
        if (p.index as usize) < s.watermarks.len() {
            dropped.push(s.watermarks.remove(p.index as usize).image);
        }
        let out = StyleWatermarkOutput {
            style: s.name.clone(),
            watermarks: s.watermarks.len(),
        };
        tx.replace_style(id, s)?;
        tx.release_images(&dropped);
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        meta("style-watermark-remove", "Remove Watermark", false)
    }
}

// ----- libraries ----------------------------------------------------------------------------

/// `style-library-save` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StyleLibrarySaveParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Library file to write (`.plyrs`).
    pub path: String,
    /// Styles to include (all when empty).
    #[serde(default)]
    pub styles: Vec<String>,
}

/// Output of the style library commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StyleLibraryOutput {
    /// Styles written or added.
    pub styles: Vec<String>,
    /// Styles not added because the document has one with that name.
    #[serde(default)]
    pub skipped: Vec<String>,
}

/// The `style-library-save` command (changes nothing in the document).
#[derive(Debug, Default, Clone, Copy)]
pub struct StyleLibrarySave;

impl Command for StyleLibrarySave {
    type Params = StyleLibrarySaveParams;
    type Output = StyleLibraryOutput;

    fn name(&self) -> &'static str {
        "style-library-save"
    }

    fn validate(&self, doc: &Document, p: &StyleLibrarySaveParams) -> Result<(), CommandError> {
        if p.path.trim().is_empty() {
            return Err(CommandError::invalid_param("path", "must not be empty"));
        }
        for name in &p.styles {
            style_named(doc, Some(name))?;
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: StyleLibrarySaveParams,
    ) -> Result<StyleLibraryOutput, CommandError> {
        let doc = tx.document();
        let chosen: Vec<Style> = doc
            .styles
            .iter()
            .map(|(_, s)| s)
            .filter(|s| p.styles.is_empty() || p.styles.contains(&s.name))
            .cloned()
            .collect();
        let mut library = Document::new();
        library.info.title = "Style library".into();
        // The container's own style slot takes the first style.
        let first = library.styles.active();
        let mut names = Vec::new();
        for (i, style) in chosen.into_iter().enumerate() {
            for w in &style.watermarks {
                if let Some(data) = doc.images.get(w.image) {
                    library.images.insert(data.clone());
                }
            }
            names.push(style.name.clone());
            if i == 0 {
                library.styles.replace(first, style);
            } else {
                library.styles.insert(style);
            }
        }
        let bytes = write_document(&library, &[], &library_options(), &NoProgress)?;
        storage(tx, &p.path)?.write(&StorageKey::new(&p.path), &bytes)?;
        Ok(StyleLibraryOutput {
            styles: names,
            skipped: Vec::new(),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("style-library-save", "Save Style Library", true)
    }
}

/// `style-library-load` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StyleLibraryLoadParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Library file to read (`.plyrs`, or any `.plyr`).
    pub path: String,
    /// Replace styles of the same name instead of skipping them (false).
    #[serde(default)]
    pub replace: bool,
}

/// The `style-library-load` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct StyleLibraryLoad;

impl Command for StyleLibraryLoad {
    type Params = StyleLibraryLoadParams;
    type Output = StyleLibraryOutput;

    fn name(&self) -> &'static str {
        "style-library-load"
    }

    fn validate(&self, _doc: &Document, p: &StyleLibraryLoadParams) -> Result<(), CommandError> {
        if p.path.trim().is_empty() {
            return Err(CommandError::invalid_param("path", "must not be empty"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: StyleLibraryLoadParams,
    ) -> Result<StyleLibraryOutput, CommandError> {
        let bytes = storage(tx, &p.path)?.read(&StorageKey::new(&p.path))?;
        let library = read_document(&bytes, &NoProgress)?.document;
        let mut out = StyleLibraryOutput {
            styles: Vec::new(),
            skipped: Vec::new(),
        };
        for (_, style) in library.styles.iter() {
            let mut style = style.clone();
            for w in &mut style.watermarks {
                let data = library.images.get(w.image).ok_or_else(|| {
                    CommandError::Io(IoError::Corrupt {
                        part: p.path.clone(),
                        reason: format!("a watermark of `{}` is missing", style.name),
                    })
                })?;
                w.image = tx.insert_image(data.bytes.clone())?;
            }
            match tx.document().styles.by_name(&style.name) {
                Some(id) if p.replace => {
                    out.styles.push(style.name.clone());
                    let dropped: Vec<_> = tx
                        .document()
                        .styles
                        .get(id)
                        .map(|old| old.watermarks.iter().map(|w| w.image).collect())
                        .unwrap_or_default();
                    tx.replace_style(id, style)?;
                    tx.release_images(&dropped);
                }
                Some(_) => out.skipped.push(style.name.clone()),
                None => {
                    out.styles.push(style.name.clone());
                    tx.insert_style(style)?;
                }
            }
        }
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        meta("style-library-load", "Load Style Library", false)
    }
}
