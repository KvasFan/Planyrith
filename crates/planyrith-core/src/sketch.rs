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
//! Arcs and circles take part through their center and radius (§11.4).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::{AttributeDictionaries, Entities, Value};
use crate::geom::{CurveDescriptor, HalfEdgeMesh};
use crate::ids::{CurveId, EdgeId, EntityId, Pid, VertexId};
use crate::math::Plane;
use crate::units::Angle;

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
    /// The radius of an arc or circle (a driving dimension).
    Radius,
    /// Arcs and circles with the first one's radius.
    EqualRadius,
    /// Arcs and circles sharing the first one's center.
    Concentric,
    /// A point on an arc's or circle's circle.
    OnCurve,
    /// An edge's line touching an arc or circle, or two arcs or circles
    /// touching (outside each other, or one inside the other).
    Tangent,
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
            Perpendicular | Collinear | Angle | Tangent => (0, 2, 2),
            Midpoint | OnLine | OnCurve => (1, 1, 1),
            Symmetric => (2, 1, 1),
            Length | Radius => (0, 1, 1),
            EqualRadius | Concentric => (0, 2, usize::MAX),
        }
    }

    /// Whether it carries a length.
    pub fn takes_length(self) -> bool {
        matches!(
            self,
            ConstraintKind::Length | ConstraintKind::Distance | ConstraintKind::Radius
        )
    }

    /// Whether every edge it names stands for its arc or circle.
    pub fn edges_are_curves(self) -> bool {
        use ConstraintKind::*;
        matches!(self, Radius | EqualRadius | Concentric | OnCurve)
    }

    /// Whether some edge it names may stand for its arc or circle.
    pub fn uses_curves(self) -> bool {
        self.edges_are_curves() || self == ConstraintKind::Tangent
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
    /// A tangency between two arcs or circles: one inside the other
    /// (recorded from the drawing when the constraint is made).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internal: Option<bool>,
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
                Some(l) if l.is_finite() && l > 0.0 && self.kind == ConstraintKind::Radius => {}
                Some(l) if l.is_finite() && l >= 0.0 && self.kind != ConstraintKind::Radius => {}
                _ if self.kind == ConstraintKind::Radius => {
                    return Err("give a radius greater than zero".into());
                }
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
    /// A constraint on arcs or circles names an edge of neither.
    #[error("constraint {0}: edge {1} is not part of an arc or circle")]
    NotCircular(u32, u64),
}

/// The solver's verdict on a set of constraints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Solution {
    /// New positions of the vertices that move (container coordinates).
    #[serde(skip)]
    #[schemars(skip)]
    pub moves: BTreeMap<VertexId, DVec3>,
    /// Arcs and circles that changed.
    #[serde(skip)]
    #[schemars(skip)]
    pub curves: Vec<Reshaped>,
    /// When circles were turned half a segment to clear a contact: the
    /// moves and curves without that turn, for contacts the turn makes
    /// cross instead (one circle inside another).
    #[serde(skip)]
    #[schemars(skip)]
    #[allow(clippy::type_complexity)]
    pub unturned: Option<(BTreeMap<VertexId, DVec3>, Vec<Reshaped>)>,
    /// Vertices that moved.
    pub moved: usize,
    /// Degrees of freedom left among the named points.
    pub degrees_of_freedom: usize,
    /// Equations that repeat others (consistent but redundant).
    pub redundant: usize,
    /// The largest remaining error, mm.
    pub error_mm: f64,
}

/// An arc or circle the solver reshaped.
#[derive(Debug, Clone, PartialEq)]
pub struct Reshaped {
    /// Its edges before the change, in curve order.
    pub members: Vec<EdgeId>,
    /// Its vertices after the change, in curve order (to find its edges
    /// again when moving merged a vertex).
    pub points: Vec<DVec3>,
    /// The vertices close a circle.
    pub closed: bool,
    /// The new descriptor.
    pub descriptor: CurveDescriptor,
}

