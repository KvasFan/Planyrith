//! Implements docs/05 §3.4: oriented rectangles and angled second sides.
use super::{RectangleOutput, draw_chain, face_of_chain};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, ToolStep, Transaction,
    params::{PointSchema, VectorSchema, check_point, target_container},
};
use crate::{
    document::Document,
    inference::InferenceKind,
    tolerance::Tolerances,
    units::{Angle, Length},
};
use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
fn right_angle() -> Angle {
    Angle::degrees(90.0)
}
/// Oriented rectangle parameters (world coordinates, mm).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RotatedRectangleParams {
    /// Common command context.
    #[serde(flatten)]
    pub common: CommonParams,
    /// First corner, world mm.
    #[schemars(with = "PointSchema")]
    pub origin: DVec3,
    /// End of the first edge, world mm.
    #[schemars(with = "PointSchema")]
    pub edge_end: DVec3,
    /// Length of the second side, mm (positive).
    pub width: Length,
    /// Signed angle from the first edge to the second side (90 degrees).
    #[serde(default = "right_angle")]
    pub angle: Angle,
    /// Drawing-plane normal; absent: Z, or X for a vertical first edge.
    #[serde(default)]
    #[schemars(with = "Option<VectorSchema>")]
    pub normal: Option<DVec3>,
}
/// Rotated Rectangle command.
pub struct RotatedRectangle;
fn corners(p: &RotatedRectangleParams, tol: &Tolerances) -> Result<[DVec3; 4], CommandError> {
    let edge = p.edge_end - p.origin;
    if edge.length() <= tol.length {
        return Err(CommandError::invalid_param(
            "edge_end",
            "first edge is too short",
        ));
    }
    let u = edge.normalize();
    let normal = p.normal.unwrap_or_else(|| {
        if u.cross(DVec3::Z).length() <= tol.angle {
            DVec3::X
        } else {
            DVec3::Z
        }
    });
    if !normal.is_finite() || normal.length() <= tol.angle {
        return Err(CommandError::invalid_param(
            "normal",
            "give a finite, nonzero vector",
        ));
    }
    let n = normal.normalize();
    if n.dot(u).abs() > tol.angle {
        return Err(CommandError::invalid_param(
            "normal",
            "must be perpendicular to the first edge",
        ));
    }
    let w = p.width.as_mm();
    let angle = p.angle.as_radians();
    if !w.is_finite() || w <= tol.length {
        return Err(CommandError::invalid_param(
            "width",
            "must be positive and longer than the geometric tolerance",
        ));
    }
    if !angle.is_finite() || libm::sin(angle).abs() <= tol.angle {
        return Err(CommandError::invalid_param(
            "angle",
            "second side must not be collinear with the first",
        ));
    }
    let side = (u * libm::cos(angle) + n.cross(u) * libm::sin(angle)) * w;
    let points = [p.origin, p.edge_end, p.edge_end + side, p.origin + side];
    for p in &points {
        check_point("corners", *p)?;
    }
    Ok(points)
}
impl Command for RotatedRectangle {
    type Params = RotatedRectangleParams;
    type Output = RectangleOutput;
    fn name(&self) -> &'static str {
        "rotated-rectangle"
    }
    fn validate(&self, doc: &Document, p: &RotatedRectangleParams) -> Result<(), CommandError> {
        check_point("origin", p.origin)?;
        check_point("edge_end", p.edge_end)?;
        corners(p, doc.tolerances()).map(|_| ())
    }
    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: RotatedRectangleParams,
    ) -> Result<RectangleOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let points = corners(&p, tx.tolerances())?;
        let drawn = draw_chain(tx, &target, &points, true, None)?;
        Ok(RectangleOutput {
            face: face_of_chain(tx, &target, &drawn)?,
            edges: drawn.edges,
            faces_split: drawn.faces_split,
        })
    }
    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "rotated-rectangle".into(),
            category: CommandCategory::Draw,
            label: "Rotated Rectangle".into(),
            view_only: false,
            steps: vec![
                ToolStep {
                    prompt: "Pick the first corner".into(),
                    value_field: String::new(),
                    inference: InferenceKind::ALL.to_vec(),
                },
                ToolStep {
                    prompt: "Pick the first edge end".into(),
                    value_field: "first edge length, mm".into(),
                    inference: InferenceKind::ALL.to_vec(),
                },
                ToolStep {
                    prompt: "Pick the width and angle".into(),
                    value_field: "width, angle (degrees)".into(),
                    inference: InferenceKind::ALL.to_vec(),
                },
            ],
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn corners_keep_both_side_lengths(a in 1.0f64..10000.0,b in 1.0f64..10000.0,angle in 1.0f64..179.0) {
            let p=RotatedRectangleParams {common:Default::default(),origin:DVec3::ZERO,edge_end:DVec3::new(a,a,0.0),width:Length::mm(b),angle:Angle::degrees(angle),normal:None};
            let points=corners(&p,&Tolerances::default()).unwrap();
            prop_assert!((points[1].distance(points[0])-a*libm::sqrt(2.0)).abs()<1e-8);
            prop_assert!((points[2].distance(points[1])-b).abs()<1e-8);
            prop_assert!((points[3]-points[2]-points[0]+points[1]).length()<1e-8);
        }
    }
}
