//! Implements docs/05-modeling-tools.md §7.1 `dimension`: aligned and
//! linear dimensions between two points, radius and diameter dimensions of
//! arcs and circles. Points snap to the vertex or edge midpoint they lie on
//! (`annotations::find_anchor`), so the dimension re-measures when that
//! geometry moves (docs/04 §3.5).

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::params::{
    PointSchema, VectorSchema, check_point, command_context, resolve_ref, select, target_container,
};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::annotations::{dimension_view, find_anchor};
use crate::document::{DimStyle, Dimension, Document, EntityRecord, EntityRef, RadialKind};
use crate::ids::{EntityId, Pid};
use crate::units::Length;

/// What a dimension measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DimensionKind {
    /// The true distance between two points.
    #[default]
    Aligned,
    /// The distance along an axis (`direction`, else the model axis the two
    /// points spread most along, across the offset).
    Linear,
    /// The radius of an arc or circle.
    Radius,
    /// The diameter of an arc or circle.
    Diameter,
}

/// `dimension` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DimensionParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// What to measure.
    #[serde(default)]
    pub kind: DimensionKind,
    /// First point (aligned, linear).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub start: Option<DVec3>,
    /// Second point (aligned, linear).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub end: Option<DVec3>,
    /// An edge of the arc or circle (radius, diameter).
    #[serde(default)]
    pub curve: Option<EntityRef>,
    /// From the measured points to the dimension line (aligned, linear),
    /// or the leader from the curve (radius, diameter).
    #[serde(default)]
    #[schemars(with = "VectorSchema")]
    pub offset: DVec3,
    /// Measuring direction of a linear dimension.
    #[serde(default)]
    #[schemars(with = "Option<VectorSchema>")]
    pub direction: Option<DVec3>,
    /// Replaces the text; `<>` stands for the measurement.
    #[serde(default)]
    pub text_override: Option<String>,
    /// Text height (100 mm).
    #[serde(default)]
    pub text_height: Option<Length>,
}

/// `dimension` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DimensionOutput {
    /// The dimension.
    pub dimension: Pid,
    /// The measurement.
    pub value: Length,
    /// Its text.
    pub text: String,
}

/// The `dimension` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct DimensionCommand;

/// The model axis `d` spreads most along, among those across `offset`.
fn linear_axis(d: DVec3, offset: DVec3) -> DVec3 {
    let across = offset.try_normalize();
    [DVec3::X, DVec3::Y, DVec3::Z]
        .into_iter()
        .filter(|a| across.is_none_or(|o| o.dot(*a).abs() < 0.9))
        .max_by(|a, b| d.dot(*a).abs().total_cmp(&d.dot(*b).abs()))
        .unwrap_or(DVec3::X)
}

impl Command for DimensionCommand {
    type Params = DimensionParams;
    type Output = DimensionOutput;

