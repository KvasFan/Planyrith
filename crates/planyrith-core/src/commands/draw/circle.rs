//! Implements docs/05-modeling-tools.md §3.5 `circle` and §3.6 `polygon`.

use glam::{DVec2, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::draw::{check_count, draw_chain, face_of_chain};
use crate::commands::params::{PointSchema, Target, VectorSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, ToolStep, Transaction,
};
use crate::document::Document;
use crate::geom::CurveDescriptor;
use crate::ids::Pid;
use crate::inference::InferenceKind;
use crate::math::Plane;
use crate::units::Length;

fn default_normal() -> DVec3 {
    DVec3::Z
}

fn default_segments() -> u32 {
    24
}

fn default_sides() -> u32 {
    6
}

fn default_true() -> bool {
    true
}

/// `circle` parameters (docs/05 §3.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CircleParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Center (world, mm).
    #[schemars(with = "PointSchema")]
    pub center: DVec3,
    /// Radius.
    pub radius: Length,
    /// Plane normal (+Z by default).
    #[serde(default = "default_normal")]
    #[schemars(with = "VectorSchema")]
    pub normal: DVec3,
    /// Number of segments, 3–999 (24).
    #[serde(default = "default_segments")]
    pub segments: u32,
}

/// `polygon` parameters (docs/05 §3.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PolygonParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Center (world, mm).
    #[schemars(with = "PointSchema")]
    pub center: DVec3,
    /// Radius: of the vertex circle when `inscribed`, else the apothem.
    pub radius: Length,
    /// Plane normal (+Z by default).
    #[serde(default = "default_normal")]
    #[schemars(with = "VectorSchema")]
    pub normal: DVec3,
    /// Number of sides, 3–999 (6).
    #[serde(default = "default_sides")]
    pub sides: u32,
    /// Vertices on the radius circle (true) or sides tangent to it (false).
    #[serde(default = "default_true")]
    pub inscribed: bool,
}

/// Output of curve-based shapes (`circle`, `polygon`, arcs, `pie`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ShapeOutput {
    /// Edges of the shape.
    pub edges: Vec<Pid>,
    /// The shape's own face, when one formed.
    pub face: Option<Pid>,
    /// Pieces of existing faces split by the shape.
    pub faces_split: Vec<Pid>,
    /// Number of segments of the curve.
    pub segments: u32,
}

/// The `circle` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Circle;

/// The `polygon` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Polygon;

/// Points of a regular n-gon of vertex radius `r` around `center`.
pub(crate) fn ring(center: DVec3, normal: DVec3, r: f64, n: u32) -> Vec<DVec3> {
    let frame = Plane::from_point_normal(center, normal).frame_at(center);
    let c2 = frame.to_2d(center);
    (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * f64::from(i) / f64::from(n);
            frame.from_2d(c2 + DVec2::new(r * a.cos(), r * a.sin()))
        })
        .collect()
}

fn check_disc(
    doc: &Document,
    center: DVec3,
    radius: f64,
    normal: DVec3,
) -> Result<(), CommandError> {
    check_point("center", center)?;
    if !(radius.is_finite() && radius > doc.tolerances().length) {
        return Err(CommandError::OutOfRange {
            name: "radius".into(),
            reason: "must be positive".into(),
        });
    }
    if normal.length_squared() <= 0.0 || !normal.is_finite() {
        return Err(CommandError::invalid_param(
            "normal",
            "must be a non-zero vector",
        ));
    }
    Ok(())
}

/// Builds a closed curve shape: points, descriptor in container coordinates.
fn closed_shape(
    tx: &mut Transaction<'_>,
    target: &Target,
    points: Vec<DVec3>,
    descriptor: CurveDescriptor,
    segments: u32,
) -> Result<ShapeOutput, CommandError> {
    let drawn = draw_chain(tx, target, &points, true, Some(descriptor))?;
    let face = face_of_chain(tx, target, &drawn)?;
    Ok(ShapeOutput {
        edges: drawn.edges,
        face,
        faces_split: drawn.faces_split,
        segments,
    })
}

