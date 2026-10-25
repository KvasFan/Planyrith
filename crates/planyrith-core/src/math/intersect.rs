//! Implements the mesh-free intersection primitives of
//! docs/03-geometry-kernel.md §4.2 (segment–segment, with `length` contact and
//! `parametric` clamping per §8), §4.12 (segment–plane) and §6–§7 /
//! docs/06 §8 (ray–plane, ray–triangle). `geom::intersect` builds the
//! mesh-aware operations on top of these.

#![warn(unused_variables, dead_code)]

use glam::DVec3;

use crate::math::{Plane, Ray, RayTriangleHit, Segment};
use crate::tolerance::Tolerances;

/// Outcome of a segment–segment test (docs/03 §4.2 step 2). Parameters are in
/// `[0, 1]` on the respective segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SegmentContact {
    /// No contact within `tol.length`.
    None,
    /// A single crossing or touching point.
    Point {
        /// Parameter on the first segment.
        t_a: f64,
        /// Parameter on the second segment.
        t_b: f64,
        /// The contact point (midpoint of the two closest points).
        point: DVec3,
    },
    /// Collinear overlap on `[t0, t1]` of the first segment (`t0 < t1`).
    Overlap {
        /// Start parameter on the first segment.
        t0: f64,
        /// End parameter on the first segment.
        t1: f64,
    },
}

impl SegmentContact {
    /// `true` for [`SegmentContact::None`].
    pub fn is_none(&self) -> bool {
        matches!(self, SegmentContact::None)
    }

    /// `true` for a crossing strictly inside both segments: a `Point` contact
    /// farther than `tol.length` from every endpoint.
    pub fn is_proper_crossing(&self, a: &Segment, b: &Segment, tol: &Tolerances) -> bool {
        match *self {
            SegmentContact::Point { point, .. } => {
                !tol.same_point(point, a.a)
                    && !tol.same_point(point, a.b)
                    && !tol.same_point(point, b.a)
                    && !tol.same_point(point, b.b)
            }
            _ => false,
        }
    }
}

/// Segment–segment intersection with `tol.length` contact and `tol.parametric`
/// clamping (docs/03 §4.2, §8). Works for coplanar and skew segments alike:
/// skew segments whose closest points are within `tol.length` touch.
///
/// Collinear segments (both endpoints of one within `tol.length` of the line
/// of the other, tested both ways so the answer does not depend on the
/// argument order) report an `Overlap` when the shared part is longer than
/// `tol.length`, a `Point` when they merely touch end to end, and `None`
/// otherwise.
pub fn segment_segment(a: &Segment, b: &Segment, tol: &Tolerances) -> SegmentContact {
    let la = a.length();
    let lb = b.length();
    // Collinear case first: the closest-points formula degenerates there.
    let b_on_a = a.distance_to_line(b.a) <= tol.length && a.distance_to_line(b.b) <= tol.length;
    let a_on_b = b.distance_to_line(a.a) <= tol.length && b.distance_to_line(a.b) <= tol.length;
    if la > 0.0 && lb > 0.0 && (b_on_a || a_on_b) {
        let t0 = a.line_param(b.a);
        let t1 = a.line_param(b.b);
        let lo = t0.min(t1).max(0.0);
        let hi = t0.max(t1).min(1.0);
        let overlap_len = (hi - lo) * la;
        if lo > hi + tol.length / la {
            return SegmentContact::None;
        }
        if overlap_len > tol.length {
            return SegmentContact::Overlap {
                t0: snap_param(lo, tol),
                t1: snap_param(hi, tol),
            };
        }
        let t_a = snap_param(((lo + hi) * 0.5).clamp(0.0, 1.0), tol);
        let point = a.point_at(t_a);
        let t_b = snap_param(b.closest_param(point), tol);
        return SegmentContact::Point { t_a, t_b, point };
    }
    let (t_a, t_b) = a.closest_params(b, tol);
    let pa = a.point_at(t_a);
    let pb = b.point_at(t_b);
    if pa.distance_squared(pb) <= tol.length_sq {
        SegmentContact::Point {
            t_a,
            t_b,
            point: (pa + pb) * 0.5,
        }
    } else {
        SegmentContact::None
    }
}

