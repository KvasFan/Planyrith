//! Implements the `math/` entry of docs/01-architecture.md §2 and docs/03 §1:
//! re-exports of the `glam` `f64` types used by the kernel plus plane, ray,
//! segment, bounding-box, intersection, 2D polygon and transform utilities.
//!
//! Coordinates are right-handed, Z up, millimeters (docs/03 §1). Transforms are
//! `DAffine3` and may be mirrored or non-uniformly scaled; algorithms must not
//! assume orthonormality unless they normalize first.

#![warn(unused_variables, dead_code)]

pub mod aabb;
pub mod intersect;
pub mod plane;
pub mod polygon;
pub mod ray;
pub mod segment;
pub mod transform;

pub use aabb::Aabb;
pub use glam::{DAffine2, DAffine3, DMat2, DMat3, DMat4, DQuat, DVec2, DVec3, DVec4};
pub use intersect::SegmentContact;
pub use plane::{Plane, PlaneFrame, PlaneSide};
pub use polygon::PointInPolygon;
pub use ray::{Ray, RayTriangleHit};
pub use segment::Segment;
pub use transform::TransformParts;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One of the three drawing axes (docs/03 §1: X red, Y green, Z blue).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    /// +X, drawn red.
    X,
    /// +Y, drawn green.
    Y,
    /// +Z (up), drawn blue.
    Z,
}

impl Axis {
    /// All axes in order.
    pub const ALL: [Axis; 3] = [Axis::X, Axis::Y, Axis::Z];

    /// Unit direction of the axis.
    pub const fn direction(self) -> DVec3 {
        match self {
            Axis::X => DVec3::X,
            Axis::Y => DVec3::Y,
            Axis::Z => DVec3::Z,
        }
    }

    /// Index of the axis in a `DVec3` (`0`, `1`, `2`).
    pub const fn index(self) -> usize {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }

    /// The component of `v` along this axis.
    pub fn component(self, v: DVec3) -> f64 {
        match self {
            Axis::X => v.x,
            Axis::Y => v.y,
            Axis::Z => v.z,
        }
    }

    /// The axis whose direction is most aligned with `v` (ties resolved X, Y,
    /// Z), with the sign of that component; `None` for a zero vector.
    pub fn dominant(v: DVec3) -> Option<(Axis, f64)> {
        let a = v.abs();
        if a.max_element() <= 0.0 || !v.is_finite() {
            return None;
        }
        let axis = if a.x >= a.y && a.x >= a.z {
            Axis::X
        } else if a.y >= a.z {
            Axis::Y
        } else {
            Axis::Z
        };
        Some((axis, axis.component(v).signum()))
    }

    /// The coordinate plane perpendicular to this axis through the origin.
    pub fn perpendicular_plane(self) -> Plane {
        Plane::from_point_normal(DVec3::ZERO, self.direction())
    }

    /// The lowercase name (`x`, `y`, `z`).
    pub const fn name(self) -> &'static str {
        match self {
            Axis::X => "x",
            Axis::Y => "y",
            Axis::Z => "z",
        }
    }

    /// Parses `x|y|z|-x|-y|-z` into an axis and a sign (docs/08 §2 `Vector`).
    pub fn parse_signed(text: &str) -> Option<(Axis, f64)> {
        let text = text.trim();
        let (sign, body) = match text.strip_prefix('-') {
            Some(rest) => (-1.0, rest),
            None => (1.0, text.strip_prefix('+').unwrap_or(text)),
        };
        let axis = match body {
            "x" | "X" => Axis::X,
            "y" | "Y" => Axis::Y,
            "z" | "Z" => Axis::Z,
            _ => return None,
        };
        Some((axis, sign))
    }
}

impl std::fmt::Display for Axis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_parse_handles_signs() {
        assert_eq!(Axis::parse_signed("-z"), Some((Axis::Z, -1.0)));
        assert_eq!(Axis::parse_signed("x"), Some((Axis::X, 1.0)));
        assert_eq!(Axis::parse_signed(" +Y "), Some((Axis::Y, 1.0)));
        assert_eq!(Axis::parse_signed("w"), None);
    }

    #[test]
    fn axis_helpers() {
        assert_eq!(
            Axis::dominant(DVec3::new(0.1, -0.9, 0.5)),
            Some((Axis::Y, -1.0))
        );
        assert_eq!(
            Axis::dominant(DVec3::new(1.0, 1.0, 1.0)),
            Some((Axis::X, 1.0))
        );
        assert_eq!(Axis::dominant(DVec3::ZERO), None);
        for axis in Axis::ALL {
            assert_eq!(axis.component(axis.direction()), 1.0);
            assert_eq!(axis.direction()[axis.index()], 1.0);
            assert_eq!(axis.name().parse::<String>().unwrap(), axis.to_string());
        }
        assert_eq!(Axis::Z.perpendicular_plane(), Plane::XY);
    }
}
