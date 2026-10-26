//! Implements docs/09-file-formats.md §5 DXF import (ASCII, R12–2018) with
//! the core's own reader (docs/02 Decisions): LINE, LWPOLYLINE (bulges as
//! arcs), POLYLINE (2D, 3D, polyface and polygon meshes), ARC, CIRCLE,
//! ELLIPSE and SPLINE (segmented), 3DFACE become edges and faces; BLOCKs
//! become components placed by INSERT (with arrays); LAYERs become tags on
//! one group per layer. Other entities (text, dimensions, hatches…) are
//! skipped and counted (`W2004`). Units from `$INSUNITS` unless
//! `--units`; Z up. Edges go through the kernel's `add_edge`, so closed
//! planar outlines become faces as when they are drawn.

use std::collections::{BTreeMap, BTreeSet};
use std::f64::consts::TAU;
use std::io::Read;

use glam::{DAffine3, DMat3, DVec2, DVec3};

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::diagnostics::{Warning, WarningCode};
use crate::document::{
    ComponentDefinition, ComponentInstance, Entities, EntityRecord, Group, Rgba8, Tag,
};
use crate::geom::HalfEdgeMesh;
use crate::geom::NoDeltas;
use crate::ids::{DefinitionId, TagId};
use crate::io::import::build::{
    ImportedMesh, entities_of_pieces, io, place, top_name, triangle_pieces,
};
use crate::io::import::{
    Confidence, ImportAs, ImportFormat, ImportOptions, ImportReport, Importer, source_stem,
};
use crate::io::{IoError, Progress};
use crate::tolerance::Tolerances;
use crate::units::LengthUnit;

/// The DXF importer.
#[derive(Debug, Default, Clone, Copy)]
pub struct DxfImporter;

/// Segments of a full circle when arcs, circles and ellipses are segmented.
const CIRCLE_SEGMENTS: f64 = 24.0;

/// Deepest block nesting followed.
const MAX_NESTING: usize = 32;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "dxf".into(),
        reason: reason.into(),
    }
}

/// The group code / value pairs of an ASCII DXF.
pub(crate) fn pairs(text: &str) -> Result<Vec<(i32, &str)>, IoError> {
    let mut lines = text.lines();
    let mut out = Vec::new();
    let mut line = 0usize;
    while let Some(code) = lines.next() {
        line += 1;
        let code = code.trim();
        if code.is_empty() && lines.clone().all(|l| l.trim().is_empty()) {
            break;
        }
        let code: i32 = code
            .parse()
            .map_err(|_| corrupt(format!("line {line}: `{code}` is not a group code")))?;
        let value = lines
            .next()
            .ok_or_else(|| corrupt(format!("line {line}: group code without a value")))?;
        line += 1;
        out.push((code, value.trim()));
    }
    Ok(out)
}

/// One entity (or table entry): its type and its group codes in order.
#[derive(Debug, Clone)]
struct Raw<'a> {
    kind: &'a str,
    pairs: Vec<(i32, &'a str)>,
}

impl<'a> Raw<'a> {
    fn str(&self, code: i32) -> Option<&'a str> {
        self.pairs.iter().find(|(c, _)| *c == code).map(|(_, v)| *v)
    }

    fn num(&self, code: i32) -> Option<f64> {
        self.str(code)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|x| x.is_finite())
    }

    fn int(&self, code: i32) -> i64 {
        self.str(code)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    }

    fn nums(&self, code: i32) -> Vec<f64> {
        self.pairs
            .iter()
            .filter(|(c, _)| *c == code)
            .filter_map(|(_, v)| v.parse::<f64>().ok().filter(|x| x.is_finite()))
            .collect()
    }

    /// The point of codes `base`, `base + 10`, `base + 20` (z defaults to 0).
    fn point(&self, base: i32) -> Option<DVec3> {
        Some(DVec3::new(
            self.num(base)?,
            self.num(base + 10)?,
            self.num(base + 20).unwrap_or(0.0),
        ))
    }

    /// All points of codes `base`/`base + 10`/`base + 20`, in order.
    fn points(&self, base: i32) -> Vec<DVec3> {
        let mut out: Vec<DVec3> = Vec::new();
        for (c, v) in &self.pairs {
            let Ok(x) = v.parse::<f64>() else { continue };
            if !x.is_finite() {
                continue;
            }
            if *c == base {
                out.push(DVec3::new(x, 0.0, 0.0));
            } else if *c == base + 10 {
                if let Some(p) = out.last_mut() {
                    p.y = x;
                }
            } else if *c == base + 20
                && let Some(p) = out.last_mut()
            {
                p.z = x;
            }
        }
        out
    }

    fn layer(&self) -> &'a str {
        self.str(8).unwrap_or("0")
    }

    /// The object coordinate system of the entity's extrusion direction.
    fn ocs(&self) -> DMat3 {
        let n = self.point(210).unwrap_or(DVec3::Z);
        ocs(n)
    }
}

