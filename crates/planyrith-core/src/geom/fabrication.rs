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

/// A further bend at the free end of a flange: a flange on a flange
/// (milestone 1.34).
#[derive(
    Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct Bend {
    /// How far it bends, radians: positive continues toward the flange's
    /// inside (a return or a hem), negative bends the other way (a Z-bend);
    /// at most π either way.
    pub angle_rad: f64,
    /// Its straight length beyond the bend, mm.
    pub length_mm: f64,
}

/// One flange of a sheet-metal part.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Flange {
    /// The outline edge it stands on (from point `edge` to the next).
    pub edge: usize,
    /// How far it bends up, radians (0 to π).
    pub angle_rad: f64,
    /// Its straight length beyond the bend, mm.
    pub length_mm: f64,
    /// Flanges standing on its free end, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub more: Vec<Bend>,
}

impl Flange {
    /// Its bends in order, the first one included.
    pub fn bends(&self) -> Vec<Bend> {
        std::iter::once(Bend {
            angle_rad: self.angle_rad,
            length_mm: self.length_mm,
        })
        .chain(self.more.iter().copied())
        .collect()
    }
}

/// The shape of a corner relief.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ReliefShape {
    /// A square (a rhombus at other angles) with sides along both edges.
    Square,
    /// A circle centered on the corner.
    Round,
}

/// Material cut from the plate where two flanges meet at a convex corner.
#[derive(
    Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CornerRelief {
    /// Its shape.
    pub shape: ReliefShape,
    /// How far it reaches along each edge from the corner, mm.
    pub size_mm: f64,
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
    /// Reliefs at the corners where two flanges meet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relief: Option<CornerRelief>,
}

/// The most bends one flange may carry, the first included.
pub const MAX_BENDS: usize = 8;

