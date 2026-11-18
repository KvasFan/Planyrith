//! Implements docs/03-geometry-kernel.md §4.23 and docs/05 §8.18:
//! a compensated part, two boolean-cut blanks and a planar, stepped or
//! freeform parting surface, undercuts and side actions (milestone 1.48).

use glam::{DAffine3, DVec2, DVec3};

use crate::geom::boolean::{BooleanOp, boolean, piece, stitch};
use crate::geom::loft::loft;
use crate::geom::mesh::{IndexedEdge, IndexedMesh, transform_indexed};
use crate::geom::section::slice;
use crate::geom::{EdgeFlags, FaceSides, GeometryError, HalfEdgeMesh};
use crate::math::Plane;
use crate::tolerance::Tolerances;

/// The meshes and measurements of a planar two-half mold.
pub struct MoldMesh {
    /// Material on the negative side of the parting plane.
    pub core: HalfEdgeMesh,
    /// Material on the positive side of the parting plane.
    pub cavity: HalfEdgeMesh,
    /// Wire edges along the compensated part's section.
    pub parting_line: HalfEdgeMesh,
    /// Blank's plane outside the compensated part, with section holes.
    pub parting_surface: HalfEdgeMesh,
    /// Compensated part volume, mm³.
    pub compensated_volume_mm3: f64,
    /// Total section perimeter, mm.
    pub parting_length_mm: f64,
    /// Whether either boolean used the optional B-rep backend.
    pub fallback_used: bool,
}

fn bad(message: &str) -> GeometryError {
    GeometryError::NotApplicable(message.into())
}

/// Splits an enclosing blank on `plane` and subtracts a scaled copy of
/// `part` from each half. The source is never modified. `margin` is the
/// blank's clearance on all six sides, in the plane's orthonormal frame.
pub fn split(
    part: &HalfEdgeMesh,
    plane: Plane,
    margin: f64,
    shrink_factor: f64,
    origin: DVec3,
    tol: &Tolerances,
) -> Result<MoldMesh, GeometryError> {
    if !part.is_solid() || part.volume()? <= tol.length * tol.length_sq {
        return Err(bad("a mold needs a closed, outward-facing solid"));
    }
    if !margin.is_finite()
        || margin <= tol.planarity
        || !shrink_factor.is_finite()
        || shrink_factor <= 0.0
        || !tol.in_range(origin)
        || !plane.normal.is_finite()
        || !plane.d.is_finite()
        || (plane.normal.length() - 1.0).abs() > tol.angle
    {
        return Err(bad(
            "give a finite unit plane, positive shrink factor and a margin above the planarity tolerance",
        ));
    }
    let transform = DAffine3::from_translation(origin)
        * DAffine3::from_scale(DVec3::splat(shrink_factor))
        * DAffine3::from_translation(-origin);
    let (source, _) = part.to_indexed();
    let compensated = HalfEdgeMesh::from_indexed(&transform_indexed(&source, &transform), tol)?;
    let frame = plane.frame();
    let mut low = DVec3::splat(f64::INFINITY);
    let mut high = DVec3::splat(f64::NEG_INFINITY);
    for (_, v) in compensated.vertices() {
        let xy = frame.to_2d(v.position);
        let p = DVec3::new(xy.x, xy.y, plane.signed_distance(v.position));
        low = low.min(p);
        high = high.max(p);
    }
    if low.z >= -tol.planarity || high.z <= tol.planarity {
        return Err(bad("the parting plane must cross the compensated part"));
    }
    low -= DVec3::splat(margin);
    high += DVec3::splat(margin);
    let square = |z: f64| -> Vec<DVec3> {
        [
            DVec2::new(low.x, low.y),
            DVec2::new(high.x, low.y),
            DVec2::new(high.x, high.y),
            DVec2::new(low.x, high.y),
        ]
        .map(|p| frame.from_2d(p) + plane.normal * z)
        .to_vec()
    };
    let mut halves = Vec::with_capacity(2);
    let mut fallback_used = false;
    for (bottom, top) in [(low.z, 0.0), (0.0, high.z)] {
        let blank = loft(&[square(bottom), square(top)], FaceSides::default(), tol)?;
        let result = boolean(
            BooleanOp::Subtract,
            &blank,
            &DAffine3::IDENTITY,
            &compensated,
            &DAffine3::IDENTITY,
            tol,
        )?;
        fallback_used |= result.fallback_used;
        let mut faces = Vec::new();
        for solid in &result.solids {
            for (f, _) in solid.faces() {
                faces.push(piece(solid, f, false)?);
            }
        }
        if faces.is_empty() {
            return Err(bad("the subtraction left an empty mold half"));
        }
        let mesh = stitch(&faces, tol)?;
        if !mesh.is_solid() || mesh.volume()? <= tol.length * tol.length_sq {
            return Err(bad("the subtraction did not leave a solid mold half"));
        }
        halves.push(mesh);
    }
    let cavity = halves.pop().ok_or_else(|| bad("missing cavity"))?;
    let core = halves.pop().ok_or_else(|| bad("missing core"))?;
    let mut surface = Vec::new();
    for (f, face) in core.faces() {
        if face.plane.normal.dot(plane.normal) >= 1.0 - tol.angle
            && core.face_vertices(f)?.iter().all(|v| {
                core.position(*v)
                    .is_ok_and(|p| plane.signed_distance(p).abs() <= tol.planarity)
            })
        {
            surface.push(piece(&core, f, false)?);
        }
    }
    if surface.is_empty() {
        return Err(bad("could not extract the parting surface"));
    }
    let parting_surface = stitch(&surface, tol)?;
    let segments = slice(&compensated, &plane, tol);
    let mut wire = IndexedMesh::default();
    for (a, b) in segments {
        let mut vertex = |p: DVec3| -> u32 {
            if let Some(i) = wire.vertices.iter().position(|q| tol.same_point(p, *q)) {
                i as u32
            } else {
                wire.vertices.push(p);
                (wire.vertices.len() - 1) as u32
            }
        };
        let (a, b) = (vertex(a), vertex(b));
        if a != b
            && !wire
                .edges
                .iter()
                .any(|e| (e.a == a && e.b == b) || (e.a == b && e.b == a))
        {
            wire.edges.push(IndexedEdge {
                a,
                b,
                flags: EdgeFlags::NONE,
                curve: None,
            });
        }
    }
    let mut degree = vec![0; wire.vertices.len()];
    let mut parting_length_mm = 0.0;
    for e in &wire.edges {
        degree[e.a as usize] += 1;
        degree[e.b as usize] += 1;
        parting_length_mm += wire.vertices[e.a as usize].distance(wire.vertices[e.b as usize]);
    }
    if wire.edges.is_empty() || degree.iter().any(|d| *d != 2) {
        return Err(bad(
            "the parting line is not closed; move the plane away from coincident edges",
        ));
    }
    Ok(MoldMesh {
        core,
        cavity,
        parting_line: HalfEdgeMesh::from_indexed(&wire, tol)?,
        parting_surface,
        compensated_volume_mm3: compensated.volume()?,
        parting_length_mm,
        fallback_used,
    })
}

