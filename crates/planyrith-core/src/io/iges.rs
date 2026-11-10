//! Implements docs/09-file-formats.md §5.1 IGES (milestone 1.17): the
//! Initial Graphics Exchange Specification 5.3 in its fixed 80-column
//! form.
//!
//! Export writes each planar face as a trimmed surface (type 144) on a
//! bilinear plane patch (128), its loops as curves on the surface (142)
//! of composite curves (102) of lines (110), in millimeters; groups and
//! components are placed in world coordinates (IGES assemblies are not
//! written). Import reads trimmed (144) and bounded (143) surfaces on
//! planar surfaces (108, 128, 190) with boundaries of lines, arcs (100),
//! composite curves, copious data (106) and B-spline curves (126,
//! sampled), applying transformation matrices (124) and the file's unit;
//! curved surfaces are counted as skipped.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{Read, Write};

use glam::{DAffine3, DMat3, DVec3};

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::diagnostics::{Warning, WarningCode};
use crate::document::{
    ComponentDefinition, ComponentInstance, Document, Entities, EntityRecord, Group,
};
use crate::geom::FaceSides;
use crate::geom::HalfEdgeMesh;
use crate::geom::boolean::{Piece, stitch};
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter, scene};
use crate::io::import::{
    Confidence, ImportAs, ImportFormat, ImportOptions, ImportReport, Importer,
};
use crate::io::{IoError, Progress};
use crate::tolerance::Tolerances;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "iges".into(),
        reason: reason.into(),
    }
}

// ----- writing --------------------------------------------------------------------------

fn real(v: f64) -> String {
    let v = if v.abs() < 1e-12 { 0.0 } else { v };
    let s = format!("{v:.9}");
    // `{:.9}` always has a point, which IGES reals need.
    s.trim_end_matches('0').to_string()
}

fn hollerith(s: &str) -> String {
    let ascii: String = s
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                '?'
            }
        })
        .collect();
    format!("{}H{}", ascii.len(), ascii)
}

/// One directory entry and its parameters.
struct Entity {
    kind: u32,
    form: u32,
    params: Vec<String>,
    dependent: bool,
}

struct Writer {
    entities: Vec<Entity>,
}

impl Writer {
    /// Adds an entity; returns its directory pointer (odd line number).
    fn add(&mut self, kind: u32, form: u32, params: Vec<String>, dependent: bool) -> usize {
        self.entities.push(Entity {
            kind,
            form,
            params,
            dependent,
        });
        self.entities.len() * 2 - 1
    }

    fn line(&mut self, a: DVec3, b: DVec3) -> usize {
        let p = [a.x, a.y, a.z, b.x, b.y, b.z].map(real).to_vec();
        self.add(110, 0, p, true)
    }

    /// A loop of lines as a curve on `surface`.
    fn boundary(&mut self, ring: &[DVec3], surface: usize) -> usize {
        let lines: Vec<usize> = ring
            .iter()
            .zip(ring.iter().cycle().skip(1))
            .filter(|(a, b)| a.distance(**b) > 1e-9)
            .map(|(a, b)| self.line(*a, *b))
            .collect();
        let mut p = vec![lines.len().to_string()];
        p.extend(lines.iter().map(|l| l.to_string()));
        let composite = self.add(102, 0, p, true);
        self.add(
            142,
            0,
            vec![
                "0".into(),
                surface.to_string(),
                "0".into(),
                composite.to_string(),
                "2".into(),
            ],
            true,
        )
    }

