//! Implements the narrow-phase primitives behind the spatial queries of
//! docs/03-geometry-kernel.md §6 and the picking rules of docs/06 §8: ray–plane
//! intersection, point-in-face tests in the face plane, closest points between
//! a ray or a segment and a segment, and the loop walk that turns a face into
//! its boundary polygons.
//!
//! Everything here is a small pure function over `f64` millimeters that takes
//! the document [`Tolerances`] explicitly (docs/03 §2, §8); nothing compares
//! floats with `==`.
//!
//! Owner key: `spatial`.

use glam::{DVec2, DVec3};

use crate::geom::{GeometryError, HalfEdgeMesh};
use crate::ids::{FaceId, LoopId};
use crate::math::{Plane, Ray, Segment};
use crate::tolerance::Tolerances;

/// Ray parameter of the intersection with `plane`: `None` when the ray is
/// parallel to the plane within `tol.angle` or the intersection lies more than
/// `tol.length` behind the origin (docs/03 §7 depth resolution). Intersections
/// within `tol.length` behind the origin are clamped to `t = 0`.
pub(super) fn ray_plane(ray: &Ray, plane: &Plane, tol: &Tolerances) -> Option<f64> {
    let denom = plane.normal.dot(ray.dir);
    if denom.abs() <= tol.angle {
        return None;
    }
    let t = -plane.signed_distance(ray.origin) / denom;
    if t < -tol.length || !t.is_finite() {
        return None;
    }
    Some(t.max(0.0))
}

/// Ray parameter and distance of the point of the ray closest to `p`
/// (`t` clamped to `[0, max_t]`).
pub(super) fn ray_point_closest(ray: &Ray, p: DVec3, max_t: f64) -> (f64, f64) {
    let t = ray.closest_param(p).min(max_t);
    (t, ray.at(t).distance(p))
}

/// Closest points between a ray (`t ≥ 0`, clamped to `max_t`) and a segment:
/// `(t_ray, t_segment, distance)`. For a segment parallel to the ray within
/// `tol.angle` the segment point closest to the ray origin is reported, so
/// picking a parallel edge yields a stable point (docs/03 §7 determinism).
pub(super) fn ray_segment_closest(
    ray: &Ray,
    seg: &Segment,
    max_t: f64,
    tol: &Tolerances,
) -> (f64, f64, f64) {
    let d2 = seg.b - seg.a;
    let r = seg.a - ray.origin;
    let e = d2.length_squared();
    let b = ray.dir.dot(d2);
    let c = ray.dir.dot(r);

    let project_on_segment = |p: DVec3| -> f64 {
        if e <= tol.length_sq {
            0.0
        } else {
            ((p - seg.a).dot(d2) / e).clamp(0.0, 1.0)
        }
    };

    let (mut t_ray, mut t_seg);
    if e <= tol.length_sq {
        // Degenerate segment: treat as the point `seg.a`.
        t_seg = 0.0;
        t_ray = c;
    } else {
        // `denom = |d2|² sin²θ` for a unit ray direction; parallel when θ ≤ tol.angle.
        let denom = e - b * b;
        if denom <= tol.angle * tol.angle * e {
            t_seg = project_on_segment(ray.origin);
            t_ray = (seg.point_at(t_seg) - ray.origin).dot(ray.dir);
        } else {
            let f = d2.dot(r);
            t_seg = ((b * c - f) / denom).clamp(0.0, 1.0);
            t_ray = b * t_seg + c;
        }
    }

    if t_ray < 0.0 {
        t_ray = 0.0;
        t_seg = project_on_segment(ray.origin);
    } else if t_ray > max_t {
        t_ray = max_t;
        t_seg = project_on_segment(ray.at(max_t));
    }
    let dist = ray.at(t_ray).distance(seg.point_at(t_seg));
    (t_ray, t_seg, dist)
}

