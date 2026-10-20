//! Implements the type bridge of docs/03-geometry-kernel.md §6 between the
//! kernel's `glam` types (`math::Aabb`, `math::Ray`) and the bounding volumes
//! of `parry3d-f64`'s `partitioning::Bvh`, plus the ray/box slab test used to
//! prune BVH traversals.
//!
//! `parry3d-f64` 0.31 is built on `glam` (through `glamx`), so its `Vector` is
//! the kernel's `DVec3` and the conversions below are field renames; keeping
//! them in one place isolates the rest of the module from the partitioning
//! crate's types (docs/02 §4 lockstep note on `glam`).
//!
//! Owner key: `spatial`.

use parry3d_f64::bounding_volume::Aabb as ParryAabb;

use crate::math::{Aabb, Ray};

/// A kernel box as a `parry3d-f64` box.
pub(super) fn to_parry_aabb(aabb: &Aabb) -> ParryAabb {
    ParryAabb::new(aabb.min, aabb.max)
}

/// A `parry3d-f64` box as a kernel box.
pub(super) fn from_parry_aabb(aabb: &ParryAabb) -> Aabb {
    Aabb::new(aabb.mins, aabb.maxs)
}

/// Direction components with a smaller magnitude are treated as parallel to
/// the slab in [`ray_aabb_entry`] (avoids `0 / 0` for a ray lying on a face).
const PARALLEL_DIR: f64 = 1e-300;

/// Slab test of a ray against a box: the entry parameter in `[0, max_t]`, or
/// `None` when the ray misses the box, starts past it, or exits before `t = 0`.
/// A ray starting inside the box enters at `t = 0`.
pub(super) fn ray_aabb_entry(ray: &Ray, aabb: &Aabb, max_t: f64) -> Option<f64> {
    if aabb.is_empty() {
        return None;
    }
    let mut t_enter = 0.0_f64;
    let mut t_exit = max_t;
    for axis in 0..3 {
        let origin = ray.origin[axis];
        let dir = ray.dir[axis];
        let (lo, hi) = (aabb.min[axis], aabb.max[axis]);
        if dir.abs() < PARALLEL_DIR {
            if origin < lo || origin > hi {
                return None;
            }
            continue;
        }
        let inv = 1.0 / dir;
        let mut t1 = (lo - origin) * inv;
        let mut t2 = (hi - origin) * inv;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        t_enter = t_enter.max(t1);
        t_exit = t_exit.min(t2);
        if t_enter > t_exit {
            return None;
        }
    }
    Some(t_enter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec3;

    #[test]
    fn aabb_round_trips_through_parry() {
        let a = Aabb::new(DVec3::new(-1.0, 2.0, -3.0), DVec3::new(4.0, 5.0, 6.0));
        assert_eq!(from_parry_aabb(&to_parry_aabb(&a)), a);
    }

    #[test]
    fn slab_test_reports_entry_and_misses() {
        let b = Aabb::new(DVec3::ZERO, DVec3::splat(10.0));
        let hit = Ray::new(DVec3::new(-5.0, 5.0, 5.0), DVec3::X);
        assert!((ray_aabb_entry(&hit, &b, f64::INFINITY).unwrap() - 5.0).abs() < 1e-12);
        let inside = Ray::new(DVec3::splat(5.0), DVec3::Z);
        assert_eq!(ray_aabb_entry(&inside, &b, f64::INFINITY), Some(0.0));
        let away = Ray::new(DVec3::new(-5.0, 5.0, 5.0), -DVec3::X);
        assert_eq!(ray_aabb_entry(&away, &b, f64::INFINITY), None);
        let parallel_outside = Ray::new(DVec3::new(-5.0, 20.0, 5.0), DVec3::X);
        assert_eq!(ray_aabb_entry(&parallel_outside, &b, f64::INFINITY), None);
        let parallel_inside = Ray::new(DVec3::new(-5.0, 5.0, 5.0), DVec3::X);
        assert_eq!(ray_aabb_entry(&parallel_inside, &b, 2.0), None);
        assert!(ray_aabb_entry(&parallel_inside, &b, 5.5).is_some());
        assert_eq!(ray_aabb_entry(&hit, &Aabb::EMPTY, f64::INFINITY), None);
    }
}
