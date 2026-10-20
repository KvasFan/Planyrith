//! Implements docs/05-modeling-tools.md §3.7: `arc-2point` (center, start,
//! end), `arc-3point`, `arc-bulge` (start, end, bulge) and `pie`.

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::draw::circle::ShapeOutput;
use crate::commands::draw::{Drawn, check_count, draw_chain, face_of_chain};
use crate::commands::params::{PointSchema, Target, VectorSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, ToolStep, Transaction,
};
use crate::document::Document;
use crate::geom::CurveDescriptor;
use crate::inference::InferenceKind;
use crate::math::Plane;
use crate::tolerance::Tolerances;
use crate::units::{Angle, Length};

fn default_segments() -> u32 {
    12
}

fn default_normal() -> DVec3 {
    DVec3::Z
}

/// A circular arc: center, unit normal, radius, start point and signed sweep
/// (positive = counter-clockwise about the normal).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ArcGeometry {
    pub(crate) center: DVec3,
    pub(crate) normal: DVec3,
    pub(crate) radius: f64,
    pub(crate) start: DVec3,
    pub(crate) sweep: f64,
}

impl ArcGeometry {
    /// `segments + 1` points from start to end.
    pub(crate) fn points(&self, segments: u32) -> Vec<DVec3> {
        let u = (self.start - self.center).normalize();
        let v = self.normal.cross(u);
        (0..=segments)
            .map(|k| {
                let a = self.sweep * f64::from(k) / f64::from(segments);
                self.center + (u * a.cos() + v * a.sin()) * self.radius
            })
            .collect()
    }

    /// The `Arc` descriptor of these points in container coordinates; angles
    /// are measured in the arc plane's frame (docs/03 §3.3).
    fn descriptor(&self, target: &Target, segments: u32) -> CurveDescriptor {
        let points = self.points(segments);
        let local: Vec<DVec3> = points.iter().map(|&p| target.to_local(p)).collect();
        let center = target.to_local(self.center);
        let normal = crate::math::transform::transform_normal(&target.local, self.normal);
        let frame = Plane::from_point_normal(center, normal).frame_at(center);
        let angle = |p: DVec3| {
            let q = frame.to_2d(p) - frame.to_2d(center);
            q.y.atan2(q.x)
        };
        let start_angle = angle(local[0]);
        let local_sweep = {
            let end = angle(local[local.len() - 1]) - start_angle;
            let full = std::f64::consts::TAU;
            // Same turning direction as the world arc (a mirror context flips it).
            let mut sweep = end.rem_euclid(full);
            if self.sweep < 0.0 {
                sweep -= full;
            }
            if sweep.abs() < 1e-12 {
                self.sweep
            } else {
                sweep
            }
        };
        CurveDescriptor::Arc {
            center,
            normal,
            radius: center.distance(local[0]),
            start_angle: Angle::radians(start_angle),
            end_angle: Angle::radians(start_angle + local_sweep),
            segments,
        }
    }
}

/// The circle through three points, if they are not collinear: center,
/// unit normal of the triangle `a → b → c`, radius.
pub(crate) fn circle_through(
    a: DVec3,
    b: DVec3,
    c: DVec3,
    tol: &Tolerances,
) -> Option<(DVec3, DVec3, f64)> {
    if tol.collinear(a, b, c) {
        return None;
    }
    let u = b - a;
    let v = c - a;
    let w = u.cross(v);
    let w2 = w.length_squared();
    if w2 <= 0.0 {
        return None;
    }
    let center =
        a + (v.cross(w) * u.length_squared() + w.cross(u) * v.length_squared()) / (2.0 * w2);
    Some((center, w / w2.sqrt(), center.distance(a)))
}

