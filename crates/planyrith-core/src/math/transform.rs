//! Implements `DAffine3` helpers for docs/03 §1 (instances may be mirrored or
//! non-uniformly scaled), docs/04 §3 (instance transforms) and the `Transform`
//! CLI parameter of docs/08 §2 (`t=x,y,z;r=axis,angle;s=sx,sy,sz`).

#![warn(unused_variables, dead_code)]

use std::fmt;

use glam::{DAffine3, DMat3, DQuat, DVec3};

use crate::math::Axis;
use crate::tolerance::Tolerances;
use crate::units::{Angle, LengthUnit, UnitsError, parse_angle, parse_length};

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
    /// lengths accept units (bare numbers are millimeters), the angle accepts
    /// `deg`/`rad` (default degrees). Parts are optional and may come in any
    /// order; each may appear once. `r` takes an axis name (`z`, `-x`) or a
    /// direction `dx,dy,dz`; `s` takes one uniform factor or three.
    pub fn parse(text: &str) -> Result<Self, UnitsError> {
        Self::parse_with_unit(text, LengthUnit::Millimeter)
    }

    /// [`TransformParts::parse`] with the unit bare translation numbers use.
    pub fn parse_with_unit(text: &str, default_unit: LengthUnit) -> Result<Self, UnitsError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(UnitsError::Empty);
        }
        let malformed = |why: String| UnitsError::Malformed(text.to_string(), why);
        let mut parts = TransformParts::default();
        let mut seen_t = false;
        let mut seen_r = false;
        let mut seen_s = false;
        for piece in text.split(';') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            let (key, value) = piece
                .split_once('=')
                .ok_or_else(|| malformed(format!("expected `key=value`, found `{piece}`")))?;
            let fields: Vec<&str> = value.split(',').map(str::trim).collect();
            match key.trim() {
                "t" => {
                    if seen_t {
                        return Err(malformed("`t=` given twice".to_string()));
                    }
                    seen_t = true;
                    let [x, y, z] = fields[..] else {
                        return Err(malformed("`t=` needs three lengths `x,y,z`".to_string()));
                    };
                    parts.translation = DVec3::new(
                        parse_length(x, default_unit)?.as_mm(),
                        parse_length(y, default_unit)?.as_mm(),
                        parse_length(z, default_unit)?.as_mm(),
                    );
                }
                "r" => {
                    if seen_r {
                        return Err(malformed("`r=` given twice".to_string()));
                    }
                    seen_r = true;
                    let (axis, angle) = match fields[..] {
                        [axis, angle] => {
                            let (axis, sign) = Axis::parse_signed(axis).ok_or_else(|| {
                                malformed(format!(
                                    "unknown axis `{axis}` (expected x|y|z|-x|-y|-z or dx,dy,dz)"
                                ))
                            })?;
                            (axis.direction() * sign, angle)
                        }
                        [dx, dy, dz, angle] => {
                            let dir = DVec3::new(
                                parse_plain_number(dx, text)?,
                                parse_plain_number(dy, text)?,
                                parse_plain_number(dz, text)?,
                            );
                            if dir.length_squared() <= 0.0 || !dir.is_finite() {
                                return Err(malformed(
                                    "rotation axis must be non-zero".to_string(),
                                ));
                            }
                            (dir, angle)
                        }
                        _ => {
                            return Err(malformed(
                                "`r=` needs `axis,angle` or `dx,dy,dz,angle`".to_string(),
                            ));
                        }
                    };
                    parts.rotation = Some((axis.normalize(), parse_angle(angle)?));
                }
                "s" => {
                    if seen_s {
                        return Err(malformed("`s=` given twice".to_string()));
                    }
                    seen_s = true;
                    parts.scale = match fields[..] {
                        [f] => DVec3::splat(parse_plain_number(f, text)?),
                        [x, y, z] => DVec3::new(
                            parse_plain_number(x, text)?,
                            parse_plain_number(y, text)?,
                            parse_plain_number(z, text)?,
                        ),
                        _ => {
                            return Err(malformed(
                                "`s=` needs one factor or three `sx,sy,sz`".to_string(),
                            ));
                        }
                    };
                }
                other => {
                    return Err(malformed(format!(
                        "unknown part `{other}` (expected `t`, `r` or `s`)"
                    )));
                }
            }
        }
        Ok(parts)
    }

    /// `true` when this is the identity (no translation, no rotation, unit
    /// scale) within `tol`.
    pub fn is_identity(&self, tol: &Tolerances) -> bool {
        self.translation.abs().max_element() <= tol.length
            && self
                .rotation
                .is_none_or(|(_, a)| a.abs().as_radians() <= tol.angle)
            && (self.scale - DVec3::ONE).abs().max_element() <= tol.parametric
    }
}

impl fmt::Display for TransformParts {
    /// The CLI syntax `t=x,y,z;r=dx,dy,dz,angle;s=sx,sy,sz` (lengths in mm with
    /// the `mm` suffix, the angle in degrees with `deg`). Parses back with
    /// [`TransformParts::parse`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let t = self.translation;
        write!(f, "t={}mm,{}mm,{}mm", t.x, t.y, t.z)?;
        if let Some((axis, angle)) = self.rotation {
            write!(
                f,
                ";r={},{},{},{}deg",
                axis.x,
                axis.y,
                axis.z,
                angle.to_degrees()
            )?;
        }
        let s = self.scale;
        write!(f, ";s={},{},{}", s.x, s.y, s.z)
    }
}