/// A stepped parting surface (milestone 1.40): from the base plane through
/// `origin` (normal `normal`), the surface rises to `offset` beyond each
/// step's position `at` along `axis` (a unit direction in the plane).
#[derive(Debug, Clone, PartialEq)]
pub struct Stepped {
    /// A point of the base plane.
    pub origin: DVec3,
    /// From core toward cavity (unit).
    pub normal: DVec3,
    /// Along which the steps are placed (unit, in the plane).
    pub axis: DVec3,
    /// `(at, offset)` pairs, increasing `at`.
    pub steps: Vec<(f64, f64)>,
}

impl Stepped {
    fn frame(&self) -> (DVec3, DVec3, DVec3) {
        (self.axis, self.normal.cross(self.axis), self.normal)
    }

    /// The surface's height over the base plane at `u` along the axis.
    pub fn height_at(&self, u: f64) -> f64 {
        let mut h = 0.0;
        for &(at, offset) in &self.steps {
            if u >= at {
                h = offset;
            }
        }
        h
    }

    /// How far above the stepped surface `p` lies (along the normal).
    pub fn above(&self, p: DVec3) -> f64 {
        let (u, _, n) = self.frame();
        let d = p - self.origin;
        d.dot(n) - self.height_at(d.dot(u))
    }

    /// The staircase in (along the axis, height) from `x0` to `x1`.
    fn staircase(&self, x0: f64, x1: f64) -> Vec<DVec2> {
        let mut out = vec![DVec2::new(x0, 0.0)];
        let mut h = 0.0;
        for &(at, offset) in &self.steps {
            if (offset - h).abs() <= 1e-9 {
                continue;
            }
            out.push(DVec2::new(at, h));
            out.push(DVec2::new(at, offset));
            h = offset;
        }
        out.push(DVec2::new(x1, h));
        out
    }
}

