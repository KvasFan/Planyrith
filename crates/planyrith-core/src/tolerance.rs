//! Implements docs/03-geometry-kernel.md §2 (tolerances) and the clamping rule
//! of §8: the `Tolerances` struct stored in `ModelInfo` and the comparison
//! helpers that replace every `f64 == f64` in geometry code (AGENTS.md §2).
//!
//! Direction tests work on possibly non-unit vectors and normalize first; a
//! zero vector is parallel and perpendicular to nothing.

#![warn(unused_variables, dead_code)]

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::math::{Plane, Segment};

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

/// Why a [`Tolerances`] value is not acceptable (docs/03 §2 rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TolerancesError {
    /// `length` is below [`Tolerances::MIN_LENGTH`], not finite, or `length_sq`
    /// does not match it.
    #[error("tolerance `length` must be finite and at least 1e-6 mm, with length_sq = length²")]
    Length,
    /// `angle` is not finite or not in `(0, π/2)`.
    #[error("tolerance `angle` must be finite and in (0, π/2) rad")]
    Angle,
    /// `planarity` is not finite or below `length`.
    #[error("tolerance `planarity` must be finite and at least `length`")]
    Planarity,
    /// `parametric` is not finite or not in `(0, 0.5)`.
    #[error("tolerance `parametric` must be finite and in (0, 0.5)")]
    Parametric,
}

impl Tolerances {
    /// The smallest `length` a document may set (docs/03 §2: "never below 1e-6 mm").
    pub const MIN_LENGTH: f64 = 1e-6;
    /// Largest supported coordinate magnitude in mm (docs/03 §1: ±1 × 10⁹).
    pub const MAX_EXTENT: f64 = 1e9;

    /// Builds tolerances from a merge length; derived values follow the default
    /// ratios of docs/03 §2. The length is clamped to [`Tolerances::MIN_LENGTH`].
    pub fn with_length(length: f64) -> Self {
        let length = if length.is_finite() {
            length.max(Self::MIN_LENGTH)
        } else {
            Self::MIN_LENGTH
        };
        Tolerances {
            length,
            length_sq: length * length,
            angle: 1e-6,
            planarity: length * 10.0,
            parametric: 1e-9,
        }
    }

    /// Builds explicit tolerances, deriving `length_sq`; rejects values outside
    /// the ranges of docs/03 §2 (see [`TolerancesError`]).
    pub fn new(
        length: f64,
        angle: f64,
        planarity: f64,
        parametric: f64,
    ) -> Result<Self, TolerancesError> {
        let t = Tolerances {
            length,
            length_sq: length * length,
            angle,
            planarity,
            parametric,
        };
        t.validate()?;
        Ok(t)
    }

    /// Checks the invariants of docs/03 §2 (used by `diagnostics::validate` on
    /// loaded documents).
    pub fn validate(&self) -> Result<(), TolerancesError> {
        if !self.length.is_finite()
            || self.length < Self::MIN_LENGTH
            || !self.length_sq.is_finite()
            || (self.length_sq - self.length * self.length).abs() > self.length * self.length * 1e-9
        {
            return Err(TolerancesError::Length);
        }
        if !self.angle.is_finite() || self.angle <= 0.0 || self.angle >= std::f64::consts::FRAC_PI_2
        {
            return Err(TolerancesError::Angle);
        }
        if !self.planarity.is_finite() || self.planarity < self.length {
            return Err(TolerancesError::Planarity);
        }
        if !self.parametric.is_finite() || self.parametric <= 0.0 || self.parametric >= 0.5 {
            return Err(TolerancesError::Parametric);
        }
        Ok(())
    }

    /// `|a - b| <= length`.
    pub fn approx_eq_len(&self, a: f64, b: f64) -> bool {
        (a - b).abs() <= self.length
    }

    /// `|value| <= length`.
    pub fn approx_zero_len(&self, value: f64) -> bool {
        value.abs() <= self.length
    }

    /// `a <= b + length` (less than or equal within tolerance).
    pub fn approx_le_len(&self, a: f64, b: f64) -> bool {
        a <= b + self.length
    }

    /// `a < b - length` (definitely less).
    pub fn definitely_lt_len(&self, a: f64, b: f64) -> bool {
        a < b - self.length
    }

    /// `|a - b| <= length²` for squared lengths and areas (docs/03 §2: faces
    /// with area below `length²` are degenerate).
    pub fn approx_eq_area(&self, a: f64, b: f64) -> bool {
        (a - b).abs() <= self.length_sq
    }