/// The arbitrary axis algorithm: the OCS axes of extrusion direction `n`.
fn ocs(n: DVec3) -> DMat3 {
    let n = n.try_normalize().unwrap_or(DVec3::Z);
    let limit = 1.0 / 64.0;
    let ax = if n.x.abs() < limit && n.y.abs() < limit {
        DVec3::Y.cross(n)
    } else {
        DVec3::Z.cross(n)
    }
    .try_normalize()
    .unwrap_or(DVec3::X);
    let ay = n.cross(ax).try_normalize().unwrap_or(DVec3::Y);
    DMat3::from_cols(ax, ay, n)
}

/// Splits pairs into entities at each code 0.
fn entities<'a>(pairs: &[(i32, &'a str)]) -> Vec<Raw<'a>> {
    let mut out: Vec<Raw<'a>> = Vec::new();
    for &(c, v) in pairs {
        if c == 0 {
            out.push(Raw {
                kind: v,
                pairs: Vec::new(),
            });
        } else if let Some(last) = out.last_mut() {
            last.pairs.push((c, v));
        }
    }
    out
}

/// What the file holds, by section.
struct Sections<'a> {
    units: Option<LengthUnit>,
    /// Layer name → color index, in table order.
    layers: Vec<(&'a str, i64)>,
    /// Block name → base point and entities.
    blocks: BTreeMap<&'a str, (DVec3, Vec<Raw<'a>>)>,
    entities: Vec<Raw<'a>>,
}

fn sections<'a>(pairs: &[(i32, &'a str)]) -> Sections<'a> {
    let mut out = Sections {
        units: None,
        layers: Vec::new(),
        blocks: BTreeMap::new(),
        entities: Vec::new(),
    };
    let mut i = 0;
    while i < pairs.len() {
        if pairs[i] != (0, "SECTION") {
            i += 1;
            continue;
        }
        let name = pairs.get(i + 1).map_or("", |p| p.1);
        let start = i + 2;
        let end = pairs[start.min(pairs.len())..]
            .iter()
            .position(|p| *p == (0, "ENDSEC"))
            .map_or(pairs.len(), |k| start + k);
        let body = &pairs[start.min(end)..end];
        match name {
            "HEADER" => {
                for (k, p) in body.iter().enumerate() {
                    if *p == (9, "$INSUNITS") {
                        out.units = match body.get(k + 1).and_then(|v| v.1.parse::<i64>().ok()) {
                            Some(1) => Some(LengthUnit::Inch),
                            Some(2) => Some(LengthUnit::Foot),
                            Some(4) => Some(LengthUnit::Millimeter),
                            Some(5) => Some(LengthUnit::Centimeter),
                            Some(6) => Some(LengthUnit::Meter),
                            _ => None,
                        };
                    }
                }
            }
            "TABLES" => {
                for raw in entities(body) {
                    if raw.kind == "LAYER"
                        && let Some(name) = raw.str(2)
                        && !out.layers.iter().any(|(n, _)| *n == name)
                    {
                        out.layers.push((name, raw.int(62)));
                    }
                }
            }
            "BLOCKS" => {
                let mut current: Option<(&'a str, DVec3, Vec<Raw<'a>>)> = None;
                for raw in entities(body) {
                    match raw.kind {
                        "BLOCK" => {
                            current = Some((
                                raw.str(2).unwrap_or(""),
                                raw.point(10).unwrap_or(DVec3::ZERO),
                                Vec::new(),
                            ));
                        }
                        "ENDBLK" => {
                            if let Some((name, base, list)) = current.take()
                                && !name.is_empty()
                            {
                                out.blocks.insert(name, (base, list));
                            }
                        }
                        _ => {
                            if let Some((_, _, list)) = current.as_mut() {
                                list.push(raw);
                            }
                        }
                    }
                }
            }
            "ENTITIES" => out.entities = entities(body),
            _ => {}
        }
        i = end + 1;
    }
    out
}