    fn name(&self) -> &'static str {
        "dimension"
    }

    fn validate(&self, _doc: &Document, p: &DimensionParams) -> Result<(), CommandError> {
        match p.kind {
            DimensionKind::Aligned | DimensionKind::Linear => {
                let (Some(a), Some(b)) = (p.start, p.end) else {
                    return Err(CommandError::invalid_param(
                        "start",
                        "aligned and linear dimensions need `start` and `end`",
                    ));
                };
                check_point("start", a)?;
                check_point("end", b)?;
                if (a - b).length() <= 1e-9 {
                    return Err(CommandError::invalid_param(
                        "end",
                        "must differ from `start`",
                    ));
                }
            }
            DimensionKind::Radius | DimensionKind::Diameter => {
                if p.curve.is_none() {
                    return Err(CommandError::invalid_param(
                        "curve",
                        "radius and diameter dimensions need `curve`",
                    ));
                }
            }
        }
        if !p.offset.is_finite() {
            return Err(CommandError::invalid_param("offset", "must be finite"));
        }
        if let Some(d) = p.direction
            && (!d.is_finite() || d.try_normalize().is_none())
        {
            return Err(CommandError::invalid_param(
                "direction",
                "must be a non-zero vector",
            ));
        }
        if let Some(h) = p.text_height
            && !(h.as_mm() > 0.0 && h.as_mm().is_finite())
        {
            return Err(CommandError::OutOfRange {
                name: "text_height".into(),
                reason: "must be a positive length".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: DimensionParams,
    ) -> Result<DimensionOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let style = DimStyle {
            font: None,
            text_height: p.text_height.unwrap_or(DimStyle::default().text_height),
            ..DimStyle::default()
        };
        let offset = target.local.matrix3 * p.offset;
        let dim = match p.kind {
            DimensionKind::Aligned | DimensionKind::Linear => {
                let (a, b) = (
                    target.to_local(p.start.unwrap_or_default()),
                    target.to_local(p.end.unwrap_or_default()),
                );
                let direction = match p.kind {
                    DimensionKind::Linear => Some(
                        p.direction
                            .map(|d| (target.local.matrix3 * d).normalize())
                            .unwrap_or_else(|| linear_axis(b - a, offset)),
                    ),
                    _ => None,
                };
                Dimension::Linear {
                    start: find_anchor(tx.document(), target.container, a),
                    end: find_anchor(tx.document(), target.container, b),
                    offset,
                    direction,
                    style,
                    text_override: p.text_override.clone(),
                }
            }
            DimensionKind::Radius | DimensionKind::Diameter => {
                let curve = p.curve.clone().unwrap_or(EntityRef::Pid(Pid::NONE));
                let context = command_context(tx, &p.common);
                // A query may name the whole curve: its first edge will do.
                let (container, e, world) = match &curve {
                    EntityRef::Query(q) => {
                        let sel = select(tx, &context, "curve", q)?;
                        let doc = tx.document();
                        sel.iter()
                            .filter_map(|path| doc.resolve(path).ok())
                            .find_map(|r| match r.entity {
                                EntityId::Edge(e)
                                    if doc
                                        .container(r.container)
                                        .and_then(|c| c.mesh().edge(e))
                                        .is_some_and(|x| x.curve.is_some()) =>
                                {
                                    Some((r.container, e, r.world))
                                }
                                _ => None,
                            })
                            .ok_or_else(|| {
                                CommandError::invalid_param(
                                    "curve",
                                    "selects no edge of an arc or circle",
                                )
                            })?
                    }
                    other => {
                        let r = resolve_ref(tx, &context, "curve", other)?;
                        let EntityId::Edge(e) = r.id else {
                            return Err(CommandError::invalid_param("curve", "must be an edge"));
                        };
                        (r.container, e, r.world)
                    }
                };
                let mesh = tx
                    .document()
                    .container(container)
                    .map(|c| c.mesh())
                    .ok_or_else(|| CommandError::Internal("lost container".into()))?;
                let has_curve = mesh.edge(e).and_then(|x| x.curve).is_some();
                let seg = mesh
                    .edge_segment(e)
                    .map_err(|err| CommandError::Internal(err.to_string()))?;
                if !has_curve {
                    return Err(CommandError::invalid_param(
                        "curve",
                        "must be an edge of an arc or circle",
                    ));
                }
                let mid = target.to_local(world.transform_point3((seg.a + seg.b) / 2.0));
                Dimension::Radial {
                    curve: find_anchor(tx.document(), target.container, mid),
                    kind: if p.kind == DimensionKind::Radius {
                        RadialKind::Radius
                    } else {
                        RadialKind::Diameter
                    },
                    leader: offset,
                    style,
                    text_override: p.text_override.clone(),
                }
            }
        };
        let view = dimension_view(tx.document(), target.container, &dim);
        if view.detached {
            return Err(CommandError::invalid_param(
                "curve",
                "the edge is not part of an arc or circle in this context",
            ));
        }
        let (_, pid) = tx.insert_entity(target.container, EntityRecord::Dimension(dim))?;
        Ok(DimensionOutput {
            dimension: pid,
            value: Length::mm(view.value),
            text: view.text,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "dimension".into(),
            category: CommandCategory::Annotate,
            label: "Dimension".into(),
            view_only: false,
            steps: crate::commands::common::tool_steps(&[
                ("Pick the first point, or an arc or circle", ""),
                ("Pick the second point (radius: place the leader)", ""),
                ("Place the dimension line", ""),
            ]),
        }
    }
}
