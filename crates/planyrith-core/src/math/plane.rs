//! Implements plane utilities for docs/03-geometry-kernel.md §3–§4 (face
//! planes, coplanarity, 2D frames used by face finding and tessellation) and
//! the plane-fit guideline of §8 (centroid + covariance, no cancellation).

#![warn(unused_variables, dead_code)]

use glam::{DAffine3, DMat3, DVec2, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tolerance::Tolerances;

/// An oriented plane `normal · p + d = 0` with a unit normal (docs/03 §3.1
/// `Face::plane`). Serialized as `{ normal: [x,y,z], d }`; the file format's
/// `[f64; 4]` form is produced by [`Plane::to_array`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Plane {
    /// Unit normal; the face front is along `+normal`.
    #[schemars(with = "[f64; 3]")]
    pub normal: DVec3,
    /// Signed offset so that `normal · p + d = 0` on the plane.
    pub d: f64,
}

/// A 2D orthonormal frame on a plane (docs/03 §4.3 "P's 2D frame").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaneFrame {
    /// Frame origin on the plane.
    pub origin: DVec3,
    /// First in-plane unit axis.
    pub u: DVec3,
    /// Second in-plane unit axis (`normal × u`).
    pub v: DVec3,
    /// Plane normal (`u × v`).
    pub normal: DVec3,
}

/// Which side of an oriented plane a point is on, within `planarity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaneSide {
    /// Within `planarity` of the plane.
    On,
    /// On the `+normal` side.
    Front,
    /// On the `-normal` side.
    Back,
}

impl Plane {
    /// The ground plane Z = 0 facing +Z (docs/03 §1).
    pub const XY: Plane = Plane {
        normal: DVec3::Z,
        d: 0.0,
    };
    /// The plane X = 0 facing +X.
    pub const YZ: Plane = Plane {
        normal: DVec3::X,
        d: 0.0,
    };
    /// The plane Y = 0 facing +Y.
    pub const XZ: Plane = Plane {
        normal: DVec3::Y,
        d: 0.0,
    };

    /// Builds a plane from a unit normal and offset without normalizing.
    pub const fn new(normal: DVec3, d: f64) -> Self {
        Plane { normal, d }
    }

    /// Plane through `point` with the given (not necessarily unit) normal.
    pub fn from_point_normal(point: DVec3, normal: DVec3) -> Self {
        let n = normal.normalize_or_zero();
        Plane {
            normal: n,
            d: -n.dot(point),
        }
    }

    /// Plane through three points, oriented by the right-hand rule
    /// (`(b-a) × (c-a)`), or `None` if they are collinear within tolerance
    /// (any point within `tol.length` of the line through the other two).
    pub fn from_points(a: DVec3, b: DVec3, c: DVec3, tol: &Tolerances) -> Option<Self> {
        if tol.collinear(a, b, c) {
            return None;
        }
        let n = (b - a).cross(c - a).try_normalize()?;
        // Offset from the centroid: averages the rounding of the three points.
        let centroid = (a + b + c) / 3.0;
        Some(Plane {
            normal: n,
            d: -n.dot(centroid),
        })
    }

    /// Plane of a closed polygon by Newell's method (docs/03 §8): the normal is
    /// the area vector of the loop, so the orientation follows the winding
    /// (counter-clockwise seen from the front). `None` when the polygon has
    /// fewer than three points or its area is below `tol.length²`.
    pub fn from_polygon(points: &[DVec3], tol: &Tolerances) -> Option<Self> {
        if points.len() < 3 {
            return None;
        }
        let centroid = mean(points);
        let area_vector = newell_area_vector(points, centroid);
        // |area_vector| is twice the polygon area.
        if area_vector.length() * 0.5 <= tol.length_sq {
            return None;
        }
        let n = area_vector.try_normalize()?;
        Some(Plane {
            normal: n,
            d: -n.dot(centroid),
        })
    }