/// Parameters `(t_a, t_b)` in `[0, 1]` of the closest points of two segments
/// (docs/03 §8). Parallel segments (within `tol.angle`) report `a.a` (clamped
/// into `b`'s range) and the point of `b` closest to it, which keeps collinear
/// overlaps deterministic.
pub(super) fn segment_closest_params(a: &Segment, b: &Segment, tol: &Tolerances) -> (f64, f64) {
    let d1 = a.b - a.a;
    let d2 = b.b - b.a;
    let r = a.a - b.a;
    let la = d1.length_squared();
    let lb = d2.length_squared();
    let f = d2.dot(r);

    if la <= tol.length_sq && lb <= tol.length_sq {
        return (0.0, 0.0);
    }
    if la <= tol.length_sq {
        return (0.0, (f / lb).clamp(0.0, 1.0));
    }
    let c = d1.dot(r);
    if lb <= tol.length_sq {
        return ((-c / la).clamp(0.0, 1.0), 0.0);
    }

    let bb = d1.dot(d2);
    let denom = la * lb - bb * bb;
    // `denom = |d1|²|d2|² sin²θ`; parallel when θ ≤ tol.angle.
    let mut s = if denom <= tol.angle * tol.angle * la * lb {
        0.0
    } else {
        ((bb * f - c * lb) / denom).clamp(0.0, 1.0)
    };
    let mut t = (bb * s + f) / lb;
    if t < 0.0 {
        t = 0.0;
        s = (-c / la).clamp(0.0, 1.0);
    } else if t > 1.0 {
        t = 1.0;
        s = ((bb - c) / la).clamp(0.0, 1.0);
    }
    (s, t)
}

/// A deterministic orthonormal in-plane basis `(u, v)` for a unit `normal`:
/// `u` is the projection of the world axis least aligned with the normal
/// (first of X, Y, Z on ties), `v = normal × u` (docs/03 §4.3 "P's 2D frame").
pub(super) fn plane_basis(normal: DVec3) -> (DVec3, DVec3) {
    let dots = [normal.x.abs(), normal.y.abs(), normal.z.abs()];
    let mut best = 0;
    for axis in 1..3 {
        if dots[axis] < dots[best] {
            best = axis;
        }
    }
    let axis = match best {
        0 => DVec3::X,
        1 => DVec3::Y,
        _ => DVec3::Z,
    };
    let u = (axis - normal * normal.dot(axis)).normalize_or_zero();
    let v = normal.cross(u);
    (u, v)
}

/// Positions of a loop's vertices in cycle order, walking `next` pointers from
/// `Loop::start`. Fails with `Invariant` on a broken cycle.
pub(super) fn loop_positions(mesh: &HalfEdgeMesh, l: LoopId) -> Result<Vec<DVec3>, GeometryError> {
    let lp = mesh
        .loop_at(l)
        .ok_or_else(|| GeometryError::NotFound(l.to_string()))?;
    let limit = mesh.edge_count() * 2 + 1;
    let mut out = Vec::new();
    let mut he = lp.start;
    loop {
        let h = mesh.half_edge(he).ok_or_else(|| {
            GeometryError::Invariant(format!("{l} references a missing half-edge"))
        })?;
        out.push(mesh.position(h.origin)?);
        he = h.next;
        if he == lp.start {
            break;
        }
        if out.len() > limit {
            return Err(GeometryError::Invariant(format!("{l} does not close")));
        }
    }
    Ok(out)
}

/// A face as 2D polygons in its own plane frame: the outer loop first, then the
/// holes. `origin` is the projection origin, `(u, v)` the basis.
pub(super) struct FacePolygon {
    outer: Vec<DVec2>,
    holes: Vec<Vec<DVec2>>,
    origin: DVec3,
    u: DVec3,
    v: DVec3,
}

impl FacePolygon {
    /// Projects the loops of face `f` into its plane frame.
    pub(super) fn of(mesh: &HalfEdgeMesh, f: FaceId) -> Result<Self, GeometryError> {
        let face = mesh
            .face(f)
            .ok_or_else(|| GeometryError::NotFound(f.to_string()))?;
        let (u, v) = plane_basis(face.plane.normal);
        let origin = face.plane.origin();
        let project = |p: DVec3| {
            let rel = p - origin;
            DVec2::new(rel.dot(u), rel.dot(v))
        };
        let outer = loop_positions(mesh, face.outer)?
            .into_iter()
            .map(project)
            .collect();
        let mut holes = Vec::with_capacity(face.holes.len());
        for h in &face.holes {
            holes.push(loop_positions(mesh, *h)?.into_iter().map(project).collect());
        }
        Ok(FacePolygon {
            outer,
            holes,
            origin,
            u,
            v,
        })
    }

    /// Plane-frame coordinates of a 3D point (projected along the normal).
    pub(super) fn to_2d(&self, p: DVec3) -> DVec2 {
        let rel = p - self.origin;
        DVec2::new(rel.dot(self.u), rel.dot(self.v))
    }