/// One bend and leg of a flange, placed in space.
struct Step {
    /// The end face the bend turns: bottom edge `a`–`b`, then the top.
    face: Vec<DVec3>,
    /// A point on the bend's axis.
    pivot: DVec3,
    /// The axis direction (the rotation is positive about it).
    axis: DVec3,
    /// The bend's angle, radians (positive).
    angle: f64,
    /// Where the leg runs.
    leg: DVec3,
    /// The leg's length.
    length: f64,
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
        self.allowance(f.angle_rad)
    }

    /// The bend allowance of a bend by `angle` either way.
    pub fn allowance(&self, angle: f64) -> f64 {
        angle.abs() * (self.bend_radius_mm + self.k_factor * self.thickness_mm)
    }

    fn flange_on(&self, edge: usize) -> Option<&Flange> {
        self.flanges.iter().find(|f| f.edge == edge)
    }

    /// Which corners are relieved: convex corners whose two edges both carry
    /// flanges, when the part has a relief.
    pub fn relieved_corners(&self) -> Vec<bool> {
        let pts = self.points();
        let n = pts.len();
        (0..n)
            .map(|i| {
                let prev = (i + n - 1) % n;
                let d_in = pts[i] - pts[prev];
                let d_out = pts[(i + 1) % n] - pts[i];
                self.relief.is_some()
                    && self.flange_on(prev).is_some()
                    && self.flange_on(i).is_some()
                    && d_in.perp_dot(d_out) > 0.0
            })
            .collect()
    }

    /// The part of outline edge `i` a flange stands on: the whole edge, less
    /// the relief at a relieved end.
    fn edge_span(&self, pts: &[DVec2], relieved: &[bool], i: usize) -> (DVec2, DVec2) {
        let n = pts.len();
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let along = (b - a).normalize_or_zero();
        let s = self.relief.map_or(0.0, |r| r.size_mm);
        let a = if relieved[i] { a + along * s } else { a };
        let b = if relieved[(i + 1) % n] {
            b - along * s
        } else {
            b
        };
        (a, b)
    }

    /// The plate outline's points at corner `i`: the corner itself, or the
    /// relief cut around it.
    fn corner_points(&self, pts: &[DVec2], relieved: &[bool], i: usize) -> Vec<DVec2> {
        let n = pts.len();
        let p = pts[i];
        let (true, Some(relief)) = (relieved[i], self.relief) else {
            return vec![p];
        };
        let s = relief.size_mm;
        let d_in = (p - pts[(i + n - 1) % n]).normalize_or_zero();
        let d_out = (pts[(i + 1) % n] - p).normalize_or_zero();
        let mut out = vec![p - d_in * s];
        match relief.shape {
            ReliefShape::Square => out.push(p - d_in * s + d_out * s),
            ReliefShape::Round => {
                // Clockwise through the plate from −d_in to d_out.
                let from = libm::atan2(-d_in.y, -d_in.x);
                let mut to = libm::atan2(d_out.y, d_out.x);
                if to > from {
                    to -= std::f64::consts::TAU;
                }
                let steps = 8;
                for k in 1..steps {
                    let t = from + (to - from) * f64::from(k) / f64::from(steps);
                    out.push(p + DVec2::new(libm::cos(t), libm::sin(t)) * s);
                }
            }
        }
        out.push(p + d_out * s);
        out
    }

    /// The plate's outline: the outline with relieved corners cut away.
    pub fn plate_outline(&self) -> Vec<DVec2> {
        let pts = self.points();
        let relieved = self.relieved_corners();
        (0..pts.len())
            .flat_map(|i| self.corner_points(&pts, &relieved, i))
            .collect()
    }

    /// The bends and legs of flange `f`, in space, and its free end's
    /// bottom edge.
    fn steps(&self, f: &Flange, pts: &[DVec2], relieved: &[bool]) -> (Vec<Step>, [DVec3; 2]) {
        let (o, u, v, w) = self.frame();
        let at = |p: DVec2| o + u * p.x + v * p.y;
        let (a2, b2) = self.edge_span(pts, relieved, f.edge);
        let (mut a, mut b) = (at(a2), at(b2));
        let t = self.thickness_mm;
        let r = self.bend_radius_mm;
        let along = (b - a).normalize_or_zero();
        // Outward in the plate's plane, and the side the first bend turns to.
        let mut out = along.cross(w).normalize_or_zero();
        let mut up = w;
        let mut steps = Vec::new();
        for bend in f.bends() {
            let face = vec![a, b, b + up * t, a + up * t];
            let toward = out.cross(up);
            let (pivot, axis) = if bend.angle_rad >= 0.0 {
                (a + up * (t + r), toward)
            } else {
                (a - up * r, -toward)
            };
            let angle = bend.angle_rad.abs();
            let rot = crate::math::transform::rotation_about(
                pivot,
                axis,
                crate::units::Angle::radians(angle),
            );
            let leg = rot.transform_vector3(out);
            steps.push(Step {
                face,
                pivot,
                axis,
                angle,
                leg,
                length: bend.length_mm,
            });
            a = rot.transform_point3(a) + leg * bend.length_mm;
            b = rot.transform_point3(b) + leg * bend.length_mm;
            up = rot.transform_vector3(up);
            out = leg;
        }
        (steps, [a, b])
    }

    /// Each flange's outline edge and the bottom edge of its free end.
    pub fn free_ends(&self) -> Vec<(usize, [DVec3; 2])> {
        let pts = self.points();
        let relieved = self.relieved_corners();
        self.flanges
            .iter()
            .map(|f| (f.edge, self.steps(f, &pts, &relieved).1))
            .collect()
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
            if f.more.len() + 1 > MAX_BENDS {
                return bad("a flange carries at most eight bends");
            }
            for b in f.bends() {
                if !(b.angle_rad != 0.0 && b.angle_rad.abs() <= std::f64::consts::PI) {
                    return bad(
                        "a flange on a flange bends more than 0° and at most 180° either way",
                    );
                }
                if !(b.length_mm > 0.0 && b.length_mm.is_finite()) {
                    return bad("a flange's length must be positive");
                }
            }
        }
        if let Some(r) = self.relief
            && !(r.size_mm > 0.0 && r.size_mm.is_finite())
        {
            return bad("a corner relief's size must be positive");
        }
        let pts = self.points();
        let relieved = self.relieved_corners();
        for f in &self.flanges {
            let (a, b) = self.edge_span(&pts, &relieved, f.edge);
            let along = pts[(f.edge + 1) % n] - pts[f.edge];
            if (b - a).dot(along.normalize_or_zero()) <= 1e-6 {
                return bad("the corner reliefs leave no edge for a flange; make them smaller");
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
        let relieved = self.relieved_corners();
        let frame = ProfileFrame { origin: o, u, v };
        let plate = self.plate_outline();
        let mut solid = prism(
            frame,
            std::slice::from_ref(&plate),
            w,
            (o, w),
            (o + w * t, w),
            tol,
        )?;
        let r = self.bend_radius_mm;
        for f in &self.flanges {
            let (steps, _) = self.steps(f, &pts, &relieved);
            let mut parts = Vec::new();
            for step in steps {
                if r + t > tol.length {
                    parts.push(revolve(
                        std::slice::from_ref(&step.face),
                        step.pivot,
                        step.axis,
                        step.angle,
                        32,
                        FaceSides::default(),
                        tol,
                    )?);
                }
                // The leg starts where the bend ends.
                let rot = crate::math::transform::rotation_about(
                    step.pivot,
                    step.axis,
                    crate::units::Angle::radians(step.angle),
                );
                let start: Vec<DVec3> =
                    step.face.iter().map(|p| rot.transform_point3(*p)).collect();
                let end: Vec<DVec3> = start.iter().map(|p| *p + step.leg * step.length).collect();
                parts.push(loft(&[start, end], FaceSides::default(), tol)?);
            }
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

    /// The flat pattern in its own plane: the plate outline (reliefs cut)
    /// with each flange's bend allowances and lengths added outward, and the
    /// bend zones' lines.
    pub fn flat(&self) -> (Vec<DVec2>, Vec<[DVec2; 2]>) {
        let pts = self.points();
        let relieved = self.relieved_corners();
        let n = pts.len();
        let mut outline = Vec::new();
        let mut bends = Vec::new();
        for i in 0..n {
            outline.extend(self.corner_points(&pts, &relieved, i));
            if let Some(f) = self.flange_on(i) {
                let (a, b) = self.edge_span(&pts, &relieved, i);
                let along = (b - a).normalize();
                let out = DVec2::new(along.y, -along.x);
                let mut d = 0.0;
                for bend in f.bends() {
                    let ba = self.allowance(bend.angle_rad);
                    bends.push([a + out * d, b + out * d]);
                    bends.push([a + out * (d + ba), b + out * (d + ba)]);
                    d += ba + bend.length_mm;
                }
                outline.push(a + out * d);
                outline.push(b + out * d);
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
            relief: None,
        }
    }

    #[test]
    fn a_flanged_sheet_folds_and_unfolds() {
        let half = std::f64::consts::FRAC_PI_2;
        let s = tray(vec![Flange {
            edge: 0,
            angle_rad: half,
            length_mm: 30.0,
            more: Vec::new(),
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
            more: Vec::new(),
        }]);
        assert!(bad.check().is_err());
        bad.flanges.clear();
        bad.thickness_mm = 0.0;
        assert!(bad.check().is_err());
    }

    fn flange(edge: usize, angle_rad: f64, length_mm: f64, more: Vec<Bend>) -> Flange {
        Flange {
            edge,
            angle_rad,
            length_mm,
            more,
        }
    }

    #[test]
    fn flanges_stand_on_flanges() {
        let half = std::f64::consts::FRAC_PI_2;
        // Inside radius 2 and thickness 2: a quarter ring per bend.
        let ring = half / 2.0 * ((2.0f64 + 2.0).powi(2) - 2.0f64.powi(2)) * 200.0;
        for (second, name) in [(half, "return"), (-half, "Z")] {
            let s = tray(vec![flange(
                0,
                half,
                30.0,
                vec![Bend {
                    angle_rad: second,
                    length_mm: 15.0,
                }],
            )]);
            let folded = s.folded(&tol()).unwrap();
            assert!(folded.is_solid(), "{name}");
            let expect = 200.0 * 100.0 * 2.0 + 2.0 * ring + (30.0 + 15.0) * 2.0 * 200.0;
            let got = folded.volume().unwrap();
            assert!(
                (got - expect).abs() < 0.01 * expect,
                "{name}: {got} vs {expect}"
            );
            // The free end: after the first leg it stands 34 mm up on the
            // outside face; the second bend carries it back over the plate
            // (return) or away from it (Z), at the leg's height.
            let [a, _] = s.free_ends()[0].1;
            if second > 0.0 {
                assert!(a.y > -4.0 + 15.0 - 1e-6 && a.z > 34.0, "{name}: {a}");
            } else {
                assert!(a.y < -4.0 - 15.0 + 1e-6 && a.z > 34.0, "{name}: {a}");
            }
            // Flat: both allowances and lengths outward, four bend lines.
            let (outline, bends) = s.flat();
            let ba = half * (2.0 + 0.44 * 2.0);
            let lowest = outline.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
            assert!((lowest + 2.0 * ba + 45.0).abs() < 1e-9, "{name}: {lowest}");
            assert_eq!(bends.len(), 4);
        }
        // Too many bends, a zero bend.
        let mut bad = tray(vec![flange(
            0,
            half,
            30.0,
            vec![
                Bend {
                    angle_rad: 0.0,
                    length_mm: 5.0
                };
                1
            ],
        )]);
        assert!(bad.check().is_err());
        bad.flanges[0].more = vec![
            Bend {
                angle_rad: 0.2,
                length_mm: 5.0
            };
            MAX_BENDS
        ];
        assert!(bad.check().is_err());
    }

    #[test]
    fn corner_reliefs_cut_the_plate_and_the_blank() {
        let half = std::f64::consts::FRAC_PI_2;
        let ring_area = half / 2.0 * ((2.0f64 + 2.0).powi(2) - 2.0f64.powi(2));
        let leg = ring_area + 30.0 * 2.0;
        let mut s = tray(vec![
            flange(0, half, 30.0, vec![]),
            flange(1, half, 30.0, vec![]),
        ]);
        // Without a relief the flanges keep their whole edges.
        let plain = s.folded(&tol()).unwrap().volume().unwrap();
        let expect = 200.0 * 100.0 * 2.0 + leg * (200.0 + 100.0);
        assert!((plain - expect).abs() < 0.01 * expect);
        s.relief = Some(CornerRelief {
            shape: ReliefShape::Square,
            size_mm: 5.0,
        });
        // Only the corner between the two flanged edges is relieved.
        assert_eq!(s.relieved_corners(), vec![false, true, false, false]);
        let folded = s.folded(&tol()).unwrap();
        assert!(folded.is_solid());
        let expect = (200.0 * 100.0 - 25.0) * 2.0 + leg * (195.0 + 95.0);
        let got = folded.volume().unwrap();
        assert!((got - expect).abs() < 0.01 * expect, "{got} vs {expect}");
        let (outline, _) = s.flat();
        for q in [
            DVec2::new(195.0, 0.0),
            DVec2::new(195.0, 5.0),
            DVec2::new(200.0, 5.0),
        ] {
            assert!(outline.iter().any(|p| p.distance(q) < 1e-9), "{q} missing");
        }
        // The blank's strips start at the relief.
        let ba = half * (2.0 + 0.44 * 2.0);
        assert!(
            outline
                .iter()
                .any(|p| p.distance(DVec2::new(195.0, -(ba + 30.0))) < 1e-9)
        );
        assert!(
            outline
                .iter()
                .any(|p| p.distance(DVec2::new(200.0 + ba + 30.0, 5.0)) < 1e-9)
        );
        // A round relief removes a quarter disk (eight chords).
        s.relief = Some(CornerRelief {
            shape: ReliefShape::Round,
            size_mm: 5.0,
        });
        let folded = s.folded(&tol()).unwrap();
        assert!(folded.is_solid());
        let quarter = 0.5 * 25.0 * 8.0 * libm::sin(half / 8.0);
        let expect = (200.0 * 100.0 - quarter) * 2.0 + leg * (195.0 + 95.0);
        let got = folded.volume().unwrap();
        assert!((got - expect).abs() < 0.01 * expect, "{got} vs {expect}");
        // Reliefs larger than an edge leave no flange.
        s.relief = Some(CornerRelief {
            shape: ReliefShape::Square,
            size_mm: 150.0,
        });
        assert!(s.check().is_err());
    }
}