    /// Least-squares plane fit through `points` using centroid + covariance
    /// (docs/03 §8). Returns `None` for fewer than three non-collinear points.
    ///
    /// The normal is the eigenvector of the smallest covariance eigenvalue. Its
    /// sign follows the winding of `points` when they form a polygon with
    /// non-zero area (Newell); for unordered clouds the default orientation
    /// rule of docs/03 §4.3 applies (`+Z`, else `+Y`, else `+X`).
    pub fn fit(points: &[DVec3], tol: &Tolerances) -> Option<Self> {
        if points.len() < 3 {
            return None;
        }
        let centroid = mean(points);
        let mut cov = DMat3::ZERO;
        for &p in points {
            let r = p - centroid;
            cov.x_axis += r * r.x;
            cov.y_axis += r * r.y;
            cov.z_axis += r * r.z;
        }
        let eigen = symmetric_eigen(cov);
        // Eigenvalues ascending: λ0 is the out-of-plane variance (× n), λ1 the
        // smaller in-plane one. Collinear points have λ1 ≈ 0: the RMS distance
        // from the best line, sqrt(λ1 / n), is within `length`.
        let n_points = points.len() as f64;
        if eigen.values[1] <= tol.length_sq * n_points {
            return None;
        }
        let mut normal = eigen.vectors[0].try_normalize()?;
        let newell = newell_area_vector(points, centroid);
        if newell.length() * 0.5 > tol.length_sq {
            if newell.dot(normal) < 0.0 {
                normal = -normal;
            }
        } else {
            normal = default_orientation(normal, tol);
        }
        Some(Plane {
            normal,
            d: -normal.dot(centroid),
        })
    }

    /// Signed distance of `p` to the plane (positive on the front side).
    pub fn signed_distance(&self, p: DVec3) -> f64 {
        self.normal.dot(p) + self.d
    }

    /// Unsigned distance of `p` to the plane.
    pub fn distance(&self, p: DVec3) -> f64 {
        self.signed_distance(p).abs()
    }

    /// Classifies `p` against the plane within `tol.planarity`.
    pub fn side(&self, p: DVec3, tol: &Tolerances) -> PlaneSide {
        let d = self.signed_distance(p);
        if d.abs() <= tol.planarity {
            PlaneSide::On
        } else if d > 0.0 {
            PlaneSide::Front
        } else {
            PlaneSide::Back
        }
    }

    /// `true` when `p` lies within `tol.planarity` of the plane.
    pub fn contains(&self, p: DVec3, tol: &Tolerances) -> bool {
        self.distance(p) <= tol.planarity
    }

    /// Orthogonal projection of `p` onto the plane.
    pub fn project(&self, p: DVec3) -> DVec3 {
        p - self.normal * self.signed_distance(p)
    }

    /// Mirror image of `p` across the plane.
    pub fn reflect(&self, p: DVec3) -> DVec3 {
        p - self.normal * (2.0 * self.signed_distance(p))
    }

    /// The plane point closest to the world origin.
    pub fn origin(&self) -> DVec3 {
        -self.normal * self.d
    }

    /// Same plane, opposite orientation.
    pub fn flipped(&self) -> Self {
        Plane {
            normal: -self.normal,
            d: -self.d,
        }
    }

    /// The plane moved by `distance` along its normal.
    pub fn offset(&self, distance: f64) -> Self {
        Plane {
            normal: self.normal,
            d: self.d - distance,
        }
    }

    /// `true` when the plane is vertical (normal perpendicular to Z within
    /// `tol.angle`), used by the default face orientation rule of docs/03 §4.3.
    pub fn is_vertical(&self, tol: &Tolerances) -> bool {
        self.normal.z.abs() <= tol.angle
    }

    /// `true` when the plane is horizontal (normal parallel to Z within
    /// `tol.angle`).
    pub fn is_horizontal(&self, tol: &Tolerances) -> bool {
        tol.is_parallel(self.normal, DVec3::Z)
    }

