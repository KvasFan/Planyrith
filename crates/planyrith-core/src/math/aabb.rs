//! Implements axis-aligned bounding boxes used by spatial indexes (docs/03 §6),
//! render-mesh extraction (docs/06 §2) and `query --fields bbox` (docs/08 §2).

#![warn(unused_variables, dead_code)]

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::math::Ray;
use crate::tolerance::Tolerances;

/// An axis-aligned box. The empty box has `min = +∞`, `max = -∞`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Aabb {
    /// Minimum corner.
    #[schemars(with = "[f64; 3]")]
    pub min: DVec3,
    /// Maximum corner.
    #[schemars(with = "[f64; 3]")]
    pub max: DVec3,
}

impl Default for Aabb {
    fn default() -> Self {
        Aabb::EMPTY
    }
}

impl Aabb {
    /// The empty box (identity for [`Aabb::union`]).
    pub const EMPTY: Aabb = Aabb {
        min: DVec3::INFINITY,
        max: DVec3::NEG_INFINITY,
    };

    /// Box from explicit corners (not validated).
    pub const fn new(min: DVec3, max: DVec3) -> Self {
        Aabb { min, max }
    }

    /// Degenerate box around one point.
    pub const fn from_point(p: DVec3) -> Self {
        Aabb { min: p, max: p }
    }

    /// Box centered at `center` with full extents `size` (negative components
    /// are treated as zero).
    pub fn from_center_size(center: DVec3, size: DVec3) -> Self {
        let half = size.max(DVec3::ZERO) * 0.5;
        Aabb {
            min: center - half,
            max: center + half,
        }
    }

    /// Smallest box containing all points.
    pub fn from_points<I: IntoIterator<Item = DVec3>>(points: I) -> Self {
        points.into_iter().fold(Aabb::EMPTY, |b, p| b.including(p))
    }

    /// `true` when no point has been included.
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    /// Box grown to include `p`.
    pub fn including(&self, p: DVec3) -> Self {
        Aabb {
            min: self.min.min(p),
            max: self.max.max(p),
        }
    }

    /// Union of two boxes.
    pub fn union(&self, other: &Aabb) -> Self {
        Aabb {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    /// Box grown by `by` on every side (tolerance expansion, docs/03 §4.2).
    pub fn expanded(&self, by: f64) -> Self {
        Aabb {
            min: self.min - DVec3::splat(by),
            max: self.max + DVec3::splat(by),
        }
    }

    /// `true` when `p` is inside or on the boundary.
    pub fn contains_point(&self, p: DVec3) -> bool {
        p.cmpge(self.min).all() && p.cmple(self.max).all()
    }

    /// `true` when the boxes overlap or touch.
    pub fn intersects(&self, other: &Aabb) -> bool {
        self.min.cmple(other.max).all() && other.min.cmple(self.max).all()
    }

    /// Intersection of two boxes, or `None` when they do not overlap or touch.
    pub fn intersection(&self, other: &Aabb) -> Option<Aabb> {
        let b = Aabb {
            min: self.min.max(other.min),
            max: self.max.min(other.max),
        };
        (!b.is_empty()).then_some(b)
    }

    /// `true` when `other` lies entirely inside this box (the empty box is
    /// contained in every box).
    pub fn contains_aabb(&self, other: &Aabb) -> bool {
        other.is_empty()
            || (!self.is_empty()
                && other.min.cmpge(self.min).all()
                && other.max.cmple(self.max).all())
    }

    /// Center point (undefined for the empty box).
    pub fn center(&self) -> DVec3 {
        (self.min + self.max) * 0.5
    }

    /// Full extent per axis.
    pub fn size(&self) -> DVec3 {
        self.max - self.min
    }

    /// Length of the diagonal (`0` for the empty box).
    pub fn diagonal(&self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            self.size().length()
        }
    }

    /// Volume (`0` for the empty box).
    pub fn volume(&self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            let s = self.size();
            s.x * s.y * s.z
        }
    }

    /// Index of the longest axis (`0`, `1`, `2`; ties resolved X, Y, Z).
    pub fn longest_axis(&self) -> usize {
        let s = self.size();
        if s.x >= s.y && s.x >= s.z {
            0
        } else if s.y >= s.z {
            1
        } else {
            2
        }
    }

    /// Squared distance from `p` to the box (`0` inside; `∞` for the empty box).
    pub fn distance_squared_to_point(&self, p: DVec3) -> f64 {
        if self.is_empty() {
            return f64::INFINITY;
        }
        let d = (self.min - p).max(DVec3::ZERO) + (p - self.max).max(DVec3::ZERO);
        d.length_squared()
    }

    /// Distance from `p` to the box (`0` inside; `∞` for the empty box).
    pub fn distance_to_point(&self, p: DVec3) -> f64 {
        self.distance_squared_to_point(p).sqrt()
    }

    /// Slab test: the parameter range `(t_enter, t_exit)` along which the ray
    /// is inside the box grown by `tol.length`, with `t_exit ≥ max(t_enter, 0)`;
    /// `None` when the ray misses (docs/03 §6 broad phase).
    pub fn intersect_ray(&self, ray: &Ray, tol: &Tolerances) -> Option<(f64, f64)> {
        if self.is_empty() {
            return None;
        }
        let grown = self.expanded(tol.length);
        let mut t_enter = 0.0f64;
        let mut t_exit = f64::INFINITY;
        for i in 0..3 {
            let o = ray.origin[i];
            let d = ray.dir[i];
            if d.abs() <= f64::MIN_POSITIVE {
                if o < grown.min[i] || o > grown.max[i] {
                    return None;
                }
                continue;
            }
            let inv = 1.0 / d;
            let t0 = (grown.min[i] - o) * inv;
            let t1 = (grown.max[i] - o) * inv;
            let (near, far) = if t0 <= t1 { (t0, t1) } else { (t1, t0) };
            t_enter = t_enter.max(near);
            t_exit = t_exit.min(far);
            if t_enter > t_exit {
                return None;
            }
        }
        Some((t_enter, t_exit))
    }

