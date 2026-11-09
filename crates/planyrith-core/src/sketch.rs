//! Implements docs/03-geometry-kernel.md §11 (constraints and the solver):
//! geometric constraints and driving dimensions between the edges and edge
//! ends of one container, kept by a numeric solver (milestone 1.9).
//!
//! Constraints live in the container's attributes (dictionary
//! [`DICT`], key [`KEY`], a JSON list), so they need no new file structure
//! and undo like any attribute. They name edges by pid and points as an end
//! of an edge (`12.a`, `12.b`).
//!
//! The solver moves only the vertices that constraints name. Unknowns are
//! their world-free container coordinates; residuals are the constraints
//! (in millimeters) plus one per vertex and adjacent face keeping the
//! vertex in the face's plane, so faces stay flat. A damped least-squares
//! iteration with a weak pull toward the starting positions finds the
//! nearest solution; Gauss–Newton steps then meet the constraints exactly.
//! The rank of the constraint Jacobian gives the degrees of freedom left.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::{AttributeDictionaries, Entities, Value};
use crate::geom::HalfEdgeMesh;
use crate::ids::{EntityId, Pid, VertexId};

/// The attribute dictionary holding a container's constraints.
pub const DICT: &str = "planyrith";
/// The key of the constraint list in [`DICT`].
pub const KEY: &str = "constraints";

/// One end of an edge, written `<pid>.a` or `<pid>.b`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PointRef {
    /// The edge.
    pub edge: Pid,
    /// `false`: its first end (`a`); `true`: its second (`b`).
    pub second: bool,
}

impl fmt::Display for PointRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.edge.0, if self.second { 'b' } else { 'a' })
    }
}

impl FromStr for PointRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().trim_start_matches('@');
        let (pid, end) = s
            .rsplit_once('.')
            .ok_or_else(|| format!("`{s}`: write a point as <edge pid>.a or <edge pid>.b"))?;
        let pid: u64 = pid
            .parse()
            .map_err(|_| format!("`{s}`: `{pid}` is not an edge pid"))?;
        let second = match end {
            "a" => false,
            "b" => true,
            _ => return Err(format!("`{s}`: the end is a or b")),
        };
        Ok(PointRef {
            edge: Pid(pid),
            second,
        })
    }
}

impl Serialize for PointRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PointRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for PointRef {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PointRef".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^@?[0-9]+\\.[ab]$",
            "description": "An end of an edge: <edge pid>.a or <edge pid>.b"
        })
    }
}

/// What a constraint requires (docs/03 §11.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ConstraintKind {
    /// Two points in one place.
    Coincident,
    /// Points (or both ends of edges) stay where they are.
    Fixed,
    /// Edges parallel to the red axis.
    AlongX,
    /// Edges parallel to the green axis.
    AlongY,
    /// Edges parallel to the blue axis.
    AlongZ,
    /// Edges parallel to the first one.
    Parallel,
    /// Two edges at a right angle.
    Perpendicular,
    /// Edges as long as the first one.
    Equal,
    /// Two edges on one line.
    Collinear,
    /// A point in the middle of an edge.
    Midpoint,
    /// A point on an edge's line.
    OnLine,
    /// Two points mirror images across an edge's line.
    Symmetric,
    /// An edge's length (a driving dimension).
    Length,
    /// The distance between two points (a driving dimension).
    Distance,
    /// The angle between two edges (a driving dimension).
    Angle,
}

impl ConstraintKind {
    /// How many points and edges it takes: (points, minimum edges, maximum
    /// edges).
    fn arity(self) -> (usize, usize, usize) {
        use ConstraintKind::*;
        match self {
            Coincident | Distance => (2, 0, 0),
            Fixed => (usize::MAX, 0, usize::MAX),
            AlongX | AlongY | AlongZ => (0, 1, usize::MAX),
            Parallel | Equal => (0, 2, usize::MAX),
            Perpendicular | Collinear | Angle => (0, 2, 2),
            Midpoint | OnLine => (1, 1, 1),
            Symmetric => (2, 1, 1),
            Length => (0, 1, 1),
        }
    }

    /// Whether it carries a length.
    pub fn takes_length(self) -> bool {
        matches!(self, ConstraintKind::Length | ConstraintKind::Distance)
    }

    /// Whether it carries an angle.
    pub fn takes_angle(self) -> bool {
        matches!(self, ConstraintKind::Angle)
    }
}

