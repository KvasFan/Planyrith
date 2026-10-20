//! Implements docs/05-modeling-tools.md §3.1 `line`.

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::draw::draw_chain;
use crate::commands::params::{PointSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, ToolStep, Transaction,
};
use crate::document::Document;
use crate::ids::Pid;
use crate::inference::InferenceKind;
use crate::math::Axis;

/// `line` parameters (docs/05 §3.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LineParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Vertices of the polyline (≥ 2), world coordinates in mm.
    #[schemars(with = "Vec<PointSchema>")]
    pub points: Vec<DVec3>,
    /// Close the loop back to the first point.
    #[serde(default)]
    pub close: bool,
    /// Interactive axis lock that produced the points (informational; the
    /// points are taken as given).
    #[serde(default)]
    pub axis_lock: Option<Axis>,
}

/// `line` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LineOutput {
    /// Edges covering the drawn segments.
    pub edges: Vec<Pid>,
    /// Faces created.
    pub faces_created: Vec<Pid>,
    /// New pieces of faces the line split.
    pub faces_split: Vec<Pid>,
}

/// The `line` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Line;

impl Command for Line {
    type Params = LineParams;
    type Output = LineOutput;

    fn name(&self) -> &'static str {
        "line"
    }

    fn validate(&self, _doc: &Document, p: &LineParams) -> Result<(), CommandError> {
        if p.points.len() < 2 {
            return Err(CommandError::invalid_param("points", "at least two points"));
        }
        for &q in &p.points {
            check_point("points", q)?;
        }
        Ok(())
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: LineParams) -> Result<LineOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let closed = p.close
            || (p.points.len() > 2
                && p.points
                    .first()
                    .zip(p.points.last())
                    .is_some_and(|(a, b)| tx.tolerances().same_point(*a, *b)));
        let mut points = p.points;
        if closed
            && points.len() > 2
            && tx
                .tolerances()
                .same_point(points[0], points[points.len() - 1])
        {
            points.pop();
        }
        let drawn = draw_chain(tx, &target, &points, closed, None)?;
        Ok(LineOutput {
            edges: drawn.edges,
            faces_created: drawn.faces_created,
            faces_split: drawn.faces_split,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "line".into(),
            category: CommandCategory::Draw,
            label: "Line".into(),
            view_only: false,
            steps: vec![
                ToolStep {
                    prompt: "Pick the start point".into(),
                    value_field: String::new(),
                    inference: InferenceKind::ALL.to_vec(),
                },
                ToolStep {
                    prompt: "Pick the next point (Esc to finish)".into(),
                    value_field: "length, [x,y,z] absolute or <x,y,z> relative".into(),
                    inference: InferenceKind::ALL.to_vec(),
                },
            ],
        }
    }
}