/// The arc from `a` through `b` to `c`.
pub(crate) fn arc_through(
    a: DVec3,
    b: DVec3,
    c: DVec3,
    tol: &Tolerances,
) -> Result<ArcGeometry, CommandError> {
    let (center, normal, radius) = circle_through(a, b, c, tol)
        .ok_or_else(|| CommandError::invalid_param("points", "the three points are collinear"))?;
    let u = (a - center).normalize();
    let v = normal.cross(u);
    let q = c - center;
    let sweep = q.dot(v).atan2(q.dot(u)).rem_euclid(std::f64::consts::TAU);
    Ok(ArcGeometry {
        center,
        normal,
        radius,
        start: a,
        sweep,
    })
}

fn check_segments(segments: u32) -> Result<(), CommandError> {
    check_count("segments", segments, 1, 999)
}

/// Draws an arc (and returns the shape output with the face it closed, if any).
fn draw_arc(
    tx: &mut Transaction<'_>,
    target: &Target,
    arc: &ArcGeometry,
    segments: u32,
) -> Result<Drawn, CommandError> {
    let descriptor = arc.descriptor(target, segments);
    draw_chain(tx, target, &arc.points(segments), false, Some(descriptor))
}

fn shape(drawn: Drawn, face: Option<crate::ids::Pid>, segments: u32) -> ShapeOutput {
    ShapeOutput {
        face: face.or_else(|| drawn.faces_created.first().copied()),
        edges: drawn.edges,
        faces_split: drawn.faces_split,
        segments,
    }
}

fn steps(prompts: &[(&str, &str)]) -> Vec<ToolStep> {
    prompts
        .iter()
        .map(|(prompt, value)| ToolStep {
            prompt: (*prompt).into(),
            value_field: (*value).into(),
            inference: InferenceKind::ALL.to_vec(),
        })
        .collect()
}

// ----- arc-2point -----------------------------------------------------------------------

/// `arc-2point` parameters: center, start, and either an end angle or an end
/// point (docs/05 §3.7). Also the parameters of `pie`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Arc2PointParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Center (world, mm).
    #[schemars(with = "PointSchema")]
    pub center: DVec3,
    /// Start point; its distance to the center is the radius.
    #[schemars(with = "PointSchema")]
    pub start: DVec3,
    /// Sweep from the start, counter-clockwise about the normal (negative =
    /// clockwise).
    #[serde(default)]
    pub end_angle: Option<Angle>,
    /// End point (used when `end_angle` is absent): the arc runs
    /// counter-clockwise from start to the direction of this point.
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub end: Option<DVec3>,
    /// Arc plane normal (+Z).
    #[serde(default = "default_normal")]
    #[schemars(with = "VectorSchema")]
    pub normal: DVec3,
    /// Number of segments (12).
    #[serde(default = "default_segments")]
    pub segments: u32,
}

impl Arc2PointParams {
    fn geometry(&self, tol: &Tolerances) -> Result<ArcGeometry, CommandError> {
        check_point("center", self.center)?;
        check_point("start", self.start)?;
        check_segments(self.segments)?;
        let normal = self
            .normal
            .try_normalize()
            .ok_or_else(|| CommandError::invalid_param("normal", "must be a non-zero vector"))?;
        let plane = Plane::from_point_normal(self.center, normal);
        if !tol.point_on_plane(&plane, self.start) {
            return Err(CommandError::invalid_param(
                "start",
                "must lie in the arc plane through the center",
            ));
        }
        let radius = self.center.distance(self.start);
        if radius <= tol.length {
            return Err(CommandError::invalid_param(
                "start",
                "must differ from the center",
            ));
        }
        let sweep = match (self.end_angle, self.end) {
            (Some(angle), _) => angle.as_radians(),
            (None, Some(end)) => {
                check_point("end", end)?;
                let u = (self.start - self.center) / radius;
                let v = normal.cross(u);
                let q = plane.project(end) - self.center;
                if q.length() <= tol.length {
                    return Err(CommandError::invalid_param(
                        "end",
                        "must differ from the center",
                    ));
                }
                let a = q.dot(v).atan2(q.dot(u)).rem_euclid(std::f64::consts::TAU);
                if a <= tol.angle {
                    std::f64::consts::TAU
                } else {
                    a
                }
            }
            (None, None) => {
                return Err(CommandError::invalid_param(
                    "end_angle",
                    "give end_angle or end",
                ));
            }
        };
        if !sweep.is_finite()
            || sweep.abs() <= tol.angle
            || sweep.abs() >= std::f64::consts::TAU - tol.angle
        {
            return Err(CommandError::OutOfRange {
                name: "end_angle".into(),
                reason: "the sweep must be between 0 and a full turn (use circle for a full turn)"
                    .into(),
            });
        }
        Ok(ArcGeometry {
            center: self.center,
            normal,
            radius,
            start: self.start,
            sweep,
        })
    }
}