/// One constraint of a container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Constraint {
    /// Its number in the container (stable while it exists).
    pub id: u32,
    /// What it requires.
    pub kind: ConstraintKind,
    /// The edges it names, by pid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<Pid>,
    /// The points it names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<PointRef>,
    /// A driving length, mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_mm: Option<f64>,
    /// A driving angle, radians.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_rad: Option<f64>,
    /// Where `fixed` keeps its points, in order (points first, then edge
    /// ends).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at: Vec<[f64; 3]>,
}

impl Constraint {
    /// Checks the number of points and edges and the value.
    pub fn check(&self) -> Result<(), String> {
        let (points, lo, hi) = self.kind.arity();
        if self.kind == ConstraintKind::Fixed {
            if self.points.is_empty() && self.edges.is_empty() {
                return Err("fixed needs points or edges".into());
            }
        } else {
            if self.points.len() != points {
                return Err(format!(
                    "{:?} needs {points} point(s), not {}",
                    self.kind,
                    self.points.len()
                ));
            }
            if self.edges.len() < lo || self.edges.len() > hi {
                return Err(format!(
                    "{:?} needs {} edge(s), not {}",
                    self.kind,
                    if lo == hi {
                        lo.to_string()
                    } else if hi == usize::MAX {
                        format!("{lo} or more")
                    } else {
                        format!("{lo} to {hi}")
                    },
                    self.edges.len()
                ));
            }
        }
        if self.kind.takes_length() {
            match self.length_mm {
                Some(l) if l.is_finite() && l >= 0.0 => {}
                _ => return Err("give a length of zero or more".into()),
            }
        }
        if self.kind.takes_angle() {
            match self.angle_rad {
                Some(a) if a.is_finite() && (0.0..=std::f64::consts::PI).contains(&a) => {}
                _ => return Err("give an angle from 0° to 180°".into()),
            }
        }
        Ok(())
    }
}

/// The constraints stored in `attrs`.
pub fn read(attrs: &AttributeDictionaries) -> Vec<Constraint> {
    match attrs.get(DICT, KEY) {
        Some(Value::Str(json)) => serde_json::from_str(json).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The attribute value storing `list` (`None` when empty).
pub fn value_of(list: &[Constraint]) -> Option<Value> {
    if list.is_empty() {
        return None;
    }
    serde_json::to_string(list).ok().map(Value::Str)
}

/// Why solving failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SolveError {
    /// A constraint names an edge that is gone.
    #[error("constraint {0} names an edge that no longer exists")]
    Missing(u32),
    /// The constraints cannot all be met (with faces kept flat).
    #[error(
        "the constraints conflict (or cannot be met while faces stay flat); largest error {0:.4} mm"
    )]
    Conflict(f64),
}

/// The solver's verdict on a set of constraints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Solution {
    /// New positions of the vertices that move (container coordinates).
    #[serde(skip)]
    #[schemars(skip)]
    pub moves: BTreeMap<VertexId, DVec3>,
    /// Vertices that moved.
    pub moved: usize,
    /// Degrees of freedom left among the named points.
    pub degrees_of_freedom: usize,
    /// Equations that repeat others (consistent but redundant).
    pub redundant: usize,
    /// The largest remaining error, mm.
    pub error_mm: f64,
}

/// A constraint resolved to vertices.
struct Resolved<'a> {
    c: &'a Constraint,
    /// Variable index of each point (points first, then both ends of each
    /// edge).
    points: Vec<usize>,
    edges: Vec<(usize, usize)>,
}