    /// A deterministic 2D frame on the plane: `u` is the projection of the
    /// world axis least aligned with the normal (ties resolved X, Y, Z),
    /// `v = normal × u`, origin as [`Plane::origin`].
    pub fn frame(&self) -> PlaneFrame {
        self.frame_at(self.origin())
    }

    /// Same axes as [`Plane::frame`] with the origin at the projection of
    /// `origin` onto the plane (keeps 2D coordinates small near a face).
    pub fn frame_at(&self, origin: DVec3) -> PlaneFrame {
        let n = self.normal;
        let a = n.abs();
        let axis = if a.x <= a.y && a.x <= a.z {
            DVec3::X
        } else if a.y <= a.z {
            DVec3::Y
        } else {
            DVec3::Z
        };
        let u = (axis - n * axis.dot(n)).normalize_or_zero();
        PlaneFrame {
            origin: self.project(origin),
            u,
            v: n.cross(u),
            normal: n,
        }
    }

    /// The plane transformed by `t`. The new normal is the cross product of
    /// the transformed frame axes, which is exact for non-uniform scale and
    /// shear and keeps the front side consistent with the transformed loop
    /// winding under mirroring (docs/03 §1). A transform that flattens the
    /// plane yields a zero normal.
    pub fn transformed(&self, t: &DAffine3) -> Self {
        let frame = self.frame();
        let origin = t.transform_point3(frame.origin);
        let u = t.transform_vector3(frame.u);
        let v = t.transform_vector3(frame.v);
        let n = u.cross(v).normalize_or_zero();
        Plane {
            normal: n,
            d: -n.dot(origin),
        }
    }

    /// Line of intersection with `other` as `(point, unit direction)`, or
    /// `None` when the planes are parallel within `tol.angle`. The point is
    /// the line point closest to the world origin; the direction is
    /// `self.normal × other.normal`.
    pub fn intersect_plane(&self, other: &Plane, tol: &Tolerances) -> Option<(DVec3, DVec3)> {
        if tol.is_parallel(self.normal, other.normal) {
            return None;
        }
        let dir = self.normal.cross(other.normal);
        let len_sq = dir.length_squared();
        if len_sq <= 0.0 {
            return None;
        }
        let h1 = -self.d;
        let h2 = -other.d;
        let point = (other.normal.cross(dir) * h1 + dir.cross(self.normal) * h2) / len_sq;
        Some((point, dir / len_sq.sqrt()))
    }

    /// `[nx, ny, nz, d]` as stored in `document.msgpack` (docs/09 §2).
    pub fn to_array(&self) -> [f64; 4] {
        [self.normal.x, self.normal.y, self.normal.z, self.d]
    }

    /// Inverse of [`Plane::to_array`]; normalizes the normal.
    pub fn from_array(a: [f64; 4]) -> Self {
        let n = DVec3::new(a[0], a[1], a[2]);
        let len = n.length();
        if len > 0.0 {
            Plane {
                normal: n / len,
                d: a[3] / len,
            }
        } else {
            Plane {
                normal: DVec3::Z,
                d: a[3],
            }
        }
    }
}

impl PlaneFrame {
    /// Builds a frame from an origin, an in-plane direction and a normal,
    /// orthonormalizing `u` against the normal. Degenerate input (a zero
    /// normal, or `u` parallel to it) falls back to the deterministic axes of
    /// [`Plane::frame`].
    pub fn new(origin: DVec3, u: DVec3, normal: DVec3) -> Self {
        let n = normal.normalize_or_zero();
        let u = (u - n * u.dot(n)).normalize_or_zero();
        if n.length_squared() <= 0.0 || u.length_squared() <= 0.0 {
            return Plane::from_point_normal(
                origin,
                if n.length_squared() > 0.0 {
                    n
                } else {
                    DVec3::Z
                },
            )
            .frame_at(origin);
        }
        PlaneFrame {
            origin,
            u,
            v: n.cross(u),
            normal: n,
        }
    }

