//! Implements docs/05-modeling-tools.md §3.9 `bezier`: a cubic Bézier
//! spline drawn as a curve of straight edges (docs/03 §4.18).

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::draw_chain;
use crate::commands::params::{PointSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, ToolStep, Transaction,
};
use crate::document::Document;
use crate::geom::CurveDescriptor;
use crate::geom::loft::bezier_points;
use crate::ids::Pid;
use crate::inference::InferenceKind;

fn twelve() -> u32 {
    12
}

/// `bezier` parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BezierParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Control points, `3n + 1` (start, two handles and an end per span;
    /// spans share ends), world millimeters. A last point on the first
    /// closes the curve.
    #[schemars(with = "Vec<PointSchema>")]
    pub points: Vec<DVec3>,
    /// Straight pieces per span, 1–256 (12).
    #[serde(default = "twelve")]
    pub segments: u32,
}

/// `bezier` output.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BezierOutput {
    /// Member edges, in curve order.
    pub edges: Vec<Pid>,
    /// A member edge identifying the curve.
    pub curve: Option<Pid>,
    /// Faces created by a closed flat curve.
    pub faces_created: Vec<Pid>,
}

/// The `bezier` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Bezier;

impl Command for Bezier {
    type Params = BezierParams;
    type Output = BezierOutput;

    fn name(&self) -> &'static str {
        "bezier"
    }

    fn validate(&self, _: &Document, p: &BezierParams) -> Result<(), CommandError> {
        for q in &p.points {
            check_point("points", *q)?;
        }
        if !(1..=256).contains(&p.segments) {
            return Err(CommandError::OutOfRange {
                name: "segments".into(),
                reason: "must be between 1 and 256".into(),
            });
        }
        bezier_points(&p.points, p.segments)
            .map(|_| ())
            .map_err(|e| CommandError::invalid_param("points", e.to_string()))
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: BezierParams,
    ) -> Result<BezierOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let mut points = bezier_points(&p.points, p.segments)
            .map_err(|e| CommandError::invalid_param("points", e.to_string()))?;
        // Samples closer than the tolerance merge (a span folding back).
        points.dedup_by(|b, a| tx.tolerances().same_point(*a, *b));
        let closed = points.len() > 3
            && tx
                .tolerances()
                .same_point(points[0], points[points.len() - 1]);
        if closed {
            points.pop();
        }
        if points.len() < 2 {
            return Err(CommandError::invalid_param(
                "points",
                "the curve has no length",
            ));
        }
        let mut local: Vec<DVec3> = points.iter().map(|q| target.to_local(*q)).collect();
        if closed {
            local.push(local[0]);
        }
        let drawn = draw_chain(
            tx,
            &target,
            &points,
            closed,
            Some(CurveDescriptor::Freehand { points: local }),
        )?;
        Ok(BezierOutput {
            curve: drawn.edges.first().copied(),
            edges: drawn.edges,
            faces_created: drawn.faces_created,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "bezier".into(),
            category: CommandCategory::Draw,
            label: "Bézier Curve".into(),
            view_only: false,
            steps: vec![ToolStep {
                prompt: "Click the start, two handles and the end of each span".into(),
                value_field: "segments per span".into(),
                inference: InferenceKind::ALL.to_vec(),
            }],
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn an_open_spline_and_a_closed_one_with_its_face() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        let out = e
            .execute_erased(
                "bezier",
                json!({ "points": [[0, 0, 0], [0, 100, 0], [100, 100, 0], [100, 0, 0]], "segments": 8 }),
            )
            .unwrap();
        assert_eq!(out["edges"].as_array().unwrap().len(), 8);
        assert_eq!(out["faces_created"], json!([]));
        let closed = e
            .execute_erased(
                "bezier",
                json!({ "points": [
                    [500, 0, 0], [600, 0, 0], [700, 100, 0], [700, 200, 0],
                    [700, 300, 0], [500, 300, 0], [500, 0, 0]
                ], "segments": 6 }),
            )
            .unwrap();
        assert_eq!(closed["edges"].as_array().unwrap().len(), 12);
        assert_eq!(closed["faces_created"].as_array().unwrap().len(), 1);
        assert!(
            e.execute_erased(
                "bezier",
                json!({ "points": [[0, 0, 0], [1, 1, 0], [2, 0, 0]] })
            )
            .is_err()
        );
    }
}