/// Center, unit normal and radius in container coordinates.
pub(crate) fn local_disc(
    target: &Target,
    center: DVec3,
    normal: DVec3,
    on_circle: DVec3,
) -> (DVec3, DVec3, f64) {
    let c = target.to_local(center);
    let n = crate::math::transform::transform_normal(&target.local, normal.normalize());
    (c, n, c.distance(target.to_local(on_circle)))
}

impl Command for Circle {
    type Params = CircleParams;
    type Output = ShapeOutput;

    fn name(&self) -> &'static str {
        "circle"
    }

    fn validate(&self, doc: &Document, p: &CircleParams) -> Result<(), CommandError> {
        check_disc(doc, p.center, p.radius.as_mm(), p.normal)?;
        check_count("segments", p.segments, 3, 999)
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: CircleParams,
    ) -> Result<ShapeOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let points = ring(p.center, p.normal.normalize(), p.radius.as_mm(), p.segments);
        let (center, normal, radius) = local_disc(&target, p.center, p.normal, points[0]);
        let descriptor = CurveDescriptor::Circle {
            center,
            normal,
            radius,
            segments: p.segments,
        };
        closed_shape(tx, &target, points, descriptor, p.segments)
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "circle".into(),
            category: CommandCategory::Draw,
            label: "Circle".into(),
            view_only: false,
            steps: vec![
                ToolStep {
                    prompt: "Pick the center".into(),
                    value_field: "Ns sets the segment count".into(),
                    inference: InferenceKind::ALL.to_vec(),
                },
                ToolStep {
                    prompt: "Pick the radius".into(),
                    value_field: "radius, or Ns for segments".into(),
                    inference: vec![
                        InferenceKind::Endpoint,
                        InferenceKind::OnEdge,
                        InferenceKind::OnAxis,
                    ],
                },
            ],
        }
    }
}

impl Command for Polygon {
    type Params = PolygonParams;
    type Output = ShapeOutput;

    fn name(&self) -> &'static str {
        "polygon"
    }

    fn validate(&self, doc: &Document, p: &PolygonParams) -> Result<(), CommandError> {
        check_disc(doc, p.center, p.radius.as_mm(), p.normal)?;
        check_count("sides", p.sides, 3, 999)
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: PolygonParams,
    ) -> Result<ShapeOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let n = p.sides;
        let vertex_radius = if p.inscribed {
            p.radius.as_mm()
        } else {
            p.radius.as_mm() / (std::f64::consts::PI / f64::from(n)).cos()
        };
        let points = ring(p.center, p.normal.normalize(), vertex_radius, n);
        let (center, normal, local_vertex_radius) =
            local_disc(&target, p.center, p.normal, points[0]);
        let radius = if p.inscribed {
            local_vertex_radius
        } else {
            local_vertex_radius * (std::f64::consts::PI / f64::from(n)).cos()
        };
        let descriptor = CurveDescriptor::Polygon {
            center,
            normal,
            radius,
            sides: n,
            inscribed: p.inscribed,
        };
        closed_shape(tx, &target, points, descriptor, n)
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "polygon".into(),
            category: CommandCategory::Draw,
            label: "Polygon".into(),
            view_only: false,
            steps: vec![
                ToolStep {
                    prompt: "Pick the center".into(),
                    value_field: "Ns sets the number of sides".into(),
                    inference: InferenceKind::ALL.to_vec(),
                },
                ToolStep {
                    prompt: "Pick the radius".into(),
                    value_field: "radius, or Ns for sides".into(),
                    inference: vec![
                        InferenceKind::Endpoint,
                        InferenceKind::OnEdge,
                        InferenceKind::OnAxis,
                    ],
                },
            ],
        }
    }
}
