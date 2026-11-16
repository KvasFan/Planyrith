//! Implements docs/09-file-formats.md §5.1 IGES (milestone 1.17): the
//! Initial Graphics Exchange Specification 5.3 in its fixed 80-column
//! form.
//!
//! Export writes each planar face as a trimmed surface (type 144) on a
//! bilinear plane patch (128), its loops as curves on the surface (142)
//! of composite curves (102) of lines (110), in millimeters; loose geometry
//! and groups are placed in world coordinates, and each component
//! definition is a subfigure (308) placed by instances (408) with their
//! transformation matrices (124, milestone 1.38). Import reads trimmed
//! (144) and bounded (143) surfaces on planar (108, 128, 190) and curved
//! (128, 120, 122, 118) surfaces with boundaries of lines, arcs (100),
//! composite curves, copious data (106) and B-spline curves (126,
//! sampled), applying transformation matrices (124) and the file's unit;
//! curved surfaces become triangles (docs/09 Decisions, 2026-11-16) and
//! subfigures become components.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{Read, Write};

use glam::{DAffine3, DMat3, DVec3};

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::diagnostics::{Warning, WarningCode};
use crate::document::{
    ComponentDefinition, ComponentInstance, ContainerRef, Document, Entities, EntityRecord, Group,
    InstancePath,
};
use crate::geom::FaceSides;
use crate::geom::HalfEdgeMesh;
use crate::geom::boolean::{Piece, stitch};
use crate::ids::{DefinitionId, EntityId};
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
    /// Directory pointer of its transformation matrix (124), or 0.
    transform: usize,
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
            transform: 0,
        });
        self.entities.len() * 2 - 1
    }

    /// A transformation matrix (124): form 0 for rotations, 1 when it
    /// mirrors.
    fn matrix(&mut self, t: &DAffine3) -> usize {
        let m = t.matrix3;
        let mut p = Vec::new();
        for i in 0..3 {
            p.extend([m.x_axis[i], m.y_axis[i], m.z_axis[i], t.translation[i]].map(real));
        }
        let form = u32::from(m.determinant() < 0.0);
        self.add(124, form, p, true)
    }

    /// A subfigure definition (308) of `members`.
    fn subfigure(&mut self, name: &str, members: &[usize], nested: bool) -> usize {
        let mut p = vec![
            if nested { "1" } else { "0" }.to_string(),
            hollerith(name),
            members.len().to_string(),
        ];
        p.extend(members.iter().map(|m| m.to_string()));
        self.add(308, 0, p, true)
    }

    /// A subfigure instance (408) of `definition` placed by `t`.
    fn instance(&mut self, definition: usize, t: &DAffine3, dependent: bool) -> usize {
        let matrix = self.matrix(t);
        let p = vec![
            definition.to_string(),
            real(0.0),
            real(0.0),
            real(0.0),
            real(1.0),
        ];
        let at = self.add(408, 0, p, dependent);
        if let Some(e) = self.entities.last_mut() {
            e.transform = matrix;
        }
        at
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

    fn face(&mut self, rings: &[Vec<DVec3>], dependent: bool) -> Option<usize> {
        let outer = rings.first()?;
        if outer.len() < 3 {
            return None;
        }
        let mut n = DVec3::ZERO;
        for (a, b) in outer.iter().zip(outer.iter().cycle().skip(1)) {
            n += a.cross(*b);
        }
        let n = n.normalize_or_zero();
        let mut u = (outer[1] - outer[0]).normalize_or_zero();
        u = (u - n * u.dot(n)).normalize_or_zero();
        if u == DVec3::ZERO {
            return None;
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
        Some(self.add(144, 0, p, dependent))
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
                e.kind, p_at, 0, 0, 0, 0, e.transform, 0, status, d
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
    // Faces under a component instance go to its definition's subfigure
    // (308, once per definition, nested components flattened into it);
    // each outermost instance becomes a placed subfigure (408). Other faces
    // are written in world coordinates (milestone 1.38).
    let mut subfigures: Vec<(DefinitionId, String, Vec<FaceLoops>)> = Vec::new();
    let mut placements: Vec<(DefinitionId, DAffine3)> = Vec::new();
    let mut placed: std::collections::BTreeSet<InstancePath> = Default::default();
    for occ in &sc.occurrences {
        let Some(entities) = doc.container(occ.container) else {
            continue;
        };
        let mesh = entities.mesh();
        let outer = outermost_instance(doc, &occ.path);
        let (frame, sink) = match &outer {
            Some((prefix, world, d, name)) => {
                if placed.insert(prefix.clone()) {
                    placements.push((*d, *world));
                }
                let fresh = !subfigures.iter().any(|(e, _, _)| e == d);
                if fresh {
                    subfigures.push((*d, name.clone(), Vec::new()));
                }
                // Only the first occurrence of a definition fills it.
                let first = placements.iter().find(|(e, _)| e == d).map(|(_, t)| *t);
                if first.is_some_and(|t| t != *world) {
                    continue;
                }
                (world.inverse() * occ.world, Some(*d))
            }
            None => (occ.world, None),
        };
        let flip = frame.matrix3.determinant() < 0.0;
        for (_, face) in mesh.faces() {
            let mut loops = vec![face.outer];
            loops.extend(face.holes.iter().copied());
            let rings: Vec<Vec<DVec3>> = loops
                .into_iter()
                .filter_map(|l| mesh.loop_positions(l).ok())
                .map(|r| {
                    let mut r: Vec<DVec3> =
                        r.into_iter().map(|p| frame.transform_point3(p)).collect();
                    if flip {
                        r.reverse();
                    }
                    r
                })
                .collect();
            match sink {
                Some(d) => {
                    if let Some((_, _, list)) = subfigures.iter_mut().find(|(e, _, _)| *e == d) {
                        list.push(rings);
                    }
                }
                None => {
                    if w.face(&rings, false).is_some() {
                        faces += 1;
                    }
                }
            }
        }
    }
    let mut defined: BTreeMap<DefinitionId, usize> = BTreeMap::new();
    for (d, name, list) in &subfigures {
        let members: Vec<usize> = list.iter().filter_map(|r| w.face(r, true)).collect();
        if members.is_empty() {
            continue;
        }
        defined.insert(*d, w.subfigure(name, &members, false));
    }
    for (d, world) in &placements {
        if let Some(def) = defined.get(d) {
            w.instance(*def, world, false);
            faces += subfigures
                .iter()
                .find(|(e, _, _)| e == d)
                .map_or(0, |(_, _, l)| l.len());
        }
    }
    if faces == 0 {
        return Err(IoError::UnsupportedFormat(
            "nothing visible to export".into(),
        ));
    }
    Ok((w.text(file_name), faces))
}

/// The outermost component instance on `path`: its path, world transform,
/// definition and name.
fn outermost_instance(
    doc: &Document,
    path: &InstancePath,
) -> Option<(InstancePath, DAffine3, DefinitionId, String)> {
    let mut container = ContainerRef::Model;
    let mut world = DAffine3::IDENTITY;
    for (k, step) in path.steps.iter().enumerate() {
        let entities = doc.container(container)?;
        let id = entities.key_to_id(*step)?;
        world *= entities.transform_of(id)?;
        let d = entities.definition_of(id)?;
        if let EntityId::Instance(_) = id {
            let name = doc
                .definition(d)
                .map_or_else(|| "Component".to_string(), |def| def.name.clone());
            return Some((
                InstancePath {
                    steps: path.steps[..=k].to_vec(),
                },
                world,
                d,
                name,
            ));
        }
        container = ContainerRef::Definition(d);
    }
    None
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
    /// Physically or logically dependent (the status's subordinate switch).
    dependent: bool,
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
        // Status AABBCCDD: blank, subordinate, use, hierarchy.
        let status = format!("{:0>8}", field(a, 8));
        let dependent = status.get(2..4).is_some_and(|s| s != "00");
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
                dependent,
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

    /// A face as pieces: one planar face with its loops (outer first,
    /// counter-clockwise about its surface's normal), or the triangles of a
    /// curved one (`true`).
    fn face(&self, e: &Dir) -> Option<(Vec<FaceLoops>, bool)> {
        // The surface, whether the outer loop is the surface's own
        // boundary, and the boundaries.
        let (surface, natural, bounds): (usize, bool, Vec<(usize, bool)>) = match e.kind {
            144 => {
                let s = int(e.params.first()?);
                let n1 = int(e.params.get(1)?);
                let n2 = int(e.params.get(2)?);
                let mut b = Vec::new();
                if n1 != 0 {
                    b.push((int(e.params.get(3)?), false));
                }
                for k in 0..n2 {
                    b.push((int(e.params.get(4 + k)?), false));
                }
                (s, n1 == 0, b)
            }
            143 => {
                let s = int(e.params.get(1)?);
                let n = int(e.params.get(2)?);
                (
                    s,
                    false,
                    (0..n)
                        .filter_map(|k| Some((int(e.params.get(3 + k)?), true)))
                        .collect(),
                )
            }
            _ => return None,
        };
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
            rings.push(ring);
        }
        if let Some((normal, _)) = self.plane(surface) {
            if natural {
                return None;
            }
            for (k, ring) in rings.iter_mut().enumerate() {
                let mut a = DVec3::ZERO;
                for (p, q) in ring.iter().zip(ring.iter().cycle().skip(1)) {
                    a += p.cross(*q);
                }
                let ccw = a.dot(normal) > 0.0;
                if ccw != (k == 0) {
                    ring.reverse();
                }
            }
            return (!rings.is_empty()).then(|| (vec![rings], false));
        }
        let surf = self.surf(surface)?;
        let loops = if natural {
            if !rings.is_empty() {
                // Holes in the whole surface: not supported without an
                // outer loop to triangulate against.
                let mut whole = self.natural_loop(&surf);
                whole.extend(rings);
                Some(whole)
            } else {
                None
            }
        } else {
            Some(rings)
        };
        self.tessellate(&surf, loops).map(|t| (t, true))
    }

    /// The model-space loop around a surface's parameter rectangle.
    fn natural_loop(&self, surf: &Surf) -> Vec<Vec<DVec3>> {
        let ((u0, u1), (v0, v1)) = surf.domain();
        let corners = [(u0, v0), (u1, v0), (u1, v1), (u0, v1)];
        let mut ring = Vec::new();
        for (k, &(a, b)) in corners.iter().enumerate() {
            let (c, d) = corners[(k + 1) % 4];
            for s in 0..16 {
                let t = f64::from(s) / 16.0;
                ring.push(surf.at(a + (c - a) * t, b + (d - b) * t));
            }
        }
        vec![ring]
    }
}

// ----- curved surfaces (milestone 1.38) --------------------------------------------------

/// A surface evaluated in millimeters over its parameter rectangle.
enum Surf {
    /// A rational B-spline surface (128).
    Spline {
        degree: (usize, usize),
        knots: (Vec<f64>, Vec<f64>),
        /// Weighted control points, `j * (k1 + 1) + i`.
        ctrl: Vec<glam::DVec4>,
        rows: usize,
        domain: ((f64, f64), (f64, f64)),
    },
    /// A generatrix turned about an axis (120); `u` runs along it, `v` is
    /// the angle.
    Revolution {
        axis: (DVec3, DVec3),
        generatrix: Vec<DVec3>,
        angles: (f64, f64),
    },
    /// A directrix swept by a vector (122).
    Tabulated { directrix: Vec<DVec3>, shift: DVec3 },
    /// Straight lines between two curves (118).
    Ruled { a: Vec<DVec3>, b: Vec<DVec3> },
}

/// A polyline at arc-length share `t` (0 to 1).
fn along(points: &[DVec3], t: f64) -> DVec3 {
    let n = points.len();
    if n < 2 {
        return points.first().copied().unwrap_or_default();
    }
    let total: f64 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
    if total <= 0.0 {
        return points[0];
    }
    let mut want = t.clamp(0.0, 1.0) * total;
    for w in points.windows(2) {
        let l = w[0].distance(w[1]);
        if want <= l || l <= 0.0 && want <= 0.0 {
            return if l > 0.0 {
                w[0].lerp(w[1], want / l)
            } else {
                w[0]
            };
        }
        want -= l;
    }
    points[n - 1]
}

/// De Boor's algorithm on homogeneous points.
fn deboor(knots: &[f64], pts: &[glam::DVec4], degree: usize, u: f64) -> glam::DVec4 {
    let n = pts.len();
    if n == 0 || knots.len() < n + degree + 1 {
        return pts.first().copied().unwrap_or(glam::DVec4::W);
    }
    let mut span = degree;
    while span + 1 < n && u >= knots[span + 1] {
        span += 1;
    }
    let mut d: Vec<glam::DVec4> = (0..=degree)
        .map(|j| pts[(span + j).saturating_sub(degree).min(n - 1)])
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
            d[j] = d[j - 1] * (1.0 - alpha) + d[j] * alpha;
        }
    }
    d[degree]
}