/// The `arc-2point` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Arc2Point;

impl Command for Arc2Point {
    type Params = Arc2PointParams;
    type Output = ShapeOutput;

    fn name(&self) -> &'static str {
        "arc-2point"
    }

    fn validate(&self, doc: &Document, p: &Arc2PointParams) -> Result<(), CommandError> {
        p.geometry(doc.tolerances()).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: Arc2PointParams,
    ) -> Result<ShapeOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let arc = p.geometry(tx.tolerances())?;
        let drawn = draw_arc(tx, &target, &arc, p.segments)?;
        Ok(shape(drawn, None, p.segments))
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "arc-2point".into(),
            category: CommandCategory::Draw,
            label: "Arc".into(),
            view_only: false,
            steps: steps(&[
                ("Pick the center", ""),
                ("Pick the start point", "radius"),
                ("Pick the end point", "angle, or Ns for segments"),
            ]),
        }
    }
}

// ----- arc-3point -----------------------------------------------------------------------

/// `arc-3point` parameters (docs/05 §3.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Arc3PointParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Start point.
    #[schemars(with = "PointSchema")]
    pub a: DVec3,
    /// A point the arc passes through.
    #[schemars(with = "PointSchema")]
    pub b: DVec3,
    /// End point.
    #[schemars(with = "PointSchema")]
    pub c: DVec3,
    /// Number of segments (12).
    #[serde(default = "default_segments")]
    pub segments: u32,
}

/// The `arc-3point` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Arc3Point;

impl Command for Arc3Point {
    type Params = Arc3PointParams;
    type Output = ShapeOutput;

    fn name(&self) -> &'static str {
        "arc-3point"
    }

    fn validate(&self, doc: &Document, p: &Arc3PointParams) -> Result<(), CommandError> {
        for (name, q) in [("a", p.a), ("b", p.b), ("c", p.c)] {
            check_point(name, q)?;
        }
        check_segments(p.segments)?;
        arc_through(p.a, p.b, p.c, doc.tolerances()).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: Arc3PointParams,
    ) -> Result<ShapeOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let arc = arc_through(p.a, p.b, p.c, tx.tolerances())?;
        let drawn = draw_arc(tx, &target, &arc, p.segments)?;
        Ok(shape(drawn, None, p.segments))
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "arc-3point".into(),
            category: CommandCategory::Draw,
            label: "Arc".into(),
            view_only: false,
            steps: steps(&[
                ("Pick the start point", ""),
                ("Pick a point on the arc", ""),
                ("Pick the end point", "Ns for segments"),
            ]),
        }
    }
}

// ----- arc-bulge ------------------------------------------------------------------------

/// `arc-bulge` parameters (docs/05 §3.7): the default arc tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ArcBulgeParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Start point.
    #[schemars(with = "PointSchema")]
    pub start: DVec3,
    /// End point.
    #[schemars(with = "PointSchema")]
    pub end: DVec3,
    /// Signed sagitta: distance from the chord midpoint to the arc, positive
    /// to the left of the chord seen from the plane normal.
    #[serde(default)]
    pub bulge: Option<Length>,
    /// A point the arc passes through (instead of `bulge`).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub through: Option<DVec3>,
    /// Plane normal for `bulge` (+Z, or +Y for a vertical chord).
    #[serde(default)]
    #[schemars(with = "Option<VectorSchema>")]
    pub normal: Option<DVec3>,
    /// Number of segments (12).
    #[serde(default = "default_segments")]
    pub segments: u32,
}

