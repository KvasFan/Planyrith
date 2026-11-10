//! Implements docs/03-geometry-kernel.md §4.20 (sheet metal and frames,
//! milestone 1.14): prisms between two planes (members with mitered ends),
//! the profile library of structural members, and sheet-metal parts folded
//! from a flat outline with flanges, with their flat pattern.

use glam::{DVec2, DVec3};

use crate::geom::boolean::{BooleanOp, Piece, boolean, stitch};
use crate::geom::loft::loft;
use crate::geom::planar::reverse_faces;
use crate::geom::revolve::revolve;
use crate::geom::{FaceSides, GeometryError, HalfEdgeMesh, NoDeltas};
use crate::tolerance::Tolerances;

/// A plane by a point and a normal.
pub type PlaneAt = (DVec3, DVec3);

fn signed_area(points: &[DVec2]) -> f64 {
    let n = points.len();
    (0..n)
        .map(|i| points[i].perp_dot(points[(i + 1) % n]))
        .sum::<f64>()
        / 2.0
}

/// `loops` (outer first, then holes) with the outer counter-clockwise and
/// the holes clockwise.
fn oriented(loops: &[Vec<DVec2>]) -> Vec<Vec<DVec2>> {
    loops
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let mut l = l.clone();
            let ccw = signed_area(&l) > 0.0;
            if (i == 0) != ccw {
                l.reverse();
            }
            l
        })
        .collect()
}

/// Where a 2D profile lies in space: its origin and axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProfileFrame {
    /// The profile's (0, 0).
    pub origin: DVec3,
    /// Its x axis.
    pub u: DVec3,
    /// Its y axis.
    pub v: DVec3,
}

