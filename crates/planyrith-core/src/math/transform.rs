//! Implements `DAffine3` helpers for docs/03 §1 (instances may be mirrored or
//! non-uniformly scaled), docs/04 §3 (instance transforms) and the `Transform`
//! CLI parameter of docs/08 §2 (`t=x,y,z;r=axis,angle;s=sx,sy,sz`).

use glam::{DAffine3, DMat3, DQuat, DVec3};

use crate::tolerance::Tolerances;
use crate::units::Angle;

/// Pure translation.
pub fn translation(delta: DVec3) -> DAffine3 {
    DAffine3::from_translation(delta)
}

/// Rotation by `angle` about the line through `origin` with direction `axis`.
pub fn rotation_about(origin: DVec3, axis: DVec3, angle: Angle) -> DAffine3 {
    let q = DQuat::from_axis_angle(axis.normalize_or_zero(), angle.as_radians());
    DAffine3::from_translation(origin)
        * DAffine3::from_quat(q)
        * DAffine3::from_translation(-origin)
}

/// Non-uniform scale about `origin`.
pub fn scale_about(origin: DVec3, factors: DVec3) -> DAffine3 {
    DAffine3::from_translation(origin)
        * DAffine3::from_scale(factors)
        * DAffine3::from_translation(-origin)
}

/// `true` when the linear part has a negative determinant (mirrored instance).
pub fn is_mirrored(t: &DAffine3) -> bool {
    t.matrix3.determinant() < 0.0
}

/// `true` when the linear part is invertible: `|det| > tol.parametric`
/// (docs/04 §13 flags non-invertible transforms).
pub fn is_invertible(t: &DAffine3, tol: &Tolerances) -> bool {
    let det = t.matrix3.determinant();
    det.is_finite() && det.abs() > tol.parametric
}

/// Transforms a normal with the inverse transpose of the linear part and
/// re-normalizes (correct for non-uniform scale and shear).
pub fn transform_normal(t: &DAffine3, normal: DVec3) -> DVec3 {
    let inv_t: DMat3 = t.matrix3.inverse().transpose();
    (inv_t * normal).normalize_or_zero()
}

/// Decomposed form used by the CLI `Transform` syntax (docs/08 §2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformParts {
    /// Translation `t=x,y,z` in mm.
    pub translation: DVec3,
    /// Rotation `r=axis,angle` (axis as a direction, angle in radians).
    pub rotation: Option<(DVec3, Angle)>,
    /// Scale `s=sx,sy,sz`.
    pub scale: DVec3,
}

impl Default for TransformParts {
    fn default() -> Self {
        TransformParts {
            translation: DVec3::ZERO,
            rotation: None,
            scale: DVec3::ONE,
        }
    }
}

impl TransformParts {
    /// Composes `T · R · S` about the origin.
    pub fn to_affine(&self) -> DAffine3 {
        let r = match self.rotation {
            Some((axis, angle)) => rotation_about(DVec3::ZERO, axis, angle),
            None => DAffine3::IDENTITY,
        };
        translation(self.translation) * r * DAffine3::from_scale(self.scale)
    }

    /// Parses `t=x,y,z;r=axis,angle;s=sx,sy,sz` (docs/08 §2 `Transform`);
    /// lengths accept units, the angle accepts `deg`/`rad` (default degrees).
    pub fn parse(text: &str) -> Result<Self, crate::units::UnitsError> {
        todo!("docs/08 §2 Transform parameter syntax")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_and_invertibility() {
        let t = DAffine3::from_scale(DVec3::new(-1.0, 1.0, 1.0));
        assert!(is_mirrored(&t));
        assert!(is_invertible(&t, &Tolerances::default()));
        let flat = DAffine3::from_scale(DVec3::new(1.0, 1.0, 0.0));
        assert!(!is_invertible(&flat, &Tolerances::default()));
    }

    #[test]
    fn rotation_about_keeps_origin_fixed() {
        let o = DVec3::new(10.0, 5.0, 0.0);
        let r = rotation_about(o, DVec3::Z, Angle::degrees(90.0));
        assert!(r.transform_point3(o).distance(o) < 1e-9);
        let p = r.transform_point3(o + DVec3::X);
        assert!(p.distance(o + DVec3::Y) < 1e-9);
    }

    #[test]
    fn normals_under_non_uniform_scale() {
        let t = DAffine3::from_scale(DVec3::new(2.0, 1.0, 1.0));
        let n = transform_normal(&t, DVec3::new(1.0, 1.0, 0.0).normalize());
        // The surface x + y = c stretched in x has normal (1, 2, 0)/√5.
        assert!(n.distance(DVec3::new(1.0, 2.0, 0.0).normalize()) < 1e-9);
    }
}
