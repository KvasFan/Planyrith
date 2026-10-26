//! Implements docs/05-modeling-tools.md §6 (category `guide`):
//! `tape-measure` (measures, and leaves a guide line parallel to the edge it
//! starts on or a guide point when it starts on a vertex), `protractor`
//! (measures an angle and leaves an angular guide line), `guide-delete-all`
//! and `resize-model` (scales the context so a measurement becomes a typed
//! length).

use glam::{DQuat, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::edit::transform::transform_entities;
use crate::commands::params::{PointSchema, VectorSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::annotations::{edge_direction_at, find_anchor};
use crate::document::{AnchorKind, Document, EntityRecord, Guide};
use crate::geom::TransformOptions;
use crate::ids::{EntityId, Pid};
use crate::math::transform::scale_about;
use crate::units::{Angle, Length};

fn yes() -> bool {
    true
}

fn z_axis() -> DVec3 {
    DVec3::Z
}

fn meta(name: &str, label: &str) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        category: CommandCategory::Guide,
        label: label.into(),
        view_only: false,
        steps: Vec::new(),
    }
}

/// The kind of guide `tape-measure` leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum GuideKind {
    /// An infinite guide line through `to`, parallel to the measured edge
    /// (or along `from`→`to`).
    Line,
    /// A guide point at `to`.
    Point,
}

/// `tape-measure` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TapeMeasureParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Where the measurement starts (world).
    #[schemars(with = "PointSchema")]
    pub from: DVec3,
    /// Where it ends (world).
    #[schemars(with = "PointSchema")]
    pub to: DVec3,
    /// Leave a guide (true).
    #[serde(default = "yes")]
    pub create_guide: bool,
    /// Which guide (default: a line from an edge, a point from a vertex,
    /// none from empty space).
    #[serde(default)]
    pub guide: Option<GuideKind>,
}

/// `tape-measure` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TapeMeasureOutput {
    /// The distance measured.
    pub distance: Length,
    /// The guide left.
    pub guide: Option<Pid>,
}

/// The `tape-measure` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct TapeMeasure;

impl Command for TapeMeasure {
    type Params = TapeMeasureParams;
    type Output = TapeMeasureOutput;

    fn name(&self) -> &'static str {
        "tape-measure"
    }

    fn validate(&self, _doc: &Document, p: &TapeMeasureParams) -> Result<(), CommandError> {
        check_point("from", p.from)?;
        check_point("to", p.to)?;
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: TapeMeasureParams,
    ) -> Result<TapeMeasureOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let (from, to) = (target.to_local(p.from), target.to_local(p.to));
        let distance = Length::mm((p.to - p.from).length());
        if !p.create_guide {
            return Ok(TapeMeasureOutput {
                distance,
                guide: None,
            });
        }
        let doc = tx.document();
        let anchor = find_anchor(doc, target.container, from);
        let on_edge = edge_direction_at(doc, target.container, from);
        let kind = p.guide.or(match anchor.kind {
            AnchorKind::Start | AnchorKind::End => Some(GuideKind::Point),
            _ if on_edge.is_some() => Some(GuideKind::Line),
            _ => None,
        });
        let guide = match kind {
            None => None,
            Some(GuideKind::Point) => Some(Guide::Point { pos: to }),
            Some(GuideKind::Line) => {
                let dir = on_edge
                    .or_else(|| (to - from).try_normalize())
                    .ok_or_else(|| {
                        CommandError::invalid_param("to", "a guide line needs a direction")
                    })?;
                Some(Guide::Line {
                    origin: to,
                    dir,
                    finite: None,
                })
            }
        };
        let guide = match guide {
            Some(g) => Some(
                tx.insert_entity(target.container, EntityRecord::Guide(g))?
                    .1,
            ),
            None => None,
        };
        Ok(TapeMeasureOutput { distance, guide })
    }

    fn meta(&self) -> CommandMeta {
        meta("tape-measure", "Tape Measure")
    }
}

/// `protractor` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProtractorParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Vertex of the angle (world).
    #[schemars(with = "PointSchema")]
    pub center: DVec3,
    /// Axis the angle turns about (+Z).
    #[serde(default = "z_axis")]
    #[schemars(with = "VectorSchema")]
    pub axis: DVec3,
    /// A point on the first leg.
    #[schemars(with = "PointSchema")]
    pub from_point: DVec3,
    /// The angle to the second leg (counter-clockwise about `axis`).
    #[serde(default)]
    pub angle: Option<Angle>,
    /// A point on the second leg (instead of `angle`).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub to_point: Option<DVec3>,
    /// Leave a guide line along the second leg (true).
    #[serde(default = "yes")]
    pub create_guide: bool,
}

/// `protractor` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProtractorOutput {
    /// The angle measured or used.
    pub angle: Angle,
    /// The guide left.
    pub guide: Option<Pid>,
}

/// The `protractor` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Protractor;

impl Command for Protractor {
    type Params = ProtractorParams;
    type Output = ProtractorOutput;