    /// `|area| <= length²`.
    pub fn approx_zero_area(&self, area: f64) -> bool {
        area.abs() <= self.length_sq
    }

    /// `|a - b| <= angle` (radians).
    pub fn approx_eq_angle(&self, a: f64, b: f64) -> bool {
        (a - b).abs() <= self.angle
    }

    /// `|value| <= angle` (radians).
    pub fn approx_zero_angle(&self, value: f64) -> bool {
        value.abs() <= self.angle
    }

    /// `|a - b| <= parametric`.
    pub fn approx_eq_param(&self, a: f64, b: f64) -> bool {
        (a - b).abs() <= self.parametric
    }

    /// Clamps a segment parameter into `[0, 1]` when it lies within
    /// `parametric` of that range, `None` when it is clearly outside
    /// (docs/03 §8 "clamp parameters to \[0,1\] with parametric tolerance").
    pub fn clamp_param(&self, t: f64) -> Option<f64> {
        if !t.is_finite() || t < -self.parametric || t > 1.0 + self.parametric {
            None
        } else {
            Some(t.clamp(0.0, 1.0))
        }
    }

    /// Two points are the same point (distance ≤ `length`).
    pub fn same_point(&self, a: DVec3, b: DVec3) -> bool {
        a.distance_squared(b) <= self.length_sq
    }

    /// Two directions (not necessarily unit) are parallel or anti-parallel within
    /// `angle`. Zero vectors are parallel to nothing.
    pub fn is_parallel(&self, a: DVec3, b: DVec3) -> bool {
        let (Some(a), Some(b)) = (a.try_normalize(), b.try_normalize()) else {
            return false;
        };
        // |a × b| = sin θ; for the tiny angles involved sin θ ≈ θ, and the
        // cross product stays accurate where 1 - |a·b| suffers cancellation.
        a.cross(b).length() <= self.angle
    }

    /// Two directions point the same way within `angle` (parallel and
    /// `a · b > 0`).
    pub fn same_direction(&self, a: DVec3, b: DVec3) -> bool {
        self.is_parallel(a, b) && a.dot(b) > 0.0
    }

    /// Two directions are perpendicular within `angle`. Zero vectors are
    /// perpendicular to nothing.
    pub fn is_perpendicular(&self, a: DVec3, b: DVec3) -> bool {
        let (Some(a), Some(b)) = (a.try_normalize(), b.try_normalize()) else {
            return false;
        };
        // a·b = cos θ = sin(π/2 - θ) ≈ deviation from a right angle.
        a.dot(b).abs() <= self.angle
    }

    /// Two planes are coplanar: parallel normals (either orientation) and offsets
    /// within `planarity`, measured at each plane's foot point.
    pub fn is_coplanar(&self, a: &Plane, b: &Plane) -> bool {
        self.is_parallel(a.normal, b.normal)
            && a.signed_distance(b.origin()).abs() <= self.planarity
            && b.signed_distance(a.origin()).abs() <= self.planarity
    }

    /// Two planes are coplanar *and* face the same way.
    pub fn is_coplanar_same_side(&self, a: &Plane, b: &Plane) -> bool {
        self.is_coplanar(a, b) && a.normal.dot(b.normal) > 0.0
    }

    /// `p` lies within `planarity` of the plane.
    pub fn point_on_plane(&self, plane: &Plane, p: DVec3) -> bool {
        plane.signed_distance(p).abs() <= self.planarity
    }

    /// Every point lies within `planarity` of the plane (an empty set does).
    pub fn points_on_plane(&self, plane: &Plane, points: &[DVec3]) -> bool {
        points.iter().all(|&p| self.point_on_plane(plane, p))
    }

    /// `p` lies on the segment within `length`, endpoints included
    /// (docs/03 §4.2).
    pub fn point_on_segment(&self, p: DVec3, segment: &Segment) -> bool {
        segment.contains_point(p, self)
    }

    /// `p` lies strictly inside the segment: on it within `length` but not within
    /// `length` of either endpoint (the "split here" case of docs/03 §4.2).
    pub fn point_inside_segment(&self, p: DVec3, segment: &Segment) -> bool {
        segment.contains_point(p, self)
            && !self.same_point(p, segment.a)
            && !self.same_point(p, segment.b)
    }