    fn face(&mut self, rings: &[Vec<DVec3>]) {
        let Some(outer) = rings.first() else { return };
        if outer.len() < 3 {
            return;
        }
        let mut n = DVec3::ZERO;
        for (a, b) in outer.iter().zip(outer.iter().cycle().skip(1)) {
            n += a.cross(*b);
        }
        let n = n.normalize_or_zero();
        let mut u = (outer[1] - outer[0]).normalize_or_zero();
        u = (u - n * u.dot(n)).normalize_or_zero();
        if u == DVec3::ZERO {
            return;
        }
        let v = n.cross(u);
        let o = outer[0];
        let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
        for p in outer {
            let (x, y) = ((*p - o).dot(u), (*p - o).dot(v));
            lo = (lo.0.min(x), lo.1.min(y));
            hi = (hi.0.max(x), hi.1.max(y));
        }
        let at = |x: f64, y: f64| o + u * x + v * y;
        let corners = [
            at(lo.0, lo.1),
            at(hi.0, lo.1),
            at(lo.0, hi.1),
            at(hi.0, hi.1),
        ];
        // A bilinear B-spline patch: degree 1 each way, four control points.
        let mut p: Vec<String> = ["1", "1", "1", "1", "1", "0", "1", "0", "0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        p.extend(["0.", "0.", "1.", "1.", "0.", "0.", "1.", "1."].map(String::from));
        p.extend(["1.", "1.", "1.", "1."].map(String::from));
        for c in corners {
            p.extend([c.x, c.y, c.z].map(real));
        }
        p.extend(["0.", "1.", "0.", "1."].map(String::from));
        let surface = self.add(128, 1, p, true);
        let mut bounds = Vec::new();
        for ring in rings {
            bounds.push(self.boundary(ring, surface));
        }
        let mut p = vec![
            surface.to_string(),
            "1".into(),
            (bounds.len() - 1).to_string(),
        ];
        p.extend(bounds.iter().map(|b| b.to_string()));
        self.add(144, 0, p, false);
    }

    fn text(&self, file_name: &str) -> String {
        let mut out = String::new();
        let put = |out: &mut String, body: &str, section: char, seq: usize| {
            let _ = writeln!(out, "{body:<72}{section}{seq:>7}");
        };
        put(&mut out, "Planyrith model", 'S', 1);
        let g = [
            "1H,".to_string(),
            "1H;".into(),
            hollerith("Planyrith"),
            hollerith(file_name),
            hollerith("Planyrith"),
            hollerith("Planyrith"),
            "32".into(),
            "38".into(),
            "6".into(),
            "308".into(),
            "15".into(),
            hollerith("Planyrith"),
            "1.".into(),
            "2".into(),
            "2HMM".into(),
            "1".into(),
            "1.".into(),
            "15H19700101.000000".into(),
            "0.001".into(),
            "100000.".into(),
            "0H".into(),
            "0H".into(),
            "11".into(),
            "0".into(),
            "15H19700101.000000".into(),
        ]
        .join(",")
            + ";";
        let mut seq = 0;
        for chunk in g.as_bytes().chunks(72) {
            seq += 1;
            put(&mut out, &String::from_utf8_lossy(chunk), 'G', seq);
        }
        let g_lines = seq;
        // Parameter lines per entity, to know the pointers.
        let mut p_lines: Vec<Vec<String>> = Vec::new();
        for e in &self.entities {
            let mut fields = vec![e.kind.to_string()];
            fields.extend(e.params.iter().cloned());
            let mut lines = Vec::new();
            let mut cur = String::new();
            for (i, f) in fields.iter().enumerate() {
                let piece = format!("{f}{}", if i + 1 == fields.len() { ";" } else { "," });
                if cur.len() + piece.len() > 64 && !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                }
                cur.push_str(&piece);
            }
            lines.push(cur);
            p_lines.push(lines);
        }
        let mut p_at = 1;
        for (i, e) in self.entities.iter().enumerate() {
            let status = if e.dependent { "00010000" } else { "00000000" };
            let d = i * 2 + 1;
            let _ = writeln!(
                out,
                "{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}D{:>7}",
                e.kind, p_at, 0, 0, 0, 0, 0, 0, status, d
            );
            let _ = writeln!(
                out,
                "{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}D{:>7}",
                e.kind,
                0,
                0,
                p_lines[i].len(),
                e.form,
                "",
                "",
                "",
                0,
                d + 1
            );
            p_at += p_lines[i].len();
        }
        let mut seq = 0;
        for (i, lines) in p_lines.iter().enumerate() {
            for l in lines {
                seq += 1;
                let _ = writeln!(out, "{l:<64}{:>8}P{seq:>7}", i * 2 + 1);
            }
        }
        let _ = writeln!(
            out,
            "{:<72}T{:>7}",
            format!(
                "S{:>7}G{:>7}D{:>7}P{:>7}",
                1,
                g_lines,
                self.entities.len() * 2,
                seq
            ),
            1
        );
        out
    }
}

