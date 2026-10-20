//! Implements ray utilities for picking and inference (docs/03 §6–§7,
//! docs/06 §8).

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

impl Ray {
    /// Builds a ray, normalizing `dir` (a zero direction stays zero).
    pub fn new(origin: DVec3, dir: DVec3) -> Self {
        Ray {
            origin,
            dir: dir.normalize_or_zero(),
        }
    }

    /// Point at parameter `t`.
    pub fn at(&self, t: f64) -> DVec3 {
        self.origin + self.dir * t
    }

    /// Parameter of the ray point closest to `p` (clamped to `t ≥ 0`).
    pub fn closest_param(&self, p: DVec3) -> f64 {
        (p - self.origin).dot(self.dir).max(0.0)
    }

    /// Intersection parameter with a plane, or `None` when the ray is parallel
    /// within `tol.angle` or the hit is behind the origin (docs/03 §7 depth
    /// resolution).
    pub fn intersect_plane(&self, plane: &Plane, tol: &Tolerances) -> Option<f64> {
        todo!("docs/03 §7 ray–plane intersection")
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

    #[test]
    fn at_and_closest_param() {
        let r = Ray::new(DVec3::ZERO, DVec3::new(0.0, 0.0, 2.0));
        assert_eq!(r.at(3.0), DVec3::new(0.0, 0.0, 3.0));
        assert_eq!(r.closest_param(DVec3::new(5.0, 0.0, 4.0)), 4.0);
        assert_eq!(r.closest_param(DVec3::new(0.0, 0.0, -4.0)), 0.0);
    }
}