impl Surf {
    fn domain(&self) -> ((f64, f64), (f64, f64)) {
        match self {
            Surf::Spline { domain, .. } => *domain,
            Surf::Revolution { angles, .. } => ((0.0, 1.0), *angles),
            Surf::Tabulated { .. } | Surf::Ruled { .. } => ((0.0, 1.0), (0.0, 1.0)),
        }
    }

    fn at(&self, u: f64, v: f64) -> DVec3 {
        match self {
            Surf::Spline {
                degree,
                knots,
                ctrl,
                rows,
                ..
            } => {
                let cols = ctrl.len() / rows.max(&1);
                let column: Vec<glam::DVec4> = (0..*rows)
                    .map(|j| deboor(&knots.0, &ctrl[j * cols..(j + 1) * cols], degree.0, u))
                    .collect();
                let h = deboor(&knots.1, &column, degree.1, v);
                if h.w.abs() < 1e-300 {
                    h.truncate()
                } else {
                    h.truncate() / h.w
                }
            }
            Surf::Revolution {
                axis, generatrix, ..
            } => {
                let p = along(generatrix, u);
                let k = (axis.1 - axis.0).normalize_or_zero();
                let r = p - axis.0;
                let (s, c) = (libm::sin(v), libm::cos(v));
                axis.0 + r * c + k.cross(r) * s + k * k.dot(r) * (1.0 - c)
            }
            Surf::Tabulated { directrix, shift } => along(directrix, u) + *shift * v,
            Surf::Ruled { a, b } => along(a, u).lerp(along(b, u), v),
        }
    }