/// A prism of the 2D `loops` (in `frame`) along
/// `dir`, from the plane `start` to the plane `end`: each corner runs along
/// `dir` until it meets the planes, so ends can be mitered.
pub fn prism(
    frame: ProfileFrame,
    loops: &[Vec<DVec2>],
    dir: DVec3,
    start: PlaneAt,
    end: PlaneAt,
    tol: &Tolerances,
) -> Result<HalfEdgeMesh, GeometryError> {
    let dir = dir
        .try_normalize()
        .ok_or_else(|| GeometryError::NotApplicable("a prism needs a direction".into()))?;
    if loops.first().is_none_or(|l| l.len() < 3) {
        return Err(GeometryError::NotApplicable(
            "a prism needs a closed profile".into(),
        ));
    }
    let loops = oriented(loops);
    let ProfileFrame { origin, u, v } = frame;
    let hit = |p: DVec2, (at, n): PlaneAt| -> Result<DVec3, GeometryError> {
        let q = origin + u * p.x + v * p.y;
        let d = dir.dot(n);
        if d.abs() < 1e-9 {
            return Err(GeometryError::NotApplicable(
                "an end plane runs along the member".into(),
            ));
        }
        Ok(q + dir * ((at - q).dot(n) / d))
    };
    let mut bottom = Vec::new();
    let mut top = Vec::new();
    for l in &loops {
        bottom.push(
            l.iter()
                .map(|p| hit(*p, start))
                .collect::<Result<Vec<_>, _>>()?,
        );
        top.push(
            l.iter()
                .map(|p| hit(*p, end))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    let sides = FaceSides::default();
    let mut pieces = vec![
        Piece {
            loops: bottom
                .iter()
                .map(|l| l.iter().rev().copied().collect())
                .collect(),
            sides: sides.clone(),
        },
        Piece {
            loops: top.clone(),
            sides: sides.clone(),
        },
    ];
    for (b, t) in bottom.iter().zip(&top) {
        let n = b.len();
        for i in 0..n {
            let j = (i + 1) % n;
            pieces.push(Piece {
                loops: vec![vec![b[i], b[j], t[j], t[i]]],
                sides: sides.clone(),
            });
        }
    }
    let mut solid = stitch(&pieces, tol)?;
    if !solid.is_solid() {
        return Err(GeometryError::NonRobust(
            "the member's ends cross; shorten the miters".into(),
        ));
    }
    if solid.volume()? < 0.0 {
        let all = solid.faces().map(|(f, _)| f).collect();
        reverse_faces(&mut solid, &all, tol, &mut NoDeltas)?;
    }
    Ok(solid)
}

/// The 2D loops of a structural profile, centered on its bounding box, by
/// name: `square-tube 40x40x3`, `rect-tube 60x40x3`, `round-tube 42.4x3.2`,
/// `round 20`, `flat 40x5`, `angle 50x50x5`, `channel 100x50x6`,
/// `i-beam 100x55x4.1x5.7` or `IPE100`.
pub fn profile(name: &str) -> Result<(Vec<Vec<DVec2>>, String), GeometryError> {
    let bad = || {
        GeometryError::NotApplicable(format!(
            "unknown profile `{name}`: square-tube WxWxT, rect-tube WxHxT, round-tube DxT, \
             round D, flat WxT, angle AxBxT, channel HxWxT, i-beam HxWxTwxTf or IPE80–IPE300"
        ))
    };
    let name = name.trim();
    let ipe: [(&str, [f64; 4]); 10] = [
        ("IPE80", [80.0, 46.0, 3.8, 5.2]),
        ("IPE100", [100.0, 55.0, 4.1, 5.7]),
        ("IPE120", [120.0, 64.0, 4.4, 6.3]),
        ("IPE140", [140.0, 73.0, 4.7, 6.9]),
        ("IPE160", [160.0, 82.0, 5.0, 7.4]),
        ("IPE180", [180.0, 91.0, 5.3, 8.0]),
        ("IPE200", [200.0, 100.0, 5.6, 8.5]),
        ("IPE220", [220.0, 110.0, 5.9, 9.2]),
        ("IPE240", [240.0, 120.0, 6.2, 9.8]),
        ("IPE300", [300.0, 150.0, 7.1, 10.7]),
    ];
    let (kind, sizes) = if let Some((_, s)) = ipe.iter().find(|(n, _)| n.eq_ignore_ascii_case(name))
    {
        ("i-beam", s.to_vec())
    } else {
        let (kind, dims) = name.split_once(' ').ok_or_else(bad)?;
        let sizes = dims
            .split(['x', 'X', '×'])
            .map(|s| s.trim().parse::<f64>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| bad())?;
        (kind, sizes)
    };
    if sizes.iter().any(|s| !(s.is_finite() && *s > 0.0)) {
        return Err(bad());
    }
    let rect = |w: f64, h: f64| {
        vec![
            DVec2::new(-w / 2.0, -h / 2.0),
            DVec2::new(w / 2.0, -h / 2.0),
            DVec2::new(w / 2.0, h / 2.0),
            DVec2::new(-w / 2.0, h / 2.0),
        ]
    };
    let circle = |d: f64| {
        (0..32)
            .map(|i| {
                let t = f64::from(i) * std::f64::consts::TAU / 32.0;
                DVec2::new(d / 2.0 * libm::cos(t), d / 2.0 * libm::sin(t))
            })
            .collect::<Vec<_>>()
    };
    let canonical = format!(
        "{kind} {}",
        sizes
            .iter()
            .map(|s| format!("{s}"))
            .collect::<Vec<_>>()
            .join("x")
    );
    let loops = match (kind.to_ascii_lowercase().as_str(), sizes.as_slice()) {
        ("square-tube", [w, _, t]) | ("square-tube", [w, t]) if 2.0 * t < *w => {
            vec![rect(*w, *w), rect(w - 2.0 * t, w - 2.0 * t)]
        }
        ("rect-tube", [w, h, t]) if 2.0 * t < w.min(*h) => {
            vec![rect(*w, *h), rect(w - 2.0 * t, h - 2.0 * t)]
        }
        ("round-tube", [d, t]) if 2.0 * t < *d => vec![circle(*d), circle(d - 2.0 * t)],
        ("round", [d]) => vec![circle(*d)],
        ("flat", [w, t]) => vec![rect(*w, *t)],
        ("angle", [a, b, t]) if t < a && t < b => {
            let (x0, y0) = (-a / 2.0, -b / 2.0);
            vec![vec![
                DVec2::new(x0, y0),
                DVec2::new(x0 + a, y0),
                DVec2::new(x0 + a, y0 + t),
                DVec2::new(x0 + t, y0 + t),
                DVec2::new(x0 + t, y0 + b),
                DVec2::new(x0, y0 + b),
            ]]
        }
        ("channel", [h, w, t]) if 2.0 * t < *h && t < w => {
            let (x0, y0) = (-w / 2.0, -h / 2.0);
            vec![vec![
                DVec2::new(x0, y0),
                DVec2::new(x0 + w, y0),
                DVec2::new(x0 + w, y0 + t),
                DVec2::new(x0 + t, y0 + t),
                DVec2::new(x0 + t, y0 + h - t),
                DVec2::new(x0 + w, y0 + h - t),
                DVec2::new(x0 + w, y0 + h),
                DVec2::new(x0, y0 + h),
            ]]
        }
        ("i-beam", [h, w, tw, tf]) if 2.0 * tf < *h && tw < w => {
            let (hw, hh, ht) = (w / 2.0, h / 2.0, tw / 2.0);
            vec![vec![
                DVec2::new(-hw, -hh),
                DVec2::new(hw, -hh),
                DVec2::new(hw, -hh + tf),
                DVec2::new(ht, -hh + tf),
                DVec2::new(ht, hh - tf),
                DVec2::new(hw, hh - tf),
                DVec2::new(hw, hh),
                DVec2::new(-hw, hh),
                DVec2::new(-hw, hh - tf),
                DVec2::new(-ht, hh - tf),
                DVec2::new(-ht, -hh + tf),
                DVec2::new(-hw, -hh + tf),
            ]]
        }
        _ => return Err(bad()),
    };
    Ok((loops, canonical))
}

/// One flange of a sheet-metal part.
#[derive(
    Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct Flange {
    /// The outline edge it stands on (from point `edge` to the next).
    pub edge: usize,
    /// How far it bends up, radians (0 to π).
    pub angle_rad: f64,
    /// Its straight length beyond the bend, mm.
    pub length_mm: f64,
}

/// A sheet-metal part: a flat outline with flanges on its edges.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SheetMetal {
    /// The outline's plane: a point.
    pub origin: [f64; 3],
    /// Its first axis.
    pub u: [f64; 3],
    /// Its second axis (the sheet's top faces `u × v`).
    pub v: [f64; 3],
    /// The outline, counter-clockwise in (u, v), mm.
    pub outline: Vec<[f64; 2]>,
    /// Thickness, mm.
    pub thickness_mm: f64,
    /// Inside bend radius, mm.
    pub bend_radius_mm: f64,
    /// Where the neutral layer lies, as a share of the thickness.
    pub k_factor: f64,
    /// Flanges, one per edge at most.
    pub flanges: Vec<Flange>,
}

impl SheetMetal {
    fn frame(&self) -> (DVec3, DVec3, DVec3, DVec3) {
        let u = DVec3::from_array(self.u);
        let v = DVec3::from_array(self.v);
        (DVec3::from_array(self.origin), u, v, u.cross(v).normalize())
    }

    fn points(&self) -> Vec<DVec2> {
        let mut p: Vec<DVec2> = self.outline.iter().map(|q| DVec2::from_array(*q)).collect();
        if signed_area(&p) < 0.0 {
            p.reverse();
        }
        p
    }

    /// The bend allowance of a flange: the neutral layer's arc length.
    pub fn bend_allowance(&self, f: &Flange) -> f64 {
        f.angle_rad * (self.bend_radius_mm + self.k_factor * self.thickness_mm)
    }

    /// Checks the description.
    pub fn check(&self) -> Result<(), GeometryError> {
        let bad = |m: &str| Err(GeometryError::NotApplicable(m.into()));
        if !(self.thickness_mm > 0.0 && self.thickness_mm.is_finite()) {
            return bad("the thickness must be positive");
        }
        if !(self.bend_radius_mm >= 0.0 && self.bend_radius_mm.is_finite()) {
            return bad("the bend radius must be zero or more");
        }
        if !(0.0..=1.0).contains(&self.k_factor) {
            return bad("the K factor is between 0 and 1");
        }
        let n = self.outline.len();
        if n < 3 {
            return bad("the outline needs three corners");
        }
        let mut seen = std::collections::BTreeSet::new();
        for f in &self.flanges {
            if f.edge >= n || !seen.insert(f.edge) {
                return bad("one flange per outline edge");
            }
            if !(f.angle_rad > 0.0 && f.angle_rad <= std::f64::consts::PI) {
                return bad("a flange bends more than 0° and at most 180°");
            }
            if !(f.length_mm > 0.0 && f.length_mm.is_finite()) {
                return bad("a flange's length must be positive");
            }
        }
        Ok(())
    }

    /// The folded part: the plate, each bend and each flange's leg, joined.
    pub fn folded(&self, tol: &Tolerances) -> Result<HalfEdgeMesh, GeometryError> {
        self.check()?;
        let (o, u, v, w) = self.frame();
        let t = self.thickness_mm;
        let pts = self.points();
        let frame = ProfileFrame { origin: o, u, v };
        let mut solid = prism(
            frame,
            std::slice::from_ref(&pts),
            w,
            (o, w),
            (o + w * t, w),
            tol,
        )?;
        let at = |p: DVec2| o + u * p.x + v * p.y;
        for f in &self.flanges {
            let (a2, b2) = (pts[f.edge], pts[(f.edge + 1) % pts.len()]);
            let (a, b) = (at(a2), at(b2));
            let along = (b - a).normalize();
            // Outward, in the plate's plane.
            let out = along.cross(w).normalize();
            let axis_dir = out.cross(w);
            let r = self.bend_radius_mm;
            let face = vec![a, b, b + w * t, a + w * t];
            let mut parts = Vec::new();
            if r + t > tol.length {
                let bend = revolve(
                    std::slice::from_ref(&face),
                    a + w * (t + r),
                    axis_dir,
                    f.angle_rad,
                    32,
                    FaceSides::default(),
                    tol,
                )?;
                parts.push(bend);
            }
            // The leg starts where the bend ends.
            let rot = crate::math::transform::rotation_about(
                a + w * (t + r),
                axis_dir,
                crate::units::Angle::radians(f.angle_rad),
            );
            let start: Vec<DVec3> = face.iter().map(|p| rot.transform_point3(*p)).collect();
            let leg_dir = rot.transform_vector3(out);
            let end: Vec<DVec3> = start.iter().map(|p| *p + leg_dir * f.length_mm).collect();
            parts.push(loft(&[start, end], FaceSides::default(), tol)?);
            for p in parts {
                let joined = boolean(
                    BooleanOp::Union,
                    &solid,
                    &glam::DAffine3::IDENTITY,
                    &p,
                    &glam::DAffine3::IDENTITY,
                    tol,
                )?;
                solid = joined.solids.into_iter().next().ok_or_else(|| {
                    GeometryError::NonRobust("a flange did not join the plate".into())
                })?;
            }
        }
        Ok(solid)
    }

    /// The flat pattern in its own plane: the outline with each flange's
    /// bend allowance and length added outward, and the bend zones' lines.
    pub fn flat(&self) -> (Vec<DVec2>, Vec<[DVec2; 2]>) {
        let pts = self.points();
        let n = pts.len();
        let mut outline = Vec::new();
        let mut bends = Vec::new();
        for i in 0..n {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            outline.push(a);
            if let Some(f) = self.flanges.iter().find(|f| f.edge == i) {
                let along = (b - a).normalize();
                let out = DVec2::new(along.y, -along.x);
                let ba = self.bend_allowance(f);
                let width = ba + f.length_mm;
                outline.push(a + out * width);
                outline.push(b + out * width);
                bends.push([a, b]);
                bends.push([a + out * ba, b + out * ba]);
            }
        }
        (outline, bends)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tol() -> Tolerances {
        Tolerances::default()
    }

    #[test]
    fn a_mitered_square_tube() {
        let (loops, name) = profile("square-tube 40x40x3").unwrap();
        assert_eq!(name, "square-tube 40x40x3");
        // A 1000 mm member along +X, both ends square.
        let m = prism(
            ProfileFrame {
                origin: DVec3::ZERO,
                u: DVec3::Y,
                v: DVec3::Z,
            },
            &loops,
            DVec3::X,
            (DVec3::ZERO, DVec3::X),
            (DVec3::new(1000.0, 0.0, 0.0), DVec3::X),
            &tol(),
        )
        .unwrap();
        let area = 40.0 * 40.0 - 34.0 * 34.0;
        assert!((m.volume().unwrap() - area * 1000.0).abs() < 1e-6);
        // Mitered at 45° at the far end: the same volume (the miter adds
        // and removes equal wedges).
        let n = (DVec3::X + DVec3::Y).normalize();
        let m = prism(
            ProfileFrame {
                origin: DVec3::ZERO,
                u: DVec3::Y,
                v: DVec3::Z,
            },
            &loops,
            DVec3::X,
            (DVec3::ZERO, DVec3::X),
            (DVec3::new(1000.0, 0.0, 0.0), n),
            &tol(),
        )
        .unwrap();
        assert!((m.volume().unwrap() - area * 1000.0).abs() < 1e-6);
        assert!(profile("IPE200").is_ok());
        assert!(profile("angle 50x50x5").is_ok());
        assert!(profile("channel 100x50x6").is_ok());
        assert!(profile("round-tube 42.4x3.2").is_ok());
        assert!(profile("tube 40").is_err());
    }

    fn tray(flanges: Vec<Flange>) -> SheetMetal {
        SheetMetal {
            origin: [0.0; 3],
            u: [1.0, 0.0, 0.0],
            v: [0.0, 1.0, 0.0],
            outline: vec![[0.0, 0.0], [200.0, 0.0], [200.0, 100.0], [0.0, 100.0]],
            thickness_mm: 2.0,
            bend_radius_mm: 2.0,
            k_factor: 0.44,
            flanges,
        }
    }

    #[test]
    fn a_flanged_sheet_folds_and_unfolds() {
        let half = std::f64::consts::FRAC_PI_2;
        let s = tray(vec![Flange {
            edge: 0,
            angle_rad: half,
            length_mm: 30.0,
        }]);
        let folded = s.folded(&tol()).unwrap();
        assert!(folded.is_solid());
        // Plate + quarter ring + leg.
        let ring = half / 2.0 * ((2.0f64 + 2.0).powi(2) - 2.0f64.powi(2)) * 200.0;
        let expect = 200.0 * 100.0 * 2.0 + ring + 30.0 * 2.0 * 200.0;
        let got = folded.volume().unwrap();
        // The bend is a polygon of 32 pieces per turn: within 1 %.
        assert!((got - expect).abs() < 0.01 * expect, "{got} vs {expect}");
        // The flat pattern adds the bend allowance and the leg.
        let (outline, bends) = s.flat();
        let ba = half * (2.0 + 0.44 * 2.0);
        let ys: Vec<f64> = outline.iter().map(|p| p.y).collect();
        let lowest = ys.iter().copied().fold(f64::INFINITY, f64::min);
        assert!((lowest + ba + 30.0).abs() < 1e-9, "{lowest}");
        assert_eq!(bends.len(), 2);
        let mut bad = tray(vec![Flange {
            edge: 7,
            angle_rad: half,
            length_mm: 30.0,
        }]);
        assert!(bad.check().is_err());
        bad.flanges.clear();
        bad.thickness_mm = 0.0;
        assert!(bad.check().is_err());
    }
}
