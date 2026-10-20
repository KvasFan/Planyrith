//! Implements ray utilities for picking and inference (docs/03 §6–§7,
//! docs/06 §8): ray–plane depth resolution and the Möller–Trumbore
//! ray–triangle test used to confirm face hits on the CPU.

#![warn(unused_variables, dead_code)]

use glam::{DAffine3, DVec3};
use serde::{Deserialize, Serialize};

use crate::math::Plane;
use crate::tolerance::Tolerances;

/// A ray `origin + t · dir`, `t ≥ 0`, with a unit direction.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Ray {
    /// Start point.
    pub origin: DVec3,
    /// Unit direction.
    pub dir: DVec3,
}

/// A ray hit on a triangle (docs/06 §8 exact confirmation).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayTriangleHit {
    /// Ray parameter of the hit.
    pub t: f64,
    /// Barycentric weight of the second vertex (`b`).
    pub u: f64,
    /// Barycentric weight of the third vertex (`c`).
    pub v: f64,
    /// The hit point (`ray.at(t)`).
    pub point: DVec3,
    /// `true` when the ray enters through the front side (`(b-a) × (c-a)`
    /// points against the ray).
    pub front_facing: bool,
}

impl Ray {
    /// Builds a ray, normalizing `dir` (a zero direction stays zero).
    pub fn new(origin: DVec3, dir: DVec3) -> Self {
        Ray {
            origin,
            dir: dir.normalize_or_zero(),
        }
    }

    /// Ray from `from` towards `to` (zero direction when they coincide).
    pub fn between(from: DVec3, to: DVec3) -> Self {
        Ray::new(from, to - from)
    }

    /// Point at parameter `t`.
    pub fn at(&self, t: f64) -> DVec3 {
        self.origin + self.dir * t
    }

    /// Parameter of the ray point closest to `p` (clamped to `t ≥ 0`).
    pub fn closest_param(&self, p: DVec3) -> f64 {
        (p - self.origin).dot(self.dir).max(0.0)
    }

    /// The ray point closest to `p`.
    pub fn closest_point(&self, p: DVec3) -> DVec3 {
        self.at(self.closest_param(p))
    }

    /// Distance from `p` to the ray.
    pub fn distance_to_point(&self, p: DVec3) -> f64 {
        self.closest_point(p).distance(p)
    }

    /// Intersection parameter with a plane, or `None` when the ray is parallel
    /// within `tol.angle` or the hit is behind the origin (docs/03 §7 depth
    /// resolution). A hit within `tol.length` behind the origin is clamped to
    /// `t = 0`.
    pub fn intersect_plane(&self, plane: &Plane, tol: &Tolerances) -> Option<f64> {
        let denom = self.dir.dot(plane.normal);
        // dir and normal are unit: |denom| is the sine of the angle between the
        // ray and the plane.
        if denom.abs() <= tol.angle {
            return None;
        }
        let t = -plane.signed_distance(self.origin) / denom;
        if !t.is_finite() || t < -tol.length {
            return None;
        }
        Some(t.max(0.0))
    }

    /// Intersection point with a plane; see [`Ray::intersect_plane`].
    pub fn hit_plane(&self, plane: &Plane, tol: &Tolerances) -> Option<DVec3> {
        self.intersect_plane(plane, tol).map(|t| self.at(t))
    }