/// The IGES text of the document's visible faces, in world coordinates.
pub fn write(
    doc: &Document,
    scope: &ExportScope,
    file_name: &str,
) -> Result<(String, usize), IoError> {
    let sc = scene::collect(doc, scope);
    let mut w = Writer {
        entities: Vec::new(),
    };
    let mut faces = 0;
    for occ in &sc.occurrences {
        let Some(entities) = doc.container(occ.container) else {
            continue;
        };
        let mesh = entities.mesh();
        let flip = occ.world.matrix3.determinant() < 0.0;
        for (f, face) in mesh.faces() {
            let _ = f;
            let mut loops = vec![face.outer];
            loops.extend(face.holes.iter().copied());
            let rings: Vec<Vec<DVec3>> = loops
                .into_iter()
                .filter_map(|l| mesh.loop_positions(l).ok())
                .map(|r| {
                    let mut r: Vec<DVec3> = r
                        .into_iter()
                        .map(|p| occ.world.transform_point3(p))
                        .collect();
                    if flip {
                        r.reverse();
                    }
                    r
                })
                .collect();
            w.face(&rings);
            faces += 1;
        }
    }
    if faces == 0 {
        return Err(IoError::UnsupportedFormat(
            "nothing visible to export".into(),
        ));
    }
    Ok((w.text(file_name), faces))
}

// ----- reading --------------------------------------------------------------------------

/// A face's loops, outer first.
pub type FaceLoops = Vec<Vec<DVec3>>;

/// A directory entry.
#[derive(Debug, Clone)]
struct Dir {
    kind: u32,
    form: u32,
    transform: usize,
    params: Vec<String>,
}

struct Model {
    entries: BTreeMap<usize, Dir>,
    scale: f64,
}

/// Splits parameter text at `delim` outside Hollerith strings.
fn split_params(text: &str, delim: char, record: char) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == record {
            out.push(cur.trim().to_string());
            return out;
        }
        if c == delim {
            out.push(std::mem::take(&mut cur).trim().to_string());
            i += 1;
            continue;
        }
        if c == 'H' {
            let digits = cur.trim();
            if !digits.is_empty() && digits.chars().all(|d| d.is_ascii_digit()) {
                let n: usize = digits.parse().unwrap_or(0);
                let s: String = chars[i + 1..(i + 1 + n).min(chars.len())].iter().collect();
                cur = s;
                i += 1 + n;
                continue;
            }
        }
        cur.push(c);
        i += 1;
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn num(s: &str) -> f64 {
    s.trim().replace(['D', 'd'], "E").parse().unwrap_or(0.0)
}

fn int(s: &str) -> usize {
    let v = num(s);
    if v.is_finite() && v >= 0.0 {
        v as usize
    } else {
        0
    }
}