/// A plain decimal factor or direction component (no unit).
fn parse_plain_number(text: &str, whole: &str) -> Result<f64, UnitsError> {
    let text = text.trim();
    let value: f64 = text
        .parse()
        .map_err(|_| UnitsError::InvalidNumber(text.to_string()))?;
    if !value.is_finite() {
        return Err(UnitsError::Malformed(
            whole.to_string(),
            format!("`{text}` is not a finite number"),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

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
    fn parses_cli_transform_syntax() {
        let t = Tolerances::default();
        let p = TransformParts::parse("t=0,0,0;r=z,90").unwrap();
        assert_eq!(p.translation, DVec3::ZERO);
        let (axis, angle) = p.rotation.unwrap();
        assert_eq!(axis, DVec3::Z);
        assert!((angle.to_degrees() - 90.0).abs() < 1e-12);
        assert_eq!(p.scale, DVec3::ONE);

        let p = TransformParts::parse("t=1m,2cm,-3;s=2").unwrap();
        assert_eq!(p.translation, DVec3::new(1000.0, 20.0, -3.0));
        assert_eq!(p.rotation, None);
        assert_eq!(p.scale, DVec3::splat(2.0));

        let p = TransformParts::parse(" s=1,2,3 ; r=0,0,2,0.5rad ; t=1,1,1; ").unwrap();
        assert_eq!(p.scale, DVec3::new(1.0, 2.0, 3.0));
        let (axis, angle) = p.rotation.unwrap();
        assert_eq!(axis, DVec3::Z);
        assert!((angle.as_radians() - 0.5).abs() < 1e-12);

        let p = TransformParts::parse("r=-x,45").unwrap();
        assert_eq!(p.rotation.unwrap().0, -DVec3::X);
        assert!(TransformParts::parse("t=0,0,0").unwrap().is_identity(&t));
        assert!(!p.is_identity(&t));
        let metres = TransformParts::parse_with_unit("t=1,0,0", LengthUnit::Meter).unwrap();
        assert!((metres.translation.x - 1000.0).abs() < 1e-9);

        for bad in [
            "",
            "t=1,2",
            "t=1,2,3;t=4,5,6",
            "r=w,90",
            "r=0,0,0,90",
            "r=90",
            "s=1,2",
            "q=1",
            "t=1,2,3;r",
            "s=abc",
            "t=1km,0,0",
        ] {
            assert!(TransformParts::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn display_round_trips() {
        let parts = TransformParts {
            translation: DVec3::new(10.0, -2.5, 3.0),
            rotation: Some((DVec3::new(0.0, 0.6, 0.8), Angle::degrees(30.0))),
            scale: DVec3::new(1.0, 2.0, 0.5),
        };
        let text = parts.to_string();
        let back = TransformParts::parse(&text).unwrap();
        assert!(
            back.translation.distance(parts.translation) < 1e-9,
            "{text}"
        );
        let (axis, angle) = back.rotation.unwrap();
        assert!(axis.distance(DVec3::new(0.0, 0.6, 0.8)) < 1e-9);
        assert!((angle.to_degrees() - 30.0).abs() < 1e-9);
        assert_eq!(back.scale, parts.scale);
        let p = DVec3::new(3.0, 4.0, 5.0);
        assert!(
            back.to_affine()
                .transform_point3(p)
                .distance(parts.to_affine().transform_point3(p))
                < 1e-9
        );
    }

    #[test]
    fn normals_under_non_uniform_scale() {
        let t = DAffine3::from_scale(DVec3::new(2.0, 1.0, 1.0));
        let n = transform_normal(&t, DVec3::new(1.0, 1.0, 0.0).normalize());
        // The surface x + y = c stretched in x has normal (1, 2, 0)/√5.
        assert!(n.distance(DVec3::new(1.0, 2.0, 0.0).normalize()) < 1e-9);
    }

    fn arb_vec(range: f64) -> impl Strategy<Value = DVec3> {
        (-range..range, -range..range, -range..range).prop_map(|(x, y, z)| DVec3::new(x, y, z))
    }

    proptest! {
        /// `Display` output parses back to the same affine map.
        #[test]
        fn display_parse_round_trip(
            translation in arb_vec(1.0e6),
            axis in arb_vec(1.0),
            deg in -360.0f64..360.0,
            scale in arb_vec(10.0),
            p in arb_vec(1000.0),
        ) {
            prop_assume!(axis.length() > 0.1);
            let parts = TransformParts {
                translation,
                rotation: Some((axis.normalize(), Angle::degrees(deg))),
                scale,
            };
            let text = parts.to_string();
            let back = TransformParts::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            let a = parts.to_affine().transform_point3(p);
            let b = back.to_affine().transform_point3(p);
            prop_assert!(a.distance(b) <= 1e-9 * a.length().max(1.0), "{text}: {a} vs {b}");
        }

        /// `rotation_about` fixes the center and preserves distances; scaling
        /// about a center fixes it too, and mirrored scales are detected.
        #[test]
        fn rotation_and_scale_about_center(
            center in arb_vec(1000.0), axis in arb_vec(1.0), deg in -360.0f64..360.0,
            p in arb_vec(1000.0), factors in arb_vec(5.0),
        ) {
            prop_assume!(axis.length() > 0.1);
            let t = Tolerances::default();
            let r = rotation_about(center, axis, Angle::degrees(deg));
            prop_assert!(r.transform_point3(center).distance(center) < 1e-6);
            let q = r.transform_point3(p);
            prop_assert!((q.distance(center) - p.distance(center)).abs() < 1e-6);
            prop_assert!(!is_mirrored(&r));
            prop_assume!(factors.abs().min_element() > 0.01);
            let s = scale_about(center, factors);
            prop_assert!(s.transform_point3(center).distance(center) < 1e-6);
            prop_assert_eq!(is_mirrored(&s), factors.x * factors.y * factors.z < 0.0);
            prop_assert!(is_invertible(&s, &t));
        }
    }
}
