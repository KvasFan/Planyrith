//! Implements docs/03-geometry-kernel.md §2 (tolerances): the `Tolerances`
//! struct stored in `ModelInfo` and the comparison helpers that replace every
//! `f64 == f64` in geometry code (AGENTS.md §2).

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::math::Plane;

/// Document-level geometric thresholds (docs/03 §2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Tolerances {
    /// Two points closer than this are the same point (mm). Default `1e-3`.
    pub length: f64,
    /// `length²`, cached for squared-distance tests.
    pub length_sq: f64,
    /// Direction equality / coplanarity test (rad). Default `1e-6`.
    pub angle: f64,
    /// Max distance of a loop's vertices from its fitted plane (mm). Default `1e-2`.
    pub planarity: f64,
    /// Curve parameter equality. Default `1e-9`.
    pub parametric: f64,
}

impl Tolerances {
    /// The smallest `length` a document may set (docs/03 §2: "never below 1e-6 mm").
    pub const MIN_LENGTH: f64 = 1e-6;
    /// Largest supported coordinate magnitude in mm (docs/03 §1: ±1 × 10⁹).
    pub const MAX_EXTENT: f64 = 1e9;

    /// Builds tolerances from a merge length; derived values follow the default
    /// ratios of docs/03 §2. The length is clamped to [`Tolerances::MIN_LENGTH`].
    pub fn with_length(length: f64) -> Self {
        let length = length.max(Self::MIN_LENGTH);
        Tolerances {
            length,
            length_sq: length * length,
            angle: 1e-6,
            planarity: length * 10.0,
            parametric: 1e-9,
        }
    }

    /// `|a - b| <= length`.
    pub fn approx_eq_len(&self, a: f64, b: f64) -> bool {
        (a - b).abs() <= self.length
    }

    /// `|value| <= length`.
    pub fn approx_zero_len(&self, value: f64) -> bool {
        value.abs() <= self.length
    }

    /// `|a - b| <= angle` (radians).
    pub fn approx_eq_angle(&self, a: f64, b: f64) -> bool {
        (a - b).abs() <= self.angle
    }

    /// `|a - b| <= parametric`.
    pub fn approx_eq_param(&self, a: f64, b: f64) -> bool {
        (a - b).abs() <= self.parametric
    }

    /// Two points are the same point (distance ≤ `length`).
    pub fn same_point(&self, a: DVec3, b: DVec3) -> bool {
        a.distance_squared(b) <= self.length_sq
    }

    /// Two directions (not necessarily unit) are parallel or anti-parallel within
    /// `angle`.
    pub fn is_parallel(&self, a: DVec3, b: DVec3) -> bool {
        todo!("docs/03 §2 is_parallel")
    }

    /// Two directions are perpendicular within `angle`.
    pub fn is_perpendicular(&self, a: DVec3, b: DVec3) -> bool {
        todo!("docs/03 §2 is_perpendicular")
    }

    /// Two planes are coplanar: parallel normals (either orientation) and offsets
    /// within `planarity`.
    pub fn is_coplanar(&self, a: &Plane, b: &Plane) -> bool {
        todo!("docs/03 §2 is_coplanar")
    }

    /// Every point lies within `planarity` of the plane.
    pub fn points_on_plane(&self, plane: &Plane, points: &[DVec3]) -> bool {
        todo!("docs/03 §2 points_on_plane")
    }

    /// `true` when every coordinate is finite and within [`Tolerances::MAX_EXTENT`]
    /// (docs/03 §1 model extent).
    pub fn in_range(&self, p: DVec3) -> bool {
        p.is_finite() && p.abs().max_element() <= Self::MAX_EXTENT
    }
}

impl Default for Tolerances {
    fn default() -> Self {
        Tolerances {
            length: 1e-3,
            length_sq: 1e-6,
            angle: 1e-6,
            planarity: 1e-2,
            parametric: 1e-9,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let t = Tolerances::default();
        assert_eq!(t.length, 1e-3);
        assert_eq!(t.length_sq, 1e-6);
        assert_eq!(t.angle, 1e-6);
        assert_eq!(t.planarity, 1e-2);
        assert_eq!(t.parametric, 1e-9);
    }

    #[test]
    fn with_length_clamps_to_minimum() {
        let t = Tolerances::with_length(0.0);
        assert_eq!(t.length, Tolerances::MIN_LENGTH);
        assert!(t.approx_eq_len(1.0, 1.0 + 1e-7));
        assert!(!t.approx_eq_len(1.0, 1.1));
    }

    #[test]
    fn same_point_uses_squared_distance() {
        let t = Tolerances::default();
        assert!(t.same_point(DVec3::ZERO, DVec3::new(5e-4, 5e-4, 0.0)));
        assert!(!t.same_point(DVec3::ZERO, DVec3::new(2e-3, 0.0, 0.0)));
        assert!(t.in_range(DVec3::splat(1e9)));
        assert!(!t.in_range(DVec3::new(f64::NAN, 0.0, 0.0)));
    }
}