/// Whether a vertex of `cv` at `xs` comes within the merge distance of an
/// edge that is not one of its own.
fn touches(
    mesh: &HalfEdgeMesh,
    index: &BTreeMap<VertexId, usize>,
    xs: &[DVec3],
    cv: &CurveVar,
) -> bool {
    let at = |v: VertexId| {
        index
            .get(&v)
            .map_or_else(|| mesh.position(v).unwrap_or_default(), |&i| xs[i])
    };
    let own: std::collections::BTreeSet<EdgeId> = cv.members.iter().copied().collect();
    let points: Vec<DVec3> = cv.chain.iter().map(|&i| xs[i]).collect();
    let near = 2e-3;
    for (e, _) in mesh.edges() {
        if own.contains(&e) {
            continue;
        }
        let Ok((a, b)) = mesh.edge_endpoints(e) else {
            continue;
        };
        let (pa, pb) = (at(a), at(b));
        let d = pb - pa;
        let len2 = d.length_squared();
        for p in &points {
            let t = if len2 > 0.0 {
                ((*p - pa).dot(d) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            if (pa + d * t).distance(*p) < near {
                return true;
            }
        }
    }
    false
}

/// A constraint resolved to unknowns.
struct Resolved<'a> {
    c: &'a Constraint,
    /// Point index of each named point.
    points: Vec<usize>,
    /// Point indices of both ends of each named edge.
    edges: Vec<(usize, usize)>,
    /// For each named edge, the circular curve it stands for (curve kinds).
    curves: Vec<Option<usize>>,
    /// A tangency's vertex shared by its line and arc, or by its two arcs.
    shared: Option<usize>,
}

/// An arc or circle the solver moves through its center and radius
/// (docs/03 §11.4). A circle's vertices and an arc's interior vertices are
/// derived from them; an arc's ends stay free on its circle.
struct CurveVar {
    id: CurveId,
    members: Vec<EdgeId>,
    normal: DVec3,
    c0: DVec3,
    r0: f64,
    segments: u32,
    /// Point indices along the curve.
    chain: Vec<usize>,
    closed: bool,
    /// Circle: unit direction from the center to `chain[0]` at the start.
    dir0: DVec3,
    /// Circle: angle of each chain vertex from `chain[0]`; arc: its fraction
    /// of the sweep.
    phase: Vec<f64>,
    /// Arc: signed sweep from `chain[0]` to the last vertex, about `normal`.
    sweep0: f64,
    /// Arc: `chain[0]` is the descriptor's start.
    forward: bool,
}

/// The current state while solving.
struct State<'s> {
    pos: &'s [DVec3],
    curves: &'s [(DVec3, f64)],
    normals: &'s [DVec3],
}

/// `v` rotated by `angle` about the unit axis `n` (Rodrigues).
fn rotate(v: DVec3, n: DVec3, angle: f64) -> DVec3 {
    let (s, c) = (libm::sin(angle), libm::cos(angle));
    v * c + n.cross(v) * s + n * n.dot(v) * (1.0 - c)
}

/// The unit in-plane direction of `v` (normal `n`).
fn in_plane(v: DVec3, n: DVec3) -> DVec3 {
    (v - n * n.dot(v)).normalize_or_zero()
}

/// The signed angle from unit `a` to unit `b` about `n`, on the branch
/// nearest `near`.
fn angle_near(a: DVec3, b: DVec3, n: DVec3, near: f64) -> f64 {
    let raw = libm::atan2(n.dot(a.cross(b)), a.dot(b));
    let turns = libm::round((near - raw) / std::f64::consts::TAU);
    raw + turns * std::f64::consts::TAU
}

impl CurveVar {
    /// Places the derived vertices for center `c` and radius `r`.
    fn place(&self, c: DVec3, r: f64, pos: &mut [DVec3]) {
        let n = self.normal;
        if self.closed {
            for (k, &i) in self.chain.iter().enumerate() {
                pos[i] = c + rotate(self.dir0, n, self.phase[k]) * r;
            }
            return;
        }
        let last = self.chain.len() - 1;
        let ua = in_plane(pos[self.chain[0]] - c, n);
        let sweep = self.sweep(c, pos);
        for k in 1..last {
            pos[self.chain[k]] = c + rotate(ua, n, self.phase[k] * sweep) * r;
        }
    }

