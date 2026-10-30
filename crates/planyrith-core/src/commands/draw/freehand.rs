//! Implements docs/05 §3.2: deterministic stroke simplification and drawing.
use super::draw_chain;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, ToolStep, Transaction,
    params::{PointSchema, check_point, target_container},
};
use crate::{
    document::Document, geom::CurveDescriptor, ids::Pid, inference::InferenceKind, units::Length,
};
use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn tolerance() -> Length {
    Length::mm(0.5)
}
/// Parameters of a freehand stroke, in world millimeters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FreehandParams {
    /// Common command context.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Ordered samples (2–65535), world coordinates in mm.
    #[schemars(with = "Vec<PointSchema>")]
    pub points: Vec<DVec3>,
    /// Maximum simplification deviation, mm; zero keeps distinct samples.
    #[serde(default = "tolerance")]
    pub simplify_tolerance: Length,
}
/// The drawn stroke and faces it formed.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FreehandOutput {
    /// Member edges, in stroke order.
    pub edges: Vec<Pid>,
    /// A member edge identifying the curve.
    pub curve: Option<Pid>,
    /// Faces created by a closed planar stroke.
    pub faces_created: Vec<Pid>,
}
/// Freehand stroke command.
pub struct Freehand;

/// Iterative Douglas–Peucker; endpoint order and tie handling are stable.
fn simplify(points: &[DVec3], tolerance: f64) -> Vec<DVec3> {
    if points.len() < 3 || tolerance <= 0.0 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((first, last)) = stack.pop() {
        let a = points[first];
        let d = points[last] - a;
        let length = d.length_squared();
        let mut best = None;
        let mut maximum = tolerance * tolerance;
        for (i, p) in points.iter().enumerate().take(last).skip(first + 1) {
            let t = if length > f64::MIN_POSITIVE {
                ((*p - a).dot(d) / length).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let distance = p.distance_squared(a + d * t);
            if distance > maximum {
                maximum = distance;
                best = Some(i);
            }
        }
        if let Some(i) = best {
            keep[i] = true;
            stack.push((i, last));
            stack.push((first, i));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, keep)| keep.then_some(*p))
        .collect()
}
impl Command for Freehand {
    type Params = FreehandParams;
    type Output = FreehandOutput;
    fn name(&self) -> &'static str {
        "freehand"
    }
    fn validate(&self, _doc: &Document, p: &FreehandParams) -> Result<(), CommandError> {
        if !(2..=65535).contains(&p.points.len()) {
            return Err(CommandError::invalid_param(
                "points",
                "give 2–65535 samples",
            ));
        }
        for p in &p.points {
            check_point("points", *p)?;
        }
        let t = p.simplify_tolerance.as_mm();
        if !t.is_finite() || t < 0.0 {
            return Err(CommandError::invalid_param(
                "simplify_tolerance",
                "must be finite and nonnegative",
            ));
        }
        Ok(())
    }
    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: FreehandParams,
    ) -> Result<FreehandOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let mut distinct = Vec::new();
        for p in p.points {
            if distinct
                .last()
                .is_none_or(|last| !tx.tolerances().same_point(*last, p))
            {
                distinct.push(p);
            }
        }
        let mut points = simplify(&distinct, p.simplify_tolerance.as_mm());
        if points.len() < 2 {
            return Err(CommandError::invalid_param(
                "points",
                "stroke has no distinct segment",
            ));
        }
        let closed = tx
            .tolerances()
            .same_point(points[0], points[points.len() - 1]);
        if closed {
            points.pop();
            if points.len() < 3 {
                return Err(CommandError::invalid_param(
                    "simplify_tolerance",
                    "closed stroke collapses at this tolerance",
                ));
            }
        }
        if points
            .windows(2)
            .any(|w| tx.tolerances().same_point(w[0], w[1]))
        {
            return Err(CommandError::invalid_param(
                "points",
                "stroke contains a collapsed segment",
            ));
        }
        let mut local = points
            .iter()
            .map(|p| target.to_local(*p))
            .collect::<Vec<_>>();
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
        Ok(FreehandOutput {
            curve: drawn.edges.first().copied(),
            edges: drawn.edges,
            faces_created: drawn.faces_created,
        })
    }
    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "freehand".into(),
            category: CommandCategory::Draw,
            label: "Freehand".into(),
            view_only: false,
            steps: vec![ToolStep {
                prompt: "Drag a stroke on a face or the ground, then release".into(),
                value_field: "simplification tolerance, mm".into(),
                inference: vec![InferenceKind::OnFace],
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn simplification_preserves_corners_and_endpoints() {
        let points = [
            DVec3::ZERO,
            DVec3::new(5.0, 0.01, 0.0),
            DVec3::X * 10.0,
            DVec3::new(10.0, 10.0, 0.0),
        ];
        assert_eq!(
            simplify(&points, 0.1),
            vec![points[0], points[2], points[3]]
        );
        assert_eq!(simplify(&points, 0.0), points);
    }
    proptest! {
        #[test]
        fn simplified_stroke_stays_within_tolerance(samples in prop::collection::vec(-100.0f64..100.0,2..100),tol in 0.001f64..10.0) {
            let points=samples.iter().enumerate().map(|(i,y)|DVec3::new(i as f64,*y,0.0)).collect::<Vec<_>>();
            let simplified=simplify(&points,tol);
            for p in points {let within=simplified.windows(2).any(|w| {let d=w[1]-w[0];let t=((p-w[0]).dot(d)/d.length_squared()).clamp(0.0,1.0);p.distance(w[0]+d*t)<=tol+1e-9});prop_assert!(within);}
        }
    }
}
