//! Implements the 2D polygon predicates of docs/03-geometry-kernel.md §4.2
//! step 3 (point in polygon, with holes per §4.5), §4.3 (signed area and loop
//! orientation in a plane frame) and §4.4 (polygon containment). All inputs are
//! closed rings given without a repeated first vertex, in a [`PlaneFrame`]'s
//! 2D coordinates.
//!
//! [`PlaneFrame`]: crate::math::PlaneFrame

#![warn(unused_variables, dead_code)]

use glam::{DVec2, DVec3};

use crate::math::Segment;
use crate::math::intersect::segment_segment;
use crate::tolerance::Tolerances;

/// Where a point lies relative to a ring or a polygon with holes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointInPolygon {
    /// Strictly inside, farther than `tol.length` from every edge.
    Inside,
    /// Within `tol.length` of an edge (outer or hole).
    Boundary,
    /// Outside (or inside a hole).
    Outside,
}

impl PointInPolygon {
    /// `true` for `Inside` and `Boundary`.
    pub fn is_inside_or_on(self) -> bool {
        !matches!(self, PointInPolygon::Outside)
    }
}

/// Shoelace signed area of a closed 2D ring (positive = counter-clockwise).
pub fn signed_area(ring: &[DVec2]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    // Relative to the first vertex to avoid cancellation far from the origin.
    let o = ring[0];
    let mut sum = 0.0;
    for i in 1..ring.len() {
        let a = ring[i] - o;
        let b = ring[(i + 1) % ring.len()] - o;
        sum += a.perp_dot(b);
    }
    sum * 0.5
}

/// Unsigned area of a closed ring.
pub fn area(ring: &[DVec2]) -> f64 {
    signed_area(ring).abs()
}

/// Area of a polygon with holes: outer area minus the holes' areas.
pub fn area_with_holes(outer: &[DVec2], holes: &[&[DVec2]]) -> f64 {
    holes.iter().fold(area(outer), |acc, hole| acc - area(hole))
}

/// `true` when the ring is counter-clockwise (positive area beyond
/// `tol.length²`); degenerate rings are not.
pub fn is_counter_clockwise(ring: &[DVec2], tol: &Tolerances) -> bool {
    signed_area(ring) > tol.length_sq
}

/// `true` when the ring's area is at most `tol.length²` (docs/03 §2).
pub fn is_degenerate(ring: &[DVec2], tol: &Tolerances) -> bool {
    area(ring) <= tol.length_sq
}

/// Area centroid of a ring, or the vertex mean when the ring is degenerate;
/// `None` for an empty ring.
pub fn centroid(ring: &[DVec2]) -> Option<DVec2> {
    if ring.is_empty() {
        return None;
    }
    let o = ring[0];
    let mut twice_area = 0.0;
    let mut acc = DVec2::ZERO;
    for i in 0..ring.len() {
        let a = ring[i] - o;
        let b = ring[(i + 1) % ring.len()] - o;
        let cross = a.perp_dot(b);
        twice_area += cross;
        acc += (a + b) * cross;
    }
    if twice_area.abs() > f64::EPSILON {
        Some(o + acc / (3.0 * twice_area))
    } else {
        let mean = ring.iter().fold(DVec2::ZERO, |s, &p| s + p) / ring.len() as f64;
        Some(mean)
    }
}

/// Perimeter of a closed ring.
pub fn perimeter(ring: &[DVec2]) -> f64 {
    (0..ring.len())
        .map(|i| ring[i].distance(ring[(i + 1) % ring.len()]))
        .sum()
}

/// Distance from `p` to the closest edge of the ring (`∞` for an empty ring).
pub fn distance_to_ring(p: DVec2, ring: &[DVec2]) -> f64 {
    if ring.is_empty() {
        return f64::INFINITY;
    }
    if ring.len() == 1 {
        return p.distance(ring[0]);
    }
    (0..ring.len())
        .map(|i| distance_to_segment_2d(p, ring[i], ring[(i + 1) % ring.len()]))
        .fold(f64::INFINITY, f64::min)
}