    /// `true` when the (already in-plane) point lies inside the face: inside the
    /// outer loop and outside every hole, with points within `tol.length` of any
    /// boundary counted as inside (an edge belongs to its faces, docs/06 §8).
    pub(super) fn contains(&self, p: DVec3, tol: &Tolerances) -> bool {
        let q = self.to_2d(p);
        if on_polygon_boundary(&self.outer, q, tol) {
            return true;
        }
        if !point_in_polygon(&self.outer, q) {
            return false;
        }
        for hole in &self.holes {
            if on_polygon_boundary(hole, q, tol) {
                return true;
            }
            if point_in_polygon(hole, q) {
                return false;
            }
        }
        true
    }
}

/// Distance from `q` to the closest polygon edge is within `tol.length`.
fn on_polygon_boundary(poly: &[DVec2], q: DVec2, tol: &Tolerances) -> bool {
    let n = poly.len();
    if n == 0 {
        return false;
    }
    (0..n).any(|i| {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let ab = b - a;
        let len_sq = ab.length_squared();
        let t = if len_sq <= tol.length_sq {
            0.0
        } else {
            ((q - a).dot(ab) / len_sq).clamp(0.0, 1.0)
        };
        (a + ab * t).distance_squared(q) <= tol.length_sq
    })
}

