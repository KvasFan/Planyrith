//! Implements plane utilities for docs/03-geometry-kernel.md §3–§4 (face
//! planes, coplanarity, 2D frames used by face finding and tessellation).

use glam::{DAffine3, DVec2, DVec3};
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
    /// (`(b-a) × (c-a)`), or `None` if they are collinear within tolerance.
    pub fn from_points(a: DVec3, b: DVec3, c: DVec3, tol: &Tolerances) -> Option<Self> {
        todo!("docs/03 §4.3 plane from three points")
    }

    /// Least-squares plane fit through `points` using centroid + covariance
    /// (docs/03 §8). Returns `None` for fewer than three non-collinear points.
    pub fn fit(points: &[DVec3], tol: &Tolerances) -> Option<Self> {
        todo!("docs/03 §8 plane fit via centroid and covariance")
    }

    /// Signed distance of `p` to the plane (positive on the front side).
    pub fn signed_distance(&self, p: DVec3) -> f64 {
        self.normal.dot(p) + self.d
    }

    /// Orthogonal projection of `p` onto the plane.
    pub fn project(&self, p: DVec3) -> DVec3 {
        p - self.normal * self.signed_distance(p)
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

    /// `true` when the plane is vertical (normal perpendicular to Z within
    /// `tol.angle`), used by the default face orientation rule of docs/03 §4.3.
    pub fn is_vertical(&self, tol: &Tolerances) -> bool {
        self.normal.z.abs() <= tol.angle
    }

    /// A deterministic 2D frame on the plane: `u` is the projection of the
    /// world axis least aligned with the normal, `v = normal × u`, origin as
    /// [`Plane::origin`].
    pub fn frame(&self) -> PlaneFrame {
        todo!("docs/03 §4.3 deterministic plane frame")
    }

    /// The plane transformed by `t` (normals use the inverse transpose; mirrored
    /// transforms keep the front side consistent with the transformed loop).
    pub fn transformed(&self, t: &DAffine3) -> Self {
        todo!("docs/03 §1 plane transform under non-orthonormal affine")
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
    /// 2D coordinates of a 3D point (projected onto the plane first).
    pub fn to_2d(&self, p: DVec3) -> DVec2 {
        let rel = p - self.origin;
        DVec2::new(rel.dot(self.u), rel.dot(self.v))
    }

    /// 3D point on the plane for 2D frame coordinates.
    pub fn from_2d(&self, p: DVec2) -> DVec3 {
        self.origin + self.u * p.x + self.v * p.y
    }

    /// The plane this frame lies on.
    pub fn plane(&self) -> Plane {
        Plane::from_point_normal(self.origin, self.normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_distance_and_projection() {
        let p = Plane::from_point_normal(DVec3::new(0.0, 0.0, 10.0), DVec3::Z * 3.0);
        assert!((p.signed_distance(DVec3::new(1.0, 2.0, 12.0)) - 2.0).abs() < 1e-12);
        let q = p.project(DVec3::new(1.0, 2.0, 12.0));
        assert!((q.z - 10.0).abs() < 1e-12);
        assert!((p.flipped().signed_distance(DVec3::new(0.0, 0.0, 12.0)) + 2.0).abs() < 1e-12);
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
}
