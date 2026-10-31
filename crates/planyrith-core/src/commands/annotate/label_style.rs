//! Implements docs/05 §7.3: edit annotation appearance without changing its anchors.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{refuse_locked, targets_in};
use crate::commands::organize::parse_color_param as parse_color;
use crate::commands::params::target_container;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{
    ArrowStyle, DimStyle, Dimension, Document, EntityRecord, EntityView, Query, TextFont,
};
use crate::units::Length;

fn selection() -> Query {
    Query::from("selection")
}

/// Patch applied to the selected texts and dimensions; absent fields stay unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LabelStyleParams {
    /// Common context and selection fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Annotation query (selection by default); unrelated entity kinds are ignored.
    #[serde(default = "selection")]
    pub targets: Query,
    /// Font family, weight, point size or model height in mm.
    #[serde(default)]
    pub font: Option<TextFont>,
    /// Leader/dimension arrow shape.
    #[serde(default)]
    pub arrow: Option<ArrowStyle>,
    /// Label text color, #RRGGBB or #RRGGBBAA.
    #[serde(default)]
    pub color: Option<String>,
    /// Dimension text/arrow height in mm.
    #[serde(default)]
    pub text_height: Option<Length>,
    /// Show text leaders; dimension lines are unaffected.
    #[serde(default)]
    pub leader_visible: Option<bool>,
    /// Reset appearance before applying the patch; keep content, placement and anchors.
    #[serde(default)]
    pub reset: bool,
}

/// Number of annotations restyled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LabelStyleOutput {
    /// Modified text and dimension count.
    pub updated: usize,
}

/// Changes annotation appearance in one undoable transaction.
pub struct LabelStyle;

impl Command for LabelStyle {
    type Params = LabelStyleParams;
    type Output = LabelStyleOutput;
    fn name(&self) -> &'static str {
        "label-style"
    }
    fn validate(&self, _doc: &Document, p: &Self::Params) -> Result<(), CommandError> {
        if !p.reset
            && p.font.is_none()
            && p.arrow.is_none()
            && p.color.is_none()
            && p.text_height.is_none()
            && p.leader_visible.is_none()
        {
            return Err(CommandError::invalid_param(
                "style",
                "provide an appearance field or reset",
            ));
        }
        if let Some(font) = &p.font
            && (font.family.trim().is_empty()
                || font.family.len() > 512
                || !font.size_pt.is_finite()
                || !(0.0..=1000.0).contains(&font.size_pt)
                || font.size_pt <= 0.0
                || font
                    .model_height_mm
                    .is_some_and(|h| !h.is_finite() || h <= 0.0 || h > 1e9))
        {
            return Err(CommandError::invalid_param(
                "font",
                "use a family and positive finite sizes (at most 1000 pt or 1e9 mm)",
            ));
        }
        if p.text_height
            .is_some_and(|h| !h.as_mm().is_finite() || h.as_mm() <= 0.0 || h.as_mm() > 1e9)
        {
            return Err(CommandError::invalid_param(
                "text_height",
                "must be positive and finite, at most 1e9 mm",
            ));
        }
        if let Some(color) = &p.color {
            parse_color("color", color)?;
        }
        Ok(())
    }
    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: Self::Params,
    ) -> Result<Self::Output, CommandError> {
        let target = target_container(tx, &p.common)?;
        let ids = targets_in(tx, &target, &p.targets)?
            .into_iter()
            .filter(|id| {
                matches!(
                    id,
                    crate::ids::EntityId::Text(_) | crate::ids::EntityId::Dimension(_)
                )
            })
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Err(CommandError::NotApplicable(
                "no text or dimension to style".into(),
            ));
        }
        refuse_locked(tx, target.container, &ids)?;
        let color = p
            .color
            .as_deref()
            .map(|c| parse_color("color", c))
            .transpose()?;
        for id in &ids {
            let entities = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("annotation container vanished".into()))?;
            let record = match entities.view(*id) {
                Some(EntityView::Text(text)) => {
                    let mut text = text.clone();
                    let mut layout = text.layout.unwrap_or_else(|| crate::document::TextLayout {
                        leader_visible: text.anchor.is_some(),
                        arrow: ArrowStyle::Dot,
                        ..Default::default()
                    });
                    if p.reset {
                        layout.font = None;
                        layout.color = None;
                        layout.arrow = ArrowStyle::Dot;
                        layout.leader_visible = text.anchor.is_some();
                    }
                    if let Some(font) = &p.font {
                        layout.font = Some(font.clone());
                    }
                    if let Some(arrow) = p.arrow {
                        layout.arrow = arrow;
                    }
                    if let Some(color) = color {
                        layout.color = Some(color);
                    }
                    if let Some(visible) = p.leader_visible {
                        layout.leader_visible = visible;
                    }
                    text.layout = Some(layout);
                    EntityRecord::Text(text)
                }
                Some(EntityView::Dimension(dim)) => {
                    let mut dim = dim.clone();
                    let style = match &mut dim {
                        Dimension::Linear { style, .. } | Dimension::Radial { style, .. } => style,
                    };
                    if p.reset {
                        *style = DimStyle::default();
                    }
                    if let Some(font) = &p.font {
                        style.font = Some(font.clone());
                        if let Some(height) = font.model_height_mm {
                            style.text_height = Length::mm(height);
                        }
                    }
                    if let Some(arrow) = p.arrow {
                        style.arrow = arrow;
                    }
                    if let Some(color) = color {
                        style.color = Some(color);
                    }
                    if let Some(height) = p.text_height {
                        style.text_height = height;
                        if let Some(font) = &mut style.font {
                            font.model_height_mm = Some(height.as_mm());
                        }
                    }
                    EntityRecord::Dimension(dim)
                }
                _ => return Err(CommandError::Internal("annotation vanished".into())),
            };
            tx.replace_entity(target.container, *id, record)?;
        }
        Ok(LabelStyleOutput { updated: ids.len() })
    }
    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: self.name().into(),
            category: CommandCategory::Annotate,
            label: "Label Style".into(),
            view_only: false,
            steps: Vec::new(),
        }
    }
}