    /// 2D coordinates of a 3D point (projected onto the plane first).
    pub fn to_2d(&self, p: DVec3) -> DVec2 {
        let rel = p - self.origin;
        DVec2::new(rel.dot(self.u), rel.dot(self.v))
    }

    /// 3D point on the plane for 2D frame coordinates.
    pub fn from_2d(&self, p: DVec2) -> DVec3 {
        self.origin + self.u * p.x + self.v * p.y
    }

    /// 2D coordinates of every point (projected onto the plane first).
    pub fn project_all(&self, points: &[DVec3]) -> Vec<DVec2> {
        points.iter().map(|&p| self.to_2d(p)).collect()
    }

    /// 3D points for every 2D coordinate.
    pub fn unproject_all(&self, points: &[DVec2]) -> Vec<DVec3> {
        points.iter().map(|&p| self.from_2d(p)).collect()
    }

    /// Height of `p` above the plane (signed, along the normal).
    pub fn height(&self, p: DVec3) -> f64 {
        (p - self.origin).dot(self.normal)
    }

    /// The plane this frame lies on.
    pub fn plane(&self) -> Plane {
        Plane::from_point_normal(self.origin, self.normal)
    }

    /// Affine map from frame coordinates `(x, y, height)` to world space.
    pub fn to_world(&self) -> DAffine3 {
        DAffine3::from_mat3_translation(DMat3::from_cols(self.u, self.v, self.normal), self.origin)
    }

    /// Affine map from world space to frame coordinates (inverse of
    /// [`PlaneFrame::to_world`], exact because the frame is orthonormal).
    pub fn to_local(&self) -> DAffine3 {
        let rot = DMat3::from_cols(self.u, self.v, self.normal).transpose();
        DAffine3::from_mat3_translation(rot, -(rot * self.origin))
    }
}

/// Arithmetic mean of a non-empty point set (the caller checks emptiness).
fn mean(points: &[DVec3]) -> DVec3 {
    let sum = points.iter().fold(DVec3::ZERO, |acc, &p| acc + p);
    sum / (points.len().max(1) as f64)
}

/// Newell area vector of a closed loop relative to `origin`; its length is
/// twice the polygon area and its direction the right-hand normal.
fn newell_area_vector(points: &[DVec3], origin: DVec3) -> DVec3 {
    let mut n = DVec3::ZERO;
    for i in 0..points.len() {
        let a = points[i] - origin;
        let b = points[(i + 1) % points.len()] - origin;
        n += a.cross(b);
    }
    n
}

/// The default face orientation rule of docs/03 §4.3 step 3 applied to a
/// direction: `+Z` if not vertical, else `+Y`, else `+X`.
fn default_orientation(n: DVec3, tol: &Tolerances) -> DVec3 {
    if n.z.abs() > tol.angle {
        if n.z > 0.0 { n } else { -n }
    } else if n.y.abs() > tol.angle {
        if n.y > 0.0 { n } else { -n }
    } else if n.x >= 0.0 {
        n
    } else {
        -n
    }
}

/// Eigen-decomposition of a symmetric 3×3 matrix.
struct SymmetricEigen {
    /// Eigenvalues in ascending order.
    values: [f64; 3],
    /// Unit eigenvectors matching `values`.
    vectors: [DVec3; 3],
}