/// The equations of one constraint at `x`, in millimeters.
fn residuals(r: &Resolved<'_>, x: &[DVec3], out: &mut Vec<f64>) {
    use ConstraintKind::*;
    let p = |i: usize| x[r.points[i]];
    let e = |i: usize| (x[r.edges[i].0], x[r.edges[i].1]);
    let dir = |i: usize| {
        let (a, b) = e(i);
        b - a
    };
    match r.c.kind {
        Coincident => {
            let d = p(1) - p(0);
            out.extend([d.x, d.y, d.z]);
        }
        Fixed => {
            let mut all: Vec<usize> = r.points.clone();
            for &(a, b) in &r.edges {
                all.extend([a, b]);
            }
            for (k, &v) in all.iter().enumerate() {
                let target = r.c.at.get(k).map_or(x[v], |t| DVec3::from_array(*t));
                let d = x[v] - target;
                out.extend([d.x, d.y, d.z]);
            }
        }
        AlongX | AlongY | AlongZ => {
            for i in 0..r.edges.len() {
                let d = dir(i);
                out.extend(match r.c.kind {
                    AlongX => [d.y, d.z],
                    AlongY => [d.x, d.z],
                    _ => [d.x, d.y],
                });
            }
        }
        Parallel | Collinear => {
            let d0 = dir(0);
            let l0 = d0.length().max(1e-12);
            for i in 1..r.edges.len() {
                let d = dir(i);
                let li = d.length().max(1e-12);
                // The cross product of the unit directions, scaled to mm.
                let c = (d0 / l0).cross(d / li) * (0.5 * (l0 + li));
                out.extend([c.x, c.y, c.z]);
            }
            if r.c.kind == Collinear {
                let (a, _) = e(0);
                let (q, _) = e(1);
                let c = (q - a).cross(d0 / l0);
                out.extend([c.x, c.y, c.z]);
            }
        }
        Perpendicular => {
            let (d0, d1) = (dir(0), dir(1));
            let (l0, l1) = (d0.length().max(1e-12), d1.length().max(1e-12));
            out.push((d0 / l0).dot(d1 / l1) * 0.5 * (l0 + l1));
        }
        Equal => {
            let l0 = dir(0).length();
            for i in 1..r.edges.len() {
                out.push(dir(i).length() - l0);
            }
        }
        Midpoint => {
            let (a, b) = e(0);
            let d = p(0) - (a + b) * 0.5;
            out.extend([d.x, d.y, d.z]);
        }
        OnLine => {
            let (a, b) = e(0);
            let n = (b - a).normalize_or_zero();
            let c = (p(0) - a).cross(n);
            out.extend([c.x, c.y, c.z]);
        }
        Symmetric => {
            let (a, b) = e(0);
            let n = (b - a).normalize_or_zero();
            let m = (p(0) + p(1)) * 0.5;
            let c = (m - a).cross(n);
            out.extend([c.x, c.y, c.z]);
            out.push((p(1) - p(0)).dot(n));
        }
        Length => out.push(dir(0).length() - r.c.length_mm.unwrap_or(0.0)),
        Distance => out.push((p(1) - p(0)).length() - r.c.length_mm.unwrap_or(0.0)),
        Angle => {
            let (d0, d1) = (dir(0), dir(1));
            let (l0, l1) = (d0.length().max(1e-12), d1.length().max(1e-12));
            let got = libm::atan2(d0.cross(d1).length(), d0.dot(d1));
            out.push((got - r.c.angle_rad.unwrap_or(0.0)) * 0.5 * (l0 + l1));
        }
    }
}