/// Point-in-ring test with the even-odd rule; points within `tol.length` of an
/// edge are `Boundary` (docs/03 §4.2 step 3). Works for any orientation and
/// for self-touching rings.
pub fn point_in_ring(p: DVec2, ring: &[DVec2], tol: &Tolerances) -> PointInPolygon {
    if ring.len() < 3 {
        return if distance_to_ring(p, ring) <= tol.length {
            PointInPolygon::Boundary
        } else {
            PointInPolygon::Outside
        };
    }
    if distance_to_ring(p, ring) <= tol.length {
        return PointInPolygon::Boundary;
    }
    // Crossing number of the horizontal ray towards +x. Half-open vertex rule
    // (`a.y > p.y != b.y > p.y`) counts each vertex crossing once.
    let mut inside = false;
    for i in 0..ring.len() {
        let a = ring[i];
        let b = ring[(i + 1) % ring.len()];
        if (a.y > p.y) != (b.y > p.y) {
            // x of the edge at height p.y; the division is safe because a.y != b.y.
            let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
            if p.x < x {
                inside = !inside;
            }
        }
    }
    if inside {
        PointInPolygon::Inside
    } else {
        PointInPolygon::Outside
    }
}

/// Point in a polygon with holes (docs/03 §4.5): `Boundary` when on any ring,
/// `Inside` when inside the outer ring and outside every hole, else `Outside`.
pub fn point_in_polygon(
    p: DVec2,
    outer: &[DVec2],
    holes: &[&[DVec2]],
    tol: &Tolerances,
) -> PointInPolygon {
    match point_in_ring(p, outer, tol) {
        PointInPolygon::Inside => {}
        other => return other,
    }
    for hole in holes {
        match point_in_ring(p, hole, tol) {
            PointInPolygon::Inside => return PointInPolygon::Outside,
            PointInPolygon::Boundary => return PointInPolygon::Boundary,
            PointInPolygon::Outside => {}
        }
    }
    PointInPolygon::Inside
}

/// `true` when ring `inner` lies entirely inside (or on) ring `outer`: every
/// vertex of `inner` is inside or on `outer`, and no edge of `inner` properly
/// crosses an edge of `outer` (docs/03 §4.4 hole assignment, §4.5 hole
/// detection). A ring contains itself.
pub fn ring_contains_ring(outer: &[DVec2], inner: &[DVec2], tol: &Tolerances) -> bool {
    if outer.len() < 3 || inner.is_empty() {
        return false;
    }
    if !inner
        .iter()
        .all(|&p| point_in_ring(p, outer, tol).is_inside_or_on())
    {
        return false;
    }
    if inner.len() < 2 {
        return true;
    }
    for i in 0..inner.len() {
        let ia = lift(inner[i]);
        let ib = lift(inner[(i + 1) % inner.len()]);
        let ie = Segment::new(ia, ib);
        for j in 0..outer.len() {
            let oe = Segment::new(lift(outer[j]), lift(outer[(j + 1) % outer.len()]));
            if segment_segment(&ie, &oe, tol).is_proper_crossing(&ie, &oe, tol) {
                return false;
            }
        }
    }
    true
}

/// `true` when two rings' edges properly cross anywhere (interiors of both
/// edges meet); touching at vertices or along collinear runs is not a crossing.
pub fn rings_cross(a: &[DVec2], b: &[DVec2], tol: &Tolerances) -> bool {
    if a.len() < 2 || b.len() < 2 {
        return false;
    }
    for i in 0..a.len() {
        let ea = Segment::new(lift(a[i]), lift(a[(i + 1) % a.len()]));
        for j in 0..b.len() {
            let eb = Segment::new(lift(b[j]), lift(b[(j + 1) % b.len()]));
            if segment_segment(&ea, &eb, tol).is_proper_crossing(&ea, &eb, tol) {
                return true;
            }
        }
    }
    false
}