    /// Möller–Trumbore ray–triangle intersection. Hits within `tol.length` of
    /// the triangle's edges count (so shared edges never leave a gap), rays
    /// parallel to the triangle within `tol.angle` miss, and hits more than
    /// `tol.length` behind the origin miss.
    pub fn intersect_triangle(
        &self,
        a: DVec3,
        b: DVec3,
        c: DVec3,
        tol: &Tolerances,
    ) -> Option<RayTriangleHit> {
        let e1 = b - a;
        let e2 = c - a;
        let pvec = self.dir.cross(e2);
        let det = e1.dot(pvec);
        // det = dir · (e2 × e1): |det| / |e1 × e2| is the sine of the angle
        // between the ray and the triangle plane.
        let area2 = e1.cross(e2).length();
        if area2 <= 0.0 || det.abs() <= tol.angle * area2 {
            return None;
        }
        let inv_det = 1.0 / det;
        let tvec = self.origin - a;
        let u = tvec.dot(pvec) * inv_det;
        let qvec = tvec.cross(e1);
        let v = self.dir.dot(qvec) * inv_det;
        // Barycentric slack equivalent to `tol.length` in the plane: one unit of
        // u or v spans the longest edge at most, so dividing by the shortest
        // relevant extent keeps the band conservative.
        let extent = e1.length().min(e2.length()).min((c - b).length());
        if extent <= 0.0 {
            return None;
        }
        let eps = tol.length / extent;
        if u < -eps || v < -eps || u + v > 1.0 + eps {
            return None;
        }
        let t = e2.dot(qvec) * inv_det;
        if !t.is_finite() || t < -tol.length {
            return None;
        }
        let t = t.max(0.0);
        Some(RayTriangleHit {
            t,
            u,
            v,
            point: self.at(t),
            front_facing: det > 0.0,
        })
    }