/// Segment–plane crossing: `(t, point)` or `None` when the segment lies in the
/// plane (both ends within `tol.planarity`), runs parallel to it, or does not
/// reach it. An endpoint within `tol.planarity` of the plane is the crossing
/// (`t = 0` or `t = 1`) (docs/03 §4.12).
pub fn segment_plane(s: &Segment, plane: &Plane, tol: &Tolerances) -> Option<(f64, DVec3)> {
    let da = plane.signed_distance(s.a);
    let db = plane.signed_distance(s.b);
    let a_on = da.abs() <= tol.planarity;
    let b_on = db.abs() <= tol.planarity;
    match (a_on, b_on) {
        (true, true) => None,
        (true, false) => Some((0.0, s.a)),
        (false, true) => Some((1.0, s.b)),
        (false, false) => {
            if (da > 0.0) == (db > 0.0) {
                return None;
            }
            let t = da / (da - db);
            let t = tol.clamp_param(t)?;
            Some((t, s.point_at(t)))
        }
    }
}

/// Ray–plane intersection; see [`Ray::intersect_plane`] (docs/03 §7).
pub fn ray_plane(ray: &Ray, plane: &Plane, tol: &Tolerances) -> Option<(f64, DVec3)> {
    ray.intersect_plane(plane, tol).map(|t| (t, ray.at(t)))
}

/// Ray–triangle intersection (Möller–Trumbore); see
/// [`Ray::intersect_triangle`] (docs/06 §8).
pub fn ray_triangle(
    ray: &Ray,
    a: DVec3,
    b: DVec3,
    c: DVec3,
    tol: &Tolerances,
) -> Option<RayTriangleHit> {
    ray.intersect_triangle(a, b, c, tol)
}