/// Even–odd crossing test (boundary points are resolved by the caller).
fn point_in_polygon(poly: &[DVec2], q: DVec2) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (pi, pj) = (poly[i], poly[j]);
        if (pi.y > q.y) != (pj.y > q.y) {
            let x = pj.x + (q.y - pj.y) * (pi.x - pj.x) / (pi.y - pj.y);
            if q.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn tol() -> Tolerances {
        Tolerances::default()
    }

    #[test]
    fn ray_plane_handles_parallel_and_behind() {
        let plane = Plane::new(DVec3::Z, -10.0);
        let r = Ray::new(DVec3::ZERO, DVec3::Z);
        assert!((ray_plane(&r, &plane, &tol()).unwrap() - 10.0).abs() < 1e-12);
        let back = Ray::new(DVec3::ZERO, -DVec3::Z);
        assert_eq!(ray_plane(&back, &plane, &tol()), None);
        let parallel = Ray::new(DVec3::ZERO, DVec3::X);
        assert_eq!(ray_plane(&parallel, &plane, &tol()), None);
        let on_plane = Ray::new(DVec3::new(0.0, 0.0, 10.0 - 1e-5), DVec3::NEG_Z);
        assert_eq!(ray_plane(&on_plane, &plane, &tol()), Some(0.0));
    }

    #[test]
    fn ray_segment_closest_cases() {
        let seg = Segment::new(DVec3::new(0.0, 0.0, 0.0), DVec3::new(0.0, 10.0, 0.0));
        // Perpendicular crossing at distance 1.
        let r = Ray::new(DVec3::new(-5.0, 4.0, 1.0), DVec3::X);
        let (t_ray, t_seg, d) = ray_segment_closest(&r, &seg, f64::INFINITY, &tol());
        assert!((t_ray - 5.0).abs() < 1e-12);
        assert!((t_seg - 0.4).abs() < 1e-12);
        assert!((d - 1.0).abs() < 1e-12);
        // Parallel: the segment point nearest the origin.
        let p = Ray::new(DVec3::new(1.0, -3.0, 0.0), DVec3::Y);
        let (t_ray, t_seg, d) = ray_segment_closest(&p, &seg, f64::INFINITY, &tol());
        assert!((t_seg).abs() < 1e-12);
        assert!((t_ray - 3.0).abs() < 1e-12);
        assert!((d - 1.0).abs() < 1e-12);
        // Behind the origin: clamped to t = 0.
        let behind = Ray::new(DVec3::new(2.0, 5.0, 0.0), DVec3::X);
        let (t_ray, t_seg, d) = ray_segment_closest(&behind, &seg, f64::INFINITY, &tol());
        assert_eq!(t_ray, 0.0);
        assert!((t_seg - 0.5).abs() < 1e-12);
        assert!((d - 2.0).abs() < 1e-12);
        // Capped by max_t.
        let (t_ray, _, _) = ray_segment_closest(&r, &seg, 2.0, &tol());
        assert_eq!(t_ray, 2.0);
    }

    #[test]
    fn segment_closest_params_crossing_and_parallel() {
        let a = Segment::new(DVec3::new(-1.0, 0.0, 0.0), DVec3::new(1.0, 0.0, 0.0));
        let b = Segment::new(DVec3::new(0.5, -1.0, 0.0), DVec3::new(0.5, 1.0, 0.0));
        let (s, t) = segment_closest_params(&a, &b, &tol());
        assert!((s - 0.75).abs() < 1e-12);
        assert!((t - 0.5).abs() < 1e-12);
        let c = Segment::new(DVec3::new(-5.0, 1.0, 0.0), DVec3::new(5.0, 1.0, 0.0));
        let (s, t) = segment_closest_params(&a, &c, &tol());
        assert!(a.point_at(s).distance(c.point_at(t)) - 1.0 < 1e-12);
        let point = Segment::new(DVec3::new(0.0, 2.0, 0.0), DVec3::new(0.0, 2.0, 0.0));
        let (s, t) = segment_closest_params(&a, &point, &tol());
        assert!((s - 0.5).abs() < 1e-12);
        assert_eq!(t, 0.0);
    }

    #[test]
    fn plane_basis_is_orthonormal_and_deterministic() {
        for n in [
            DVec3::Z,
            DVec3::X,
            DVec3::new(1.0, 1.0, 1.0).normalize(),
            -DVec3::Y,
        ] {
            let (u, v) = plane_basis(n);
            assert!((u.length() - 1.0).abs() < 1e-12);
            assert!((v.length() - 1.0).abs() < 1e-12);
            assert!(u.dot(n).abs() < 1e-12);
            assert!(v.dot(n).abs() < 1e-12);
            assert!(u.dot(v).abs() < 1e-12);
            assert_eq!(plane_basis(n), (u, v));
        }
        assert_eq!(plane_basis(DVec3::Z).0, DVec3::X);
    }

    #[test]
    fn polygon_tests_with_hole_and_boundary() {
        let square = vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(10.0, 0.0),
            DVec2::new(10.0, 10.0),
            DVec2::new(0.0, 10.0),
        ];
        assert!(point_in_polygon(&square, DVec2::new(5.0, 5.0)));
        assert!(!point_in_polygon(&square, DVec2::new(15.0, 5.0)));
        assert!(on_polygon_boundary(&square, DVec2::new(5.0, 1e-4), &tol()));
        assert!(!on_polygon_boundary(&square, DVec2::new(5.0, 1e-2), &tol()));
        assert!(!point_in_polygon(&square[..2], DVec2::new(5.0, 5.0)));
    }

    proptest! {
        #[test]
        fn closest_params_are_a_lower_bound(
            ax in -50.0f64..50.0, ay in -50.0f64..50.0, az in -50.0f64..50.0,
            bx in -50.0f64..50.0, by in -50.0f64..50.0, bz in -50.0f64..50.0,
            cx in -50.0f64..50.0, cy in -50.0f64..50.0, cz in -50.0f64..50.0,
            dx in -50.0f64..50.0, dy in -50.0f64..50.0, dz in -50.0f64..50.0,
            s in 0.0f64..=1.0, t in 0.0f64..=1.0,
        ) {
            let a = Segment::new(DVec3::new(ax, ay, az), DVec3::new(bx, by, bz));
            let b = Segment::new(DVec3::new(cx, cy, cz), DVec3::new(dx, dy, dz));
            let (sa, tb) = segment_closest_params(&a, &b, &tol());
            prop_assert!((0.0..=1.0).contains(&sa));
            prop_assert!((0.0..=1.0).contains(&tb));
            let best = a.point_at(sa).distance(b.point_at(tb));
            let sampled = a.point_at(s).distance(b.point_at(t));
            prop_assert!(best <= sampled + 1e-6, "best {best} > sampled {sampled}");
        }

        #[test]
        fn ray_segment_distance_is_a_lower_bound(
            ox in -50.0f64..50.0, oy in -50.0f64..50.0, oz in -50.0f64..50.0,
            dx in -1.0f64..1.0, dy in -1.0f64..1.0, dz in -1.0f64..1.0,
            ax in -50.0f64..50.0, ay in -50.0f64..50.0, az in -50.0f64..50.0,
            bx in -50.0f64..50.0, by in -50.0f64..50.0, bz in -50.0f64..50.0,
            t_ray in 0.0f64..200.0, t_seg in 0.0f64..=1.0,
        ) {
            let dir = DVec3::new(dx, dy, dz);
            prop_assume!(dir.length() > 1e-3);
            let ray = Ray::new(DVec3::new(ox, oy, oz), dir);
            let seg = Segment::new(DVec3::new(ax, ay, az), DVec3::new(bx, by, bz));
            let (tr, ts, d) = ray_segment_closest(&ray, &seg, f64::INFINITY, &tol());
            prop_assert!(tr >= 0.0);
            prop_assert!((0.0..=1.0).contains(&ts));
            let sampled = ray.at(t_ray).distance(seg.point_at(t_seg));
            prop_assert!(d <= sampled + 1e-6, "best {d} > sampled {sampled}");
        }
    }
}