    /// The surface normal (unnormalized) by central differences.
    fn normal(&self, u: f64, v: f64) -> DVec3 {
        let ((u0, u1), (v0, v1)) = self.domain();
        let (hu, hv) = ((u1 - u0) * 1e-5, (v1 - v0) * 1e-5);
        let (ua, ub) = ((u - hu).max(u0), (u + hu).min(u1));
        let (va, vb) = ((v - hv).max(v0), (v + hv).min(v1));
        let su = self.at(ub, v) - self.at(ua, v);
        let sv = self.at(u, vb) - self.at(u, va);
        su.cross(sv)
    }

    /// The parameters of the surface point nearest `p`: the nearest of a
    /// grid, then Gauss–Newton steps.
    fn invert(&self, p: DVec3, grid: &[(f64, f64, DVec3)]) -> (f64, f64) {
        let ((u0, u1), (v0, v1)) = self.domain();
        let (mut u, mut v) = grid
            .iter()
            .min_by(|a, b| a.2.distance(p).total_cmp(&b.2.distance(p)))
            .map_or((u0, v0), |g| (g.0, g.1));
        for _ in 0..12 {
            let (hu, hv) = ((u1 - u0) * 1e-6, (v1 - v0) * 1e-6);
            let s = self.at(u, v);
            let su = (self.at((u + hu).min(u1), v) - self.at((u - hu).max(u0), v))
                / ((u + hu).min(u1) - (u - hu).max(u0));
            let sv = (self.at(u, (v + hv).min(v1)) - self.at(u, (v - hv).max(v0)))
                / ((v + hv).min(v1) - (v - hv).max(v0));
            let r = p - s;
            let (a, b, c) = (su.dot(su), su.dot(sv), sv.dot(sv));
            let det = a * c - b * b;
            if det.abs() < 1e-300 {
                break;
            }
            let (ru, rv) = (su.dot(r), sv.dot(r));
            let du = (c * ru - b * rv) / det;
            let dv = (a * rv - b * ru) / det;
            u = (u + du).clamp(u0, u1);
            v = (v + dv).clamp(v0, v1);
            if du.abs() < 1e-12 * (u1 - u0) && dv.abs() < 1e-12 * (v1 - v0) {
                break;
            }
        }
        (u, v)
    }
}

/// The most triangles one curved face may become.
const MAX_FACE_TRIANGLES: usize = 40_000;

impl Model {
    /// A curved surface, or `None` for planar and unsupported ones.
    fn surf(&self, ptr: usize) -> Option<Surf> {
        let e = self.get(ptr)?;
        let to_mm = DAffine3::from_scale(DVec3::splat(self.scale)) * self.transform(e, 0);
        // Curves come back in mm already; the surface's own transform acts
        // on them in model units.
        let conj = to_mm * DAffine3::from_scale(DVec3::splat(1.0 / self.scale));
        let curve = |k: usize| -> Option<Vec<DVec3>> {
            let c = self.curve(int(e.params.get(k)?), 0)?;
            Some(c.into_iter().map(|p| conj.transform_point3(p)).collect())
        };
        match e.kind {
            128 => {
                if self.plane(ptr).is_some() {
                    return None;
                }
                let k1 = int(e.params.first()?);
                let k2 = int(e.params.get(1)?);
                let m1 = int(e.params.get(2)?);
                let m2 = int(e.params.get(3)?);
                let n1 = 1 + k1 - m1.min(k1);
                let n2 = 1 + k2 - m2.min(k2);
                let (a, b) = (n1 + 2 * m1, n2 + 2 * m2);
                let s_knots: Vec<f64> = (0..=a)
                    .map(|i| e.params.get(9 + i).map_or(0.0, |s| num(s)))
                    .collect();
                let t_knots: Vec<f64> = (0..=b)
                    .map(|i| e.params.get(10 + a + i).map_or(0.0, |s| num(s)))
                    .collect();
                let wstart = 9 + a + 1 + b + 1;
                let count = (k1 + 1) * (k2 + 1);
                let cstart = wstart + count;
                let pts = self.pts(e, cstart, count);
                let ctrl: Vec<glam::DVec4> = (0..count)
                    .map(|i| {
                        let w = e.params.get(wstart + i).map_or(1.0, |s| num(s));
                        let q = to_mm.transform_point3(pts[i]);
                        (q * w).extend(w)
                    })
                    .collect();
                let d = cstart + 3 * count;
                let f = |k: usize, or: f64| e.params.get(d + k).map_or(or, |s| num(s));
                let domain = (
                    (f(0, s_knots[m1]), f(1, s_knots[a - m1])),
                    (f(2, t_knots[m2]), f(3, t_knots[b - m2])),
                );
                if !(domain.0.1 > domain.0.0 && domain.1.1 > domain.1.0) {
                    return None;
                }
                Some(Surf::Spline {
                    degree: (m1, m2),
                    knots: (s_knots, t_knots),
                    ctrl,
                    rows: k2 + 1,
                    domain,
                })
            }
            120 => {
                let axis = curve(0)?;
                let generatrix = curve(1)?;
                let (sa, ta) = (num(e.params.get(2)?), num(e.params.get(3)?));
                (axis.len() >= 2 && generatrix.len() >= 2 && ta > sa).then(|| Surf::Revolution {
                    axis: (axis[0], axis[axis.len() - 1]),
                    generatrix,
                    angles: (sa, ta),
                })
            }
            122 => {
                let directrix = curve(0)?;
                let end = DVec3::new(
                    num(e.params.get(1)?),
                    num(e.params.get(2)?),
                    num(e.params.get(3)?),
                );
                let end = to_mm.transform_point3(end);
                let first = *directrix.first()?;
                Some(Surf::Tabulated {
                    directrix,
                    shift: end - first,
                })
            }
            118 => {
                let a = curve(0)?;
                let mut b = curve(1)?;
                if int(e.params.get(2)?) == 1 {
                    b.reverse();
                }
                Some(Surf::Ruled { a, b })
            }
            _ => None,
        }
    }