/// Snaps a parameter within `tol.parametric` of `0` or `1` to that end.
fn snap_param(t: f64, tol: &Tolerances) -> f64 {
    if t.abs() <= tol.parametric {
        0.0
    } else if (t - 1.0).abs() <= tol.parametric {
        1.0
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn seg(a: (f64, f64, f64), b: (f64, f64, f64)) -> Segment {
        Segment::new(DVec3::new(a.0, a.1, a.2), DVec3::new(b.0, b.1, b.2))
    }

    #[test]
    fn crossing_touching_and_missing() {
        let t = Tolerances::default();
        let a = seg((0.0, 0.0, 0.0), (10.0, 0.0, 0.0));
        match segment_segment(&a, &seg((4.0, -5.0, 0.0), (4.0, 5.0, 0.0)), &t) {
            SegmentContact::Point { t_a, t_b, point } => {
                assert!((t_a - 0.4).abs() < 1e-12 && (t_b - 0.5).abs() < 1e-12);
                assert!(point.distance(DVec3::new(4.0, 0.0, 0.0)) < 1e-12);
            }
            other => panic!("{other:?}"),
        }
        // T-junction touching at an endpoint of b.
        match segment_segment(&a, &seg((6.0, 0.0005, 0.0), (6.0, 5.0, 0.0)), &t) {
            SegmentContact::Point { t_a, t_b, .. } => {
                assert!((t_a - 0.6).abs() < 1e-12);
                assert_eq!(t_b, 0.0);
            }
            other => panic!("{other:?}"),
        }
        // Separated in z beyond tolerance.
        assert!(segment_segment(&a, &seg((4.0, -5.0, 0.01), (4.0, 5.0, 0.01)), &t).is_none());
        // Within tolerance in z: skew contact.
        assert!(!segment_segment(&a, &seg((4.0, -5.0, 0.0005), (4.0, 5.0, 0.0005)), &t).is_none());
        // Would cross if extended.
        assert!(segment_segment(&a, &seg((12.0, -5.0, 0.0), (12.0, 5.0, 0.0)), &t).is_none());
    }

    #[test]
    fn collinear_cases() {
        let t = Tolerances::default();
        let a = seg((0.0, 0.0, 0.0), (10.0, 0.0, 0.0));
        assert_eq!(
            segment_segment(&a, &seg((5.0, 0.0, 0.0), (15.0, 0.0, 0.0)), &t),
            SegmentContact::Overlap { t0: 0.5, t1: 1.0 }
        );
        assert_eq!(
            segment_segment(&a, &seg((15.0, 0.0, 0.0), (5.0, 0.0, 0.0)), &t),
            SegmentContact::Overlap { t0: 0.5, t1: 1.0 }
        );
        assert_eq!(
            segment_segment(&a, &seg((2.0, 0.0, 0.0), (3.0, 0.0, 0.0)), &t),
            SegmentContact::Overlap { t0: 0.2, t1: 0.3 }
        );
        assert_eq!(
            segment_segment(&a, &seg((-5.0, 0.0, 0.0), (20.0, 0.0, 0.0)), &t),
            SegmentContact::Overlap { t0: 0.0, t1: 1.0 }
        );
        // End-to-end touch.
        match segment_segment(&a, &seg((10.0, 0.0, 0.0), (20.0, 0.0, 0.0)), &t) {
            SegmentContact::Point { t_a, t_b, point } => {
                assert_eq!((t_a, t_b), (1.0, 0.0));
                assert!(point.distance(DVec3::new(10.0, 0.0, 0.0)) < 1e-12);
            }
            other => panic!("{other:?}"),
        }
        // Collinear, disjoint.
        assert!(segment_segment(&a, &seg((11.0, 0.0, 0.0), (20.0, 0.0, 0.0)), &t).is_none());
        // Parallel offset beyond tolerance: no contact.
        assert!(segment_segment(&a, &seg((2.0, 0.5, 0.0), (8.0, 0.5, 0.0)), &t).is_none());
        // Parallel offset within tolerance: overlap.
        assert!(matches!(
            segment_segment(&a, &seg((2.0, 0.0005, 0.0), (8.0, 0.0005, 0.0)), &t),
            SegmentContact::Overlap { .. }
        ));
    }

    #[test]
    fn proper_crossing_classification() {
        let t = Tolerances::default();
        let a = seg((0.0, 0.0, 0.0), (10.0, 0.0, 0.0));
        let cross = seg((4.0, -5.0, 0.0), (4.0, 5.0, 0.0));
        assert!(segment_segment(&a, &cross, &t).is_proper_crossing(&a, &cross, &t));
        let touch = seg((4.0, 0.0, 0.0), (4.0, 5.0, 0.0));
        assert!(!segment_segment(&a, &touch, &t).is_proper_crossing(&a, &touch, &t));
        assert!(!SegmentContact::None.is_proper_crossing(&a, &touch, &t));
    }

    #[test]
    fn segment_plane_cases() {
        let t = Tolerances::default();
        let (tt, p) =
            segment_plane(&seg((0.0, 0.0, -2.0), (0.0, 0.0, 8.0)), &Plane::XY, &t).unwrap();
        assert!((tt - 0.2).abs() < 1e-12);
        assert!(p.distance(DVec3::ZERO) < 1e-12);
        assert!(segment_plane(&seg((0.0, 0.0, 1.0), (0.0, 0.0, 8.0)), &Plane::XY, &t).is_none());
        assert!(
            segment_plane(&seg((0.0, 0.0, 0.001), (5.0, 0.0, 0.001)), &Plane::XY, &t).is_none()
        );
        assert_eq!(
            segment_plane(&seg((1.0, 2.0, 0.005), (0.0, 0.0, 8.0)), &Plane::XY, &t),
            Some((0.0, DVec3::new(1.0, 2.0, 0.005)))
        );
        assert_eq!(
            segment_plane(&seg((0.0, 0.0, 8.0), (1.0, 2.0, -0.005)), &Plane::XY, &t),
            Some((1.0, DVec3::new(1.0, 2.0, -0.005)))
        );
        let (tt, p) = ray_plane(&Ray::new(DVec3::Z * 3.0, -DVec3::Z), &Plane::XY, &t).unwrap();
        assert_eq!((tt, p), (3.0, DVec3::ZERO));
        assert!(
            ray_triangle(
                &Ray::new(DVec3::new(1.0, 1.0, 3.0), -DVec3::Z),
                DVec3::ZERO,
                DVec3::X * 5.0,
                DVec3::Y * 5.0,
                &t
            )
            .is_some()
        );
    }

    fn arb_vec(range: f64) -> impl Strategy<Value = DVec3> {
        (-range..range, -range..range, -range..range).prop_map(|(x, y, z)| DVec3::new(x, y, z))
    }

    proptest! {
        /// Two coplanar segments built to cross at a chosen interior point are
        /// reported as a `Point` there.
        #[test]
        fn constructed_crossings_are_found(
            x in arb_vec(1000.0),
            d1 in arb_vec(1.0), d2 in arb_vec(1.0),
            l in (1.0f64..500.0, 1.0f64..500.0, 1.0f64..500.0, 1.0f64..500.0),
        ) {
            let t = Tolerances::default();
            prop_assume!(d1.length() > 0.1 && d2.length() > 0.1);
            let d1 = d1.normalize();
            let d2 = d2.normalize();
            prop_assume!(d1.cross(d2).length() > 0.05);
            let a = Segment::new(x - d1 * l.0, x + d1 * l.1);
            let b = Segment::new(x - d2 * l.2, x + d2 * l.3);
            match segment_segment(&a, &b, &t) {
                SegmentContact::Point { t_a, t_b, point } => {
                    prop_assert!(point.distance(x) < 1e-6, "{point} vs {x}");
                    prop_assert!((a.point_at(t_a).distance(x)) < 1e-6);
                    prop_assert!((b.point_at(t_b).distance(x)) < 1e-6);
                }
                other => prop_assert!(false, "{other:?}"),
            }
            prop_assert!(segment_segment(&a, &b, &t).is_proper_crossing(&a, &b, &t));
            // Lifting b off the plane by more than the tolerance breaks contact.
            let lift = d1.cross(d2).normalize() * 0.01;
            let b_up = Segment::new(b.a + lift, b.b + lift);
            prop_assert!(segment_segment(&a, &b_up, &t).is_none());
        }

        /// Collinear sub-segments overlap on exactly the shared parameter range.
        #[test]
        fn collinear_overlap_range(
            a0 in arb_vec(1000.0), dir in arb_vec(1.0), len in 10.0f64..1000.0,
            r in (0.0f64..1.0, 0.0f64..1.0),
        ) {
            let t = Tolerances::default();
            prop_assume!(dir.length() > 0.1);
            let a = Segment::new(a0, a0 + dir.normalize() * len);
            let (lo, hi) = (r.0.min(r.1), r.0.max(r.1));
            prop_assume!((hi - lo) * len > 0.01);
            let b = Segment::new(a.point_at(lo), a.point_at(hi));
            match segment_segment(&a, &b, &t) {
                SegmentContact::Overlap { t0, t1 } => {
                    prop_assert!((t0 - lo).abs() < 1e-9 && (t1 - hi).abs() < 1e-9, "{t0},{t1} vs {lo},{hi}");
                }
                other => prop_assert!(false, "{other:?}"),
            }
        }

        /// A segment straddling a plane crosses it where its signed distance
        /// changes sign.
        #[test]
        fn plane_crossing(n in arb_vec(1.0), o in arb_vec(1000.0), p in arb_vec(1000.0), q in arb_vec(1000.0)) {
            prop_assume!(n.length() > 0.1);
            let t = Tolerances::default();
            let plane = Plane::from_point_normal(o, n);
            let (dp, dq) = (plane.signed_distance(p), plane.signed_distance(q));
            prop_assume!(dp.abs() > 0.1 && dq.abs() > 0.1);
            let s = Segment::new(p, q);
            match segment_plane(&s, &plane, &t) {
                Some((tt, point)) => {
                    prop_assert!((dp > 0.0) != (dq > 0.0));
                    prop_assert!(plane.distance(point) < 1e-6);
                    prop_assert!(s.point_at(tt).distance(point) < 1e-9);
                }
                None => prop_assert!((dp > 0.0) == (dq > 0.0)),
            }
        }
    }
}