/// Splits an enclosing blank along a stepped surface and subtracts a
/// scaled copy of `part` from each side (docs/03 §4.23, milestone 1.40).
/// The parting surface is the core's faces on the stepped surface; the
/// parting line is its border inside the blank.
pub fn split_stepped(
    part: &HalfEdgeMesh,
    surface: &Stepped,
    margin: f64,
    shrink_factor: f64,
    origin: DVec3,
    tol: &Tolerances,
) -> Result<MoldMesh, GeometryError> {
    use crate::geom::fabrication::{ProfileFrame, prism};
    if !part.is_solid() || part.volume()? <= tol.length * tol.length_sq {
        return Err(bad("a mold needs a closed, outward-facing solid"));
    }
    if !margin.is_finite()
        || margin <= tol.planarity
        || !shrink_factor.is_finite()
        || shrink_factor <= 0.0
    {
        return Err(bad(
            "give a positive shrink factor and a margin above the planarity tolerance",
        ));
    }
    if surface.steps.windows(2).any(|w| w[1].0 <= w[0].0)
        || surface
            .steps
            .iter()
            .any(|(a, o)| !a.is_finite() || !o.is_finite())
    {
        return Err(bad("give steps at increasing positions"));
    }
    let transform = DAffine3::from_translation(origin)
        * DAffine3::from_scale(DVec3::splat(shrink_factor))
        * DAffine3::from_translation(-origin);
    let (source, _) = part.to_indexed();
    let compensated = HalfEdgeMesh::from_indexed(&transform_indexed(&source, &transform), tol)?;
    let (u, v, n) = surface.frame();
    let o = surface.origin;
    let mut low = DVec3::splat(f64::INFINITY);
    let mut high = DVec3::splat(f64::NEG_INFINITY);
    let mut above = false;
    let mut below = false;
    for (_, x) in compensated.vertices() {
        let d = x.position - o;
        let q = DVec3::new(d.dot(u), d.dot(v), d.dot(n));
        low = low.min(q);
        high = high.max(q);
        let a = surface.above(x.position);
        above |= a > tol.planarity;
        below |= a < -tol.planarity;
    }
    if !above || !below {
        return Err(bad("the parting surface must cross the compensated part"));
    }
    low -= DVec3::splat(margin);
    high += DVec3::splat(margin);
    for &(at, offset) in &surface.steps {
        if at <= low.x || at >= high.x || offset <= low.z || offset >= high.z {
            return Err(bad("every step must lie inside the blank"));
        }
    }
    let stairs = surface.staircase(low.x, high.x);
    let mut lower = vec![DVec2::new(low.x, low.z), DVec2::new(high.x, low.z)];
    lower.extend(stairs.iter().rev().copied());
    let mut upper = stairs.clone();
    upper.extend([DVec2::new(high.x, high.z), DVec2::new(low.x, high.z)]);
    let frame = ProfileFrame { origin: o, u, v: n };
    let mut halves = Vec::with_capacity(2);
    let mut fallback_used = false;
    for profile in [lower, upper] {
        let blank = prism(
            frame,
            std::slice::from_ref(&profile),
            v,
            (o + v * low.y, v),
            (o + v * high.y, v),
            tol,
        )?;
        let result = boolean(
            BooleanOp::Subtract,
            &blank,
            &DAffine3::IDENTITY,
            &compensated,
            &DAffine3::IDENTITY,
            tol,
        )?;
        fallback_used |= result.fallback_used;
        let mut faces = Vec::new();
        for solid in &result.solids {
            for (f, _) in solid.faces() {
                faces.push(piece(solid, f, false)?);
            }
        }
        if faces.is_empty() {
            return Err(bad("the subtraction left an empty mold half"));
        }
        let mesh = stitch(&faces, tol)?;
        if !mesh.is_solid() || mesh.volume()? <= tol.length * tol.length_sq {
            return Err(bad("the subtraction did not leave a solid mold half"));
        }
        halves.push(mesh);
    }
    let cavity = halves.pop().ok_or_else(|| bad("missing cavity"))?;
    let core = halves.pop().ok_or_else(|| bad("missing core"))?;
    // The core's faces on the staircase.
    let on_stairs = |p: DVec3| {
        let d = p - o;
        let q = DVec2::new(d.dot(u), d.dot(n));
        stairs.windows(2).any(|w| {
            let (a, b) = (w[0], w[1]);
            let ab = b - a;
            let t = if ab.length_squared() > 0.0 {
                ((q - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (a + ab * t).distance(q) <= tol.planarity.max(tol.length)
        })
    };
    let mut parting = Vec::new();
    for (f, _) in core.faces() {
        let corners: Vec<DVec3> = core
            .face_vertices(f)?
            .iter()
            .filter_map(|x| core.position(*x).ok())
            .collect();
        let center = corners.iter().copied().sum::<DVec3>() / corners.len().max(1) as f64;
        if corners.iter().all(|p| on_stairs(*p)) && on_stairs(center) {
            parting.push(piece(&core, f, false)?);
        }
    }
    if parting.is_empty() {
        return Err(bad("could not extract the parting surface"));
    }
    let parting_surface = stitch(&parting, tol)?;
    // The parting line: the surface's border edges inside the blank.
    let edge = tol.length.max(tol.planarity);
    let at_blank = |p: DVec3| {
        let d = p - o;
        let (x, y) = (d.dot(u), d.dot(v));
        (x - low.x).abs() <= edge
            || (x - high.x).abs() <= edge
            || (y - low.y).abs() <= edge
            || (y - high.y).abs() <= edge
    };
    let mut wire = IndexedMesh::default();
    let mut parting_length_mm = 0.0;
    for (e, _) in parting_surface.edges() {
        let [a, b] = parting_surface.edge_faces(e)?;
        if a.is_some() && b.is_some() {
            continue;
        }
        let s = parting_surface.edge_segment(e)?;
        if at_blank(s.a) && at_blank(s.b) {
            continue;
        }
        let mut vertex = |p: DVec3| -> u32 {
            if let Some(i) = wire.vertices.iter().position(|q| tol.same_point(p, *q)) {
                i as u32
            } else {
                wire.vertices.push(p);
                (wire.vertices.len() - 1) as u32
            }
        };
        let (ia, ib) = (vertex(s.a), vertex(s.b));
        parting_length_mm += s.a.distance(s.b);
        wire.edges.push(IndexedEdge {
            a: ia,
            b: ib,
            flags: EdgeFlags::NONE,
            curve: None,
        });
    }
    if wire.edges.is_empty() {
        return Err(bad("the parting surface does not meet the part"));
    }
    Ok(MoldMesh {
        core,
        cavity,
        parting_line: HalfEdgeMesh::from_indexed(&wire, tol)?,
        parting_surface,
        compensated_volume_mm3: compensated.volume()?,
        parting_length_mm,
        fallback_used,
    })
}

/// Why a face of a molded part cannot be pulled out of its half.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum UndercutKind {
    /// It faces against its half's pull.
    Facing,
    /// It faces along the pull, but the part stands in the way.
    Shadowed,
}

/// One undercut face.
#[derive(Debug, Clone, PartialEq)]
pub struct Undercut {
    /// The face of the source mesh.
    pub face: crate::ids::FaceId,
    /// On the cavity side (pulled along the normal); else the core side.
    pub cavity: bool,
    /// Why.
    pub kind: UndercutKind,
    /// Its area, mm² (world).
    pub area_mm2: f64,
}

/// The faces of `mesh` (placed in the world by `world`) that cannot be
/// released (milestone 1.40): each face belongs to the half on its side of
/// `surface`, pulled along the normal (cavity) or against it (core). A face
/// turned against its pull is an undercut; one turned with it is when a ray
/// from its middle along the pull meets the part.
pub fn undercuts(
    mesh: &HalfEdgeMesh,
    world: &DAffine3,
    surface: &Stepped,
    tol: &Tolerances,
) -> Result<Vec<Undercut>, GeometryError> {
    undercuts_of(mesh, world, surface, tol)
}

/// [`undercuts`] for any parting surface: a stepped one or a freeform one
/// (milestone 1.48).
pub fn undercuts_of(
    mesh: &HalfEdgeMesh,
    world: &DAffine3,
    surface: &dyn Parting,
    tol: &Tolerances,
) -> Result<Vec<Undercut>, GeometryError> {
    use crate::geom::tessellate::tessellate_face;
    let normal_matrix = world.matrix3.inverse().transpose();
    let mut triangles: Vec<(crate::ids::FaceId, [DVec3; 3])> = Vec::new();
    let mut faces = Vec::new();
    for (f, face) in mesh.faces() {
        let t = tessellate_face(mesh, f, tol)?;
        let mut area = 0.0;
        let mut center = DVec3::ZERO;
        for tri in &t.triangles {
            let p = tri.map(|i| world.transform_point3(t.positions[i as usize]));
            let a = 0.5 * (p[1] - p[0]).cross(p[2] - p[0]).length();
            area += a;
            center += (p[0] + p[1] + p[2]) / 3.0 * a;
            triangles.push((f, p));
        }
        if area <= 0.0 {
            continue;
        }
        let n = (normal_matrix * face.plane.normal).normalize_or_zero();
        faces.push((f, center / area, n, area));
    }
    let mut out = Vec::new();
    for (f, center, n, area) in faces {
        let cavity = surface.above(center) > 0.0;
        let pull = if cavity {
            surface.pull()
        } else {
            -surface.pull()
        };
        let along = n.dot(pull);
        let kind = if along < -1e-9 {
            Some(UndercutKind::Facing)
        } else if along > 1e-9 {
            let start = center + n * tol.length * 10.0;
            ray_hits(&triangles, f, start, pull, tol).then_some(UndercutKind::Shadowed)
        } else {
            None
        };
        if let Some(kind) = kind {
            out.push(Undercut {
                face: f,
                cavity,
                kind,
                area_mm2: area,
            });
        }
    }
    Ok(out)
}

// ----- freeform parting surfaces and side actions (milestone 1.48) ----------------------

/// Which side of a parting surface a point lies on, and the pull.
pub trait Parting {
    /// How far above the surface `p` lies, along the pull of the cavity
    /// (negative below).
    fn above(&self, p: DVec3) -> f64;
    /// The cavity's pull (unit, from core toward cavity).
    fn pull(&self) -> DVec3;
}

impl Parting for Stepped {
    fn above(&self, p: DVec3) -> f64 {
        Stepped::above(self, p)
    }
    fn pull(&self) -> DVec3 {
        self.normal
    }
}

/// A freeform parting surface (milestone 1.48): faces that one direction
/// sees from one side only (docs/03 §4.21), the cavity on that side.
#[derive(Debug, Clone)]
pub struct Freeform {
    /// The faces (loops, outer first), world millimeters, facing `normal`.
    pub faces: Vec<Vec<Vec<DVec3>>>,
    /// The direction the faces face: the cavity's pull.
    pub normal: DVec3,
    /// The faces as triangles.
    triangles: Vec<[DVec3; 3]>,
}

impl Freeform {
    /// The surface of `faces`, turned to face along `hint` (the requested
    /// normal) when it faces the other way.
    pub fn new(
        faces: Vec<Vec<Vec<DVec3>>>,
        hint: DVec3,
        tol: &Tolerances,
    ) -> Result<Self, GeometryError> {
        let mut faces = faces;
        let mut normal = crate::geom::surface::facing(&faces)?;
        if normal.dot(hint) < 0.0 {
            for face in &mut faces {
                for ring in face.iter_mut() {
                    ring.reverse();
                }
            }
            normal = -normal;
        }
        let mut triangles = Vec::new();
        for face in &faces {
            let Some(outer) = face.first() else { continue };
            let Some(plane) = Plane::fit(outer, tol) else {
                continue;
            };
            let frame = plane.frame_at(outer[0]);
            let ring = |r: &Vec<DVec3>| r.iter().map(|p| frame.to_2d(*p)).collect::<Vec<_>>();
            let holes: Vec<Vec<DVec2>> = face[1..].iter().map(ring).collect();
            let all: Vec<DVec3> = face.iter().flatten().copied().collect();
            let tris = crate::geom::tessellate::tessellate_polygon_2d(&ring(outer), &holes)?;
            for t in tris {
                triangles.push(t.map(|i| all[i as usize]));
            }
        }
        if triangles.is_empty() {
            return Err(bad("the parting surface has no area"));
        }
        Ok(Freeform {
            faces,
            normal,
            triangles,
        })
    }

    /// The signed distance along the normal from the surface to `p`, when
    /// the surface lies over or under `p`.
    pub fn height(&self, p: DVec3) -> Option<f64> {
        let n = self.normal;
        let mut best: Option<f64> = None;
        for t in &self.triangles {
            let (e1, e2) = (t[1] - t[0], t[2] - t[0]);
            let h = n.cross(e2);
            let det = e1.dot(h);
            if det.abs() < 1e-18 {
                continue;
            }
            let s = p - t[0];
            let a = s.dot(h) / det;
            let q = s.cross(e1);
            let b = n.dot(q) / det;
            if a < -1e-9 || b < -1e-9 || a + b > 1.0 + 1e-9 {
                continue;
            }
            // p = t0 + a e1 + b e2 + d n ⇒ d = −(e2·q)/det.
            let d = -e2.dot(q) / det;
            if best.is_none_or(|x| d.abs() < x.abs()) {
                best = Some(d);
            }
        }
        best
    }
}

impl Parting for Freeform {
    fn above(&self, p: DVec3) -> f64 {
        self.height(p).unwrap_or(0.0)
    }
    fn pull(&self) -> DVec3 {
        self.normal
    }
}

/// An orthonormal frame `(u, v, n)` for pull `n`: `u` world red projected
/// across it (else green).
fn pull_frame(n: DVec3) -> (DVec3, DVec3, DVec3) {
    let across = |d: DVec3| (d - n * d.dot(n)).try_normalize();
    let u = across(DVec3::X)
        .filter(|_| n.dot(DVec3::X).abs() < 0.9)
        .or_else(|| across(DVec3::Y))
        .unwrap_or(DVec3::X);
    (u, n.cross(u), n)
}

/// A solid from the pieces of a boolean result (several solids together).
fn joined(result: &crate::geom::boolean::BooleanResult, tol: &Tolerances) -> Option<HalfEdgeMesh> {
    let mut faces = Vec::new();
    for solid in &result.solids {
        for (f, _) in solid.faces() {
            faces.push(piece(solid, f, false).ok()?);
        }
    }
    if faces.is_empty() {
        return None;
    }
    stitch(&faces, tol).ok()
}

/// An axis-aligned box in the frame `(u, v, n)` from `lo` to `hi`
/// (frame coordinates).
fn frame_box(
    (u, v, n): (DVec3, DVec3, DVec3),
    lo: DVec3,
    hi: DVec3,
    tol: &Tolerances,
) -> Result<HalfEdgeMesh, GeometryError> {
    let at = |x: f64, y: f64, z: f64| u * x + v * y + n * z;
    let ring = |z: f64| {
        vec![
            at(lo.x, lo.y, z),
            at(hi.x, lo.y, z),
            at(hi.x, hi.y, z),
            at(lo.x, hi.y, z),
        ]
    };
    loft(&[ring(lo.z), ring(hi.z)], FaceSides::default(), tol)
}

/// Splits an enclosing blank along a freeform surface and subtracts a
/// scaled copy of `part` from each side (milestone 1.48): the cavity is
/// the blank inside the surface's slab (its pull side), the core the rest.
/// The surface must span the blank across the pull, within its height.
pub fn split_freeform(
    part: &HalfEdgeMesh,
    surface: &Freeform,
    margin: f64,
    shrink_factor: f64,
    origin: DVec3,
    tol: &Tolerances,
) -> Result<MoldMesh, GeometryError> {
    if !part.is_solid() || part.volume()? <= tol.length * tol.length_sq {
        return Err(bad("a mold needs a closed, outward-facing solid"));
    }
    if !margin.is_finite()
        || margin <= tol.planarity
        || !shrink_factor.is_finite()
        || shrink_factor <= 0.0
    {
        return Err(bad(
            "give a positive shrink factor and a margin above the planarity tolerance",
        ));
    }
    let transform = DAffine3::from_translation(origin)
        * DAffine3::from_scale(DVec3::splat(shrink_factor))
        * DAffine3::from_translation(-origin);
    let (source, _) = part.to_indexed();
    let compensated = HalfEdgeMesh::from_indexed(&transform_indexed(&source, &transform), tol)?;
    let frame = pull_frame(surface.normal);
    let (u, v, n) = frame;
    let coords = |p: DVec3| DVec3::new(p.dot(u), p.dot(v), p.dot(n));
    let mut low = DVec3::splat(f64::INFINITY);
    let mut high = DVec3::splat(f64::NEG_INFINITY);
    let (mut above, mut below) = (false, false);
    for (_, x) in compensated.vertices() {
        let q = coords(x.position);
        low = low.min(q);
        high = high.max(q);
        match surface.height(x.position) {
            Some(h) if h > tol.planarity => above = true,
            Some(h) if h < -tol.planarity => below = true,
            Some(_) => {}
            None => return Err(bad("the parting surface must reach across the whole part")),
        }
    }
    if !above || !below {
        return Err(bad("the parting surface must cross the compensated part"));
    }
    low -= DVec3::splat(margin);
    high += DVec3::splat(margin);
    // The surface spans the blank: over a grid of its footprint, the
    // surface lies inside the blank's height.
    for i in 0..=8 {
        for j in 0..=8 {
            let x = low.x + (high.x - low.x) * f64::from(i) / 8.0;
            let y = low.y + (high.y - low.y) * f64::from(j) / 8.0;
            let p = u * x + v * y + n * low.z;
            match surface.height(p) {
                Some(h) if -h > tol.planarity && -h < high.z - low.z - tol.planarity => {}
                _ => {
                    return Err(bad(
                        "the parting surface must span the blank (part and margin) across its pull",
                    ));
                }
            }
        }
    }
    let blank = frame_box(frame, low, high, tol)?;
    let lowest = surface
        .faces
        .iter()
        .flatten()
        .flatten()
        .map(|p| p.dot(n))
        .fold(f64::INFINITY, f64::min);
    let slab = crate::geom::surface::slab(&surface.faces, n, high.z - lowest + margin, tol)?;
    let mut halves = Vec::with_capacity(2);
    let mut fallback_used = false;
    for op in [BooleanOp::Subtract, BooleanOp::Intersect] {
        let side = boolean(
            op,
            &blank,
            &DAffine3::IDENTITY,
            &slab,
            &DAffine3::IDENTITY,
            tol,
        )?;
        fallback_used |= side.fallback_used;
        let side =
            joined(&side, tol).ok_or_else(|| bad("the parting surface left an empty half"))?;
        let result = boolean(
            BooleanOp::Subtract,
            &side,
            &DAffine3::IDENTITY,
            &compensated,
            &DAffine3::IDENTITY,
            tol,
        )?;
        fallback_used |= result.fallback_used;
        let mesh =
            joined(&result, tol).ok_or_else(|| bad("the subtraction left an empty mold half"))?;
        if !mesh.is_solid() || mesh.volume()? <= tol.length * tol.length_sq {
            return Err(bad("the subtraction did not leave a solid mold half"));
        }
        halves.push(mesh);
    }
    let cavity = halves.pop().ok_or_else(|| bad("missing cavity"))?;
    let core = halves.pop().ok_or_else(|| bad("missing core"))?;
    let on_surface = |p: DVec3| {
        surface
            .height(p)
            .is_some_and(|h| h.abs() <= tol.planarity.max(tol.length))
    };
    let mut parting = Vec::new();
    for (f, face) in core.faces() {
        if face.plane.normal.dot(n) <= tol.angle {
            continue;
        }
        let corners: Vec<DVec3> = core
            .face_vertices(f)?
            .iter()
            .filter_map(|x| core.position(*x).ok())
            .collect();
        let center = corners.iter().copied().sum::<DVec3>() / corners.len().max(1) as f64;
        if corners.iter().all(|p| on_surface(*p)) && on_surface(center) {
            parting.push(piece(&core, f, false)?);
        }
    }
    if parting.is_empty() {
        return Err(bad("could not extract the parting surface"));
    }
    let parting_surface = stitch(&parting, tol)?;
    let edge = tol.length.max(tol.planarity);
    let at_blank = |p: DVec3| {
        let q = coords(p);
        (q.x - low.x).abs() <= edge
            || (q.x - high.x).abs() <= edge
            || (q.y - low.y).abs() <= edge
            || (q.y - high.y).abs() <= edge
    };
    let mut wire = IndexedMesh::default();
    let mut parting_length_mm = 0.0;
    for (e, _) in parting_surface.edges() {
        let [a, b] = parting_surface.edge_faces(e)?;
        if a.is_some() && b.is_some() {
            continue;
        }
        let s = parting_surface.edge_segment(e)?;
        if at_blank(s.a) && at_blank(s.b) {
            continue;
        }
        let mut vertex = |p: DVec3| -> u32 {
            if let Some(i) = wire.vertices.iter().position(|q| tol.same_point(p, *q)) {
                i as u32
            } else {
                wire.vertices.push(p);
                (wire.vertices.len() - 1) as u32
            }
        };
        let (ia, ib) = (vertex(s.a), vertex(s.b));
        parting_length_mm += s.a.distance(s.b);
        wire.edges.push(IndexedEdge {
            a: ia,
            b: ib,
            flags: EdgeFlags::NONE,
            curve: None,
        });
    }
    if wire.edges.is_empty() {
        return Err(bad("the parting surface does not meet the part"));
    }
    Ok(MoldMesh {
        core,
        cavity,
        parting_line: HalfEdgeMesh::from_indexed(&wire, tol)?,
        parting_surface,
        compensated_volume_mm3: compensated.volume()?,
        parting_length_mm,
        fallback_used,
    })
}

/// A side action (milestone 1.48): a piece of a mold half that slides out
/// sideways, freeing undercut faces.
pub struct SideAction {
    /// Its material.
    pub mesh: HalfEdgeMesh,
    /// The direction it slides out (unit, across the pull).
    pub direction: DVec3,
    /// Taken from the cavity (else the core).
    pub cavity: bool,
    /// The undercut faces it frees.
    pub faces: usize,
}

/// Whether a ray from `start` along `dir` meets a triangle of another face
/// than `own` further than the length tolerance.
fn ray_hits(
    triangles: &[(crate::ids::FaceId, [DVec3; 3])],
    own: crate::ids::FaceId,
    start: DVec3,
    dir: DVec3,
    tol: &Tolerances,
) -> bool {
    triangles.iter().any(|(g, p)| {
        if *g == own {
            return false;
        }
        let (e1, e2) = (p[1] - p[0], p[2] - p[0]);
        let h = dir.cross(e2);
        let det = e1.dot(h);
        if det.abs() < 1e-18 {
            return false;
        }
        let s = start - p[0];
        let a = s.dot(h) / det;
        let q = s.cross(e1);
        let b = dir.dot(q) / det;
        a >= 0.0 && b >= 0.0 && a + b <= 1.0 && e2.dot(q) / det > tol.length
    })
}

/// Cuts side actions out of the halves of `mold` for the undercuts of
/// `part` (milestone 1.48). The part is scaled as for the mold (`origin`,
/// `shrink_factor`). Undercut faces are grouped by half and shared edges;
/// a group slides along the first of `±u`, `±v` across the pull (`u`
/// world red across it, else green) in which none of its faces turns
/// against the motion or
/// is shadowed by the part, the nearest way out of the blank first. Its
/// piece is the half's material in a box over the group's extent across
/// the motion, from the group to the blank's side along it, and from the
/// parting side up to the group's far end along the pull. Returns the side
/// actions and the undercut faces left.
pub fn side_actions(
    part: &HalfEdgeMesh,
    mold: &mut MoldMesh,
    surface: &dyn Parting,
    shrink_factor: f64,
    origin: DVec3,
    tol: &Tolerances,
) -> Result<(Vec<SideAction>, usize), GeometryError> {
    use crate::geom::tessellate::tessellate_face;
    let transform = DAffine3::from_translation(origin)
        * DAffine3::from_scale(DVec3::splat(shrink_factor))
        * DAffine3::from_translation(-origin);
    let (source, _) = part.to_indexed();
    let compensated = HalfEdgeMesh::from_indexed(&transform_indexed(&source, &transform), tol)?;
    let found = undercuts_of(&compensated, &DAffine3::IDENTITY, surface, tol)?;
    if found.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let frame = pull_frame(surface.pull());
    let (u, v, n) = frame;
    let coords = |p: DVec3| DVec3::new(p.dot(u), p.dot(v), p.dot(n));
    // The blank's bounds, from both halves.
    let mut low = DVec3::splat(f64::INFINITY);
    let mut high = DVec3::splat(f64::NEG_INFINITY);
    for half in [&mold.core, &mold.cavity] {
        for (_, x) in half.vertices() {
            let q = coords(x.position);
            low = low.min(q);
            high = high.max(q);
        }
    }
    let mut triangles: Vec<(crate::ids::FaceId, [DVec3; 3])> = Vec::new();
    let mut info: std::collections::BTreeMap<crate::ids::FaceId, (DVec3, DVec3)> =
        Default::default();
    for (f, face) in compensated.faces() {
        let t = tessellate_face(&compensated, f, tol)?;
        let mut area = 0.0;
        let mut center = DVec3::ZERO;
        for tri in &t.triangles {
            let p = tri.map(|i| t.positions[i as usize]);
            let a = 0.5 * (p[1] - p[0]).cross(p[2] - p[0]).length();
            area += a;
            center += (p[0] + p[1] + p[2]) / 3.0 * a;
            triangles.push((f, p));
        }
        if area > 0.0 {
            info.insert(f, (center / area, face.plane.normal));
        }
    }
    // Groups: undercut faces of one half joined by shared edges.
    let undercut: std::collections::BTreeMap<crate::ids::FaceId, bool> =
        found.iter().map(|x| (x.face, x.cavity)).collect();
    let mut groups: Vec<(bool, Vec<crate::ids::FaceId>)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (&f, &cavity) in &undercut {
        if !seen.insert(f) {
            continue;
        }
        let mut group = vec![f];
        let mut k = 0;
        while k < group.len() {
            for g in compensated.face_neighbors(group[k])? {
                if undercut.get(&g) == Some(&cavity) && seen.insert(g) {
                    group.push(g);
                }
            }
            k += 1;
        }
        groups.push((cavity, group));
    }
    let mut actions = Vec::new();
    let mut left = 0;
    for (cavity, group) in groups {
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for &f in &group {
            for x in compensated.face_vertices(f)? {
                let q = coords(compensated.position(x)?);
                lo = lo.min(q);
                hi = hi.max(q);
            }
        }
        // Candidate motions, the shortest way out first.
        let mut options: Vec<(f64, DVec3, usize, bool)> = vec![
            (high.x - hi.x, u, 0, true),
            (lo.x - low.x, -u, 0, false),
            (high.y - hi.y, v, 1, true),
            (lo.y - low.y, -v, 1, false),
        ];
        options.sort_by(|a, b| a.0.total_cmp(&b.0));
        let free = options.into_iter().find(|&(_, dir, _, _)| {
            group.iter().all(|f| {
                let Some(&(center, normal)) = info.get(f) else {
                    return false;
                };
                normal.dot(dir) >= -1e-9
                    && !ray_hits(
                        &triangles,
                        *f,
                        center + normal * tol.length * 10.0,
                        dir,
                        tol,
                    )
            })
        });
        let Some((_, dir, axis, positive)) = free else {
            left += group.len();
            continue;
        };
        let mut b_lo = lo;
        let mut b_hi = hi;
        // Across the motion's other axis: the group's extent. Along the
        // motion: to the blank's side. Along the pull: from beyond the
        // blank on the parting side to the group's far end.
        if axis == 0 {
            if positive {
                b_hi.x = high.x + 1.0;
            } else {
                b_lo.x = low.x - 1.0;
            }
        } else if positive {
            b_hi.y = high.y + 1.0;
        } else {
            b_lo.y = low.y - 1.0;
        }
        if cavity {
            b_lo.z = low.z - 1.0;
        } else {
            b_hi.z = high.z + 1.0;
        }
        let cutter = frame_box(frame, b_lo, b_hi, tol)?;
        let half = if cavity { &mold.cavity } else { &mold.core };
        let inside = boolean(
            BooleanOp::Intersect,
            half,
            &DAffine3::IDENTITY,
            &cutter,
            &DAffine3::IDENTITY,
            tol,
        )?;
        let Some(piece_mesh) = joined(&inside, tol)
            .filter(|m| m.is_solid() && m.volume().is_ok_and(|x| x > tol.length * tol.length_sq))
        else {
            left += group.len();
            continue;
        };
        let rest = boolean(
            BooleanOp::Subtract,
            half,
            &DAffine3::IDENTITY,
            &cutter,
            &DAffine3::IDENTITY,
            tol,
        )?;
        let rest = joined(&rest, tol)
            .filter(|m| m.is_solid())
            .ok_or_else(|| bad("a side action left the mold half in pieces"))?;
        if cavity {
            mold.cavity = rest;
        } else {
            mold.core = rest;
        }
        actions.push(SideAction {
            mesh: piece_mesh,
            direction: dir,
            cavity,
            faces: group.len(),
        });
    }
    Ok((actions, left))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn block(tol: &Tolerances) -> HalfEdgeMesh {
        let ring = |z| {
            vec![
                DVec3::new(0.0, 0.0, z),
                DVec3::new(100.0, 0.0, z),
                DVec3::new(100.0, 60.0, z),
                DVec3::new(0.0, 60.0, z),
            ]
        };
        loft(&[ring(0.0), ring(40.0)], FaceSides::default(), tol).unwrap()
    }

    #[test]
    fn compensated_block_volume_perimeter_and_parting_hole() {
        let tol = Tolerances::default();
        let part = block(&tol);
        let origin = part.aabb().center();
        let result = split(
            &part,
            Plane::from_point_normal(origin, DVec3::Z),
            10.0,
            1.02,
            origin,
            &tol,
        )
        .unwrap();
        let scaled = 100.0 * 60.0 * 40.0 * 1.02f64.powi(3);
        let blank = (102.0 + 20.0) * (61.2 + 20.0) * (40.8 + 20.0);
        assert!((result.compensated_volume_mm3 - scaled).abs() < 1e-5);
        assert!(
            (result.core.volume().unwrap() + result.cavity.volume().unwrap() - (blank - scaled))
                .abs()
                < 1e-5
        );
        assert!((result.parting_length_mm - 320.0 * 1.02).abs() < tol.length);
        assert_eq!(result.parting_surface.faces().count(), 1);
        assert_eq!(
            result.parting_surface.faces().next().unwrap().1.holes.len(),
            1
        );
        assert_eq!(result.parting_line.faces().count(), 0);
        assert_eq!(result.parting_line.edges().count(), 4);
        for m in [
            &result.core,
            &result.cavity,
            &result.parting_line,
            &result.parting_surface,
        ] {
            assert!(m.validate(&tol).is_empty());
        }
    }

    #[test]
    fn oblique_plane_and_invalid_requests() {
        let tol = Tolerances::default();
        let part = block(&tol);
        let origin = part.aabb().center();
        let result = split(
            &part,
            Plane::from_point_normal(origin, DVec3::new(1.0, 1.0, 2.0)),
            10.0,
            1.0,
            origin,
            &tol,
        )
        .unwrap();
        assert!(result.core.is_solid());
        assert!(result.cavity.is_solid());
        assert!(split(&part, Plane::XY, 10.0, 1.0, origin, &tol).is_err());
        assert!(split(&part, Plane::XY, -10.0, 1.0, origin, &tol).is_err());
        assert!(split(&part, Plane::XY, 10.0, f64::NAN, origin, &tol).is_err());
    }

    /// A prism of a profile in (x, z), 60 deep along y.
    fn profile(points: &[(f64, f64)], tol: &Tolerances) -> HalfEdgeMesh {
        use crate::geom::fabrication::{ProfileFrame, prism};
        let loops = vec![
            points
                .iter()
                .map(|(x, z)| DVec2::new(*x, *z))
                .collect::<Vec<_>>(),
        ];
        prism(
            ProfileFrame {
                origin: DVec3::ZERO,
                u: DVec3::X,
                v: DVec3::Z,
            },
            &loops,
            DVec3::Y,
            (DVec3::ZERO, DVec3::Y),
            (DVec3::new(0.0, 60.0, 0.0), DVec3::Y),
            tol,
        )
        .unwrap()
    }

    #[test]
    fn a_stepped_parting_surface() {
        let tol = Tolerances::default();
        let part = block(&tol);
        let surface = Stepped {
            origin: DVec3::new(50.0, 30.0, 20.0),
            normal: DVec3::Z,
            axis: DVec3::X,
            steps: vec![(0.0, 10.0)],
        };
        let r = split_stepped(&part, &surface, 10.0, 1.0, part.aabb().center(), &tol).unwrap();
        assert!((r.core.volume().unwrap() - 186_000.0).abs() < 1e-3);
        assert!((r.cavity.volume().unwrap() - 150_000.0).abs() < 1e-3);
        assert!(
            (r.parting_length_mm - 340.0).abs() < 1e-6,
            "{}",
            r.parting_length_mm
        );
        // Two terraces and the riser between them, each holed by the part.
        let normals: Vec<DVec3> = r
            .parting_surface
            .faces()
            .map(|(_, f)| f.plane.normal)
            .collect();
        assert_eq!(
            normals.iter().filter(|n| n.z > 0.999).count(),
            2,
            "{normals:?}"
        );
        assert_eq!(
            normals.iter().filter(|n| n.x.abs() > 0.999).count(),
            2,
            "{normals:?}"
        );
        for m in [&r.core, &r.cavity, &r.parting_line, &r.parting_surface] {
            assert!(m.validate(&tol).is_empty());
        }
        // Steps out of order or outside the blank are refused.
        let mut bad = surface.clone();
        bad.steps = vec![(10.0, 5.0), (0.0, 5.0)];
        assert!(split_stepped(&part, &bad, 10.0, 1.0, DVec3::ZERO, &tol).is_err());
        bad.steps = vec![(0.0, 500.0)];
        assert!(split_stepped(&part, &bad, 10.0, 1.0, DVec3::ZERO, &tol).is_err());
    }

    /// A ridge: two planes meeting along x = 50 at height 20, falling 0.2
    /// per mm on either side, wide enough for any blank here.
    fn ridge(tol: &Tolerances) -> Freeform {
        let z = |x: f64| 20.0 - 0.2 * (x - 50.0).abs();
        let quad = |x0: f64, x1: f64| {
            vec![vec![
                DVec3::new(x0, -200.0, z(x0)),
                DVec3::new(x1, -200.0, z(x1)),
                DVec3::new(x1, 300.0, z(x1)),
                DVec3::new(x0, 300.0, z(x0)),
            ]]
        };
        Freeform::new(vec![quad(-100.0, 50.0), quad(50.0, 200.0)], DVec3::Z, tol).unwrap()
    }

    /// Milestone 1.48: a block split along a ridge.
    #[test]
    fn a_freeform_parting_surface() {
        let tol = Tolerances::default();
        let part = block(&tol);
        let surface = ridge(&tol);
        assert!((surface.height(DVec3::new(50.0, 0.0, 30.0)).unwrap() - 10.0).abs() < 1e-9);
        assert!((surface.height(DVec3::new(0.0, 0.0, 0.0)).unwrap() + 10.0).abs() < 1e-9);
        let r = split_freeform(&part, &surface, 10.0, 1.0, DVec3::ZERO, &tol).unwrap();
        // Blank 120 × 80 × 60 less the part; under the ridge the blank
        // holds 80 × 2880 and the part 60 × 1500.
        let (core, cavity) = (r.core.volume().unwrap(), r.cavity.volume().unwrap());
        assert!((core - 140_400.0).abs() < 1e-3, "{core}");
        assert!((cavity - 195_600.0).abs() < 1e-3, "{cavity}");
        // The line runs over the ridge on both long sides and straight
        // across both ends.
        let expect = 4.0 * (50.0f64.powi(2) + 10.0f64.powi(2)).sqrt() + 2.0 * 60.0;
        assert!(
            (r.parting_length_mm - expect).abs() < 1e-6,
            "{}",
            r.parting_length_mm
        );
        for m in [&r.core, &r.cavity, &r.parting_line, &r.parting_surface] {
            assert!(m.validate(&tol).is_empty());
        }
        assert!(r.core.is_solid() && r.cavity.is_solid());
        // The undercut check works on it: a plain block has none.
        assert!(
            undercuts_of(&part, &DAffine3::IDENTITY, &surface, &tol)
                .unwrap()
                .is_empty()
        );
        // A surface that does not span the blank, or turns back.
        let small = Freeform::new(
            vec![vec![vec![
                DVec3::new(-5.0, -5.0, 20.0),
                DVec3::new(105.0, -5.0, 20.0),
                DVec3::new(105.0, 65.0, 20.0),
                DVec3::new(-5.0, 65.0, 20.0),
            ]]],
            DVec3::Z,
            &tol,
        )
        .unwrap();
        assert!(split_freeform(&part, &small, 10.0, 1.0, DVec3::ZERO, &tol).is_err());
        let fold = vec![
            vec![vec![
                DVec3::new(0.0, 0.0, 0.0),
                DVec3::new(10.0, 0.0, 0.0),
                DVec3::new(10.0, 10.0, 0.0),
            ]],
            vec![vec![
                DVec3::new(0.0, 0.0, 0.0),
                DVec3::new(10.0, 10.0, 0.0),
                DVec3::new(10.0, 0.0, 0.0),
            ]],
        ];
        assert!(Freeform::new(fold, DVec3::Z, &tol).is_err());
    }

    /// Milestone 1.48: the T's overhangs get side actions sliding out
    /// sideways, and nothing is left undercut.
    #[test]
    fn side_actions_free_a_tees_overhangs() {
        let tol = Tolerances::default();
        let t = profile(
            &[
                (0.0, 0.0),
                (100.0, 0.0),
                (100.0, 20.0),
                (120.0, 20.0),
                (120.0, 30.0),
                (-20.0, 30.0),
                (-20.0, 20.0),
                (0.0, 20.0),
            ],
            &tol,
        );
        let plane = Stepped {
            origin: DVec3::new(0.0, 0.0, 10.0),
            normal: DVec3::Z,
            axis: DVec3::X,
            steps: Vec::new(),
        };
        let mut r = split(
            &t,
            Plane::from_point_normal(plane.origin, plane.normal),
            10.0,
            1.0,
            DVec3::ZERO,
            &tol,
        )
        .unwrap();
        let before = r.cavity.volume().unwrap();
        let (actions, left) = side_actions(&t, &mut r, &plane, 1.0, DVec3::ZERO, &tol).unwrap();
        assert_eq!((actions.len(), left), (2, 0));
        let mut dirs: Vec<f64> = actions.iter().map(|a| a.direction.x).collect();
        dirs.sort_by(f64::total_cmp);
        assert_eq!(dirs, [-1.0, 1.0]);
        for a in &actions {
            assert!(a.cavity && a.faces == 1);
            assert!(a.mesh.is_solid());
            // Under each overhang: 30 × 60 × 10.
            let v = a.mesh.volume().unwrap();
            assert!((v - 18_000.0).abs() < 1e-3, "{v}");
        }
        let after = r.cavity.volume().unwrap();
        assert!((before - after - 36_000.0).abs() < 1e-3, "{before} {after}");
        assert!(r.cavity.is_solid() && r.cavity.validate(&tol).is_empty());
        // A part without undercuts needs none.
        let mut plain = split(
            &block(&tol),
            Plane::from_point_normal(DVec3::new(0.0, 0.0, 20.0), DVec3::Z),
            10.0,
            1.0,
            DVec3::ZERO,
            &tol,
        )
        .unwrap();
        let flat = Stepped {
            origin: DVec3::new(0.0, 0.0, 20.0),
            ..plane
        };
        let (none, left) =
            side_actions(&block(&tol), &mut plain, &flat, 1.0, DVec3::ZERO, &tol).unwrap();
        assert!(none.is_empty() && left == 0);
    }

    #[test]
    fn overhangs_and_shadows_are_undercuts() {
        let tol = Tolerances::default();
        let plane = |z: f64| Stepped {
            origin: DVec3::new(0.0, 0.0, z),
            normal: DVec3::Z,
            axis: DVec3::X,
            steps: Vec::new(),
        };
        // A T: the cap's undersides face down in the cavity.
        let t = profile(
            &[
                (0.0, 0.0),
                (100.0, 0.0),
                (100.0, 20.0),
                (120.0, 20.0),
                (120.0, 30.0),
                (-20.0, 30.0),
                (-20.0, 20.0),
                (0.0, 20.0),
            ],
            &tol,
        );
        let found = undercuts(&t, &DAffine3::IDENTITY, &plane(10.0), &tol).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found
                .iter()
                .all(|u| u.cavity && u.kind == UndercutKind::Facing)
        );
        assert!(found.iter().all(|u| (u.area_mm2 - 1200.0).abs() < 1e-6));
        // Below the cap, the same faces belong to the core and pull free.
        assert!(
            undercuts(&t, &DAffine3::IDENTITY, &plane(25.0), &tol)
                .unwrap()
                .is_empty()
        );
        // A C: the lower arm's top is under the upper arm.
        let c = profile(
            &[
                (0.0, 0.0),
                (100.0, 0.0),
                (100.0, 20.0),
                (30.0, 20.0),
                (30.0, 40.0),
                (100.0, 40.0),
                (100.0, 60.0),
                (0.0, 60.0),
            ],
            &tol,
        );
        let found = undercuts(&c, &DAffine3::IDENTITY, &plane(10.0), &tol).unwrap();
        let kinds: Vec<UndercutKind> = found.iter().map(|u| u.kind).collect();
        assert!(kinds.contains(&UndercutKind::Shadowed), "{found:?}");
        assert!(kinds.contains(&UndercutKind::Facing), "{found:?}");
        assert_eq!(found.len(), 2);
    }
}