    /// An arc's sweep from its first to its last vertex.
    fn sweep(&self, c: DVec3, pos: &[DVec3]) -> f64 {
        let n = self.normal;
        let ua = in_plane(pos[self.chain[0]] - c, n);
        let ub = in_plane(pos[self.chain[self.chain.len() - 1]] - c, n);
        angle_near(ua, ub, n, self.sweep0)
    }

    /// The descriptor for center `c` and radius `r`.
    fn descriptor(&self, c: DVec3, r: f64, pos: &[DVec3]) -> CurveDescriptor {
        if self.closed {
            return CurveDescriptor::Circle {
                center: c,
                normal: self.normal,
                radius: r,
                segments: self.segments,
            };
        }
        let frame = Plane::from_point_normal(c, self.normal).frame_at(c);
        let angle = |p: DVec3| {
            let q = frame.to_2d(p) - frame.to_2d(c);
            libm::atan2(q.y, q.x)
        };
        let sweep = self.sweep(c, pos);
        let (start, end) = if self.forward {
            let a = angle(pos[self.chain[0]]);
            (a, a + sweep)
        } else {
            let a = angle(pos[self.chain[self.chain.len() - 1]]);
            (a, a - sweep)
        };
        CurveDescriptor::Arc {
            center: c,
            normal: self.normal,
            radius: r,
            start_angle: Angle::radians(start),
            end_angle: Angle::radians(end),
            segments: self.segments,
        }
    }
}

/// The equations of one constraint, in millimeters.
fn residuals(r: &Resolved<'_>, s: &State<'_>, out: &mut Vec<f64>) {
    use ConstraintKind::*;
    let x = s.pos;
    let p = |i: usize| x[r.points[i]];
    let e = |i: usize| (x[r.edges[i].0], x[r.edges[i].1]);
    let dir = |i: usize| {
        let (a, b) = e(i);
        b - a
    };
    let curve = |i: usize| r.curves.get(i).copied().flatten().map(|k| s.curves[k]);
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
        Radius => {
            if let Some((_, radius)) = curve(0) {
                out.push(radius - r.c.length_mm.unwrap_or(0.0));
            }
        }
        EqualRadius => {
            if let Some((_, r0)) = curve(0) {
                for i in 1..r.edges.len() {
                    if let Some((_, ri)) = curve(i) {
                        out.push(ri - r0);
                    }
                }
            }
        }
        Concentric => {
            if let Some((c0, _)) = curve(0) {
                for i in 1..r.edges.len() {
                    if let Some((ci, _)) = curve(i) {
                        let d = ci - c0;
                        out.extend([d.x, d.y, d.z]);
                    }
                }
            }
        }
        OnCurve => {
            if let Some((c, radius)) = curve(0) {
                out.push((p(0) - c).length() - radius);
            }
        }
        Tangent => match (curve(0), curve(1)) {
            (Some((c1, r1)), Some((c2, r2))) => {
                if let (Some(v), Some(k)) = (r.shared, r.curves[0]) {
                    // Joined at a vertex: the centers and the vertex in line.
                    let u = (x[v] - c1).normalize_or_zero();
                    out.push(s.normals[k].dot(u.cross(c2 - x[v])));
                    return;
                }
                let d = (c2 - c1).length();
                if r.c.internal.unwrap_or(false) {
                    out.push(d - (r1 - r2).abs());
                } else {
                    out.push(d - (r1 + r2));
                }
            }
            (Some((c, radius)), None) | (None, Some((c, radius))) => {
                let line = if r.curves[0].is_some() { 1 } else { 0 };
                let (a, b) = e(line);
                let u = (b - a).normalize_or_zero();
                if let Some(v) = r.shared {
                    // Joined at a vertex: the center on the line's normal
                    // there (the vertex is on the circle already).
                    out.push((c - x[v]).dot(u));
                } else {
                    out.push((c - a).cross(u).length() - radius);
                }
            }
            (None, None) => {}
        },
    }
}