/// Geometry of one container, in source units.
#[derive(Debug, Default)]
struct Content {
    chains: Vec<(Vec<DVec3>, bool)>,
    triangles: Vec<[DVec3; 3]>,
    inserts: Vec<(String, DAffine3)>,
}

impl Content {
    fn is_empty(&self) -> bool {
        self.chains.is_empty() && self.triangles.is_empty() && self.inserts.is_empty()
    }

    fn quad(&mut self, q: [DVec3; 4], tol: &Tolerances) {
        self.triangles.push([q[0], q[1], q[2]]);
        if !tol.same_point(q[2], q[3]) && !tol.same_point(q[3], q[0]) {
            self.triangles.push([q[0], q[2], q[3]]);
        }
    }
}

/// Points of an arc around `center` (OCS plane) from angle `a0` sweeping
/// `sweep` radians, both ends included.
fn arc_points(center: DVec2, radius: f64, a0: f64, sweep: f64) -> Vec<DVec2> {
    let n = ((sweep.abs() / (TAU / CIRCLE_SEGMENTS)).ceil() as usize).clamp(1, 1024);
    (0..=n)
        .map(|k| {
            let a = a0 + sweep * k as f64 / n as f64;
            center + DVec2::new(libm::cos(a), libm::sin(a)) * radius
        })
        .collect()
}

/// The points from `p0` to `p1` along a polyline segment with `bulge`
/// (tangent of a quarter of the included angle; 0: straight), `p0` left
/// out.
fn bulge_points(p0: DVec2, p1: DVec2, bulge: f64) -> Vec<DVec2> {
    let chord = p1 - p0;
    let d = chord.length();
    if bulge.abs() < 1e-9 || d <= f64::MIN_POSITIVE {
        return vec![p1];
    }
    let theta = 4.0 * libm::atan(bulge);
    let h = (d / 2.0) / libm::tan(theta / 2.0);
    let center = (p0 + p1) / 2.0 + DVec2::new(-chord.y, chord.x) / d * h;
    let r = (p0 - center).length();
    let a0 = libm::atan2(p0.y - center.y, p0.x - center.x);
    let mut pts = arc_points(center, r, a0, theta);
    pts.remove(0);
    if let Some(last) = pts.last_mut() {
        *last = p1;
    }
    pts
}

/// A NURBS curve sampled at `n + 1` parameters (de Boor, homogeneous).
fn nurbs(degree: usize, knots: &[f64], controls: &[DVec3], weights: &[f64]) -> Vec<DVec3> {
    let count = controls.len();
    if degree == 0 || count <= degree || knots.len() != count + degree + 1 {
        return controls.to_vec();
    }
    let w = |i: usize| weights.get(i).copied().filter(|w| *w > 0.0).unwrap_or(1.0);
    let (t0, t1) = (knots[degree], knots[count]);
    if t1 - t0 <= f64::MIN_POSITIVE {
        return controls.to_vec();
    }
    let samples = (count * 8).clamp(8, 256);
    (0..=samples)
        .map(|s| {
            let t = t0 + (t1 - t0) * s as f64 / samples as f64;
            let mut k = degree;
            while k + 1 < count && knots[k + 1] <= t {
                k += 1;
            }
            let mut d: Vec<(DVec3, f64)> = (0..=degree)
                .map(|j| {
                    let i = j + k - degree;
                    (controls[i] * w(i), w(i))
                })
                .collect();
            for r in 1..=degree {
                for j in (r..=degree).rev() {
                    let lo = knots[j + k - degree];
                    let hi = knots[j + 1 + k - r];
                    let alpha = if hi - lo > f64::MIN_POSITIVE {
                        (t - lo) / (hi - lo)
                    } else {
                        0.0
                    };
                    d[j] = (
                        d[j - 1].0 * (1.0 - alpha) + d[j].0 * alpha,
                        d[j - 1].1 * (1.0 - alpha) + d[j].1 * alpha,
                    );
                }
            }
            let (p, w) = d[degree];
            if w.abs() > f64::MIN_POSITIVE {
                p / w
            } else {
                p
            }
        })
        .collect()
}