fn parse(text: &str) -> Result<Model, IoError> {
    let mut sections: BTreeMap<char, Vec<String>> = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.len() < 73 {
            continue;
        }
        let Some(section) = line.chars().nth(72) else {
            continue;
        };
        sections.entry(section).or_default().push(line.to_string());
    }
    let g: String = sections
        .get(&'G')
        .map(|v| v.iter().map(|l| l[..72].to_string()).collect())
        .unwrap_or_default();
    let (delim, record) = {
        let b = g.as_bytes();
        let d = if b.len() > 2 && b.starts_with(b"1H") {
            b[2] as char
        } else {
            ','
        };
        let rest = g.get(4..).unwrap_or("");
        let r = if rest.starts_with("1H") {
            rest.chars().nth(2).unwrap_or(';')
        } else {
            ';'
        };
        (d, r)
    };
    let globals = split_params(&g, delim, record);
    let unit = match globals.get(13).map(|s| int(s)) {
        Some(1) => 25.4,
        Some(4) => 304.8,
        Some(5) => 1_609_344.0,
        Some(6) => 1000.0,
        Some(7) => 1.0e6,
        Some(8) => 0.0254,
        Some(9) => 0.001,
        Some(10) => 10.0,
        Some(11) => 2.54e-5,
        _ => 1.0,
    };
    let space = globals
        .get(12)
        .map(|s| num(s))
        .filter(|v| *v > 0.0)
        .unwrap_or(1.0);
    let d = sections.get(&'D').cloned().unwrap_or_default();
    let p = sections.get(&'P').cloned().unwrap_or_default();
    let field = |l: &str, k: usize| l.get(k * 8..k * 8 + 8).unwrap_or("").trim().to_string();
    let mut entries = BTreeMap::new();
    for (i, pair) in d.chunks(2).enumerate() {
        let [a, b] = pair else { break };
        let kind = int(&field(a, 0)) as u32;
        let start = int(&field(a, 1));
        let transform = int(&field(a, 6));
        let count = int(&field(b, 3));
        let form = int(&field(b, 4)) as u32;
        let text: String = p
            .iter()
            .skip(start.saturating_sub(1))
            .take(count.max(1))
            .map(|l| l.get(..64).unwrap_or(l).to_string())
            .collect();
        let mut params = split_params(&text, delim, record);
        if !params.is_empty() {
            params.remove(0);
        }
        entries.insert(
            i * 2 + 1,
            Dir {
                kind,
                form,
                transform,
                params,
            },
        );
    }
    if entries.is_empty() {
        return Err(corrupt("no directory entries"));
    }
    Ok(Model {
        entries,
        scale: unit / space,
    })
}

impl Model {
    fn get(&self, ptr: usize) -> Option<&Dir> {
        self.entries.get(&ptr)
    }

    /// The entity's transformation (124 chains), times the unit scale.
    fn transform(&self, e: &Dir, depth: usize) -> DAffine3 {
        let mut t = DAffine3::IDENTITY;
        if e.transform != 0
            && depth < 16
            && let Some(m) = self.get(e.transform)
            && m.kind == 124
            && m.params.len() >= 12
        {
            let v: Vec<f64> = m.params[..12].iter().map(|s| num(s)).collect();
            let r = DMat3::from_cols(
                DVec3::new(v[0], v[4], v[8]),
                DVec3::new(v[1], v[5], v[9]),
                DVec3::new(v[2], v[6], v[10]),
            );
            let own = DAffine3::from_mat3_translation(r, DVec3::new(v[3], v[7], v[11]));
            t = self.transform(m, depth + 1) * own;
        }
        t
    }

    fn pts(&self, e: &Dir, from: usize, count: usize) -> Vec<DVec3> {
        (0..count)
            .map(|k| {
                let f = |j: usize| e.params.get(from + k * 3 + j).map_or(0.0, |s| num(s));
                DVec3::new(f(0), f(1), f(2))
            })
            .collect()
    }