    /// The ray transformed by `t` (direction re-normalized; parameters are not
    /// preserved under scaling — callers transform hit points back, docs/03 §6).
    pub fn transformed(&self, t: &DAffine3) -> Self {
        Ray::new(
            t.transform_point3(self.origin),
            t.transform_vector3(self.dir),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn at_and_closest_param() {
        let r = Ray::new(DVec3::ZERO, DVec3::new(0.0, 0.0, 2.0));
        assert_eq!(r.at(3.0), DVec3::new(0.0, 0.0, 3.0));
        assert_eq!(r.closest_param(DVec3::new(5.0, 0.0, 4.0)), 4.0);
        assert_eq!(r.closest_param(DVec3::new(0.0, 0.0, -4.0)), 0.0);
        assert_eq!(r.distance_to_point(DVec3::new(3.0, 4.0, 7.0)), 5.0);
        assert_eq!(Ray::between(DVec3::ZERO, DVec3::X * 9.0).dir, DVec3::X);
    }

    #[test]
    fn ray_plane_cases() {
        let t = Tolerances::default();
        let down = Ray::new(DVec3::new(1.0, 2.0, 10.0), -DVec3::Z);
        assert!((down.intersect_plane(&Plane::XY, &t).unwrap() - 10.0).abs() < 1e-12);
        assert!(
            down.hit_plane(&Plane::XY, &t)
                .unwrap()
                .distance(DVec3::new(1.0, 2.0, 0.0))
                < 1e-12
        );
        // Looking up misses the ground (docs/03 §7).
        let up = Ray::new(DVec3::new(1.0, 2.0, 10.0), DVec3::Z);
        assert_eq!(up.intersect_plane(&Plane::XY, &t), None);
        // Parallel misses.
        let along = Ray::new(DVec3::new(0.0, 0.0, 1.0), DVec3::X);
        assert_eq!(along.intersect_plane(&Plane::XY, &t), None);
        // Starting on the plane hits at t = 0 even slightly behind.
        let on = Ray::new(DVec3::new(0.0, 0.0, -0.0005), -DVec3::Z);
        assert_eq!(on.intersect_plane(&Plane::XY, &t), Some(0.0));
    }

    #[test]
    fn ray_triangle_cases() {
        let t = Tolerances::default();
        let (a, b, c) = (DVec3::ZERO, DVec3::X * 10.0, DVec3::Y * 10.0);
        let hit = Ray::new(DVec3::new(2.0, 3.0, 5.0), -DVec3::Z)
            .intersect_triangle(a, b, c, &t)
            .unwrap();
        assert!((hit.t - 5.0).abs() < 1e-12);
        assert!(hit.point.distance(DVec3::new(2.0, 3.0, 0.0)) < 1e-12);
        assert!((hit.u - 0.2).abs() < 1e-12 && (hit.v - 0.3).abs() < 1e-12);
        assert!(hit.front_facing);
        // From below: back facing.
        let back = Ray::new(DVec3::new(2.0, 3.0, -5.0), DVec3::Z)
            .intersect_triangle(a, b, c, &t)
            .unwrap();
        assert!(!back.front_facing);
        // Outside the triangle.
        assert!(
            Ray::new(DVec3::new(8.0, 8.0, 5.0), -DVec3::Z)
                .intersect_triangle(a, b, c, &t)
                .is_none()
        );
        // Just outside an edge, within tolerance: counts.
        assert!(
            Ray::new(DVec3::new(5.0, -0.0005, 5.0), -DVec3::Z)
                .intersect_triangle(a, b, c, &t)
                .is_some()
        );
        assert!(
            Ray::new(DVec3::new(5.0, -0.01, 5.0), -DVec3::Z)
                .intersect_triangle(a, b, c, &t)
                .is_none()
        );
        // Behind the origin and parallel.
        assert!(
            Ray::new(DVec3::new(2.0, 3.0, 5.0), DVec3::Z)
                .intersect_triangle(a, b, c, &t)
                .is_none()
        );
        assert!(
            Ray::new(DVec3::new(-1.0, 3.0, 0.0), DVec3::X)
                .intersect_triangle(a, b, c, &t)
                .is_none()
        );
        // Degenerate triangle.
        assert!(
            Ray::new(DVec3::new(2.0, 0.0, 5.0), -DVec3::Z)
                .intersect_triangle(a, b, b, &t)
                .is_none()
        );
    }

    fn arb_vec(range: f64) -> impl Strategy<Value = DVec3> {
        (-range..range, -range..range, -range..range).prop_map(|(x, y, z)| DVec3::new(x, y, z))
    }

    proptest! {
        /// A ray aimed at a known barycentric point of a non-degenerate
        /// triangle hits it there.
        #[test]
        fn triangle_hit_recovers_barycentrics(
            a in arb_vec(1000.0), b in arb_vec(1000.0), c in arb_vec(1000.0),
            w in (0.0f64..1.0, 0.0f64..1.0),
            eye in arb_vec(5000.0),
        ) {
            let t = Tolerances::default();
            let area2 = (b - a).cross(c - a).length();
            prop_assume!(area2 > 1.0);
            let (u, v) = if w.0 + w.1 <= 1.0 { w } else { (1.0 - w.0, 1.0 - w.1) };
            let target = a + (b - a) * u + (c - a) * v;
            let dist = eye.distance(target);
            prop_assume!(dist > 1.0);
            let ray = Ray::between(eye, target);
            let normal = (b - a).cross(c - a) / area2;
            prop_assume!(ray.dir.dot(normal).abs() > 1e-3);
            let hit = ray.intersect_triangle(a, b, c, &t).expect("hit");
            prop_assert!((hit.t - dist).abs() < 1e-6 * dist.max(1.0), "t {} vs {dist}", hit.t);
            prop_assert!(hit.point.distance(target) < 1e-6 * dist.max(1.0));
            prop_assert!((hit.u - u).abs() < 1e-6 && (hit.v - v).abs() < 1e-6);
            prop_assert_eq!(hit.front_facing, ray.dir.dot(normal) < 0.0);
            // The plane hit agrees with the triangle hit.
            let plane = Plane::from_points(a, b, c, &t).expect("plane");
            let tp = ray.intersect_plane(&plane, &t).expect("plane hit");
            prop_assert!((tp - hit.t).abs() < 1e-6 * dist.max(1.0));
        }

        /// A ray from a point towards its plane projection hits the plane at
        /// exactly that distance; the reversed ray misses.
        #[test]
        fn plane_hit_distance(n in arb_vec(1.0), origin in arb_vec(1000.0), eye in arb_vec(1000.0)) {
            prop_assume!(n.length() > 0.1);
            let t = Tolerances::default();
            let plane = Plane::from_point_normal(origin, n);
            let d = plane.signed_distance(eye);
            prop_assume!(d.abs() > 1.0);
            let ray = Ray::new(eye, -plane.normal * d.signum());
            let hit = ray.intersect_plane(&plane, &t).expect("hit");
            prop_assert!((hit - d.abs()).abs() < 1e-9 * d.abs().max(1.0));
            prop_assert_eq!(Ray::new(eye, plane.normal * d.signum()).intersect_plane(&plane, &t), None);
        }
    }
}
