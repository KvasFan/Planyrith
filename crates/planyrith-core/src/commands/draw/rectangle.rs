//! Implements docs/05-modeling-tools.md §3.3 `rectangle`.

use glam::{DVec2, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::draw::{draw_chain, face_of_chain};
use crate::commands::params::{PointSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, ToolStep, Transaction,
};
use crate::document::Document;
use crate::ids::Pid;
use crate::inference::InferenceKind;
use crate::math::Plane;
use crate::tolerance::Tolerances;

/// Aspect constraint of the second corner (docs/05 §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AspectLock {
    /// Free aspect.
    #[default]
    None,
    /// Equal sides (the longer side wins).
    Square,
    /// Height = width / φ.
    Golden,
}

/// `rectangle` parameters (docs/05 §3.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RectangleParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// First corner (world, mm).
    #[schemars(with = "PointSchema")]
    pub corner_a: DVec3,
    /// Opposite corner (projected onto the plane).
    #[schemars(with = "PointSchema")]
    pub corner_b: DVec3,
    /// Drawing plane; by default the axis-aligned plane through both corners
    /// (ground-parallel when they share Z, else a vertical plane).
    #[serde(default)]
    pub plane: Option<Plane>,
    /// Aspect constraint.
    #[serde(default)]
    pub aspect_lock: AspectLock,
}

/// `rectangle` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RectangleOutput {
    /// The four sides (more when they were split by existing geometry).
    pub edges: Vec<Pid>,
    /// The rectangle's face, when one formed.
    pub face: Option<Pid>,
    /// Pieces of existing faces split by the rectangle.
    pub faces_split: Vec<Pid>,
}

/// The `rectangle` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Rectangle;

/// The four corners of the rectangle, in order.
pub(crate) fn rectangle_corners(
    p: &RectangleParams,
    tol: &Tolerances,
) -> Result<[DVec3; 4], CommandError> {
    let (a, b) = (p.corner_a, p.corner_b);
    let plane = match p.plane {
        Some(plane) => {
            if (plane.normal.length() - 1.0).abs() > 1e-6 {
                return Err(CommandError::invalid_param(
                    "plane",
                    "the normal must be a unit vector",
                ));
            }
            plane
        }
        None => {
            let d = (b - a).abs();
            let normal = if d.z <= tol.length {
                DVec3::Z
            } else if d.x <= tol.length {
                DVec3::X
            } else if d.y <= tol.length {
                DVec3::Y
            } else {
                return Err(CommandError::invalid_param(
                    "plane",
                    "the corners are not on an axis-aligned plane; give --plane",
                ));
            };
            Plane::from_point_normal(a, normal)
        }
    };
    let frame = plane.frame_at(a);
    let b2 = frame.to_2d(b) - frame.to_2d(a);
    let sign = |x: f64| if x < 0.0 { -1.0 } else { 1.0 };
    let (w, h) = match p.aspect_lock {
        AspectLock::None => (b2.x, b2.y),
        AspectLock::Square => {
            let s = b2.x.abs().max(b2.y.abs());
            (s * sign(b2.x), s * sign(b2.y))
        }
        AspectLock::Golden => {
            let phi = (1.0 + 5f64.sqrt()) * 0.5;
            (b2.x, b2.x.abs() / phi * sign(b2.y))
        }
    };
    if w.abs() <= tol.length || h.abs() <= tol.length {
        return Err(CommandError::invalid_param(
            "corner_b",
            "the rectangle has zero width or height on its plane",
        ));
    }
    let a2 = frame.to_2d(a);
    let at = |x: f64, y: f64| frame.from_2d(a2 + DVec2::new(x, y));
    Ok([at(0.0, 0.0), at(w, 0.0), at(w, h), at(0.0, h)])
}

impl Command for Rectangle {
    type Params = RectangleParams;
    type Output = RectangleOutput;

    fn name(&self) -> &'static str {
        "rectangle"
    }

    fn validate(&self, doc: &Document, p: &RectangleParams) -> Result<(), CommandError> {
        check_point("corner_a", p.corner_a)?;
        check_point("corner_b", p.corner_b)?;
        rectangle_corners(p, doc.tolerances()).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: RectangleParams,
    ) -> Result<RectangleOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let corners = rectangle_corners(&p, tx.tolerances())?;
        let drawn = draw_chain(tx, &target, &corners, true, None)?;
        let face = face_of_chain(tx, &target, &drawn)?;
        Ok(RectangleOutput {
            edges: drawn.edges,
            face,
            faces_split: drawn.faces_split,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "rectangle".into(),
            category: CommandCategory::Draw,
            label: "Rectangle".into(),
            view_only: false,
            steps: vec![
                ToolStep {
                    prompt: "Pick the first corner".into(),
                    value_field: String::new(),
                    inference: InferenceKind::ALL.to_vec(),
                },
                ToolStep {
                    prompt: "Pick the opposite corner".into(),
                    value_field: "w,h (w alone keeps the other side)".into(),
                    inference: InferenceKind::ALL.to_vec(),
                },
            ],
        }
    }
}
