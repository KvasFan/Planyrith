//! Implements docs/05-modeling-tools.md §9 `style-set`, `style-activate` and
//! `fog-set`: the active style's quick switches, which style is active, and
//! fog, as view commands (not undoable, like the other display switches; the
//! style is saved with the document). The full editor is `style-edit`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Document, EdgeColorMode, FaceMode, Fog};
use crate::units::Length;

/// `style-set` parameters (docs/05 §9); absent fields keep their value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StyleSetParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Face mode.
    #[serde(default)]
    pub faces: Option<FaceMode>,
    /// Draw edges.
    #[serde(default)]
    pub edges: Option<bool>,
    /// Profile edge width in pixels (0 = no profiles).
    #[serde(default)]
    pub profiles: Option<u8>,
    /// Draw back edges dashed.
    #[serde(default)]
    pub back_edges: Option<bool>,
    /// Edge color mode.
    #[serde(default)]
    pub edge_color: Option<EdgeColorMode>,
}

/// `style-set` output.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct StyleSetOutput {
    /// The edited style's name.
    pub style: String,
}

/// The `style-set` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct StyleSet;

impl Command for StyleSet {
    type Params = StyleSetParams;
    type Output = StyleSetOutput;

    fn name(&self) -> &'static str {
        "style-set"
    }

    fn validate(&self, _doc: &Document, _p: &StyleSetParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: StyleSetParams,
    ) -> Result<StyleSetOutput, CommandError> {
        let id = tx.document().styles.active();
        let mut style = tx
            .document()
            .styles
            .get(id)
            .cloned()
            .ok_or_else(|| CommandError::Internal("the active style does not exist".into()))?;
        if let Some(mode) = p.faces {
            style.faces.mode = mode;
        }
        if let Some(show) = p.edges {
            style.edges.show = show;
        }
        if let Some(width) = p.profiles {
            style.edges.profiles = (width > 0).then_some(width);
        }
        if let Some(back) = p.back_edges {
            style.edges.back_edges = back;
        }
        if let Some(color) = p.edge_color {
            style.edges.color = color;
        }
        let name = style.name.clone();
        tx.replace_style(id, style)?;
        Ok(StyleSetOutput { style: name })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "style-set".into(),
            category: CommandCategory::View,
            label: "Style".into(),
            view_only: true,
            steps: Vec::new(),
        }
    }
}

// ----- style-activate -----------------------------------------------------------------------

/// `style-activate` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StyleActivateParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The style.
    pub style: String,
}

/// The `style-activate` command: shows the model in another style (a view
/// command, like activating a scene).
#[derive(Debug, Default, Clone, Copy)]
pub struct StyleActivate;

impl Command for StyleActivate {
    type Params = StyleActivateParams;
    type Output = StyleSetOutput;

    fn name(&self) -> &'static str {
        "style-activate"
    }

    fn validate(&self, doc: &Document, p: &StyleActivateParams) -> Result<(), CommandError> {
        doc.styles.by_name(&p.style).map(|_| ()).ok_or_else(|| {
            CommandError::invalid_param("style", format!("no style named `{}`", p.style))
        })
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: StyleActivateParams,
    ) -> Result<StyleSetOutput, CommandError> {
        let id = tx.document().styles.by_name(&p.style).ok_or_else(|| {
            CommandError::invalid_param("style", format!("no style named `{}`", p.style))
        })?;
        tx.set_active_style(id)?;
        Ok(StyleSetOutput { style: p.style })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "style-activate".into(),
            category: CommandCategory::View,
            label: "Activate Style".into(),
            view_only: true,
            steps: Vec::new(),
        }
    }
}

// ----- fog-set ------------------------------------------------------------------------------

/// `fog-set` parameters (docs/05 §9); absent fields keep their value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FogSetParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Fog on or off.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Distance where the fog starts.
    #[serde(default)]
    pub start: Option<Length>,
    /// Distance where the fog is complete.
    #[serde(default)]
    pub end: Option<Length>,
    /// `#RRGGBB`, or `background` to follow the background color.
    #[serde(default)]
    pub color: Option<String>,
}

/// `fog-set` output.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct FogSetOutput {
    /// Fog on.
    pub enabled: bool,
    /// Start distance, mm.
    pub start: f64,
    /// End distance, mm.
    pub end: f64,
}

/// The `fog-set` command: fog on/off (document rendering option) and its
/// distances and color (the active style's).
#[derive(Debug, Default, Clone, Copy)]
pub struct FogSet;

impl Command for FogSet {
    type Params = FogSetParams;
    type Output = FogSetOutput;

    fn name(&self) -> &'static str {
        "fog-set"
    }

    fn validate(&self, _doc: &Document, p: &FogSetParams) -> Result<(), CommandError> {
        for (name, l) in [("start", p.start), ("end", p.end)] {
            if let Some(l) = l
                && !(l.as_mm() >= 0.0 && l.as_mm().is_finite())
            {
                return Err(CommandError::OutOfRange {
                    name: name.into(),
                    reason: "must be a distance of 0 or more".into(),
                });
            }
        }
        if let Some(c) = &p.color
            && c != "background"
        {
            crate::commands::organize::parse_color_param("color", c)?;
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: FogSetParams,
    ) -> Result<FogSetOutput, CommandError> {
        let id = tx.document().styles.active();
        let mut style = tx
            .document()
            .styles
            .get(id)
            .cloned()
            .ok_or_else(|| CommandError::Internal("the active style does not exist".into()))?;
        let mut fog = style.fog.unwrap_or(Fog {
            start: 10_000.0,
            end: 100_000.0,
            use_background_color: true,
            color: style.background.color,
        });
        if let Some(l) = p.start {
            fog.start = l.as_mm();
        }
        if let Some(l) = p.end {
            fog.end = l.as_mm();
        }
        if fog.end <= fog.start {
            return Err(CommandError::invalid_param(
                "end",
                "the fog must end farther than it starts",
            ));
        }
        match p.color.as_deref() {
            Some("background") => fog.use_background_color = true,
            Some(c) => {
                fog.use_background_color = false;
                fog.color = crate::commands::organize::parse_color_param("color", c)?;
            }
            None => {}
        }
        style.fog = Some(fog);
        tx.replace_style(id, style)?;
        let mut rendering = tx.document().rendering.clone();
        if let Some(on) = p.enabled {
            rendering.fog_enabled = on;
        }
        let enabled = rendering.fog_enabled;
        tx.set_rendering(rendering)?;
        Ok(FogSetOutput {
            enabled,
            start: fog.start,
            end: fog.end,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "fog-set".into(),
            category: CommandCategory::View,
            label: "Fog".into(),
            view_only: true,
            steps: Vec::new(),
        }
    }
}