impl ArcBulgeParams {
    fn geometry(&self, tol: &Tolerances) -> Result<ArcGeometry, CommandError> {
        check_point("start", self.start)?;
        check_point("end", self.end)?;
        check_segments(self.segments)?;
        let chord = self.end - self.start;
        let len = chord.length();
        if len <= tol.length {
            return Err(CommandError::invalid_param("end", "must differ from start"));
        }
        if let Some(through) = self.through {
            check_point("through", through)?;
            return arc_through(self.start, through, self.end, tol);
        }
        let bulge = self
            .bulge
            .ok_or_else(|| CommandError::invalid_param("bulge", "give bulge or through"))?
            .as_mm();
        if !bulge.is_finite() || bulge.abs() <= tol.length {
            return Err(CommandError::OutOfRange {
                name: "bulge".into(),
                reason: "must be non-zero (a zero bulge is a straight line)".into(),
            });
        }
        let dir = chord / len;
        let wanted = self.normal.unwrap_or(if tol.is_parallel(dir, DVec3::Z) {
            DVec3::Y
        } else {
            DVec3::Z
        });
        let normal = (wanted - dir * wanted.dot(dir))
            .try_normalize()
            .ok_or_else(|| {
                CommandError::invalid_param("normal", "must not be parallel to the chord")
            })?;
        let apex = (self.start + self.end) * 0.5 + normal.cross(dir) * bulge;
        arc_through(self.start, apex, self.end, tol)
    }
}

/// The `arc-bulge` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct ArcBulge;

impl Command for ArcBulge {
    type Params = ArcBulgeParams;
    type Output = ShapeOutput;

    fn name(&self) -> &'static str {
        "arc-bulge"
    }

    fn validate(&self, doc: &Document, p: &ArcBulgeParams) -> Result<(), CommandError> {
        p.geometry(doc.tolerances()).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ArcBulgeParams,
    ) -> Result<ShapeOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let arc = p.geometry(tx.tolerances())?;
        let drawn = draw_arc(tx, &target, &arc, p.segments)?;
        Ok(shape(drawn, None, p.segments))
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "arc-bulge".into(),
            category: CommandCategory::Draw,
            label: "Arc".into(),
            view_only: false,
            steps: steps(&[
                ("Pick the start point", ""),
                ("Pick the end point", "chord length"),
                ("Pick the bulge", "bulge length, rR radius or Ns segments"),
            ]),
        }
    }
}

// ----- pie --------------------------------------------------------------------------------

/// The `pie` command: an `arc-2point` arc plus the two radii and its face.
#[derive(Debug, Default, Clone, Copy)]
pub struct Pie;

impl Command for Pie {
    type Params = Arc2PointParams;
    type Output = ShapeOutput;

    fn name(&self) -> &'static str {
        "pie"
    }

    fn validate(&self, doc: &Document, p: &Arc2PointParams) -> Result<(), CommandError> {
        p.geometry(doc.tolerances()).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: Arc2PointParams,
    ) -> Result<ShapeOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let arc = p.geometry(tx.tolerances())?;
        let points = arc.points(p.segments);
        let mut drawn = draw_arc(tx, &target, &arc, p.segments)?;
        let end = points[points.len() - 1];
        let radii = draw_chain(tx, &target, &[end, arc.center, arc.start], false, None)?;
        drawn.edges.extend(radii.edges);
        drawn.faces_created.extend(radii.faces_created);
        drawn.faces_split.extend(radii.faces_split);
        let face = face_of_chain(tx, &target, &drawn)?;
        Ok(shape(drawn, face, p.segments))
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "pie".into(),
            category: CommandCategory::Draw,
            label: "Pie".into(),
            view_only: false,
            steps: steps(&[
                ("Pick the center", ""),
                ("Pick the start point", "radius"),
                ("Pick the end point", "angle, or Ns for segments"),
            ]),
        }
    }
}