/// The circular curve (arc or circle) the edge `pid` belongs to.
fn circular_curve(entities: &Entities, pid: Pid) -> Option<(CurveId, &CurveDescriptor)> {
    let EntityId::Edge(e) = entities.id_of(pid)? else {
        return None;
    };
    let mesh = entities.mesh();
    let c = mesh.edge(e)?.curve?;
    let d = mesh.curve(c)?;
    matches!(
        d,
        CurveDescriptor::Arc { .. } | CurveDescriptor::Circle { .. }
    )
    .then_some((c, d))
}

/// The vertices along curve `members`, and whether they close.
fn curve_chain(mesh: &HalfEdgeMesh, members: &[EdgeId]) -> Option<(Vec<VertexId>, bool)> {
    let (first, rest) = members.split_first()?;
    let (a0, b0) = mesh.edge_endpoints(*first).ok()?;
    let mut chain = match rest.first() {
        Some(next) => {
            let (a1, b1) = mesh.edge_endpoints(*next).ok()?;
            if b0 == a1 || b0 == b1 {
                vec![a0, b0]
            } else {
                vec![b0, a0]
            }
        }
        None => vec![a0, b0],
    };
    for e in rest {
        let (a, b) = mesh.edge_endpoints(*e).ok()?;
        let last = *chain.last()?;
        if a == last {
            chain.push(b);
        } else if b == last {
            chain.push(a);
        } else {
            return None;
        }
    }
    let closed = chain.len() > 2 && chain.first() == chain.last();
    if closed {
        chain.pop();
    }
    Some((chain, closed))
}

/// Checks that the edges a curve kind treats as arcs or circles are on one,
/// and records which side a tangency between two curves touches on.
pub fn prepare(entities: &Entities, c: &mut Constraint) -> Result<(), String> {
    let curved: Vec<Option<(CurveId, &CurveDescriptor)>> = c
        .edges
        .iter()
        .map(|&pid| circular_curve(entities, pid))
        .collect();
    match c.kind {
        k if k.edges_are_curves() => {
            if let Some(i) = curved.iter().position(Option::is_none) {
                return Err(format!(
                    "edge {} is not part of an arc or circle",
                    c.edges[i].0
                ));
            }
        }
        ConstraintKind::Tangent => {
            if curved.iter().all(Option::is_none) {
                return Err("tangent needs an edge of an arc or circle".into());
            }
            if let [Some((i1, d1)), Some((i2, d2))] = curved.as_slice() {
                if i1 == i2 {
                    return Err("tangent needs two different curves".into());
                }
                if c.internal.is_none() {
                    let (Some(c1), Some(c2)) = (d1.center(), d2.center()) else {
                        return Err("tangent needs circular curves".into());
                    };
                    let d = (c2 - c1).length();
                    c.internal = Some(d < radius_of(d1).max(radius_of(d2)));
                }
            } else {
                c.internal = None;
            }
        }
        _ => {}
    }
    Ok(())
}

