//! Implements docs/05-modeling-tools.md §9 `style-set`: edits the active
//! style's face mode and edge settings as a view command (not undoable, like
//! the other display switches; the style is saved with the document).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Document, EdgeColorMode, FaceMode};

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