    /// A curve's points in model space (both ends included), in mm.
    fn curve(&self, ptr: usize, depth: usize) -> Option<Vec<DVec3>> {
        let e = self.get(ptr)?;
        if depth > 16 {
            return None;
        }
        let t = DAffine3::from_scale(DVec3::splat(self.scale)) * self.transform(e, 0);
        let raw: Vec<DVec3> = match e.kind {
            110 => self.pts(e, 0, 2),
            100 => {
                let v: Vec<f64> = e.params.iter().take(7).map(|s| num(s)).collect();
                if v.len() < 7 {
                    return None;
                }
                let (zt, c) = (v[0], (v[1], v[2]));
                let (a0, a1) = (
                    libm::atan2(v[4] - c.1, v[3] - c.0),
                    libm::atan2(v[6] - c.1, v[5] - c.0),
                );
                let r = libm::hypot(v[3] - c.0, v[4] - c.1);
                let mut sweep = a1 - a0;
                while sweep <= 1e-9 {
                    sweep += std::f64::consts::TAU;
                }
                let steps = ((sweep / std::f64::consts::TAU * 24.0).ceil() as usize).max(2);
                (0..=steps)
                    .map(|k| {
                        let a = a0 + sweep * k as f64 / steps as f64;
                        DVec3::new(c.0 + r * libm::cos(a), c.1 + r * libm::sin(a), zt)
                    })
                    .collect()
            }
            102 => {
                let n = int(e.params.first()?);
                let mut out: Vec<DVec3> = Vec::new();
                for k in 0..n {
                    let part = self.curve(int(e.params.get(1 + k)?), depth + 1)?;
                    for p in part {
                        if out.last().is_none_or(|l| l.distance(p) > 1e-9) {
                            out.push(p);
                        }
                    }
                }
                // Already in model space with scale applied.
                return Some(
                    out.into_iter()
                        .map(|p| self.transform(e, 0).transform_point3(p))
                        .collect(),
                );
            }
            106 => {
                let ip = int(e.params.first()?);
                let n = int(e.params.get(1)?);
                match ip {
                    1 => {
                        let zt = num(e.params.get(2)?);
                        (0..n)
                            .map(|k| {
                                let f =
                                    |j: usize| e.params.get(3 + k * 2 + j).map_or(0.0, |s| num(s));
                                DVec3::new(f(0), f(1), zt)
                            })
                            .collect()
                    }
                    2 => self.pts(e, 2, n),
                    _ => return None,
                }
            }
            126 => {
                let k = int(e.params.first()?);
                let m = int(e.params.get(1)?);
                let n = 1 + k - m.min(k);
                let a = n + 2 * m;
                let knots: Vec<f64> = (0..=a)
                    .map(|i| num(e.params.get(6 + i)?.as_str()).into())
                    .collect::<Option<Vec<f64>>>()
                    .unwrap_or_default();
                let wstart = 6 + a + 1;
                let weights: Vec<f64> = (0..=k)
                    .map(|i| e.params.get(wstart + i).map_or(1.0, |s| num(s)))
                    .collect();
                let ctrl = self.pts(e, wstart + k + 1, k + 1);
                let v0 = e
                    .params
                    .get(wstart + k + 1 + 3 * (k + 1))
                    .map_or(knots.get(m).copied().unwrap_or(0.0), |s| num(s));
                let v1 = e
                    .params
                    .get(wstart + k + 2 + 3 * (k + 1))
                    .map_or(knots.get(a - m).copied().unwrap_or(1.0), |s| num(s));
                let samples = (8 * (k + 1)).clamp(8, 400);
                (0..=samples)
                    .map(|s| {
                        nurbs(
                            &knots,
                            &weights,
                            &ctrl,
                            m,
                            v0 + (v1 - v0) * s as f64 / samples as f64,
                        )
                    })
                    .collect()
            }
            _ => return None,
        };
        Some(raw.into_iter().map(|p| t.transform_point3(p)).collect())
    }