    /// Triangles approximating `surface` inside the loops (model-space
    /// points; `None` takes the whole parameter rectangle), each a face of
    /// three corners.
    fn tessellate(&self, surface: &Surf, loops: Option<Vec<Vec<DVec3>>>) -> Option<Vec<FaceLoops>> {
        let ((u0, u1), (v0, v1)) = surface.domain();
        let grid: Vec<(f64, f64, DVec3)> = (0..=24)
            .flat_map(|i| {
                (0..=24).map(move |j| {
                    let u = u0 + (u1 - u0) * f64::from(i) / 24.0;
                    let v = v0 + (v1 - v0) * f64::from(j) / 24.0;
                    (u, v)
                })
            })
            .map(|(u, v)| (u, v, surface.at(u, v)))
            .collect();
        // Closed in u or v (the far edge meets the near one).
        let size = grid
            .iter()
            .fold(Aabb3::default(), |b, g| b.with(g.2))
            .diagonal()
            .max(1e-9);
        let closed_u = (0..=8).all(|k| {
            let v = v0 + (v1 - v0) * f64::from(k) / 8.0;
            surface.at(u0, v).distance(surface.at(u1, v)) < 1e-6 * size
        });
        let closed_v = (0..=8).all(|k| {
            let u = u0 + (u1 - u0) * f64::from(k) / 8.0;
            surface.at(u, v0).distance(surface.at(u, v1)) < 1e-6 * size
        });
        // Loops in parameters, with their model-space points.
        let rings: Vec<Vec<(f64, f64, DVec3)>> = match loops {
            Some(loops) => loops
                .into_iter()
                .map(|ring| {
                    let mut out: Vec<(f64, f64, DVec3)> = Vec::new();
                    for p in ring {
                        let (mut u, mut v) = surface.invert(p, &grid);
                        if let Some(&(pu, pv, _)) = out.last() {
                            if closed_u && (u - pu).abs() > 0.5 * (u1 - u0) {
                                u += if u > pu { u0 - u1 } else { u1 - u0 };
                            }
                            if closed_v && (v - pv).abs() > 0.5 * (v1 - v0) {
                                v += if v > pv { v0 - v1 } else { v1 - v0 };
                            }
                        }
                        out.push((u, v, p));
                    }
                    out
                })
                .collect(),
            None => {
                let steps = 16;
                let mut ring = Vec::new();
                let corners = [(u0, v0), (u1, v0), (u1, v1), (u0, v1)];
                for (k, &(a, b)) in corners.iter().enumerate() {
                    let (c, d) = corners[(k + 1) % 4];
                    for s in 0..steps {
                        let t = f64::from(s) / f64::from(steps);
                        let (u, v) = (a + (c - a) * t, b + (d - b) * t);
                        ring.push((u, v, surface.at(u, v)));
                    }
                }
                vec![ring]
            }
        };
        if rings.first().is_none_or(|r| r.len() < 3) {
            return None;
        }
        // Ear clipping in parameters scaled by the surface's mean speed
        // each way, so lengths there are close to lengths on the surface.
        let speed = |du: bool| {
            let mut total = 0.0;
            for k in 0..=8 {
                let t = f64::from(k) / 8.0;
                let (a, b) = if du {
                    let v = v0 + (v1 - v0) * t;
                    (surface.at(u0, v), surface.at(u1, v))
                } else {
                    let u = u0 + (u1 - u0) * t;
                    (surface.at(u, v0), surface.at(u, v1))
                };
                // A closed direction's chord is zero: use the halfway point.
                let m = if du {
                    surface.at((u0 + u1) * 0.5, v0 + (v1 - v0) * t)
                } else {
                    surface.at(u0 + (u1 - u0) * t, (v0 + v1) * 0.5)
                };
                total += a.distance(m) + m.distance(b);
            }
            (total / 9.0).max(1e-9)
        };
        let (su, sv) = ((u1 - u0) / speed(true), (v1 - v0) / speed(false));
        let mut uv: Vec<(f64, f64)> = Vec::new();
        let mut xyz: Vec<Option<DVec3>> = Vec::new();
        let mut data = Vec::new();
        let mut holes = Vec::new();
        for (k, ring) in rings.iter().enumerate() {
            if k > 0 {
                holes.push(uv.len());
            }
            for &(u, v, p) in ring {
                data.push((u - u0) / su);
                data.push((v - v0) / sv);
                uv.push((u, v));
                xyz.push(Some(p));
            }
        }
        let flat = earcutr::earcut(&data, &holes, 2).ok()?;
        // Collinear boundary samples give triangles without area in the
        // parameters; on a curved surface they would be slivers over
        // others, so they go.
        let uv_area = |t: &[usize; 3]| {
            let (a, b, c) = (
                data_at(&data, t[0]),
                data_at(&data, t[1]),
                data_at(&data, t[2]),
            );
            ((b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)).abs() * 0.5
        };
        let mut tris: Vec<[usize; 3]> = flat
            .chunks(3)
            .filter(|c| c.len() == 3)
            .map(|c| [c[0], c[1], c[2]])
            .filter(|t| uv_area(t) > 1e-12)
            .collect();
        let point = |uv: &[(f64, f64)], xyz: &[Option<DVec3>], i: usize| {
            xyz[i].unwrap_or_else(|| surface.at(uv[i].0, uv[i].1))
        };
        // Grid points inside the loops refine the triangles: the coarsest
        // grid whose cells stay within a chord tolerance of the surface.
        let limit = (2e-3 * size).max(0.01);
        let mut n = 4usize;
        while n < 96 {
            let cell = |i: usize, j: usize| {
                let u = |k: usize| u0 + (u1 - u0) * k as f64 / n as f64;
                let v = |k: usize| v0 + (v1 - v0) * k as f64 / n as f64;
                let corners = [
                    surface.at(u(i), v(j)),
                    surface.at(u(i + 1), v(j)),
                    surface.at(u(i), v(j + 1)),
                    surface.at(u(i + 1), v(j + 1)),
                ];
                let mid = corners.iter().copied().sum::<DVec3>() / 4.0;
                surface
                    .at((u(i) + u(i + 1)) * 0.5, (v(j) + v(j + 1)) * 0.5)
                    .distance(mid)
            };
            let worst = (0..n)
                .flat_map(|i| (0..n).map(move |j| (i, j)))
                .map(|(i, j)| cell(i, j))
                .fold(0.0, f64::max);
            if worst <= limit {
                break;
            }
            n *= 2;
        }
        let scaled = |p: (f64, f64)| ((p.0 - u0) / su, (p.1 - v0) / sv);
        let segments: Vec<((f64, f64), (f64, f64))> = rings
            .iter()
            .flat_map(|r| {
                r.iter()
                    .zip(r.iter().cycle().skip(1))
                    .map(|(a, b)| (scaled((a.0, a.1)), scaled((b.0, b.1))))
                    .collect::<Vec<_>>()
            })
            .collect();
        let spacing =
            (1.0 / su.max(1e-300) * (u1 - u0)).min(1.0 / sv.max(1e-300) * (v1 - v0)) / n as f64;
        let near_loop = |q: (f64, f64)| {
            segments.iter().any(|(a, b)| {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let len2 = dx * dx + dy * dy;
                let t = if len2 > 0.0 {
                    (((q.0 - a.0) * dx + (q.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (px, py) = (a.0 + dx * t - q.0, a.1 + dy * t - q.1);
                (px * px + py * py).sqrt() < 0.35 * spacing
            })
        };
        for i in 1..n {
            for j in 1..n {
                if tris.len() >= MAX_FACE_TRIANGLES {
                    break;
                }
                // Slightly off the lattice, so points rarely fall on the
                // edges of the loops' triangles.
                let p = (
                    u0 + (u1 - u0) * (i as f64 + 0.0137) / n as f64,
                    v0 + (v1 - v0) * (j as f64 + 0.0071) / n as f64,
                );
                let q = scaled(p);
                if near_loop(q) {
                    continue;
                }
                // The triangle holding the point, split in three.
                let holder = tris.iter().position(|t| {
                    let (a, b, c) = (scaled(uv[t[0]]), scaled(uv[t[1]]), scaled(uv[t[2]]));
                    let side = |p: (f64, f64), r: (f64, f64)| {
                        (r.0 - p.0) * (q.1 - p.1) - (r.1 - p.1) * (q.0 - p.0)
                    };
                    let (x, y, z) = (side(a, b), side(b, c), side(c, a));
                    let twice = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
                    twice.abs() > 1e-12 * spacing * spacing
                        && ((x > 0.0 && y > 0.0 && z > 0.0) || (x < 0.0 && y < 0.0 && z < 0.0))
                });
                let Some(k) = holder else { continue };
                let t = tris[k];
                uv.push(p);
                xyz.push(None);
                let m = uv.len() - 1;
                tris[k] = [t[0], t[1], m];
                tris.push([t[1], t[2], m]);
                tris.push([t[2], t[0], m]);
            }
        }
        tris.retain(|t| {
            let (a, b, c) = (scaled(uv[t[0]]), scaled(uv[t[1]]), scaled(uv[t[2]]));
            ((b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)).abs() > 1e-9 * spacing * spacing
        });
        delaunay_flips(&mut tris, &uv, (su, sv));
        let mut out = Vec::with_capacity(tris.len());
        for t in tris {
            let mut p = [
                point(&uv, &xyz, t[0]),
                point(&uv, &xyz, t[1]),
                point(&uv, &xyz, t[2]),
            ];
            let n = (p[1] - p[0]).cross(p[2] - p[0]);
            if n.length_squared() < 1e-18 {
                continue;
            }
            let c = (
                (uv[t[0]].0 + uv[t[1]].0 + uv[t[2]].0) / 3.0,
                (uv[t[0]].1 + uv[t[1]].1 + uv[t[2]].1) / 3.0,
            );
            if n.dot(surface.normal(c.0.clamp(u0, u1), c.1.clamp(v0, v1))) < 0.0 {
                p.swap(1, 2);
            }
            out.push(vec![p.to_vec()]);
        }
        (!out.is_empty()).then_some(out)
    }
}

/// Lawson's edge flips toward the Delaunay triangulation of `uv` scaled by
/// `scale`: well-shaped triangles keep a curved surface's area (thin ones
/// overestimate it). Edges used by one triangle (the loops) never flip.
fn delaunay_flips(tris: &mut [[usize; 3]], uv: &[(f64, f64)], scale: (f64, f64)) {
    let at = |i: usize| (uv[i].0 / scale.0, uv[i].1 / scale.1);
    let orient = |a: usize, b: usize, c: usize| {
        let (pa, pb, pc) = (at(a), at(b), at(c));
        (pb.0 - pa.0) * (pc.1 - pa.1) - (pb.1 - pa.1) * (pc.0 - pa.0)
    };
    // Counter-clockwise triangles.
    for t in tris.iter_mut() {
        if orient(t[0], t[1], t[2]) < 0.0 {
            t.swap(1, 2);
        }
    }
    // Whether d lies inside the circle through a, b, c (counter-clockwise).
    let inside = |a: usize, b: usize, c: usize, d: usize| {
        let (pa, pb, pc, pd) = (at(a), at(b), at(c), at(d));
        let (ax, ay) = (pa.0 - pd.0, pa.1 - pd.1);
        let (bx, by) = (pb.0 - pd.0, pb.1 - pd.1);
        let (cx, cy) = (pc.0 - pd.0, pc.1 - pd.1);
        let det = (ax * ax + ay * ay) * (bx * cy - cx * by)
            - (bx * bx + by * by) * (ax * cy - cx * ay)
            + (cx * cx + cy * cy) * (ax * by - bx * ay);
        det > 1e-12
            * (ax * ax + ay * ay + bx * bx + by * by + cx * cx + cy * cy)
                .powi(2)
                .max(1e-300)
    };
    for _ in 0..64 {
        let mut edges: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
        for (k, t) in tris.iter().enumerate() {
            for e in 0..3 {
                let (a, b) = (t[e], t[(e + 1) % 3]);
                edges.entry((a.min(b), a.max(b))).or_default().push(k);
            }
        }
        let mut flipped = false;
        let mut touched = vec![false; tris.len()];
        for ((a, b), users) in edges {
            let [k, l] = users[..] else { continue };
            if touched[k] || touched[l] {
                continue;
            }
            let other = |t: &[usize; 3]| t.iter().copied().find(|v| *v != a && *v != b);
            let (Some(c), Some(d)) = (other(&tris[k]), other(&tris[l])) else {
                continue;
            };
            // Order a, b so that a, b, c is counter-clockwise.
            let (a, b) = if orient(a, b, c) > 0.0 {
                (a, b)
            } else {
                (b, a)
            };
            if !inside(a, b, c, d) {
                continue;
            }
            // The flipped pair must stay counter-clockwise (convex quad).
            if orient(c, d, b) <= 0.0 || orient(d, c, a) <= 0.0 {
                continue;
            }
            tris[k] = [c, a, d];
            tris[l] = [d, b, c];
            touched[k] = true;
            touched[l] = true;
            flipped = true;
        }
        if !flipped {
            break;
        }
    }
}

/// The normalized parameters of point `i` in ear-clipping data.
fn data_at(data: &[f64], i: usize) -> (f64, f64) {
    (data[2 * i], data[2 * i + 1])
}

/// A box that grows to hold points.
#[derive(Clone, Copy)]
struct Aabb3 {
    lo: DVec3,
    hi: DVec3,
}

impl Default for Aabb3 {
    fn default() -> Self {
        Aabb3 {
            lo: DVec3::splat(f64::INFINITY),
            hi: DVec3::splat(f64::NEG_INFINITY),
        }
    }
}

impl Aabb3 {
    fn with(self, p: DVec3) -> Self {
        Aabb3 {
            lo: self.lo.min(p),
            hi: self.hi.max(p),
        }
    }

    fn diagonal(&self) -> f64 {
        if self.lo.x.is_finite() {
            self.lo.distance(self.hi)
        } else {
            0.0
        }
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

/// A subfigure definition (308) read from IGES (milestone 1.38).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IgesDefinition {
    /// Its name.
    pub name: String,
    /// Its faces, in its own coordinates (mm).
    pub faces: Vec<FaceLoops>,
    /// Faces of curved surfaces among them (triangles).
    pub curved: usize,
    /// Subfigures placed in it.
    pub instances: Vec<IgesInstance>,
}

/// A placed subfigure (408).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IgesInstance {
    /// The definition, an index into [`IgesRead::definitions`].
    pub definition: usize,
    /// Definition to parent coordinates, mm.
    pub transform: DAffine3,
}

/// What an IGES file holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IgesRead {
    /// Faces outside subfigures, in model space (mm).
    pub faces: Vec<FaceLoops>,
    /// Faces of curved surfaces among them (triangles).
    pub curved: usize,
    /// Subfigure definitions.
    pub definitions: Vec<IgesDefinition>,
    /// Subfigures placed in the model.
    pub instances: Vec<IgesInstance>,
    /// Faces that could not be read.
    pub skipped: usize,
}

/// Reads an IGES file's faces — planar ones with their loops, curved ones
/// as triangles — with its subfigures and their placements.
pub fn read_structure(text: &str) -> Result<IgesRead, IoError> {
    let model = parse(text)?;
    let mut out = IgesRead::default();
    // Subfigures, their members, and surfaces that trimmed faces use.
    let mut index: BTreeMap<usize, usize> = BTreeMap::new();
    let mut owner: BTreeMap<usize, usize> = BTreeMap::new();
    let mut used: std::collections::BTreeSet<usize> = Default::default();
    for (&ptr, e) in &model.entries {
        match e.kind {
            308 => {
                let n = e.params.get(2).map_or(0, |s| int(s));
                index.insert(ptr, out.definitions.len());
                out.definitions.push(IgesDefinition {
                    name: e.params.get(1).cloned().unwrap_or_default(),
                    ..Default::default()
                });
                for k in 0..n {
                    if let Some(m) = e.params.get(3 + k) {
                        owner.insert(int(m), ptr);
                    }
                }
            }
            144 => {
                used.extend(e.params.first().map(|s| int(s)));
            }
            143 | 141 => {
                let k = if e.kind == 143 { 1 } else { 2 };
                used.extend(e.params.get(k).map(|s| int(s)));
            }
            142 => {
                used.extend(e.params.get(1).map(|s| int(s)));
            }
            _ => {}
        }
    }
    let unit = DAffine3::from_scale(DVec3::splat(model.scale));
    let unit_back = DAffine3::from_scale(DVec3::splat(1.0 / model.scale));
    for (&ptr, e) in &model.entries {
        let into = owner.get(&ptr).and_then(|o| index.get(o)).copied();
        if into.is_none() && e.dependent && e.kind != 408 {
            continue;
        }
        let (faces, curved): (Option<Vec<FaceLoops>>, bool) = match e.kind {
            144 | 143 => match model.face(e) {
                Some((f, c)) => (Some(f), c),
                None => {
                    out.skipped += 1;
                    continue;
                }
            },
            128 | 120 | 122 | 118 if !used.contains(&ptr) => match model.surf(ptr) {
                Some(surf) => (model.tessellate(&surf, None), true),
                None => continue,
            },
            408 => {
                let Some(&definition) = e.params.first().and_then(|d| index.get(&int(d))) else {
                    out.skipped += 1;
                    continue;
                };
                let f = |k: usize, or: f64| e.params.get(k).map_or(or, |s| num(s));
                let local = DAffine3::from_translation(DVec3::new(f(1, 0.0), f(2, 0.0), f(3, 0.0)))
                    * DAffine3::from_scale(DVec3::splat(f(4, 1.0)));
                let transform = unit * model.transform(e, 0) * local * unit_back;
                let instance = IgesInstance {
                    definition,
                    transform,
                };
                match into {
                    Some(d) => out.definitions[d].instances.push(instance),
                    None => out.instances.push(instance),
                }
                continue;
            }
            _ => continue,
        };
        let Some(faces) = faces else {
            out.skipped += 1;
            continue;
        };
        let n = if curved { faces.len() } else { 0 };
        match into {
            Some(d) => {
                out.definitions[d].faces.extend(faces);
                out.definitions[d].curved += n;
            }
            None => {
                out.faces.extend(faces);
                out.curved += n;
            }
        }
    }
    if out.faces.is_empty() && out.instances.is_empty() {
        return Err(IoError::UnsupportedFormat(format!(
            "no readable trimmed, bounded or freestanding surfaces ({} other faces)",
            out.skipped
        )));
    }
    Ok(out)
}

/// `faces` placed by `t` (loops reversed when it mirrors).
fn placed(faces: &[FaceLoops], t: &DAffine3) -> Vec<FaceLoops> {
    let flip = t.matrix3.determinant() < 0.0;
    faces
        .iter()
        .map(|f| {
            f.iter()
                .map(|r| {
                    let mut r: Vec<DVec3> = r.iter().map(|p| t.transform_point3(*p)).collect();
                    if flip {
                        r.reverse();
                    }
                    r
                })
                .collect()
        })
        .collect()
}

/// The faces of an IGES file in model space (subfigures placed) and how
/// many faces were skipped.
pub fn read(text: &str) -> Result<(Vec<FaceLoops>, usize), IoError> {
    let r = read_structure(text)?;
    let mut faces = r.faces.clone();
    fn add(r: &IgesRead, i: &IgesInstance, t: DAffine3, depth: usize, out: &mut Vec<FaceLoops>) {
        if depth > 16 {
            return;
        }
        let Some(d) = r.definitions.get(i.definition) else {
            return;
        };
        let t = t * i.transform;
        out.extend(placed(&d.faces, &t));
        for nested in &d.instances {
            add(r, nested, t, depth + 1, out);
        }
    }
    for i in &r.instances {
        add(&r, i, DAffine3::IDENTITY, 0, &mut faces);
    }
    Ok((faces, r.skipped))
}

/// One container's mesh from faces: stitched, curved parts softened.
fn mesh_of(faces: &[FaceLoops], curved: usize, tol: &Tolerances) -> Result<HalfEdgeMesh, IoError> {
    if faces.is_empty() {
        return Ok(HalfEdgeMesh::new());
    }
    let pieces: Vec<Piece> = faces
        .iter()
        .map(|loops| Piece {
            loops: loops.clone(),
            sides: FaceSides::default(),
        })
        .collect();
    let mesh = stitch(&pieces, tol).map_err(|e| corrupt(e.to_string()))?;
    if curved == 0 {
        return Ok(mesh);
    }
    let (mut indexed, _) = mesh.to_indexed();
    crate::geom::surface::soften(&mut indexed, 30f64.to_radians());
    HalfEdgeMesh::from_indexed(&indexed, tol).map_err(|e| corrupt(e.to_string()))
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
        let structure = read_structure(&source)?;
        let tol: Tolerances = *tx.tolerances();
        let mesh = mesh_of(&structure.faces, structure.curved, &tol)?;
        let mut report = ImportReport {
            faces: mesh.face_count(),
            edges: mesh.edge_count(),
            skipped: structure.skipped,
            ..Default::default()
        };
        if structure.skipped > 0 {
            report.warnings.push(Warning::new(
                WarningCode::W2008,
                format!("{} unreadable IGES faces were skipped", structure.skipped),
            ));
        }
        let curved = structure.curved
            + structure
                .definitions
                .iter()
                .map(|d| d.curved)
                .sum::<usize>();
        if curved > 0 {
            report.warnings.push(Warning::new(
                WarningCode::W2008,
                format!("curved IGES surfaces became {curved} soft triangles"),
            ));
        }
        // Subfigures become component definitions, nested ones first.
        let mut made: Vec<Option<crate::ids::DefinitionId>> =
            vec![None; structure.definitions.len()];
        for i in 0..structure.definitions.len() {
            define(tx, &structure, i, &mut made, &tol, 0)?;
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
        for inst in &structure.instances {
            if let Some(Some(d)) = made.get(inst.definition) {
                place(tx, definition, *d, inst.transform)?;
            }
        }
        // Faces of the components made count too, once each.
        report.faces += made
            .iter()
            .flatten()
            .filter_map(|d| tx.document().definition(*d))
            .map(|def| def.entities.mesh().face_count())
            .sum::<usize>();
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

/// Places a component of `definition` in `parent`.
fn place(
    tx: &mut Transaction<'_>,
    parent: crate::ids::DefinitionId,
    definition: crate::ids::DefinitionId,
    transform: DAffine3,
) -> Result<(), IoError> {
    tx.insert_entity(
        ContainerRef::Definition(parent),
        EntityRecord::Instance(ComponentInstance {
            definition,
            transform,
            name: None,
            material_override: None,
            glued_to: None,
            dynamic_attrs: Default::default(),
        }),
    )
    .map_err(|e| corrupt(e.to_string()))?;
    Ok(())
}

/// The component definition of subfigure `i`, made once with the
/// subfigures placed in it.
fn define(
    tx: &mut Transaction<'_>,
    structure: &IgesRead,
    i: usize,
    made: &mut [Option<crate::ids::DefinitionId>],
    tol: &Tolerances,
    depth: usize,
) -> Result<Option<crate::ids::DefinitionId>, IoError> {
    if let Some(Some(d)) = made.get(i) {
        return Ok(Some(*d));
    }
    let Some(source) = structure.definitions.get(i) else {
        return Ok(None);
    };
    if depth > 16 {
        return Err(corrupt("subfigures nest too deep (or in a cycle)"));
    }
    let mut nested = Vec::new();
    for inst in &source.instances {
        if let Some(d) = define(tx, structure, inst.definition, made, tol, depth + 1)? {
            nested.push((d, inst.transform));
        }
    }
    let mesh = mesh_of(&source.faces, source.curved, tol)?;
    let name = if source.name.trim().is_empty() {
        format!("Subfigure {}", i + 1)
    } else {
        source.name.trim().to_string()
    };
    let entities = Entities::from_mesh(mesh, || tx.allocate_pid());
    let uuid = crate::commands::common::definition_uuid(tx.document(), &name, &entities);
    let mut def = ComponentDefinition::new(uuid, name);
    def.entities = entities;
    let d = tx
        .insert_definition(def)
        .map_err(|e| corrupt(e.to_string()))?;
    for (n, t) in nested {
        place(tx, d, n, t)?;
    }
    if let Some(slot) = made.get_mut(i) {
        *slot = Some(d);
    }
    Ok(Some(d))
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

    fn reals(v: &[f64]) -> Vec<String> {
        v.iter().map(|x| real(*x)).collect()
    }

    fn area(faces: &[FaceLoops]) -> f64 {
        faces
            .iter()
            .map(|f| {
                let r = &f[0];
                let mut a = DVec3::ZERO;
                for (p, q) in r.iter().zip(r.iter().cycle().skip(1)) {
                    a += p.cross(*q);
                }
                0.5 * a.length()
            })
            .sum()
    }

    /// Three curved surfaces: a rational quarter cylinder (128) on its own,
    /// a full surface of revolution (120) and a quarter of a tabulated
    /// cylinder (122) bounded by arcs and lines (143/141).
    fn curved_file() -> String {
        let mut w = Writer {
            entities: Vec::new(),
        };
        let h = std::f64::consts::FRAC_1_SQRT_2;
        let mut p: Vec<String> = ["2", "1", "2", "1", "0", "0", "0", "0", "0"]
            .map(String::from)
            .to_vec();
        p.extend(reals(&[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]));
        p.extend(reals(&[0.0, 0.0, 1.0, 1.0]));
        p.extend(reals(&[1.0, h, 1.0, 1.0, h, 1.0]));
        for z in [0.0, 100.0] {
            p.extend(reals(&[50.0, 0.0, z, 50.0, 50.0, z, 0.0, 50.0, z]));
        }
        p.extend(reals(&[0.0, 1.0, 0.0, 1.0]));
        w.add(128, 0, p, false);
        let axis = w.add(110, 0, reals(&[200.0, 0.0, 0.0, 200.0, 0.0, 1.0]), true);
        let generatrix = w.add(110, 0, reals(&[230.0, 0.0, 0.0, 230.0, 0.0, 50.0]), true);
        let mut p = vec![axis.to_string(), generatrix.to_string()];
        p.extend(reals(&[0.0, std::f64::consts::TAU]));
        w.add(120, 0, p, false);
        let arc = |w: &mut Writer, z: f64| {
            w.add(
                100,
                0,
                reals(&[z, 500.0, 0.0, 540.0, 0.0, 500.0, 40.0]),
                true,
            )
        };
        let bottom = arc(&mut w, 0.0);
        let mut p = vec![bottom.to_string()];
        p.extend(reals(&[540.0, 0.0, 60.0]));
        let surface = w.add(122, 0, p, true);
        let up = w.add(110, 0, reals(&[500.0, 40.0, 0.0, 500.0, 40.0, 60.0]), true);
        let top = arc(&mut w, 60.0);
        let down = w.add(110, 0, reals(&[540.0, 0.0, 60.0, 540.0, 0.0, 0.0]), true);
        let mut p = vec!["0".into(), "0".into(), surface.to_string(), "4".into()];
        for (c, sense) in [(bottom, 1), (up, 1), (top, 2), (down, 1)] {
            p.extend([c.to_string(), sense.to_string(), "0".into()]);
        }
        let boundary = w.add(141, 0, p, true);
        w.add(
            143,
            0,
            vec![
                "0".into(),
                surface.to_string(),
                "1".into(),
                boundary.to_string(),
            ],
            false,
        );
        w.text("curved.igs")
    }

    #[test]
    fn curved_surfaces_become_triangles() {
        let text = curved_file();
        let r = read_structure(&text).unwrap();
        assert_eq!(r.skipped, 0);
        assert_eq!(r.curved, r.faces.len());
        // Split by place: the quarter cylinder, the turned cylinder, the
        // bounded tabulated quarter.
        let part = |lo: f64, hi: f64| -> Vec<FaceLoops> {
            r.faces
                .iter()
                .filter(|f| {
                    let x = f[0].iter().map(|p| p.x).sum::<f64>() / 3.0;
                    (lo..hi).contains(&x)
                })
                .cloned()
                .collect()
        };
        let pi = std::f64::consts::PI;
        for (faces, want) in [
            (part(-1.0, 100.0), pi / 2.0 * 50.0 * 100.0),
            (part(150.0, 300.0), 2.0 * pi * 30.0 * 50.0),
            (part(450.0, 600.0), pi / 2.0 * 40.0 * 60.0),
        ] {
            assert!(!faces.is_empty());
            let got = area(&faces);
            assert!((got - want).abs() < 0.01 * want, "{got} vs {want}");
        }
        // Every triangle lies on its surface's circle.
        for f in part(-1.0, 100.0) {
            for p in &f[0] {
                assert!((p.truncate().length() - 50.0).abs() < 0.5, "{p}");
            }
        }
        // Imported, the curved surfaces shade smoothly.
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        let storage = Arc::new(crate::io::MemoryStorage::new());
        e.set_storage(storage.clone());
        crate::io::Storage::write(
            storage.as_ref(),
            &crate::io::StorageKey::new("c.igs"),
            text.as_bytes(),
        )
        .unwrap();
        let out = e
            .execute_erased("import", json!({ "path": "c.igs" }))
            .unwrap();
        assert!(out["faces"].as_u64().unwrap() > 0, "{out}");
        let doc = e.document();
        let (_, def) = doc.definitions.iter().next().unwrap();
        let soft = def
            .entities
            .mesh()
            .edges()
            .filter(|(_, x)| x.flags.contains(crate::geom::EdgeFlags::SOFT))
            .count();
        assert!(soft > 100, "{soft}");
    }

    #[test]
    fn components_travel_as_subfigures() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        for (name, params) in [
            (
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [100, 50, 0] }),
            ),
            (
                "push-pull",
                json!({ "face": { "Query": "face" }, "distance": 30 }),
            ),
            (
                "make-component",
                json!({ "targets": "all", "name": "Block" }),
            ),
            (
                "move",
                json!({ "targets": "instance", "delta": [300, 0, 0], "copy": true }),
            ),
            (
                "rotate",
                json!({ "targets": "instance & last", "center": [350, 25, 0],
                              "axis": [0, 0, 1], "angle": std::f64::consts::FRAC_PI_2 }),
            ),
            (
                "rectangle",
                json!({ "corner_a": [0, 500, 0], "corner_b": [40, 540, 0] }),
            ),
        ] {
            e.execute_erased(name, params).unwrap();
        }
        let (text, faces) = write(e.document(), &ExportScope::default(), "a.igs").unwrap();
        assert_eq!(faces, 13);
        let r = read_structure(&text).unwrap();
        assert_eq!(
            (r.faces.len(), r.definitions.len(), r.instances.len()),
            (1, 1, 2)
        );
        assert_eq!(r.definitions[0].name, "Block");
        assert_eq!(r.definitions[0].faces.len(), 6);
        // The placements are the instances' transforms.
        let doc = e.document();
        let mut placed: Vec<DAffine3> = doc.model.instances().map(|(_, i)| i.transform).collect();
        placed.sort_by(|a, b| a.translation.x.total_cmp(&b.translation.x));
        for (want, got) in placed.iter().zip(&r.instances) {
            let d = (want.to_cols_array().iter())
                .zip(got.transform.to_cols_array().iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max);
            assert!(d < 1e-9, "{want:?} vs {:?}", got.transform);
        }
        // Flattened, the faces are where they were.
        let (flat, _) = read(&text).unwrap();
        assert_eq!(flat.len(), 13);
        // Imported, the subfigure is a component placed twice.
        let mut back = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        let storage = Arc::new(crate::io::MemoryStorage::new());
        back.set_storage(storage.clone());
        crate::io::Storage::write(
            storage.as_ref(),
            &crate::io::StorageKey::new("a.igs"),
            text.as_bytes(),
        )
        .unwrap();
        back.execute_erased("import", json!({ "path": "a.igs" }))
            .unwrap();
        let doc = back.document();
        let block = doc
            .definitions
            .iter()
            .find(|(_, d)| d.name == "Block")
            .map(|(id, d)| (id, d.entities.mesh().volume().unwrap()))
            .unwrap();
        assert!((block.1 - 150_000.0).abs() < 1e-6);
        let uses = doc
            .definitions
            .iter()
            .flat_map(|(_, d)| {
                d.entities
                    .instances()
                    .map(|(_, i)| i.definition)
                    .collect::<Vec<_>>()
            })
            .filter(|d| *d == block.0)
            .count();
        assert_eq!(uses, 2);
    }
}
