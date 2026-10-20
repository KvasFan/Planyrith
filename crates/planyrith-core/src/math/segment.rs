//! Implements segment utilities for edges (docs/03 §3 edges are straight
//! segments; §4.2 splitting; §6 spatial queries).

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::math::Aabb;
use crate::tolerance::Tolerances;

/// A straight segment between two points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// Start point.
    pub a: DVec3,
    /// End point.
    pub b: DVec3,
}

impl Segment {
    /// Builds a segment.
    pub const fn new(a: DVec3, b: DVec3) -> Self {
        Segment { a, b }
    }

    /// Length `|b - a|`.
    pub fn length(&self) -> f64 {
        self.a.distance(self.b)
    }

    /// Unit direction from `a` to `b` (zero for a degenerate segment).
    pub fn direction(&self) -> DVec3 {
        (self.b - self.a).normalize_or_zero()
    }

    /// Midpoint (the "Midpoint" inference, docs/03 §7).
    pub fn midpoint(&self) -> DVec3 {
        (self.a + self.b) * 0.5
    }

    /// Point at parameter `t ∈ [0, 1]`.
    pub fn point_at(&self, t: f64) -> DVec3 {
        self.a.lerp(self.b, t)
    }

    /// Reversed segment.
    pub fn reversed(&self) -> Self {
        Segment {
            a: self.b,
            b: self.a,
        }
    }

    /// Bounding box of the segment.
    pub fn aabb(&self) -> Aabb {
        Aabb::from_points([self.a, self.b])
    }

    /// Parameter in `[0, 1]` of the segment point closest to `p`.
    pub fn closest_param(&self, p: DVec3) -> f64 {
        let ab = self.b - self.a;
        let len_sq = ab.length_squared();
        if len_sq == 0.0 {
            return 0.0;
        }
        ((p - self.a).dot(ab) / len_sq).clamp(0.0, 1.0)
    }

    /// Distance from `p` to the segment.
    pub fn distance_to_point(&self, p: DVec3) -> f64 {
        self.point_at(self.closest_param(p)).distance(p)
    }

    /// `true` when `p` lies on the segment within `tol.length` (endpoints
    /// included) — used to split new edges at existing vertices (docs/03 §4.2).
    pub fn contains_point(&self, p: DVec3, tol: &Tolerances) -> bool {
        todo!("docs/03 §4.2 point-on-segment test")
    }

    /// Parameters `(t_self, t_other)` of the closest points of two segments,
    /// clamped to `[0, 1]` with `tol.parametric` (docs/03 §8).
    pub fn closest_params(&self, other: &Segment, tol: &Tolerances) -> (f64, f64) {
        todo!("docs/03 §8 segment–segment closest points")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_measures() {
        let s = Segment::new(DVec3::ZERO, DVec3::new(0.0, 4.0, 0.0));
        assert_eq!(s.length(), 4.0);
        assert_eq!(s.midpoint(), DVec3::new(0.0, 2.0, 0.0));
        assert_eq!(s.closest_param(DVec3::new(1.0, 1.0, 0.0)), 0.25);
        assert_eq!(
            s.distance_to_point(DVec3::new(3.0, 10.0, 0.0)),
            (9.0f64 + 36.0).sqrt()
        );
    }
}