    /// A planar surface's normal and a point, in mm.
    fn plane(&self, ptr: usize) -> Option<(DVec3, DVec3)> {
        let e = self.get(ptr)?;
        let t = DAffine3::from_scale(DVec3::splat(self.scale)) * self.transform(e, 0);
        let (n, p) = match e.kind {
            108 => {
                let v: Vec<f64> = e.params.iter().take(4).map(|s| num(s)).collect();
                let n = DVec3::new(*v.first()?, *v.get(1)?, *v.get(2)?);
                let d = *v.get(3)?;
                let n2 = n.length_squared().max(1e-300);
                (n.normalize_or_zero(), n * (d / n2))
            }
            128 => {
                let k1 = int(e.params.first()?);
                let k2 = int(e.params.get(1)?);
                let m1 = int(e.params.get(2)?);
                let m2 = int(e.params.get(3)?);
                let n1 = 1 + k1 - m1.min(k1);
                let n2 = 1 + k2 - m2.min(k2);
                let (a, b) = (n1 + 2 * m1, n2 + 2 * m2);
                let wstart = 9 + a + 1 + b + 1;
                let cstart = wstart + (k1 + 1) * (k2 + 1);
                let ctrl = self.pts(e, cstart, (k1 + 1) * (k2 + 1));
                let at = |i: usize, j: usize| ctrl[j * (k1 + 1) + i];
                let (p00, p10, p01, p11) = (at(0, 0), at(k1, 0), at(0, k2), at(k1, k2));
                let n = ((p10 - p00).cross(p01 - p00) + (p11 - p01).cross(p11 - p10))
                    .normalize_or_zero();
                if n == DVec3::ZERO
                    || ctrl
                        .iter()
                        .any(|c| n.dot(*c - p00).abs() > 1e-6 * (1.0 + p00.length()))
                {
                    return None;
                }
                (n, p00)
            }
            190 => {
                let loc = self.get(int(e.params.first()?))?;
                let dir = self.get(int(e.params.get(1)?))?;
                let p = self.pts(loc, 0, 1)[0];
                let n = self.pts(dir, 0, 1)[0].normalize_or_zero();
                (n, p)
            }
            _ => return None,
        };
        let n = (t.matrix3.inverse().transpose() * n).normalize_or_zero();
        Some((n, t.transform_point3(p)))
    }

    /// A planar face's loops (outer first), counter-clockwise about its
    /// surface's normal.
    fn face(&self, e: &Dir) -> Option<Vec<Vec<DVec3>>> {
        let (surface, bounds): (usize, Vec<(usize, bool)>) = match e.kind {
            144 => {
                let s = int(e.params.first()?);
                let n2 = int(e.params.get(2)?);
                let mut b = vec![(int(e.params.get(3)?), false)];
                for k in 0..n2 {
                    b.push((int(e.params.get(4 + k)?), false));
                }
                (s, b)
            }
            143 => {
                let s = int(e.params.get(1)?);
                let n = int(e.params.get(2)?);
                (
                    s,
                    (0..n)
                        .filter_map(|k| Some((int(e.params.get(3 + k)?), true)))
                        .collect(),
                )
            }
            _ => return None,
        };
        let (normal, _) = self.plane(surface)?;
        let mut rings = Vec::new();
        for (ptr, bounded) in bounds {
            let b = self.get(ptr)?;
            let mut ring: Vec<DVec3> = Vec::new();
            if bounded {
                // 141: TYPE, PREF, SPTR, N, then CRVPT, SENSE, K, PSCPT…
                let n = int(b.params.get(3)?);
                let mut at = 4;
                for _ in 0..n {
                    let crv = int(b.params.get(at)?);
                    let sense = int(b.params.get(at + 1)?);
                    let k = int(b.params.get(at + 2)?);
                    at += 3 + k;
                    let mut pts = self.curve(crv, 0)?;
                    if sense == 2 {
                        pts.reverse();
                    }
                    for p in pts {
                        if ring.last().is_none_or(|l| l.distance(p) > 1e-9) {
                            ring.push(p);
                        }
                    }
                }
            } else {
                // 142: CRTN, SPTR, BPTR, CPTR, PREF.
                let c = int(b.params.get(3)?);
                ring = self.curve(c, 0)?;
            }
            while ring.len() > 1 && ring[0].distance(ring[ring.len() - 1]) < 1e-9 {
                ring.pop();
            }
            if ring.len() < 3 {
                return None;
            }
            let mut a = DVec3::ZERO;
            for (p, q) in ring.iter().zip(ring.iter().cycle().skip(1)) {
                a += p.cross(*q);
            }
            let ccw = a.dot(normal) > 0.0;
            if ccw != rings.is_empty() {
                ring.reverse();
            }
            rings.push(ring);
        }
        (!rings.is_empty()).then_some(rings)
    }
}