fn radius_of(d: &CurveDescriptor) -> f64 {
    match d {
        CurveDescriptor::Arc { radius, .. }
        | CurveDescriptor::Circle { radius, .. }
        | CurveDescriptor::Polygon { radius, .. } => *radius,
        CurveDescriptor::Freehand { .. } => 0.0,
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
    // Pass 1: the circular curves that curve kinds name.
    let mut curves: Vec<CurveVar> = Vec::new();
    let mut curve_of: BTreeMap<CurveId, usize> = BTreeMap::new();
    let mut usable: Vec<&Constraint> = Vec::new();
    for c in constraints {
        let missing = c.points.iter().any(|p| ends(p.edge).is_none())
            || c.edges.iter().any(|&e| ends(e).is_none());
        if missing {
            if skip_missing {
                continue;
            }
            return Err(SolveError::Missing(c.id));
        }
        if c.kind.uses_curves() {
            for &pid in &c.edges {
                let Some((id, d)) = circular_curve(entities, pid) else {
                    if c.kind.edges_are_curves() {
                        return Err(SolveError::NotCircular(c.id, pid.0));
                    }
                    continue;
                };
                if curve_of.contains_key(&id) {
                    continue;
                }
                let members = mesh
                    .curve_edges(id)
                    .map_err(|_| SolveError::Missing(c.id))?;
                let (chain, closed) =
                    curve_chain(mesh, &members).ok_or(SolveError::NotCircular(c.id, pid.0))?;
                let chain: Vec<usize> = chain.into_iter().map(|v| var(v, &mut index)).collect();
                let (center, normal, radius, segments, angles) = match *d {
                    CurveDescriptor::Arc {
                        center,
                        normal,
                        radius,
                        start_angle,
                        end_angle,
                        segments,
                    } => (
                        center,
                        normal,
                        radius,
                        segments,
                        Some((start_angle.as_radians(), end_angle.as_radians())),
                    ),
                    CurveDescriptor::Circle {
                        center,
                        normal,
                        radius,
                        segments,
                    } => (center, normal, radius, segments, None),
                    _ => return Err(SolveError::NotCircular(c.id, pid.0)),
                };
                curve_of.insert(id, curves.len());
                curves.push(CurveVar {
                    id,
                    members,
                    normal: normal.normalize_or_zero(),
                    c0: center,
                    r0: radius,
                    segments,
                    chain,
                    closed,
                    dir0: DVec3::ZERO,
                    phase: Vec::new(),
                    sweep0: angles.map_or(0.0, |(s, e)| e - s),
                    forward: true,
                });
            }
        }
        usable.push(c);
    }
    // Pass 2: points and edges.
    let mut resolved = Vec::new();
    for c in usable {
        let mut points = Vec::new();
        let mut edges = Vec::new();
        let mut slots = Vec::new();
        for p in &c.points {
            if let Some((a, b)) = ends(p.edge) {
                points.push(var(if p.second { b } else { a }, &mut index));
            }
        }
        for &e in &c.edges {
            if let Some((a, b)) = ends(e) {
                edges.push((var(a, &mut index), var(b, &mut index)));
            }
            slots.push(if c.kind.uses_curves() {
                circular_curve(entities, e).and_then(|(id, _)| curve_of.get(&id).copied())
            } else {
                None
            });
        }
        // A tangency's shared vertex: an end of each side (an arc's ends,
        // a line's ends).
        let shared = if c.kind == ConstraintKind::Tangent && edges.len() == 2 {
            let ends_of = |slot: usize| -> Vec<usize> {
                match slots[slot].map(|k| &curves[k]) {
                    Some(cv) if cv.closed => Vec::new(),
                    Some(cv) => vec![cv.chain[0], cv.chain[cv.chain.len() - 1]],
                    None => vec![edges[slot].0, edges[slot].1],
                }
            };
            let (one, two) = (ends_of(0), ends_of(1));
            one.into_iter().find(|v| two.contains(v))
        } else {
            None
        };
        resolved.push(Resolved {
            c,
            points,
            edges,
            curves: slots,
            shared,
        });
    }
    let x0: Vec<DVec3> = verts
        .iter()
        .map(|v| mesh.position(*v).unwrap_or_default())
        .collect();
    let scale = x0.iter().fold(1.0f64, |m, p| m.max(p.abs().max_element()));
    // Curve phases from the drawing as it is.
    let mut derived: Vec<Option<usize>> = vec![None; verts.len()];
    for (k, cv) in curves.iter_mut().enumerate() {
        let n = cv.normal;
        let first = in_plane(x0[cv.chain[0]] - cv.c0, n);
        if cv.closed {
            cv.dir0 = first;
            cv.phase = cv
                .chain
                .iter()
                .map(|&i| angle_near(first, in_plane(x0[i] - cv.c0, n), n, 0.0))
                .collect();
            for &i in &cv.chain {
                derived[i] = Some(k);
            }
        } else {
            // Which end the descriptor starts at decides the sweep's sign.
            let frame = Plane::from_point_normal(cv.c0, n).frame_at(cv.c0);
            let a = cv.sweep0;
            let start_angle = match mesh.curve(cv.id) {
                Some(CurveDescriptor::Arc { start_angle, .. }) => start_angle.as_radians(),
                _ => 0.0,
            };
            let start = frame.from_2d(
                frame.to_2d(cv.c0)
                    + glam::DVec2::new(libm::cos(start_angle), libm::sin(start_angle)) * cv.r0,
            );
            let last = cv.chain[cv.chain.len() - 1];
            cv.forward = x0[cv.chain[0]].distance(start) <= x0[last].distance(start);
            cv.sweep0 = if cv.forward { a } else { -a };
            let sweep0 = cv.sweep0;
            cv.phase = cv
                .chain
                .iter()
                .map(|&i| {
                    let mut t = angle_near(first, in_plane(x0[i] - cv.c0, n), n, 0.0);
                    if sweep0 > 0.0 && t < -1e-12 {
                        t += std::f64::consts::TAU;
                    } else if sweep0 < 0.0 && t > 1e-12 {
                        t -= std::f64::consts::TAU;
                    }
                    if sweep0.abs() > 1e-12 {
                        t / sweep0
                    } else {
                        0.0
                    }
                })
                .collect();
            for &i in &cv.chain[1..cv.chain.len() - 1] {
                derived[i] = Some(k);
            }
        }
    }
    let free: Vec<usize> = (0..verts.len()).filter(|&i| derived[i].is_none()).collect();
    let coplanar = |k: usize, (m, d): (DVec3, f64)| -> bool {
        let cv = &curves[k];
        (m.dot(cv.normal).abs() - 1.0).abs() < 1e-9 && (m.dot(cv.c0) - d).abs() < 1e-6 * scale
    };
    // Faces keep their planes: one equation per vertex and face, except
    // derived curve vertices in their own curve's plane (the center's plane
    // equation keeps those).
    let mut flat: Vec<(usize, DVec3, f64)> = Vec::new();
    let mut end_off_plane: Vec<(usize, usize)> = Vec::new();
    for (i, &v) in verts.iter().enumerate() {
        let planes = face_planes(mesh, v);
        for plane in &planes {
            if derived[i].is_some_and(|k| coplanar(k, *plane)) {
                continue;
            }
            flat.push((i, plane.0, plane.1));
        }
        for (k, cv) in curves.iter().enumerate() {
            let is_end = !cv.closed && (cv.chain[0] == i || cv.chain[cv.chain.len() - 1] == i);
            if is_end && !planes.iter().any(|pl| coplanar(k, *pl)) {
                end_off_plane.push((i, k));
            }
        }
    }
    let n = free.len() * 3 + curves.len() * 4;
    let mut start = Vec::with_capacity(n);
    for &i in &free {
        start.extend(x0[i].to_array());
    }
    for cv in &curves {
        start.extend([cv.c0.x, cv.c0.y, cv.c0.z, cv.r0]);
    }
    let base = free.len() * 3;
    let normals: Vec<DVec3> = curves.iter().map(|cv| cv.normal).collect();
    let eval = |u: &[f64]| -> (Vec<DVec3>, Vec<(DVec3, f64)>) {
        let mut pos = x0.clone();
        for (k, &i) in free.iter().enumerate() {
            pos[i] = DVec3::new(u[3 * k], u[3 * k + 1], u[3 * k + 2]);
        }
        let cs: Vec<(DVec3, f64)> = (0..curves.len())
            .map(|k| {
                let o = base + 4 * k;
                (DVec3::new(u[o], u[o + 1], u[o + 2]), u[o + 3])
            })
            .collect();
        for (cv, &(c, r)) in curves.iter().zip(&cs) {
            cv.place(c, r, &mut pos);
        }
        (pos, cs)
    };
    let all = |u: &[f64]| -> Vec<f64> {
        let (pos, cs) = eval(u);
        let s = State {
            pos: &pos,
            curves: &cs,
            normals: &normals,
        };
        let mut out = Vec::new();
        for r in &resolved {
            residuals(r, &s, &mut out);
        }
        for &(i, m, d) in &flat {
            out.push(m.dot(pos[i]) - d);
        }
        for (cv, &(c, r)) in curves.iter().zip(&cs) {
            out.push(cv.normal.dot(c - cv.c0));
            if !cv.closed {
                for &i in [cv.chain[0], cv.chain[cv.chain.len() - 1]].iter() {
                    out.push((pos[i] - c).length() - r);
                }
            }
        }
        for &(i, k) in &end_off_plane {
            out.push(curves[k].normal.dot(pos[i] - cs[k].0));
        }
        out
    };
    let jacobian = |v: &[f64], rows: usize| -> Vec<Vec<f64>> {
        let mut jac = vec![vec![0.0; n]; rows];
        let mut w = v.to_vec();
        for k in 0..n {
            let h = 1e-7 * scale.max(1.0);
            let keep = w[k];
            w[k] = keep + h;
            let plus = all(&w);
            w[k] = keep - h;
            let minus = all(&w);
            w[k] = keep;
            for (row, (a, b)) in jac.iter_mut().zip(plus.iter().zip(&minus)) {
                row[k] = (a - b) / (2.0 * h);
            }
        }
        jac
    };
    let mut x = start.clone();
    let mut r = all(&x);
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
            let rn = all(&next);
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
        let rn = all(&next);
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
    let (mut xs, cs) = eval(&x);
    let unturned = xs.clone();
    let mut turned = false;
    // A curve must keep a positive radius larger than the merge distance.
    if let Some(&(_, radius)) = cs.iter().find(|(_, r)| *r <= 1e-3) {
        return Err(SolveError::Conflict(radius.abs()));
    }
    // A circle's segments are inscribed: where one of its vertices would
    // touch another edge (a tangent line or circle at a vertex), its
    // vertices turn half a segment so that the segments clear the contact.
    for (k, cv) in curves.iter().enumerate() {
        if !cv.closed || cv.segments == 0 {
            continue;
        }
        let (c, radius) = cs[k];
        if touches(mesh, &index, &xs, cv) {
            let half = std::f64::consts::PI / f64::from(cv.segments);
            for (j, &i) in cv.chain.iter().enumerate() {
                xs[i] = c + rotate(cv.dir0, cv.normal, cv.phase[j] + half) * radius;
            }
            turned = true;
        }
    }
    let jac = jacobian(&x, r.len());
    let rank = rank(jac.clone());
    let rows = r.len();
    let outcome = |xs: &[DVec3]| {
        let mut moves = BTreeMap::new();
        for (i, v) in verts.iter().enumerate() {
            if xs[i].distance(x0[i]) > 1e-9 {
                moves.insert(*v, xs[i]);
            }
        }
        let mut reshaped = Vec::new();
        for (cv, &(c, radius)) in curves.iter().zip(&cs) {
            if cv.chain.iter().any(|&i| moves.contains_key(&verts[i])) {
                reshaped.push(Reshaped {
                    members: cv.members.clone(),
                    points: cv.chain.iter().map(|&i| xs[i]).collect(),
                    closed: cv.closed,
                    descriptor: cv.descriptor(c, radius, xs),
                });
            }
        }
        (moves, reshaped)
    };
    let (moves, reshaped) = outcome(&xs);
    let unturned = turned.then(|| outcome(&unturned));
    Ok(Solution {
        moved: moves.len(),
        moves,
        curves: reshaped,
        unturned,
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