/// Reads the entities of one container; POLYLINE consumes its VERTEX list.
/// Returns the content per layer (in order of first use) and the number of
/// skipped entities by type.
fn read_content<'a>(
    list: &[Raw<'a>],
    origin: DVec3,
    tol: &Tolerances,
    skipped: &mut BTreeMap<String, usize>,
) -> Vec<(&'a str, Content)> {
    let mut layers: Vec<(&'a str, Content)> = Vec::new();
    let mut i = 0;
    while i < list.len() {
        let raw = &list[i];
        i += 1;
        let layer = raw.layer();
        let index = match layers.iter().position(|(l, _)| *l == layer) {
            Some(k) => k,
            None => {
                layers.push((layer, Content::default()));
                layers.len() - 1
            }
        };
        let c = &mut layers[index].1;
        let at = |p: DVec3| p - origin;
        match raw.kind {
            "LINE" => {
                if let (Some(a), Some(b)) = (raw.point(10), raw.point(11)) {
                    c.chains.push((vec![at(a), at(b)], false));
                }
            }
            "LWPOLYLINE" => {
                let m = raw.ocs();
                let elevation = raw.num(38).unwrap_or(0.0);
                let mut verts: Vec<(DVec2, f64)> = Vec::new();
                for (code, v) in &raw.pairs {
                    let Ok(x) = v.parse::<f64>() else { continue };
                    match code {
                        10 => verts.push((DVec2::new(x, 0.0), 0.0)),
                        20 => {
                            if let Some(last) = verts.last_mut() {
                                last.0.y = x;
                            }
                        }
                        42 => {
                            if let Some(last) = verts.last_mut() {
                                last.1 = x;
                            }
                        }
                        _ => {}
                    }
                }
                let closed = raw.int(70) & 1 == 1;
                let pts = flatten_bulges(&verts, closed);
                let world: Vec<DVec3> = pts
                    .iter()
                    .map(|p| at(m * DVec3::new(p.x, p.y, elevation)))
                    .collect();
                c.chains.push((world, closed));
            }
            "POLYLINE" => {
                let mut vertices: Vec<&Raw<'a>> = Vec::new();
                while i < list.len() && list[i].kind == "VERTEX" {
                    vertices.push(&list[i]);
                    i += 1;
                }
                if i < list.len() && list[i].kind == "SEQEND" {
                    i += 1;
                }
                let flags = raw.int(70);
                if flags & 64 != 0 {
                    // Polyface mesh: position vertices, then face records.
                    let positions: Vec<DVec3> = vertices
                        .iter()
                        .filter(|v| v.int(70) & 64 != 0)
                        .filter_map(|v| v.point(10))
                        .map(at)
                        .collect();
                    for v in vertices.iter().filter(|v| v.int(70) & 192 == 128) {
                        let idx: Vec<usize> = [71, 72, 73, 74]
                            .iter()
                            .map(|&code| v.int(code).unsigned_abs() as usize)
                            .take_while(|&k| k > 0)
                            .collect();
                        let corner = |k: usize| positions.get(k - 1).copied();
                        match idx.as_slice() {
                            [a, b, cc] => {
                                if let (Some(a), Some(b), Some(cc)) =
                                    (corner(*a), corner(*b), corner(*cc))
                                {
                                    c.triangles.push([a, b, cc]);
                                }
                            }
                            [a, b, cc, d] => {
                                if let (Some(a), Some(b), Some(cc), Some(d)) =
                                    (corner(*a), corner(*b), corner(*cc), corner(*d))
                                {
                                    c.quad([a, b, cc, d], tol);
                                }
                            }
                            _ => {}
                        }
                    }
                } else if flags & 16 != 0 {
                    // Polygon mesh of M×N vertices.
                    let (m, n) = (raw.int(71).max(0) as usize, raw.int(72).max(0) as usize);
                    let pts: Vec<DVec3> = vertices
                        .iter()
                        .filter_map(|v| v.point(10))
                        .map(at)
                        .collect();
                    if m >= 2 && n >= 2 && pts.len() >= m * n {
                        let (close_m, close_n) = (flags & 1 != 0, flags & 32 != 0);
                        let rows = if close_m { m } else { m - 1 };
                        let cols = if close_n { n } else { n - 1 };
                        for r in 0..rows {
                            for k in 0..cols {
                                let p = |r: usize, k: usize| pts[(r % m) * n + (k % n)];
                                c.quad([p(r, k), p(r + 1, k), p(r + 1, k + 1), p(r, k + 1)], tol);
                            }
                        }
                    }
                } else if flags & 8 != 0 {
                    let pts: Vec<DVec3> = vertices
                        .iter()
                        .filter_map(|v| v.point(10))
                        .map(at)
                        .collect();
                    c.chains.push((pts, flags & 1 != 0));
                } else {
                    let m = raw.ocs();
                    let elevation = raw.point(10).map_or(0.0, |p| p.z);
                    let verts: Vec<(DVec2, f64)> = vertices
                        .iter()
                        .filter_map(|v| {
                            v.point(10)
                                .map(|p| (DVec2::new(p.x, p.y), v.num(42).unwrap_or(0.0)))
                        })
                        .collect();
                    let closed = flags & 1 != 0;
                    let pts = flatten_bulges(&verts, closed);
                    c.chains.push((
                        pts.iter()
                            .map(|p| at(m * DVec3::new(p.x, p.y, elevation)))
                            .collect(),
                        closed,
                    ));
                }
            }
            "ARC" | "CIRCLE" => {
                let (Some(center), Some(r)) = (raw.point(10), raw.num(40)) else {
                    continue;
                };
                if r <= 0.0 {
                    continue;
                }
                let m = raw.ocs();
                let (a0, sweep, closed) = if raw.kind == "CIRCLE" {
                    (0.0, TAU * (1.0 - 1.0 / CIRCLE_SEGMENTS), true)
                } else {
                    let start = raw.num(50).unwrap_or(0.0).to_radians();
                    let mut sweep = raw.num(51).unwrap_or(360.0).to_radians() - start;
                    while sweep <= 0.0 {
                        sweep += TAU;
                    }
                    (start, sweep, false)
                };
                let pts = arc_points(DVec2::new(center.x, center.y), r, a0, sweep);
                let pts = if closed {
                    // Evenly spaced, the closing segment implied.
                    (0..CIRCLE_SEGMENTS as usize)
                        .map(|k| {
                            let a = TAU * k as f64 / CIRCLE_SEGMENTS;
                            DVec2::new(center.x, center.y)
                                + DVec2::new(libm::cos(a), libm::sin(a)) * r
                        })
                        .collect()
                } else {
                    pts
                };
                c.chains.push((
                    pts.iter()
                        .map(|p| at(m * DVec3::new(p.x, p.y, center.z)))
                        .collect(),
                    closed,
                ));
            }
            "ELLIPSE" => {
                let (Some(center), Some(major)) = (raw.point(10), raw.point(11)) else {
                    continue;
                };
                let n = raw
                    .point(210)
                    .unwrap_or(DVec3::Z)
                    .try_normalize()
                    .unwrap_or(DVec3::Z);
                let ratio = raw.num(40).unwrap_or(1.0);
                let minor =
                    n.cross(major).try_normalize().unwrap_or(DVec3::Y) * major.length() * ratio;
                let t0 = raw.num(41).unwrap_or(0.0);
                let mut t1 = raw.num(42).unwrap_or(TAU);
                while t1 <= t0 {
                    t1 += TAU;
                }
                let full = (t1 - t0 - TAU).abs() < 1e-6;
                let segments =
                    (((t1 - t0) / (TAU / CIRCLE_SEGMENTS)).ceil() as usize).clamp(2, 1024);
                let count = if full { segments } else { segments + 1 };
                let pts: Vec<DVec3> = (0..count)
                    .map(|k| {
                        let t = t0 + (t1 - t0) * k as f64 / segments as f64;
                        at(center + major * libm::cos(t) + minor * libm::sin(t))
                    })
                    .collect();
                c.chains.push((pts, full));
            }
            "SPLINE" => {
                let degree = raw.int(71).clamp(1, 10) as usize;
                let controls = raw.points(10);
                let pts = if controls.len() > degree {
                    nurbs(degree, &raw.nums(40), &controls, &raw.nums(41))
                } else {
                    raw.points(11)
                };
                if pts.len() >= 2 {
                    c.chains
                        .push((pts.into_iter().map(at).collect(), raw.int(70) & 1 == 1));
                }
            }
            "3DFACE" => {
                let corners: Vec<DVec3> = [10, 11, 12, 13]
                    .iter()
                    .filter_map(|&b| raw.point(b))
                    .collect();
                if let [a, b, cc, d] = corners.as_slice() {
                    c.quad([at(*a), at(*b), at(*cc), at(*d)], tol);
                } else if let [a, b, cc] = corners.as_slice() {
                    c.triangles.push([at(*a), at(*b), at(*cc)]);
                }
            }
            "INSERT" => {
                let Some(block) = raw.str(2) else { continue };
                let p = raw.point(10).unwrap_or(DVec3::ZERO);
                let scale = DVec3::new(
                    raw.num(41).unwrap_or(1.0),
                    raw.num(42).unwrap_or(1.0),
                    raw.num(43).unwrap_or(1.0),
                );
                let rotation = raw.num(50).unwrap_or(0.0).to_radians();
                let m = raw.ocs();
                let (cols, rows) = (raw.int(70).max(1), raw.int(71).max(1));
                let (dx, dy) = (raw.num(44).unwrap_or(0.0), raw.num(45).unwrap_or(0.0));
                let rotate = DMat3::from_cols(
                    DVec3::new(libm::cos(rotation), libm::sin(rotation), 0.0),
                    DVec3::new(-libm::sin(rotation), libm::cos(rotation), 0.0),
                    DVec3::Z,
                );
                for row in 0..rows.min(1000) {
                    for col in 0..cols.min(1000) {
                        let offset = rotate * DVec3::new(col as f64 * dx, row as f64 * dy, 0.0);
                        let local = DAffine3::from_mat3_translation(
                            rotate * DMat3::from_diagonal(scale),
                            p + offset,
                        );
                        let world =
                            DAffine3::from_translation(-origin) * DAffine3::from_mat3(m) * local;
                        c.inserts.push((block.to_string(), world));
                    }
                }
            }
            "VERTEX" | "SEQEND" | "ATTRIB" => {}
            other => *skipped.entry(other.to_string()).or_default() += 1,
        }
    }
    layers.retain(|(_, c)| !c.is_empty());
    layers
}

/// The points of a 2D polyline with bulges, arcs segmented.
fn flatten_bulges(verts: &[(DVec2, f64)], closed: bool) -> Vec<DVec2> {
    let Some(first) = verts.first() else {
        return Vec::new();
    };
    let mut pts = vec![first.0];
    let n = verts.len();
    let segments = if closed { n } else { n.saturating_sub(1) };
    for k in 0..segments {
        let (p0, bulge) = verts[k];
        let p1 = verts[(k + 1) % n].0;
        pts.extend(bulge_points(p0, p1, bulge));
    }
    if closed && pts.len() > 1 {
        pts.pop();
    }
    pts
}

/// AutoCAD color index → a color for the layer's tag (the first seven; a
/// neutral gray otherwise).
fn aci(index: i64) -> Rgba8 {
    match index.abs() {
        1 => Rgba8::rgb(255, 0, 0),
        2 => Rgba8::rgb(255, 255, 0),
        3 => Rgba8::rgb(0, 255, 0),
        4 => Rgba8::rgb(0, 255, 255),
        5 => Rgba8::rgb(0, 0, 255),
        6 => Rgba8::rgb(255, 0, 255),
        _ => Rgba8::rgb(128, 128, 128),
    }
}

/// Builds definitions from DXF content.
struct Builder<'s, 'a> {
    sections: &'s Sections<'a>,
    factor: f64,
    tol: Tolerances,
    opts: &'s ImportOptions,
    blocks: BTreeMap<String, Option<DefinitionId>>,
    building: BTreeSet<String>,
    skipped: BTreeMap<String, usize>,
    bad_segments: usize,
    report: ImportReport,
}

impl Builder<'_, '_> {
    /// The definition of `content` (in source units): faces, edges, and
    /// the instances of the blocks it inserts.
    fn definition(
        &mut self,
        tx: &mut Transaction<'_>,
        content: &Content,
        name: String,
        internal: bool,
        depth: usize,
    ) -> Result<DefinitionId, IoError> {
        let f = self.factor;
        let mut mesh = ImportedMesh {
            name: name.clone(),
            ..ImportedMesh::default()
        };
        for t in &content.triangles {
            let base = mesh.positions.len() as u32;
            mesh.positions.extend(t.iter().map(|p| *p * f));
            mesh.triangles.push([base, base + 1, base + 2]);
            mesh.materials.push(None);
        }
        let keys = vec![0u32; mesh.triangles.len()];
        let maps = vec![None; mesh.triangles.len()];
        let mut pieces = triangle_pieces(
            &mesh.positions,
            &[],
            &mesh,
            &keys,
            &maps,
            &[],
            self.opts,
            &self.tol,
            &mut self.report,
        )?;
        if !content.chains.is_empty() {
            if pieces.is_empty() {
                pieces.push(HalfEdgeMesh::new());
            }
            let target = &mut pieces[0];
            for (pts, closed) in &content.chains {
                let n = pts.len();
                let segments = if *closed { n } else { n.saturating_sub(1) };
                for k in 0..segments {
                    let (a, b) = (pts[k] * f, pts[(k + 1) % n] * f);
                    if self.tol.same_point(a, b) || !self.tol.in_range(a) || !self.tol.in_range(b) {
                        continue;
                    }
                    if target.add_edge(a, b, &self.tol, &mut NoDeltas).is_err() {
                        self.bad_segments += 1;
                    }
                }
            }
        }
        for piece in &pieces {
            self.report.faces += piece.face_count();
            self.report.edges += piece.edge_count();
        }
        let chain_points: Vec<DVec3> = content.chains.iter().flat_map(|(p, _)| p.clone()).collect();
        let source: [&[u8]; 2] = [
            bytemuck::cast_slice(&mesh.positions),
            bytemuck::cast_slice(&chain_points),
        ];
        let mut entities = entities_of_pieces(tx, pieces, &source, &name)?;
        for (block, transform) in &content.inserts {
            let Some(definition) = self.block(tx, block, depth + 1)? else {
                continue;
            };
            let transform =
                DAffine3::from_mat3_translation(transform.matrix3, transform.translation * f);
            let pid = tx.allocate_pid();
            entities
                .insert(
                    EntityRecord::Instance(ComponentInstance {
                        definition,
                        transform,
                        name: None,
                        material_override: None,
                        glued_to: None,
                        dynamic_attrs: Default::default(),
                    }),
                    pid,
                )
                .map_err(|e| corrupt(e.to_string()))?;
        }
        let uuid = crate::commands::common::definition_uuid_of(
            tx.document(),
            &name,
            &[source[0], source[1], &entities.order().len().to_le_bytes()],
        );
        let mut def = ComponentDefinition::new(uuid, name);
        def.is_internal_group = internal;
        def.entities = entities;
        tx.insert_definition(def).map_err(io)
    }

    /// The component of block `name`, built on first use; `None` for
    /// missing, empty or recursive blocks.
    fn block(
        &mut self,
        tx: &mut Transaction<'_>,
        name: &str,
        depth: usize,
    ) -> Result<Option<DefinitionId>, IoError> {
        if let Some(done) = self.blocks.get(name) {
            return Ok(*done);
        }
        if depth > MAX_NESTING || self.building.contains(name) {
            *self.skipped.entry("INSERT (recursive)".into()).or_default() += 1;
            return Ok(None);
        }
        let Some((base, list)) = self.sections.blocks.get(name) else {
            *self
                .skipped
                .entry("INSERT (missing block)".into())
                .or_default() += 1;
            self.blocks.insert(name.to_string(), None);
            return Ok(None);
        };
        self.building.insert(name.to_string());
        let mut content = Content::default();
        for (_, layer) in read_content(list, *base, &self.tol, &mut self.skipped) {
            content.chains.extend(layer.chains);
            content.triangles.extend(layer.triangles);
            content.inserts.extend(layer.inserts);
        }
        let id = if content.is_empty() {
            None
        } else {
            let component = crate::io::import::build::unique_component_name(tx.document(), name);
            Some(self.definition(tx, &content, component, false, depth)?)
        };
        self.building.remove(name);
        self.blocks.insert(name.to_string(), id);
        Ok(id)
    }
}

/// The tag of a layer (made when missing); `None` for layer `0`.
fn layer_tag(tx: &mut Transaction<'_>, layer: &str, color: i64) -> Result<Option<TagId>, IoError> {
    if layer == "0" || layer.trim().is_empty() {
        return Ok(None);
    }
    if let Some(t) = tx.document().tags.by_name(layer) {
        return Ok(Some(t));
    }
    tx.insert_tag(Tag::new(layer, aci(color)))
        .map(Some)
        .map_err(io)
}

impl Importer for DxfImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Dxf
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]);
        let mut lines = head.lines().map(str::trim);
        if lines.next() == Some("0") && lines.next() == Some("SECTION") {
            Confidence::CERTAIN
        } else if name.to_ascii_lowercase().ends_with(".dxf") {
            Confidence::EXTENSION
        } else {
            Confidence::NONE
        }
    }

    fn import(
        &self,
        src: &mut dyn Read,
        opts: &ImportOptions,
        tx: &mut Transaction<'_>,
        target: &Target,
        progress: &dyn Progress,
    ) -> Result<ImportReport, IoError> {
        let mut bytes = Vec::new();
        src.read_to_end(&mut bytes)
            .map_err(|e| corrupt(e.to_string()))?;
        if bytes.starts_with(b"AutoCAD Binary DXF") {
            return Err(IoError::UnsupportedFormat(
                "binary DXF (save the drawing as ASCII DXF)".into(),
            ));
        }
        let text = String::from_utf8_lossy(&bytes);
        let all = pairs(&text)?;
        let sections = sections(&all);
        progress.report(0.3, "reading entities");
        let unit = opts
            .units
            .or(sections.units)
            .unwrap_or(LengthUnit::Millimeter);
        let tol = *tx.tolerances();
        let mut builder = Builder {
            sections: &sections,
            factor: unit.millimeters() * opts.scale,
            tol,
            opts,
            blocks: BTreeMap::new(),
            building: BTreeSet::new(),
            skipped: BTreeMap::new(),
            bad_segments: 0,
            report: ImportReport::default(),
        };
        let mut skipped = BTreeMap::new();
        let layers = read_content(&sections.entities, DVec3::ZERO, &tol, &mut skipped);
        builder.skipped = skipped;
        progress.report(0.5, "building geometry");

        let stem = source_stem(&opts.source, "DXF");
        let name = top_name(tx.document(), opts);
        let internal = !matches!(opts.import_as, ImportAs::Component(_));
        let top = match layers.as_slice() {
            [(layer, content)] if *layer == "0" => {
                builder.definition(tx, content, name, internal, 0)?
            }
            _ => {
                // One group per layer, tagged with the layer.
                let mut entities = Entities::new();
                for (layer, content) in &layers {
                    let group_name = format!(
                        "Group {}",
                        crate::commands::common::group_number(tx.document())
                    );
                    let def = builder.definition(tx, content, group_name, true, 0)?;
                    let color = sections
                        .layers
                        .iter()
                        .find(|(n, _)| n == layer)
                        .map_or(7, |(_, c)| *c);
                    let tag = layer_tag(tx, layer, color)?;
                    let pid = tx.allocate_pid();
                    let id = entities
                        .insert(
                            EntityRecord::Group(Group {
                                definition: def,
                                transform: DAffine3::IDENTITY,
                                name: (*layer).to_string(),
                            }),
                            pid,
                        )
                        .map_err(|e| corrupt(e.to_string()))?;
                    entities
                        .set_tag(id, tag)
                        .map_err(|e| corrupt(e.to_string()))?;
                }
                let uuid = crate::commands::common::definition_uuid_of(
                    tx.document(),
                    &name,
                    &[text.as_bytes()],
                );
                let mut def = ComponentDefinition::new(uuid, name);
                def.is_internal_group = internal;
                def.entities = entities;
                tx.insert_definition(def).map_err(io)?
            }
        };
        let pid = place(tx, target, top, opts, &stem)?;
        let mut report = builder.report;
        report.created.push(pid);
        if builder.bad_segments > 0 {
            report.warnings.push(Warning::new(
                WarningCode::W2004,
                format!("{} segment(s) could not be added", builder.bad_segments),
            ));
            report.skipped += builder.bad_segments;
        }
        for (kind, count) in &builder.skipped {
            report.warnings.push(Warning::new(
                WarningCode::W2004,
                format!(
                    "{count} {kind} entit{} skipped",
                    if *count == 1 { "y" } else { "ies" }
                ),
            ));
            report.skipped += count;
        }
        Ok(report)
    }
}
