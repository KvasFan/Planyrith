//! Implements segment utilities for edges (docs/03 §3 edges are straight
//! segments; §4.2 splitting; §6 spatial queries; §8 parameter clamping).

#![warn(unused_variables, dead_code)]

use glam::{DAffine3, DVec3};
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

    /// Squared length.
    pub fn length_squared(&self) -> f64 {
        self.a.distance_squared(self.b)
    }

    /// `b - a` (not normalized).
    pub fn delta(&self) -> DVec3 {
        self.b - self.a
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

    /// `true` when the segment is shorter than `tol.length` (docs/03 §2: such
    /// edges must not exist).
    pub fn is_degenerate(&self, tol: &Tolerances) -> bool {
        tol.same_point(self.a, self.b)
    }

    /// Bounding box of the segment.
    pub fn aabb(&self) -> Aabb {
        Aabb::from_points([self.a, self.b])
    }

    /// Parameter in `[0, 1]` of the segment point closest to `p`.
    pub fn closest_param(&self, p: DVec3) -> f64 {
        let ab = self.b - self.a;
        let len_sq = ab.length_squared();
        if len_sq <= 0.0 {
            return 0.0;
        }
        ((p - self.a).dot(ab) / len_sq).clamp(0.0, 1.0)
    }

    /// Unclamped parameter of the projection of `p` onto the segment's line
    /// (`0` for a degenerate segment).
    pub fn line_param(&self, p: DVec3) -> f64 {
        let ab = self.b - self.a;
        let len_sq = ab.length_squared();
        if len_sq <= 0.0 {
            return 0.0;
        }
        (p - self.a).dot(ab) / len_sq
    }

    /// The segment point closest to `p`.
    pub fn closest_point(&self, p: DVec3) -> DVec3 {
        self.point_at(self.closest_param(p))
    }

    /// Distance from `p` to the segment.
    pub fn distance_to_point(&self, p: DVec3) -> f64 {
        self.closest_point(p).distance(p)
    }

    /// Distance from `p` to the infinite line through the segment (the
    /// distance to `a` for a degenerate segment).
    pub fn distance_to_line(&self, p: DVec3) -> f64 {
        let ab = self.b - self.a;
        let len = ab.length();
        if len <= 0.0 {
            return self.a.distance(p);
        }
        ab.cross(p - self.a).length() / len
    }

    /// `true` when `p` lies on the segment within `tol.length` (endpoints
    /// included) — used to split new edges at existing vertices (docs/03 §4.2).
    pub fn contains_point(&self, p: DVec3, tol: &Tolerances) -> bool {
        self.closest_point(p).distance_squared(p) <= tol.length_sq
    }

    /// Parameters `(t_self, t_other)` of the closest points of two segments,
    /// clamped to `[0, 1]` with `tol.parametric` (docs/03 §8).
    ///
    /// Nearly parallel segments use the well-conditioned form of the line
    /// parameter (through `d1 × d2`), so long, slightly tilted segments still
    /// get their true closest points. Only numerically parallel segments take
    /// the midpoint of the overlapping parameter range on `self`, which is
    /// deterministic and symmetric.
    pub fn closest_params(&self, other: &Segment, tol: &Tolerances) -> (f64, f64) {
        let d1 = self.b - self.a;
        let d2 = other.b - other.a;
        let r = self.a - other.a;
        let a = d1.length_squared();
        let e = d2.length_squared();
        let f = d2.dot(r);

        let snap = |t: f64| -> f64 {
            // Snap to the ends within the parametric tolerance, then clamp.
            if t.abs() <= tol.parametric {
                0.0
            } else if (t - 1.0).abs() <= tol.parametric {
                1.0
            } else {
                t.clamp(0.0, 1.0)
            }
        };

        if a <= 0.0 && e <= 0.0 {
            return (0.0, 0.0);
        }
        if a <= 0.0 {
            return (0.0, snap(f / e));
        }
        let c = d1.dot(r);
        if e <= 0.0 {
            return (snap(-c / a), 0.0);
        }

        let b = d1.dot(d2);
        let n = d1.cross(d2);
        let nn = n.length_squared();
        let parallel = nn <= 1e-24 * a * e;

        let s = if parallel {
            // Parallel: pick the midpoint of other's projection onto self,
            // clamped, which is symmetric in the two segments' roles.
            let t0 = self.line_param(other.a);
            let t1 = self.line_param(other.b);
            let lo = t0.min(t1).max(0.0);
            let hi = t0.max(t1).min(1.0);
            if lo <= hi {
                (lo + hi) * 0.5
            } else if t0.min(t1) > 1.0 {
                1.0
            } else {
                0.0
            }
        } else {
            snap((other.a - self.a).cross(d2).dot(n) / nn)
        };
        // Closest point on other's line to self.point_at(s).
        let t = (b * s + f) / e;
        if (-tol.parametric..=1.0 + tol.parametric).contains(&t) {
            return (s, snap(t));
        }
        // t is out of range: clamp it and recompute s.
        let t = t.clamp(0.0, 1.0);
        let s = snap((t * b - c) / a);
        (s, t)
    }

    /// The closest points `(on_self, on_other)` of two segments.
    pub fn closest_points(&self, other: &Segment, tol: &Tolerances) -> (DVec3, DVec3) {
        let (s, t) = self.closest_params(other, tol);
        (self.point_at(s), other.point_at(t))
    }

    /// Distance between two segments.
    pub fn distance_to_segment(&self, other: &Segment, tol: &Tolerances) -> f64 {
        let (p, q) = self.closest_points(other, tol);
        p.distance(q)
    }

    /// The segment transformed by `t`.
    pub fn transformed(&self, t: &DAffine3) -> Self {
        Segment {
            a: t.transform_point3(self.a),
            b: t.transform_point3(self.b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

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
        assert_eq!(s.distance_to_line(DVec3::new(3.0, 10.0, 0.0)), 3.0);
        assert_eq!(s.line_param(DVec3::new(0.0, 8.0, 0.0)), 2.0);
        assert!(!s.is_degenerate(&Tolerances::default()));
        assert!(
            Segment::new(DVec3::X, DVec3::X + DVec3::Y * 1e-4)
                .is_degenerate(&Tolerances::default())
        );
    }

    #[test]
    fn contains_point_within_tolerance() {
        let t = Tolerances::default();
        let s = Segment::new(DVec3::ZERO, DVec3::new(10.0, 0.0, 0.0));
        assert!(s.contains_point(DVec3::new(5.0, 0.0, 0.0), &t));
        assert!(s.contains_point(DVec3::new(5.0, 0.0009, 0.0), &t));
        assert!(s.contains_point(DVec3::ZERO, &t));
        assert!(s.contains_point(DVec3::new(10.0009, 0.0, 0.0), &t));
        assert!(!s.contains_point(DVec3::new(5.0, 0.002, 0.0), &t));
        assert!(!s.contains_point(DVec3::new(10.002, 0.0, 0.0), &t));
        let dot = Segment::new(DVec3::X, DVec3::X);
        assert!(dot.contains_point(DVec3::X, &t));
        assert!(!dot.contains_point(DVec3::ZERO, &t));
    }

    #[test]
    fn closest_params_cases() {
        let t = Tolerances::default();
        let a = Segment::new(DVec3::ZERO, DVec3::new(10.0, 0.0, 0.0));
        // Crossing in projection, separated in z.
        let b = Segment::new(DVec3::new(4.0, -5.0, 2.0), DVec3::new(4.0, 5.0, 2.0));
        let (s, u) = a.closest_params(&b, &t);
        assert!((s - 0.4).abs() < 1e-12 && (u - 0.5).abs() < 1e-12);
        assert!((a.distance_to_segment(&b, &t) - 2.0).abs() < 1e-12);
        // Parallel overlapping: midpoint of the overlap.
        let c = Segment::new(DVec3::new(5.0, 1.0, 0.0), DVec3::new(15.0, 1.0, 0.0));
        let (s, u) = a.closest_params(&c, &t);
        assert!((s - 0.75).abs() < 1e-12, "{s}");
        assert!((u - 0.25).abs() < 1e-12, "{u}");
        assert!((a.distance_to_segment(&c, &t) - 1.0).abs() < 1e-12);
        // Parallel disjoint: nearest ends.
        let d = Segment::new(DVec3::new(20.0, 1.0, 0.0), DVec3::new(30.0, 1.0, 0.0));
        assert_eq!(a.closest_params(&d, &t), (1.0, 0.0));
        // Endpoint beyond the other segment's range: clamped.
        let e = Segment::new(DVec3::new(12.0, -1.0, 0.0), DVec3::new(12.0, 1.0, 0.0));
        let (s, u) = a.closest_params(&e, &t);
        assert_eq!(s, 1.0);
        assert!((u - 0.5).abs() < 1e-12);
        // Degenerate segments.
        let dot = Segment::new(DVec3::new(3.0, 7.0, 0.0), DVec3::new(3.0, 7.0, 0.0));
        assert_eq!(a.closest_params(&dot, &t), (0.3, 0.0));
        assert_eq!(dot.closest_params(&a, &t), (0.0, 0.3));
        assert_eq!(dot.closest_params(&dot, &t), (0.0, 0.0));
        // Parametric snapping to exact ends.
        let f = Segment::new(
            DVec3::new(10.0 + 1e-12, -1.0, 0.0),
            DVec3::new(10.0 + 1e-12, 1.0, 0.0),
        );
        assert_eq!(a.closest_params(&f, &t).0, 1.0);
    }

    fn arb_vec(range: f64) -> impl Strategy<Value = DVec3> {
        (-range..range, -range..range, -range..range).prop_map(|(x, y, z)| DVec3::new(x, y, z))
    }

    proptest! {
        /// Closest parameters are in range, the points they give are no farther
        /// apart than any sampled pair, and the result is symmetric.
        #[test]
        fn closest_points_minimize_distance(
            a0 in arb_vec(1000.0), a1 in arb_vec(1000.0),
            b0 in arb_vec(1000.0), b1 in arb_vec(1000.0),
            samples in prop::collection::vec((0.0f64..=1.0, 0.0f64..=1.0), 1..16),
        ) {
            let t = Tolerances::default();
            let a = Segment::new(a0, a1);
            let b = Segment::new(b0, b1);
            let (s, u) = a.closest_params(&b, &t);
            prop_assert!((0.0..=1.0).contains(&s) && (0.0..=1.0).contains(&u));
            let best = a.distance_to_segment(&b, &t);
            let (u2, s2) = b.closest_params(&a, &t);
            let best_rev = b.point_at(u2).distance(a.point_at(s2));
            prop_assert!((best - best_rev).abs() <= 1e-6 * best.max(1.0), "{best} vs {best_rev}");
            for (x, y) in samples {
                let d = a.point_at(x).distance(b.point_at(y));
                prop_assert!(best <= d + 1e-6 * d.max(1.0), "{best} > {d}");
            }
        }

        /// Any point on the segment is contained; a point pushed more than the
        /// tolerance away perpendicular to it is not.
        #[test]
        fn containment(a0 in arb_vec(1000.0), a1 in arb_vec(1000.0), f in 0.0f64..=1.0, side in arb_vec(1.0)) {
            let t = Tolerances::default();
            let s = Segment::new(a0, a1);
            prop_assume!(s.length() > 1.0);
            let p = s.point_at(f);
            prop_assert!(s.contains_point(p, &t));
            let perp = side - s.direction() * side.dot(s.direction());
            prop_assume!(perp.length() > 0.1);
            prop_assert!(!s.contains_point(p + perp.normalize() * 0.01, &t));
        }
    }
}