/// Solves `constraints` on the mesh of `entities`. Unknown edges make the
/// solve fail unless `skip_missing`, which leaves those constraints out.
pub fn solve(
    entities: &Entities,
    constraints: &[Constraint],
    skip_missing: bool,
) -> Result<Solution, SolveError> {
    let mesh = entities.mesh();
    let mut index: BTreeMap<VertexId, usize> = BTreeMap::new();
    let mut verts: Vec<VertexId> = Vec::new();
    let mut var = |v: VertexId, index: &mut BTreeMap<VertexId, usize>| -> usize {
        *index.entry(v).or_insert_with(|| {
            verts.push(v);
            verts.len() - 1
        })
    };
    let ends = |pid: Pid| -> Option<(VertexId, VertexId)> {
        match entities.id_of(pid)? {
            EntityId::Edge(e) => mesh.edge_endpoints(e).ok(),
            _ => None,
        }
    };
    let mut resolved = Vec::new();
    for c in constraints {
        let mut points = Vec::new();
        let mut edges = Vec::new();
        let mut ok = true;
        for p in &c.points {
            match ends(p.edge) {
                Some((a, b)) => points.push(var(if p.second { b } else { a }, &mut index)),
                None => ok = false,
            }
        }
        for &e in &c.edges {
            match ends(e) {
                Some((a, b)) => {
                    let (a, b) = (var(a, &mut index), var(b, &mut index));
                    edges.push((a, b));
                }
                None => ok = false,
            }
        }
        if !ok {
            if skip_missing {
                continue;
            }
            return Err(SolveError::Missing(c.id));
        }
        resolved.push(Resolved { c, points, edges });
    }
    let x0: Vec<DVec3> = verts
        .iter()
        .map(|v| mesh.position(*v).unwrap_or_default())
        .collect();
    // Faces keep their planes: one equation per named vertex and face.
    let mut flat: Vec<(usize, DVec3, f64)> = Vec::new();
    for (i, &v) in verts.iter().enumerate() {
        for plane in face_planes(mesh, v) {
            flat.push((i, plane.0, plane.1));
        }
    }
    let all = |x: &[DVec3]| -> Vec<f64> {
        let mut out = Vec::new();
        for r in &resolved {
            residuals(r, x, &mut out);
        }
        for &(i, n, d) in &flat {
            out.push(n.dot(x[i]) - d);
        }
        out
    };
    let n = verts.len() * 3;
    let flatten = |x: &[DVec3]| -> Vec<f64> { x.iter().flat_map(|p| p.to_array()).collect() };
    let unflatten =
        |v: &[f64]| -> Vec<DVec3> { v.chunks(3).map(|c| DVec3::new(c[0], c[1], c[2])).collect() };
    let start = flatten(&x0);
    let scale = x0.iter().fold(1.0f64, |m, p| m.max(p.abs().max_element()));
    let jacobian = |v: &[f64], rows: usize| -> Vec<Vec<f64>> {
        let mut jac = vec![vec![0.0; n]; rows];
        let mut w = v.to_vec();
        for k in 0..n {
            let h = 1e-7 * scale.max(1.0);
            let keep = w[k];
            w[k] = keep + h;
            let plus = all(&unflatten(&w));
            w[k] = keep - h;
            let minus = all(&unflatten(&w));
            w[k] = keep;
            for (row, (a, b)) in jac.iter_mut().zip(plus.iter().zip(&minus)) {
                row[k] = (a - b) / (2.0 * h);
            }
        }
        jac
    };
    let mut x = start.clone();
    let mut r = all(&unflatten(&x));
    let max_abs = |r: &[f64]| r.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    // Phase 1: damped least squares, pulled weakly toward the start.
    let mu = 1e-6;
    let mut lambda = 1e-3;
    for _ in 0..200 {
        if max_abs(&r) < 1e-10 * scale {
            break;
        }
        let jac = jacobian(&x, r.len());
        let cost = |x: &[f64], r: &[f64]| -> f64 {
            r.iter().map(|v| v * v).sum::<f64>()
                + mu * x
                    .iter()
                    .zip(&start)
                    .map(|(a, b)| (a - b).powi(2))
                    .sum::<f64>()
        };
        let before = cost(&x, &r);
        let mut improved = false;
        for _ in 0..12 {
            let mut a = vec![vec![0.0; n]; n];
            let mut g = vec![0.0; n];
            for (row, ri) in jac.iter().zip(&r) {
                for i in 0..n {
                    if row[i] == 0.0 {
                        continue;
                    }
                    g[i] += row[i] * ri;
                    for j in 0..n {
                        a[i][j] += row[i] * row[j];
                    }
                }
            }
            for i in 0..n {
                g[i] += mu * (x[i] - start[i]);
                a[i][i] += mu + lambda * (1.0 + a[i][i]);
            }
            let Some(step) = solve_dense(a, g.iter().map(|v| -v).collect()) else {
                lambda *= 10.0;
                continue;
            };
            let next: Vec<f64> = x.iter().zip(&step).map(|(a, b)| a + b).collect();
            let rn = all(&unflatten(&next));
            if cost(&next, &rn) < before {
                x = next;
                r = rn;
                lambda = (lambda * 0.3).max(1e-12);
                improved = true;
                break;
            }
            lambda *= 10.0;
        }
        if !improved {
            break;
        }
    }
    // Phase 2: meet the equations exactly (minimum-norm Gauss–Newton).
    for _ in 0..30 {
        if max_abs(&r) < 1e-10 * scale {
            break;
        }
        let jac = jacobian(&x, r.len());
        let m = r.len();
        // (J Jᵀ + εI) y = -r, step = Jᵀ y.
        let mut a = vec![vec![0.0; m]; m];
        for i in 0..m {
            for j in 0..m {
                a[i][j] = (0..n).map(|k| jac[i][k] * jac[j][k]).sum();
            }
            a[i][i] += 1e-12;
        }
        let Some(y) = solve_dense(a, r.iter().map(|v| -v).collect()) else {
            break;
        };
        let step: Vec<f64> = (0..n)
            .map(|k| (0..m).map(|i| jac[i][k] * y[i]).sum())
            .collect();
        let next: Vec<f64> = x.iter().zip(&step).map(|(a, b)| a + b).collect();
        let rn = all(&unflatten(&next));
        if max_abs(&rn) >= max_abs(&r) {
            break;
        }
        x = next;
        r = rn;
    }
    let error = max_abs(&r);
    if error > 1e-6 * scale.max(1.0) {
        return Err(SolveError::Conflict(error));
    }
    let jac = jacobian(&x, r.len());
    let rank = rank(jac.clone());
    let rows = r.len();
    let xs = unflatten(&x);
    let mut moves = BTreeMap::new();
    for (i, v) in verts.iter().enumerate() {
        if xs[i].distance(x0[i]) > 1e-9 {
            moves.insert(*v, xs[i]);
        }
    }
    Ok(Solution {
        moved: moves.len(),
        moves,
        degrees_of_freedom: n - rank.min(n),
        redundant: rows - rank.min(rows),
        error_mm: error,
    })
}