    fn name(&self) -> &'static str {
        "protractor"
    }

    fn validate(&self, _doc: &Document, p: &ProtractorParams) -> Result<(), CommandError> {
        check_point("center", p.center)?;
        check_point("from_point", p.from_point)?;
        if p.axis.try_normalize().is_none() || !p.axis.is_finite() {
            return Err(CommandError::invalid_param(
                "axis",
                "must be a non-zero vector",
            ));
        }
        let leg = p.from_point - p.center;
        if (leg - p.axis.normalize() * leg.dot(p.axis.normalize())).length() <= 1e-9 {
            return Err(CommandError::invalid_param(
                "from_point",
                "must not lie on the axis through `center`",
            ));
        }
        match (p.angle, p.to_point) {
            (Some(a), None) if a.0.is_finite() => Ok(()),
            (None, Some(t)) => check_point("to_point", t),
            _ => Err(CommandError::invalid_param(
                "angle",
                "give either `angle` or `to_point`",
            )),
        }
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ProtractorParams,
    ) -> Result<ProtractorOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let axis = p.axis.normalize();
        let flat = |v: DVec3| v - axis * v.dot(axis);
        let first = flat(p.from_point - p.center).normalize();
        let angle = match (p.angle, p.to_point) {
            (Some(a), _) => a,
            (None, Some(t)) => {
                let second = flat(t - p.center).try_normalize().ok_or_else(|| {
                    CommandError::invalid_param("to_point", "must not lie on the axis")
                })?;
                Angle::radians(libm::atan2(
                    first.cross(second).dot(axis),
                    first.dot(second),
                ))
            }
            _ => unreachable_angle()?,
        };
        let dir = DQuat::from_axis_angle(axis, angle.0) * first;
        let guide = if p.create_guide {
            let g = Guide::Line {
                origin: target.to_local(p.center),
                dir: (target.local.matrix3 * dir).normalize(),
                finite: None,
            };
            Some(
                tx.insert_entity(target.container, EntityRecord::Guide(g))?
                    .1,
            )
        } else {
            None
        };
        Ok(ProtractorOutput { angle, guide })
    }

    fn meta(&self) -> CommandMeta {
        meta("protractor", "Protractor")
    }
}

fn unreachable_angle() -> Result<Angle, CommandError> {
    Err(CommandError::invalid_param(
        "angle",
        "give either `angle` or `to_point`",
    ))
}

/// `guide-delete-all` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GuideDeleteAllParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
}

/// `guide-delete-all` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GuideDeleteAllOutput {
    /// Guides deleted.
    pub deleted: usize,
}

/// The `guide-delete-all` command: every guide of the edit context.
#[derive(Debug, Default, Clone, Copy)]
pub struct GuideDeleteAll;

impl Command for GuideDeleteAll {
    type Params = GuideDeleteAllParams;
    type Output = GuideDeleteAllOutput;

    fn name(&self) -> &'static str {
        "guide-delete-all"
    }

    fn validate(&self, _doc: &Document, _p: &GuideDeleteAllParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: GuideDeleteAllParams,
    ) -> Result<GuideDeleteAllOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let guides: Vec<EntityId> = tx
            .document()
            .container(target.container)
            .map(|c| {
                c.order()
                    .iter()
                    .copied()
                    .filter(|id| matches!(id, EntityId::Guide(_)))
                    .collect()
            })
            .unwrap_or_default();
        for &id in &guides {
            tx.remove_entity(target.container, id)?;
        }
        Ok(GuideDeleteAllOutput {
            deleted: guides.len(),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("guide-delete-all", "Delete Guides")
    }
}

/// `resize-model` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResizeModelParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The length measured now.
    pub measured: Length,
    /// The length it should be.
    pub target: Length,
}

/// `resize-model` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResizeModelOutput {
    /// The uniform scale applied.
    pub factor: f64,
    /// Entities scaled.
    pub scaled: usize,
}

/// The `resize-model` command: scales everything in the edit context
/// uniformly about the context's origin (docs/05 §6.4).
#[derive(Debug, Default, Clone, Copy)]
pub struct ResizeModel;

impl Command for ResizeModel {
    type Params = ResizeModelParams;
    type Output = ResizeModelOutput;

    fn name(&self) -> &'static str {
        "resize-model"
    }

    fn validate(&self, _doc: &Document, p: &ResizeModelParams) -> Result<(), CommandError> {
        for (name, l) in [("measured", p.measured), ("target", p.target)] {
            if !(l.as_mm() > 0.0 && l.as_mm().is_finite()) {
                return Err(CommandError::OutOfRange {
                    name: name.into(),
                    reason: "must be a positive length".into(),
                });
            }
        }
        let factor = p.target.as_mm() / p.measured.as_mm();
        if !(1e-6..=1e6).contains(&factor) {
            return Err(CommandError::OutOfRange {
                name: "target".into(),
                reason: "the scale must stay between 1e-6 and 1e6".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ResizeModelParams,
    ) -> Result<ResizeModelOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let factor = p.target.as_mm() / p.measured.as_mm();
        let ids: Vec<EntityId> = tx
            .document()
            .container(target.container)
            .map(|c| c.order().to_vec())
            .unwrap_or_default();
        let origin = target.world.transform_point3(DVec3::ZERO);
        transform_entities(
            tx,
            &target,
            &ids,
            scale_about(origin, DVec3::splat(factor)),
            TransformOptions {
                auto_fold: false,
                check_intersections: false,
            },
        )?;
        Ok(ResizeModelOutput {
            factor,
            scaled: ids.len(),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("resize-model", "Resize Model")
    }
}
