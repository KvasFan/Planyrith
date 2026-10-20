//! Implements the `math/` entry of docs/01-architecture.md §2 and docs/03 §1:
//! re-exports of the `glam` `f64` types used by the kernel plus plane, ray,
//! segment, bounding-box and transform utilities.
//!
//! Coordinates are right-handed, Z up, millimeters (docs/03 §1). Transforms are
//! `DAffine3` and may be mirrored or non-uniformly scaled; algorithms must not
//! assume orthonormality unless they normalize first.

pub mod aabb;
pub mod plane;
pub mod ray;
pub mod segment;
pub mod transform;

pub use aabb::Aabb;
pub use glam::{DAffine2, DAffine3, DMat2, DMat3, DMat4, DQuat, DVec2, DVec3, DVec4};
pub use plane::{Plane, PlaneFrame};
pub use ray::Ray;
pub use segment::Segment;

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

    /// The coordinate plane perpendicular to this axis through the origin.
    pub fn perpendicular_plane(self) -> Plane {
        Plane::from_point_normal(DVec3::ZERO, self.direction())
    }

    /// Parses `x|y|z|-x|-y|-z` into an axis and a sign (docs/08 §2 `Vector`).
    pub fn parse_signed(text: &str) -> Option<(Axis, f64)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_parse_handles_signs() {
        assert_eq!(Axis::parse_signed("-z"), Some((Axis::Z, -1.0)));
        assert_eq!(Axis::parse_signed("x"), Some((Axis::X, 1.0)));
        assert_eq!(Axis::parse_signed("w"), None);
    }
}