/// The planes (unit normal, offset) of the faces around `v`.
fn face_planes(mesh: &HalfEdgeMesh, v: VertexId) -> Vec<(DVec3, f64)> {
    let mut out: Vec<(DVec3, f64)> = Vec::new();
    for &h in mesh.vertex_star(v) {
        let Ok(he) = mesh.he(h) else { continue };
        let Some(f) = he.face else { continue };
        let Some(face) = mesh.face(f) else { continue };
        let n = face.plane.normal;
        let d = -face.plane.d;
        if !out
            .iter()
            .any(|(m, e)| (m.dot(n) - 1.0).abs() < 1e-12 && (e - d).abs() < 1e-9)
        {
            out.push((n, d));
        }
    }
    out
}

/// Solves `a x = b` by Gaussian elimination with partial pivoting.
pub(crate) fn solve_dense(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-300 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        let pivot_row = a[col].clone();
        for row in col + 1..n {
            let f = a[row][col] / pivot_row[col];
            if f == 0.0 {
                continue;
            }
            for (x, p) in a[row][col..].iter_mut().zip(&pivot_row[col..]) {
                *x -= f * p;
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let s: f64 = (i + 1..n).map(|k| a[i][k] * x[k]).sum();
        x[i] = (b[i] - s) / a[i][i];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// The numeric rank of a matrix (row echelon with a relative threshold).
pub(crate) fn rank(mut m: Vec<Vec<f64>>) -> usize {
    let rows = m.len();
    let cols = m.first().map_or(0, Vec::len);
    let largest = m.iter().flatten().fold(0.0f64, |acc, v| acc.max(v.abs()));
    let eps = 1e-8 * largest.max(1e-300);
    let mut r = 0;
    for c in 0..cols {
        if r == rows {
            break;
        }
        let Some(p) = (r..rows).max_by(|&i, &j| m[i][c].abs().total_cmp(&m[j][c].abs())) else {
            break;
        };
        if m[p][c].abs() <= eps {
            continue;
        }
        m.swap(r, p);
        let pivot_row = m[r].clone();
        for row in m.iter_mut().skip(r + 1) {
            let f = row[c] / pivot_row[c];
            if f != 0.0 {
                for (x, q) in row[c..].iter_mut().zip(&pivot_row[c..]) {
                    *x -= f * q;
                }
            }
        }
        r += 1;
    }
    r
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn point_refs_read_and_write() {
        let p: PointRef = "12.b".parse().unwrap();
        assert_eq!(
            p,
            PointRef {
                edge: Pid(12),
                second: true
            }
        );
        assert_eq!(p.to_string(), "12.b");
        assert_eq!("@7.a".parse::<PointRef>().unwrap().to_string(), "7.a");
        assert!("12".parse::<PointRef>().is_err());
        assert!("12.c".parse::<PointRef>().is_err());
    }

    #[test]
    fn dense_solve_and_rank() {
        let x = solve_dense(vec![vec![2.0, 1.0], vec![1.0, 3.0]], vec![3.0, 5.0]).unwrap();
        assert!((x[0] - 0.8).abs() < 1e-12 && (x[1] - 1.4).abs() < 1e-12);
        assert_eq!(rank(vec![vec![1.0, 2.0], vec![2.0, 4.0]]), 1);
        assert_eq!(
            rank(vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![1.0, 1.0]]),
            2
        );
    }
}
