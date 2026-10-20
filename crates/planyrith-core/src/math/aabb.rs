//! Implements axis-aligned bounding boxes used by spatial indexes (docs/03 §6),
//! render-mesh extraction (docs/06 §2) and `query --fields bbox` (docs/08 §2).

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

    /// Center point (undefined for the empty box).
    pub fn center(&self) -> DVec3 {
        (self.min + self.max) * 0.5
    }

    /// Full extent per axis.
    pub fn size(&self) -> DVec3 {
        self.max - self.min
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
    fn transform_translates_corners() {
        let b = Aabb::new(DVec3::ZERO, DVec3::ONE);
        let t = DAffine3::from_translation(DVec3::new(10.0, 0.0, 0.0));
        assert_eq!(
            b.transformed(&t),
            Aabb::new(DVec3::new(10.0, 0.0, 0.0), DVec3::new(11.0, 1.0, 1.0))
        );
    }
}