    /// The eight corners in a fixed order (x fastest, then y, then z).
    pub fn corners(&self) -> [DVec3; 8] {
        let (a, b) = (self.min, self.max);
        [
            DVec3::new(a.x, a.y, a.z),
            DVec3::new(b.x, a.y, a.z),
            DVec3::new(a.x, b.y, a.z),
            DVec3::new(b.x, b.y, a.z),
            DVec3::new(a.x, a.y, b.z),
            DVec3::new(b.x, a.y, b.z),
            DVec3::new(a.x, b.y, b.z),
            DVec3::new(b.x, b.y, b.z),
        ]
    }

    /// Bounding box of the transformed corners (conservative under rotation).
    pub fn transformed(&self, t: &DAffine3) -> Self {
        if self.is_empty() {
            return Aabb::EMPTY;
        }
        Aabb::from_points(self.corners().into_iter().map(|c| t.transform_point3(c)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_union_and_containment() {
        let b = Aabb::from_points([DVec3::new(1.0, 2.0, 3.0), DVec3::new(-1.0, 0.0, 5.0)]);
        assert!(!b.is_empty());
        assert!(Aabb::EMPTY.is_empty());
        assert_eq!(Aabb::EMPTY.union(&b), b);
        assert!(b.contains_point(DVec3::new(0.0, 1.0, 4.0)));
        assert!(!b.contains_point(DVec3::new(2.0, 1.0, 4.0)));
        assert_eq!(b.size(), DVec3::new(2.0, 2.0, 2.0));
        assert!(b.intersects(&b.expanded(1.0)));
    }

    #[test]
    fn extra_measures_and_queries() {
        let t = Tolerances::default();
        let b = Aabb::from_center_size(DVec3::new(5.0, 5.0, 5.0), DVec3::splat(10.0));
        assert_eq!(b, Aabb::new(DVec3::ZERO, DVec3::splat(10.0)));
        assert_eq!(b.volume(), 1000.0);
        assert!((b.diagonal() - 300f64.sqrt()).abs() < 1e-12);
        assert_eq!(Aabb::EMPTY.volume(), 0.0);
        assert_eq!(Aabb::EMPTY.diagonal(), 0.0);
        assert_eq!(
            Aabb::new(DVec3::ZERO, DVec3::new(1.0, 5.0, 3.0)).longest_axis(),
            1
        );
        assert_eq!(b.distance_to_point(DVec3::new(13.0, 5.0, 14.0)), 5.0);
        assert_eq!(b.distance_to_point(DVec3::splat(5.0)), 0.0);
        assert_eq!(Aabb::EMPTY.distance_to_point(DVec3::ZERO), f64::INFINITY);
        let other = Aabb::new(DVec3::splat(5.0), DVec3::splat(20.0));
        assert_eq!(
            b.intersection(&other),
            Some(Aabb::new(DVec3::splat(5.0), DVec3::splat(10.0)))
        );
        assert_eq!(
            b.intersection(&Aabb::new(DVec3::splat(11.0), DVec3::splat(12.0))),
            None
        );
        assert!(b.contains_aabb(&Aabb::new(DVec3::ONE, DVec3::splat(9.0))));
        assert!(!b.contains_aabb(&other));
        assert!(b.contains_aabb(&Aabb::EMPTY));
        assert!(!Aabb::EMPTY.contains_aabb(&b));
        // Rays.
        let hit = b
            .intersect_ray(&Ray::new(DVec3::new(5.0, 5.0, 20.0), -DVec3::Z), &t)
            .unwrap();
        assert!((hit.0 - (10.0 - t.length)).abs() < 1e-12);
        assert!((hit.1 - (20.0 + t.length)).abs() < 1e-12);
        assert!(
            b.intersect_ray(&Ray::new(DVec3::new(5.0, 5.0, 20.0), DVec3::Z), &t)
                .is_none()
        );
        assert!(
            b.intersect_ray(&Ray::new(DVec3::new(15.0, 5.0, 20.0), -DVec3::Z), &t)
                .is_none()
        );
        // Ray starting inside: enters at 0.
        let inside = b
            .intersect_ray(&Ray::new(DVec3::splat(5.0), DVec3::X), &t)
            .unwrap();
        assert_eq!(inside.0, 0.0);
        assert!((inside.1 - (5.0 + t.length)).abs() < 1e-12);
        // Axis-parallel ray with a zero component on the box face plane.
        assert!(
            b.intersect_ray(&Ray::new(DVec3::new(0.0, 5.0, -5.0), DVec3::Z), &t)
                .is_some()
        );
        assert!(
            b.intersect_ray(&Ray::new(DVec3::new(-1.0, 5.0, -5.0), DVec3::Z), &t)
                .is_none()
        );
        assert!(
            Aabb::EMPTY
                .intersect_ray(&Ray::new(DVec3::ZERO, DVec3::X), &t)
                .is_none()
        );
    }

    #[test]
    fn transform_translates_corners() {
        let b = Aabb::new(DVec3::ZERO, DVec3::ONE);
        let t = DAffine3::from_translation(DVec3::new(10.0, 0.0, 0.0));
        assert_eq!(
            b.transformed(&t),
            Aabb::new(DVec3::new(10.0, 0.0, 0.0), DVec3::new(11.0, 1.0, 1.0))
        );
    }
}