    /// Three points are collinear within `length` (the third point lies within
    /// `length` of the line through the other two, for every choice of the
    /// third). Coincident points count as collinear.
    pub fn collinear(&self, a: DVec3, b: DVec3, c: DVec3) -> bool {
        let ab = b - a;
        let ac = c - a;
        let bc = c - b;
        let cross = ab.cross(ac).length();
        // |ab × ac| / |ab| is the distance of c from line ab, etc. Collinear
        // when every such distance is within tolerance.
        let longest = ab.length().max(ac.length()).max(bc.length());
        cross <= self.length * longest
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
    use proptest::prelude::*;

    #[test]
    fn defaults_match_spec() {
        let t = Tolerances::default();
        assert_eq!(t.length, 1e-3);
        assert_eq!(t.length_sq, 1e-6);
        assert_eq!(t.angle, 1e-6);
        assert_eq!(t.planarity, 1e-2);
        assert_eq!(t.parametric, 1e-9);
        assert_eq!(t.validate(), Ok(()));
    }

    #[test]
    fn with_length_clamps_to_minimum() {
        let t = Tolerances::with_length(0.0);
        assert_eq!(t.length, Tolerances::MIN_LENGTH);
        assert!(t.approx_eq_len(1.0, 1.0 + 1e-7));
        assert!(!t.approx_eq_len(1.0, 1.1));
        assert_eq!(
            Tolerances::with_length(f64::NAN).length,
            Tolerances::MIN_LENGTH
        );
        assert_eq!(t.validate(), Ok(()));
    }

    #[test]
    fn new_rejects_out_of_range_values() {
        assert_eq!(
            Tolerances::new(1e-7, 1e-6, 1e-2, 1e-9),
            Err(TolerancesError::Length)
        );
        assert_eq!(
            Tolerances::new(1e-3, 2.0, 1e-2, 1e-9),
            Err(TolerancesError::Angle)
        );
        assert_eq!(
            Tolerances::new(1e-3, 1e-6, 1e-4, 1e-9),
            Err(TolerancesError::Planarity)
        );
        assert_eq!(
            Tolerances::new(1e-3, 1e-6, 1e-2, 0.5),
            Err(TolerancesError::Parametric)
        );
        let t = Tolerances {
            length_sq: 1.0,
            ..Tolerances::default()
        };
        assert_eq!(t.validate(), Err(TolerancesError::Length));
        assert!(Tolerances::new(0.1, 1e-4, 1.0, 1e-6).is_ok());
    }

    #[test]
    fn same_point_uses_squared_distance() {
        let t = Tolerances::default();
        assert!(t.same_point(DVec3::ZERO, DVec3::new(5e-4, 5e-4, 0.0)));
        assert!(!t.same_point(DVec3::ZERO, DVec3::new(2e-3, 0.0, 0.0)));
        assert!(t.in_range(DVec3::splat(1e9)));
        assert!(!t.in_range(DVec3::new(f64::NAN, 0.0, 0.0)));
    }

    #[test]
    fn direction_predicates() {
        let t = Tolerances::default();
        assert!(t.is_parallel(DVec3::X, DVec3::X * 5.0));
        assert!(t.is_parallel(DVec3::X, -DVec3::X));
        assert!(!t.is_parallel(DVec3::X, DVec3::new(1.0, 1e-5, 0.0)));
        assert!(t.is_parallel(DVec3::X, DVec3::new(1.0, 1e-7, 0.0)));
        assert!(!t.is_parallel(DVec3::ZERO, DVec3::X));
        assert!(t.same_direction(DVec3::Y, DVec3::Y * 2.0));
        assert!(!t.same_direction(DVec3::Y, -DVec3::Y));
        assert!(t.is_perpendicular(DVec3::X, DVec3::Y * 3.0));
        assert!(t.is_perpendicular(DVec3::X, DVec3::new(1e-7, 1.0, 0.0)));
        assert!(!t.is_perpendicular(DVec3::X, DVec3::new(1e-5, 1.0, 0.0)));
        assert!(!t.is_perpendicular(DVec3::ZERO, DVec3::Y));
    }

    #[test]
    fn plane_predicates() {
        let t = Tolerances::default();
        let a = Plane::XY;
        let b = Plane::from_point_normal(DVec3::new(100.0, -5.0, 0.005), -DVec3::Z);
        let c = Plane::from_point_normal(DVec3::new(0.0, 0.0, 0.05), DVec3::Z);
        assert!(t.is_coplanar(&a, &b));
        assert!(!t.is_coplanar_same_side(&a, &b));
        assert!(t.is_coplanar_same_side(&a, &a));
        assert!(!t.is_coplanar(&a, &c));
        assert!(!t.is_coplanar(&a, &Plane::YZ));
        assert!(t.points_on_plane(
            &a,
            &[DVec3::new(1.0, 2.0, 0.009), DVec3::new(-3.0, 4.0, -0.009)]
        ));
        assert!(!t.points_on_plane(&a, &[DVec3::ZERO, DVec3::new(0.0, 0.0, 0.011)]));
        assert!(t.points_on_plane(&a, &[]));
    }

    #[test]
    fn segment_and_collinearity_predicates() {
        let t = Tolerances::default();
        let s = Segment::new(DVec3::ZERO, DVec3::new(10.0, 0.0, 0.0));
        assert!(t.point_on_segment(DVec3::new(5.0, 0.0005, 0.0), &s));
        assert!(t.point_on_segment(DVec3::ZERO, &s));
        assert!(!t.point_on_segment(DVec3::new(10.002, 0.0, 0.0), &s));
        assert!(t.point_inside_segment(DVec3::new(5.0, 0.0, 0.0), &s));
        assert!(!t.point_inside_segment(DVec3::new(10.0, 0.0, 0.0), &s));
        assert!(t.collinear(DVec3::ZERO, DVec3::X, DVec3::X * 7.0));
        assert!(t.collinear(DVec3::ZERO, DVec3::ZERO, DVec3::X));
        assert!(!t.collinear(DVec3::ZERO, DVec3::X, DVec3::new(0.5, 0.01, 0.0)));
        assert_eq!(t.clamp_param(-1e-10), Some(0.0));
        assert_eq!(t.clamp_param(1.0 + 1e-10), Some(1.0));
        assert_eq!(t.clamp_param(0.25), Some(0.25));
        assert_eq!(t.clamp_param(-1e-3), None);
        assert_eq!(t.clamp_param(f64::NAN), None);
        assert!(t.approx_le_len(1.0, 1.0005));
        assert!(t.definitely_lt_len(1.0, 1.1));
        assert!(t.approx_zero_area(5e-7));
        assert!(t.approx_eq_area(1.0, 1.0 + 5e-7));
        assert!(t.approx_zero_angle(-5e-7));
    }

    fn arb_dir() -> impl Strategy<Value = DVec3> {
        (-1.0f64..1.0, -1.0f64..1.0, -1.0f64..1.0)
            .prop_map(|(x, y, z)| DVec3::new(x, y, z))
            .prop_filter("non-degenerate", |v| v.length() > 0.1)
    }

    proptest! {
        /// A vector is parallel to any non-zero multiple of itself and
        /// perpendicular to any vector in its orthogonal complement.
        #[test]
        fn parallel_and_perpendicular_closed_forms(
            v in arb_dir(),
            w in arb_dir(),
            k in prop::sample::select(vec![-1000.0f64, -1.0, -1e-3, 1e-3, 2.5, 1e6]),
        ) {
            let t = Tolerances::default();
            prop_assert!(t.is_parallel(v, v * k));
            prop_assert_eq!(t.same_direction(v, v * k), k > 0.0);
            // Gram–Schmidt: w minus its component along v is perpendicular to v.
            let perp = w - v * (w.dot(v) / v.length_squared());
            if perp.length() > 0.1 {
                prop_assert!(t.is_perpendicular(v, perp));
                prop_assert!(!t.is_parallel(v, perp));
            }
        }

        /// Any point projected onto a plane is on it; moving it further than
        /// `planarity` along the normal takes it off.
        #[test]
        fn plane_membership(n in arb_dir(), p in arb_dir(), q in arb_dir(), off in 0.02f64..10.0) {
            let t = Tolerances::default();
            let plane = Plane::from_point_normal(p * 1000.0, n);
            let on = plane.project(q * 1000.0);
            prop_assert!(t.point_on_plane(&plane, on));
            prop_assert!(!t.point_on_plane(&plane, on + plane.normal * off));
            let shifted = Plane::from_point_normal(plane.origin() + plane.normal * off, -n);
            prop_assert!(!t.is_coplanar(&plane, &shifted));
            prop_assert!(t.is_coplanar(&plane, &plane.flipped()));
        }
    }
}