/// Cyclic Jacobi rotations (deterministic, bounded sweeps) for a symmetric
/// 3×3 matrix. Robust for the near-rank-deficient covariance matrices of
/// planar point sets, where closed-form cubic solvers lose precision.
fn symmetric_eigen(m: DMat3) -> SymmetricEigen {
    let mut a = m.to_cols_array_2d();
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let scale = a
        .iter()
        .flatten()
        .fold(0.0f64, |acc, x| acc.max(x.abs()))
        .max(f64::MIN_POSITIVE);
    for _sweep in 0..64 {
        let off = a[0][1].abs() + a[0][2].abs() + a[1][2].abs();
        if off <= scale * 1e-18 {
            break;
        }
        for (p, q) in [(0usize, 1usize), (0, 2), (1, 2)] {
            let apq = a[p][q];
            if apq.abs() <= scale * 1e-18 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * apq);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let t = if theta == 0.0 { 1.0 } else { t };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            // A ← Jᵀ A J for the rotation in the (p, q) plane.
            for row in &mut a {
                let akp = row[p];
                let akq = row[q];
                row[p] = c * akp - s * akq;
                row[q] = s * akp + c * akq;
            }
            let row_p: [f64; 3] = std::array::from_fn(|k| c * a[p][k] - s * a[q][k]);
            let row_q: [f64; 3] = std::array::from_fn(|k| s * a[p][k] + c * a[q][k]);
            a[p] = row_p;
            a[q] = row_q;
            for row in &mut v {
                let vkp = row[p];
                let vkq = row[q];
                row[p] = c * vkp - s * vkq;
                row[q] = s * vkp + c * vkq;
            }
        }
    }
    let mut order = [0usize, 1, 2];
    // Insertion sort on three keys keeps the ordering total and deterministic.
    for i in 1..3 {
        let mut j = i;
        while j > 0 && a[order[j]][order[j]] < a[order[j - 1]][order[j - 1]] {
            order.swap(j, j - 1);
            j -= 1;
        }
    }
    let column = |c: usize| DVec3::new(v[0][c], v[1][c], v[2][c]);
    SymmetricEigen {
        values: [
            a[order[0]][order[0]],
            a[order[1]][order[1]],
            a[order[2]][order[2]],
        ],
        vectors: [column(order[0]), column(order[1]), column(order[2])],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn tol() -> Tolerances {
        Tolerances::default()
    }

    #[test]
    fn signed_distance_and_projection() {
        let p = Plane::from_point_normal(DVec3::new(0.0, 0.0, 10.0), DVec3::Z * 3.0);
        assert!((p.signed_distance(DVec3::new(1.0, 2.0, 12.0)) - 2.0).abs() < 1e-12);
        let q = p.project(DVec3::new(1.0, 2.0, 12.0));
        assert!((q.z - 10.0).abs() < 1e-12);
        assert!((p.flipped().signed_distance(DVec3::new(0.0, 0.0, 12.0)) + 2.0).abs() < 1e-12);
        assert!((p.reflect(DVec3::new(1.0, 2.0, 12.0)).z - 8.0).abs() < 1e-12);
        assert!((p.offset(5.0).signed_distance(DVec3::new(0.0, 0.0, 15.0))).abs() < 1e-12);
        assert_eq!(p.side(DVec3::new(0.0, 0.0, 11.0), &tol()), PlaneSide::Front);
        assert_eq!(p.side(DVec3::new(0.0, 0.0, 9.0), &tol()), PlaneSide::Back);
        assert_eq!(p.side(DVec3::new(0.0, 0.0, 10.001), &tol()), PlaneSide::On);
    }

    #[test]
    fn array_round_trip() {
        let p = Plane::from_array([0.0, 0.0, 2.0, 4.0]);
        assert!((p.normal.z - 1.0).abs() < 1e-12);
        assert!((p.d - 2.0).abs() < 1e-12);
        assert_eq!(Plane::XY.to_array(), [0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn frame_round_trip_2d() {
        let frame = PlaneFrame {
            origin: DVec3::ZERO,
            u: DVec3::X,
            v: DVec3::Y,
            normal: DVec3::Z,
        };
        let p = DVec3::new(3.0, -2.0, 0.0);
        assert_eq!(frame.from_2d(frame.to_2d(p)), p);
    }

    #[test]
    fn three_point_plane_follows_right_hand_rule() {
        let t = tol();
        let p = Plane::from_points(DVec3::ZERO, DVec3::X, DVec3::Y, &t).unwrap();
        assert!(p.normal.distance(DVec3::Z) < 1e-12);
        let q = Plane::from_points(DVec3::ZERO, DVec3::Y, DVec3::X, &t).unwrap();
        assert!(q.normal.distance(-DVec3::Z) < 1e-12);
        assert!(Plane::from_points(DVec3::ZERO, DVec3::X, DVec3::X * 2.0, &t).is_none());
        assert!(Plane::from_points(DVec3::ZERO, DVec3::ZERO, DVec3::X, &t).is_none());
        assert!(
            Plane::from_points(
                DVec3::ZERO,
                DVec3::X * 100.0,
                DVec3::new(50.0, 0.0005, 0.0),
                &t
            )
            .is_none()
        );
    }

    #[test]
    fn polygon_and_fit_agree_on_a_square() {
        let t = tol();
        let square = [
            DVec3::new(0.0, 0.0, 5.0),
            DVec3::new(10.0, 0.0, 5.0),
            DVec3::new(10.0, 10.0, 5.0),
            DVec3::new(0.0, 10.0, 5.0),
        ];
        let newell = Plane::from_polygon(&square, &t).unwrap();
        let fitted = Plane::fit(&square, &t).unwrap();
        assert!(newell.normal.distance(DVec3::Z) < 1e-12);
        assert!((newell.d + 5.0).abs() < 1e-12);
        assert!(t.is_coplanar_same_side(&newell, &fitted));
        let mut cw = square;
        cw.reverse();
        assert!(Plane::fit(&cw, &t).unwrap().normal.distance(-DVec3::Z) < 1e-12);
        assert!(Plane::from_polygon(&square[..2], &t).is_none());
        let line: Vec<DVec3> = (0..10).map(|i| DVec3::X * f64::from(i)).collect();
        assert!(Plane::fit(&line, &t).is_none());
        assert!(Plane::from_polygon(&line, &t).is_none());
    }

    #[test]
    fn fit_of_unordered_cloud_uses_default_orientation() {
        let t = tol();
        // Zig-zag order: Newell area is zero, so the default rule picks +Z.
        let pts = [
            DVec3::new(0.0, 0.0, 1.0),
            DVec3::new(10.0, 10.0, 1.0),
            DVec3::new(10.0, 0.0, 1.0),
            DVec3::new(0.0, 10.0, 1.0),
        ];
        let p = Plane::fit(&pts, &t).unwrap();
        assert!(p.normal.distance(DVec3::Z) < 1e-9, "{p:?}");
        // Vertical plane facing -Y gets flipped to +Y.
        let wall = [
            DVec3::new(0.0, 3.0, 0.0),
            DVec3::new(10.0, 3.0, 10.0),
            DVec3::new(10.0, 3.0, 0.0),
            DVec3::new(0.0, 3.0, 10.0),
        ];
        let p = Plane::fit(&wall, &t).unwrap();
        assert!(p.normal.distance(DVec3::Y) < 1e-9, "{p:?}");
    }

    #[test]
    fn fit_is_least_squares_for_noisy_points() {
        let t = Tolerances::with_length(0.1);
        // Points on z = 0.1x + 0.2y + 3 with alternating ±0.05 noise.
        let mut pts = Vec::new();
        for i in 0..5 {
            for j in 0..5 {
                let x = f64::from(i) * 10.0;
                let y = f64::from(j) * 10.0;
                let noise = if (i + j) % 2 == 0 { 0.05 } else { -0.05 };
                pts.push(DVec3::new(x, y, 0.1 * x + 0.2 * y + 3.0 + noise));
            }
        }
        let p = Plane::fit(&pts, &t).unwrap();
        let expected = DVec3::new(-0.1, -0.2, 1.0).normalize();
        assert!(p.normal.distance(expected) < 1e-3, "{p:?}");
        assert!(t.points_on_plane(&p, &pts));
    }

    #[test]
    fn frame_is_orthonormal_and_deterministic() {
        let t = tol();
        for n in [
            DVec3::Z,
            -DVec3::Z,
            DVec3::X,
            DVec3::Y,
            DVec3::new(1.0, 1.0, 1.0),
            DVec3::new(0.3, -0.2, 0.9),
        ] {
            let plane = Plane::from_point_normal(DVec3::new(1.0, 2.0, 3.0), n);
            let f = plane.frame();
            assert!((f.u.length() - 1.0).abs() < 1e-12);
            assert!((f.v.length() - 1.0).abs() < 1e-12);
            assert!(f.u.dot(f.v).abs() < 1e-12);
            assert!(f.u.cross(f.v).distance(f.normal) < 1e-12);
            assert!(plane.contains(f.origin, &t));
            assert_eq!(plane.frame(), f);
        }
        // Ground plane: u = +X, v = +Y.
        let f = Plane::XY.frame();
        assert!(f.u.distance(DVec3::X) < 1e-12);
        assert!(f.v.distance(DVec3::Y) < 1e-12);
        // frame_at keeps the 2D origin at the requested point.
        let f = Plane::XY.frame_at(DVec3::new(5.0, 6.0, 7.0));
        assert_eq!(f.origin, DVec3::new(5.0, 6.0, 0.0));
        assert_eq!(f.to_2d(DVec3::new(5.0, 6.0, 1.0)), DVec2::ZERO);
        // Local/world affine maps invert each other.
        let p = DVec3::new(1.0, 2.0, 3.0);
        let local = f.to_local().transform_point3(p);
        assert!((local.z - 3.0).abs() < 1e-12);
        assert!(f.to_world().transform_point3(local).distance(p) < 1e-12);
        assert!(PlaneFrame::new(DVec3::ZERO, DVec3::Z, DVec3::Z).u.length() > 0.5);
    }

    #[test]
    fn transformed_plane_keeps_points_and_mirrors_orientation() {
        let t = tol();
        let plane = Plane::from_points(
            DVec3::new(1.0, 0.0, 2.0),
            DVec3::new(4.0, 1.0, 2.5),
            DVec3::new(0.0, 3.0, 1.0),
            &t,
        )
        .unwrap();
        let shear = DAffine3::from_mat3_translation(
            DMat3::from_cols(
                DVec3::new(2.0, 0.0, 0.0),
                DVec3::new(0.5, 1.0, 0.0),
                DVec3::new(0.0, 0.3, 3.0),
            ),
            DVec3::new(10.0, -5.0, 2.0),
        );
        let moved = plane.transformed(&shear);
        for p in [plane.origin(), plane.frame().from_2d(DVec2::new(3.0, -7.0))] {
            assert!(moved.contains(shear.transform_point3(p), &t));
        }
        assert!((moved.normal.length() - 1.0).abs() < 1e-12);
        // Orientation: transforming a CCW loop and refitting agrees.
        let f = plane.frame();
        let loop3d: Vec<DVec3> = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
            .iter()
            .map(|&(x, y)| shear.transform_point3(f.from_2d(DVec2::new(x, y))))
            .collect();
        let refit = Plane::from_polygon(&loop3d, &t).unwrap();
        assert!(t.is_coplanar_same_side(&moved, &refit));
        // Mirror flips the normal relative to the inverse-transpose rule.
        let mirror = DAffine3::from_scale(DVec3::new(-1.0, 1.0, 1.0));
        let mirrored = Plane::XY.transformed(&mirror);
        assert!(mirrored.normal.distance(-DVec3::Z) < 1e-12);
        // Flattening transform gives a zero normal, never NaN.
        let flat = Plane::YZ.transformed(&DAffine3::from_scale(DVec3::new(1.0, 1.0, 0.0)));
        assert_eq!(flat.normal, DVec3::ZERO);
    }

    #[test]
    fn plane_plane_intersection_line() {
        let t = tol();
        let wall = Plane::from_point_normal(DVec3::new(5.0, 0.0, 0.0), DVec3::X);
        let (p, d) = Plane::XY.intersect_plane(&wall, &t).unwrap();
        assert!(p.distance(DVec3::new(5.0, 0.0, 0.0)) < 1e-12);
        assert!(d.distance(DVec3::Y) < 1e-12);
        assert!(
            Plane::XY
                .intersect_plane(&Plane::XY.offset(3.0), &t)
                .is_none()
        );
    }

    fn arb_vec(range: f64) -> impl Strategy<Value = DVec3> {
        (-range..range, -range..range, -range..range).prop_map(|(x, y, z)| DVec3::new(x, y, z))
    }

    fn arb_normal() -> impl Strategy<Value = DVec3> {
        arb_vec(1.0).prop_filter("non-degenerate", |v| v.length() > 0.1)
    }

    proptest! {
        /// Points generated on a plane fit back to a coplanar plane whose
        /// orientation matches the loop winding.
        #[test]
        fn fit_recovers_generating_plane(
            n in arb_normal(),
            origin in arb_vec(1000.0),
            radius in 1.0f64..1000.0,
            count in 3usize..12,
            phase in 0.0f64..1.0,
        ) {
            let t = Tolerances::default();
            let plane = Plane::from_point_normal(origin, n);
            let f = plane.frame_at(origin);
            // A regular polygon, CCW in the frame: Newell normal = +normal.
            let pts: Vec<DVec3> = (0..count)
                .map(|i| {
                    let a = (i as f64 / count as f64 + phase) * std::f64::consts::TAU;
                    f.from_2d(DVec2::new(a.cos(), a.sin()) * radius)
                })
                .collect();
            let fitted = Plane::fit(&pts, &t).unwrap();
            let newell = Plane::from_polygon(&pts, &t).unwrap();
            prop_assert!(t.is_coplanar_same_side(&fitted, &plane), "{fitted:?} vs {plane:?}");
            prop_assert!(t.is_coplanar_same_side(&newell, &plane), "{newell:?} vs {plane:?}");
            prop_assert!(t.points_on_plane(&fitted, &pts));
            let three = Plane::from_points(pts[0], pts[1], pts[2], &t).unwrap();
            prop_assert!(t.is_coplanar_same_side(&three, &plane));
        }

        /// 2D frame coordinates round-trip for in-plane points and the frame
        /// is orthonormal for any normal.
        #[test]
        fn frame_round_trip(n in arb_normal(), origin in arb_vec(1000.0), uv in (-1000.0f64..1000.0, -1000.0f64..1000.0)) {
            let plane = Plane::from_point_normal(origin, n);
            let f = plane.frame();
            prop_assert!(f.u.dot(f.v).abs() < 1e-9);
            prop_assert!(f.u.cross(f.v).distance(f.normal) < 1e-9);
            let p2 = DVec2::new(uv.0, uv.1);
            let p3 = f.from_2d(p2);
            prop_assert!(plane.distance(p3) < 1e-6);
            prop_assert!(f.to_2d(p3).distance(p2) < 1e-6);
        }

        /// Points on a plane stay on its transform under any invertible affine
        /// map, and the normal stays unit.
        #[test]
        fn transform_preserves_membership(
            n in arb_normal(),
            origin in arb_vec(100.0),
            cols in prop::array::uniform3(arb_vec(2.0)),
            shift in arb_vec(100.0),
            uv in (-100.0f64..100.0, -100.0f64..100.0),
        ) {
            let m = DMat3::from_cols(cols[0], cols[1], cols[2]);
            prop_assume!(m.determinant().abs() > 0.05);
            let t = Tolerances::default();
            let affine = DAffine3::from_mat3_translation(m, shift);
            let plane = Plane::from_point_normal(origin, n);
            let moved = plane.transformed(&affine);
            prop_assert!((moved.normal.length() - 1.0).abs() < 1e-9);
            let p = plane.frame().from_2d(DVec2::new(uv.0, uv.1));
            prop_assert!(moved.contains(affine.transform_point3(p), &t), "{}", moved.distance(affine.transform_point3(p)));
        }
    }
}