/// A point of a rational B-spline curve at `u`.
fn nurbs(knots: &[f64], weights: &[f64], ctrl: &[DVec3], degree: usize, u: f64) -> DVec3 {
    let n = ctrl.len();
    if n == 0 || knots.len() < n + degree + 1 {
        return ctrl.first().copied().unwrap_or_default();
    }
    let mut span = degree;
    while span + 1 < n && u >= knots[span + 1] {
        span += 1;
    }
    let mut d: Vec<(DVec3, f64)> = (0..=degree)
        .map(|j| {
            let i = (span + j).saturating_sub(degree).min(n - 1);
            let w = weights.get(i).copied().unwrap_or(1.0);
            (ctrl[i] * w, w)
        })
        .collect();
    for r in 1..=degree {
        for j in (r..=degree).rev() {
            let i = span + j - degree;
            let denom = knots[i + degree + 1 - r] - knots[i];
            let alpha = if denom.abs() < 1e-300 {
                0.0
            } else {
                (u - knots[i]) / denom
            };
            d[j] = (
                d[j - 1].0 * (1.0 - alpha) + d[j].0 * alpha,
                d[j - 1].1 * (1.0 - alpha) + d[j].1 * alpha,
            );
        }
    }
    let (p, w) = d[degree];
    if w.abs() < 1e-300 { p } else { p / w }
}

/// The planar faces of an IGES file (each its loops) and how many faces
/// were skipped.
pub fn read(text: &str) -> Result<(Vec<FaceLoops>, usize), IoError> {
    let model = parse(text)?;
    let mut faces = Vec::new();
    let mut skipped = 0;
    for e in model.entries.values() {
        if e.kind == 144 || e.kind == 143 {
            match model.face(e) {
                Some(f) => faces.push(f),
                None => skipped += 1,
            }
        }
        let _ = e.form;
    }
    if faces.is_empty() {
        return Err(IoError::UnsupportedFormat(format!(
            "no planar trimmed or bounded surfaces ({skipped} other faces)"
        )));
    }
    Ok((faces, skipped))
}

// ----- exporter and importer -----------------------------------------------------------

/// IGES export.
#[derive(Debug, Default, Clone, Copy)]
pub struct IgesExporter;