/// `true` when the ring is convex (all turns have the same sign, ignoring
/// collinear runs within tolerance) and non-degenerate.
pub fn is_convex(ring: &[DVec2], tol: &Tolerances) -> bool {
    if ring.len() < 3 || is_degenerate(ring, tol) {
        return false;
    }
    let mut sign = 0.0f64;
    for i in 0..ring.len() {
        let a = ring[i];
        let b = ring[(i + 1) % ring.len()];
        let c = ring[(i + 2) % ring.len()];
        let cross = (b - a).perp_dot(c - b);
        if cross.abs() <= tol.length_sq {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// Reverses a ring in place so that its signed area is positive; returns
/// `true` when it was reversed.
pub fn make_counter_clockwise(ring: &mut [DVec2]) -> bool {
    if signed_area(ring) < 0.0 {
        ring.reverse();
        true
    } else {
        false
    }
}

/// Distance from `p` to the 2D segment `ab`.
pub fn distance_to_segment_2d(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let ab = b - a;
    let len_sq = ab.length_squared();
    if len_sq <= 0.0 {
        return p.distance(a);
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    (a + ab * t).distance(p)
}

fn lift(p: DVec2) -> DVec3 {
    DVec3::new(p.x, p.y, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn square(size: f64, at: DVec2) -> Vec<DVec2> {
        vec![
            at,
            at + DVec2::new(size, 0.0),
            at + DVec2::new(size, size),
            at + DVec2::new(0.0, size),
        ]
    }

    #[test]
    fn area_orientation_and_centroid() {
        let t = Tolerances::default();
        let ccw = square(10.0, DVec2::ZERO);
        assert_eq!(signed_area(&ccw), 100.0);
        assert!(is_counter_clockwise(&ccw, &t));
        let mut cw = ccw.clone();
        cw.reverse();
        assert_eq!(signed_area(&cw), -100.0);
        assert!(!is_counter_clockwise(&cw, &t));
        assert!(make_counter_clockwise(&mut cw));
        assert_eq!(signed_area(&cw), 100.0);
        assert!(!make_counter_clockwise(&mut cw));
        assert_eq!(centroid(&ccw), Some(DVec2::new(5.0, 5.0)));
        assert_eq!(centroid(&[]), None);
        assert_eq!(
            centroid(&[DVec2::ZERO, DVec2::new(2.0, 0.0)]),
            Some(DVec2::new(1.0, 0.0))
        );
        assert_eq!(perimeter(&ccw), 40.0);
        assert!(is_degenerate(&ccw[..2], &t));
        let hole = square(2.0, DVec2::new(4.0, 4.0));
        assert_eq!(area_with_holes(&ccw, &[&hole]), 96.0);
        // Far from the origin the area stays exact.
        let far = square(10.0, DVec2::splat(1e8));
        assert!((signed_area(&far) - 100.0).abs() < 1e-6);
    }

    #[test]
    fn point_in_ring_cases() {
        let t = Tolerances::default();
        let sq = square(10.0, DVec2::ZERO);
        assert_eq!(
            point_in_ring(DVec2::new(5.0, 5.0), &sq, &t),
            PointInPolygon::Inside
        );
        assert_eq!(
            point_in_ring(DVec2::new(15.0, 5.0), &sq, &t),
            PointInPolygon::Outside
        );
        assert_eq!(
            point_in_ring(DVec2::new(10.0, 5.0), &sq, &t),
            PointInPolygon::Boundary
        );
        assert_eq!(
            point_in_ring(DVec2::new(10.0005, 5.0), &sq, &t),
            PointInPolygon::Boundary
        );
        assert_eq!(
            point_in_ring(DVec2::new(0.0, 0.0), &sq, &t),
            PointInPolygon::Boundary
        );
        // Ray through a vertex height is counted once.
        assert_eq!(
            point_in_ring(DVec2::new(-5.0, 10.0), &sq, &t),
            PointInPolygon::Outside
        );
        assert_eq!(
            point_in_ring(DVec2::new(-5.0, 0.0), &sq, &t),
            PointInPolygon::Outside
        );
        // Orientation does not matter.
        let mut cw = sq.clone();
        cw.reverse();
        assert_eq!(
            point_in_ring(DVec2::new(5.0, 5.0), &cw, &t),
            PointInPolygon::Inside
        );
        // Concave ring (U shape).
        let u = vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(30.0, 0.0),
            DVec2::new(30.0, 30.0),
            DVec2::new(20.0, 30.0),
            DVec2::new(20.0, 10.0),
            DVec2::new(10.0, 10.0),
            DVec2::new(10.0, 30.0),
            DVec2::new(0.0, 30.0),
        ];
        assert_eq!(
            point_in_ring(DVec2::new(15.0, 20.0), &u, &t),
            PointInPolygon::Outside
        );
        assert_eq!(
            point_in_ring(DVec2::new(5.0, 20.0), &u, &t),
            PointInPolygon::Inside
        );
        assert_eq!(
            point_in_ring(DVec2::new(15.0, 5.0), &u, &t),
            PointInPolygon::Inside
        );
        // Degenerate rings.
        assert_eq!(point_in_ring(DVec2::ZERO, &[], &t), PointInPolygon::Outside);
        assert_eq!(
            point_in_ring(DVec2::ZERO, &[DVec2::ZERO], &t),
            PointInPolygon::Boundary
        );
        assert_eq!(
            point_in_ring(
                DVec2::new(1.0, 0.0),
                &[DVec2::ZERO, DVec2::new(2.0, 0.0)],
                &t
            ),
            PointInPolygon::Boundary
        );
    }

    #[test]
    fn point_in_polygon_with_holes() {
        let t = Tolerances::default();
        let outer = square(10.0, DVec2::ZERO);
        let hole = square(2.0, DVec2::new(4.0, 4.0));
        let holes: [&[DVec2]; 1] = [&hole];
        assert_eq!(
            point_in_polygon(DVec2::new(1.0, 1.0), &outer, &holes, &t),
            PointInPolygon::Inside
        );
        assert_eq!(
            point_in_polygon(DVec2::new(5.0, 5.0), &outer, &holes, &t),
            PointInPolygon::Outside
        );
        assert_eq!(
            point_in_polygon(DVec2::new(4.0, 5.0), &outer, &holes, &t),
            PointInPolygon::Boundary
        );
        assert_eq!(
            point_in_polygon(DVec2::new(10.0, 5.0), &outer, &holes, &t),
            PointInPolygon::Boundary
        );
        assert_eq!(
            point_in_polygon(DVec2::new(50.0, 5.0), &outer, &holes, &t),
            PointInPolygon::Outside
        );
        assert_eq!(
            point_in_polygon(DVec2::new(5.0, 5.0), &outer, &[], &t),
            PointInPolygon::Inside
        );
    }

    #[test]
    fn containment_and_crossing() {
        let t = Tolerances::default();
        let outer = square(10.0, DVec2::ZERO);
        let inner = square(2.0, DVec2::new(4.0, 4.0));
        let touching = square(5.0, DVec2::ZERO);
        let crossing = square(5.0, DVec2::new(7.0, 7.0));
        let outside = square(2.0, DVec2::new(20.0, 20.0));
        assert!(ring_contains_ring(&outer, &inner, &t));
        assert!(ring_contains_ring(&outer, &touching, &t));
        assert!(ring_contains_ring(&outer, &outer, &t));
        assert!(!ring_contains_ring(&inner, &outer, &t));
        assert!(!ring_contains_ring(&outer, &crossing, &t));
        assert!(!ring_contains_ring(&outer, &outside, &t));
        assert!(!ring_contains_ring(&outer[..2], &inner, &t));
        assert!(rings_cross(&outer, &crossing, &t));
        assert!(!rings_cross(&outer, &inner, &t));
        assert!(!rings_cross(&outer, &touching, &t));
        // A ring whose vertices are all inside but whose edges cut the outer
        // ring (a long thin spike through a concave outer ring).
        let u = vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(30.0, 0.0),
            DVec2::new(30.0, 30.0),
            DVec2::new(20.0, 30.0),
            DVec2::new(20.0, 10.0),
            DVec2::new(10.0, 10.0),
            DVec2::new(10.0, 30.0),
            DVec2::new(0.0, 30.0),
        ];
        let bridge = vec![
            DVec2::new(5.0, 20.0),
            DVec2::new(25.0, 20.0),
            DVec2::new(25.0, 25.0),
            DVec2::new(5.0, 25.0),
        ];
        assert!(!ring_contains_ring(&u, &bridge, &t));
    }

    #[test]
    fn convexity() {
        let t = Tolerances::default();
        assert!(is_convex(&square(10.0, DVec2::ZERO), &t));
        let mut cw = square(10.0, DVec2::ZERO);
        cw.reverse();
        assert!(is_convex(&cw, &t));
        // Collinear midpoint on an edge is still convex.
        let with_mid = vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(5.0, 0.0),
            DVec2::new(10.0, 0.0),
            DVec2::new(10.0, 10.0),
            DVec2::new(0.0, 10.0),
        ];
        assert!(is_convex(&with_mid, &t));
        let concave = vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(10.0, 0.0),
            DVec2::new(5.0, 5.0),
            DVec2::new(10.0, 10.0),
            DVec2::new(0.0, 10.0),
        ];
        assert!(!is_convex(&concave, &t));
        assert!(!is_convex(&with_mid[..2], &t));
    }

    fn arb_pt(range: f64) -> impl Strategy<Value = DVec2> {
        (-range..range, -range..range).prop_map(|(x, y)| DVec2::new(x, y))
    }

    /// A random convex polygon: points on an ellipse at sorted angles.
    fn arb_convex() -> impl Strategy<Value = Vec<DVec2>> {
        (
            prop::collection::vec(0.0f64..std::f64::consts::TAU, 3..12),
            1.0f64..500.0,
            1.0f64..500.0,
            arb_pt(1000.0),
        )
            .prop_map(|(mut angles, rx, ry, c)| {
                angles.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                angles.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
                angles
                    .iter()
                    .map(|a| c + DVec2::new(a.cos() * rx, a.sin() * ry))
                    .collect()
            })
            .prop_filter("non-degenerate", |ring: &Vec<DVec2>| {
                ring.len() >= 3 && area(ring) > 1.0
            })
    }

    proptest! {
        /// Convex combinations of the vertices of a convex ring are inside or
        /// on it; the centroid is strictly inside; a point pushed out past the
        /// farthest vertex is outside.
        #[test]
        fn convex_membership(ring in arb_convex(), w in prop::collection::vec(0.0f64..1.0, 12), far in 1.0f64..10.0) {
            let t = Tolerances::default();
            let n = ring.len();
            let total: f64 = w[..n].iter().sum();
            prop_assume!(total > 1e-6);
            let p = ring.iter().zip(&w[..n]).fold(DVec2::ZERO, |acc, (&v, &wi)| acc + v * (wi / total));
            prop_assert!(point_in_ring(p, &ring, &t).is_inside_or_on());
            let c = centroid(&ring).unwrap();
            prop_assert_eq!(point_in_ring(c, &ring, &t), PointInPolygon::Inside);
            let radius = ring.iter().map(|v| v.distance(c)).fold(0.0, f64::max);
            let outside = c + DVec2::new(radius * (1.0 + far), 0.0);
            prop_assert_eq!(point_in_ring(outside, &ring, &t), PointInPolygon::Outside);
            prop_assert!(is_convex(&ring, &t));
            prop_assert!(is_counter_clockwise(&ring, &t));
            // Shrinking the ring about its centroid gives a contained ring.
            let shrunk: Vec<DVec2> = ring.iter().map(|&v| c + (v - c) * 0.5).collect();
            prop_assert!(ring_contains_ring(&ring, &shrunk, &t));
            prop_assert!(!ring_contains_ring(&shrunk, &ring, &t));
            prop_assert!(!rings_cross(&ring, &shrunk, &t));
            // Signed area is invariant under translation and flips with reversal.
            let shift = DVec2::new(123.0, -456.0);
            let moved: Vec<DVec2> = ring.iter().map(|&v| v + shift).collect();
            prop_assert!((signed_area(&moved) - signed_area(&ring)).abs() < 1e-6 * area(&ring));
            let mut rev = ring.clone();
            rev.reverse();
            prop_assert!((signed_area(&rev) + signed_area(&ring)).abs() < 1e-9 * area(&ring).max(1.0));
        }

        /// Every edge midpoint is on the boundary, and a point in the hole is
        /// outside the polygon with holes.
        #[test]
        fn boundary_and_holes(ring in arb_convex(), i in 0usize..12) {
            let t = Tolerances::default();
            let n = ring.len();
            let mid = (ring[i % n] + ring[(i + 1) % n]) * 0.5;
            prop_assert_eq!(point_in_ring(mid, &ring, &t), PointInPolygon::Boundary);
            let c = centroid(&ring).unwrap();
            let hole: Vec<DVec2> = ring.iter().map(|&v| c + (v - c) * 0.25).collect();
            let holes: [&[DVec2]; 1] = [&hole];
            prop_assert_eq!(point_in_polygon(c, &ring, &holes, &t), PointInPolygon::Outside);
            let between = c + (ring[i % n] - c) * 0.6;
            prop_assert_eq!(point_in_polygon(between, &ring, &holes, &t), PointInPolygon::Inside);
        }
    }
}