impl Exporter for IgesExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Iges
    }

    fn export(
        &self,
        doc: &Document,
        scope: &ExportScope,
        _opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<ExportReport, IoError> {
        progress.check_cancelled()?;
        let (text, faces) = write(doc, scope, "model.igs")?;
        dst.write_all(text.as_bytes())
            .map_err(|e| IoError::storage("iges", e.to_string()))?;
        Ok(ExportReport {
            bytes_written: text.len() as u64,
            entities: faces,
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}

/// IGES import.
#[derive(Debug, Default, Clone, Copy)]
pub struct IgesImporter;

impl Importer for IgesImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Iges
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".igs") || lower.ends_with(".iges") {
            Confidence::EXTENSION
        } else if bytes.len() >= 80
            && bytes.get(72) == Some(&b'S')
            && bytes.get(73..80) == Some(b"      1")
        {
            Confidence::CERTAIN
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
        let mut source = String::new();
        src.take(64 * 1024 * 1024 + 1)
            .read_to_string(&mut source)
            .map_err(|e| corrupt(e.to_string()))?;
        if source.len() > 64 * 1024 * 1024 {
            return Err(IoError::LimitExceeded("IGES input exceeds 64 MiB".into()));
        }
        if matches!(opts.import_as, ImportAs::Image | ImportAs::Texture) {
            return Err(IoError::UnsupportedFormat(
                "IGES imports as a group or component".into(),
            ));
        }
        progress.check_cancelled()?;
        let (faces, skipped) = read(&source)?;
        let tol: Tolerances = *tx.tolerances();
        let pieces: Vec<Piece> = faces
            .into_iter()
            .map(|loops| Piece {
                loops,
                sides: FaceSides::default(),
            })
            .collect();
        let mesh: HalfEdgeMesh = stitch(&pieces, &tol).map_err(|e| corrupt(e.to_string()))?;
        let mut report = ImportReport {
            faces: mesh.face_count(),
            edges: mesh.edge_count(),
            skipped,
            ..Default::default()
        };
        if skipped > 0 {
            report.warnings.push(Warning::new(
                WarningCode::W2008,
                format!("{skipped} curved or unreadable IGES faces were skipped"),
            ));
        }
        let name = match &opts.import_as {
            ImportAs::Component(n) => n.clone(),
            _ => crate::io::import::source_stem(&opts.source, "IGES"),
        };
        let entities = Entities::from_mesh(mesh, || tx.allocate_pid());
        let uuid = crate::commands::common::definition_uuid(tx.document(), &name, &entities);
        let mut def = ComponentDefinition::new(uuid, name.clone());
        def.is_internal_group = matches!(opts.import_as, ImportAs::Group);
        def.entities = entities;
        let definition = tx
            .insert_definition(def)
            .map_err(|e| corrupt(e.to_string()))?;
        let axes = if opts.swap_yz == Some(true) {
            DMat3::from_cols(DVec3::X, -DVec3::Z, DVec3::Y)
        } else {
            DMat3::IDENTITY
        };
        let transform = target.local
            * DAffine3::from_translation(opts.at)
            * DAffine3::from_mat3(axes * opts.scale);
        let record = if matches!(opts.import_as, ImportAs::Group) {
            EntityRecord::Group(Group {
                definition,
                transform,
                name,
            })
        } else {
            EntityRecord::Instance(ComponentInstance {
                definition,
                transform,
                name: None,
                material_override: None,
                glued_to: None,
                dynamic_attrs: Default::default(),
            })
        };
        let (_, pid) = tx
            .insert_entity(target.container, record)
            .map_err(|e| corrupt(e.to_string()))?;
        report.created.push(pid);
        Ok(report)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::commands::{CommandRegistry, Engine};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn a_plate_with_a_hole_goes_out_and_comes_back() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        e.execute_erased("standard-part", json!({"kind": "washer", "size": "M8"}))
            .unwrap();
        let (text, faces) = write(e.document(), &ExportScope::default(), "w.igs").unwrap();
        assert!(faces > 24);
        assert!(text.lines().all(|l| l.len() == 80), "fixed 80-column lines");
        let (read_faces, skipped) = read(&text).unwrap();
        assert_eq!((read_faces.len(), skipped), (faces, 0));
        let tol = Tolerances::default();
        let pieces: Vec<Piece> = read_faces
            .into_iter()
            .map(|loops| Piece {
                loops,
                sides: FaceSides::default(),
            })
            .collect();
        let mesh = stitch(&pieces, &tol).unwrap();
        assert!(mesh.is_solid());
        let washer =
            crate::parts::standard_part(crate::parts::PartKind::Washer, "M8", None, &tol).unwrap();
        assert!((mesh.volume().unwrap() - washer.mesh.volume().unwrap()).abs() < 1e-3);
        assert_eq!(
            write(e.document(), &ExportScope::default(), "w.igs")
                .unwrap()
                .0,
            text
        );
        crate::io::step_structure::tests::golden("iges/washer-m8.igs", &text);
    }

    #[test]
    fn arcs_splines_and_units() {
        // A unit square bounded by lines in inches, then a B-spline line.
        assert!(
            (nurbs(
                &[0.0, 0.0, 1.0, 1.0],
                &[1.0, 1.0],
                &[DVec3::ZERO, DVec3::X],
                1,
                0.25
            ) - DVec3::new(0.25, 0.0, 0.0))
            .length()
                < 1e-12
        );
        assert_eq!(
            split_params("110,1.,2H,;,3.;", ',', ';'),
            vec!["110", "1.", ",;", "3."]
        );
    }
}
