//! Implements docs/05-modeling-tools.md §5.17 (assemblies, milestone 1.12):
//! mates between the groups and components of a container, kept by a
//! rigid-body solver; exploded views; and the bill of materials.
//!
//! Mates live in the container's attributes (dictionary `planyrith`, key
//! [`KEY`], JSON). Each side names an occurrence of the container and,
//! inside it, a face (a plane) or an edge (a line; a circle's or arc's edge
//! gives its axis), written `<occurrence pid>:<element pid>`; `fixed`
//! names an occurrence alone.
//!
//! Unknowns are a rotation vector and a translation per occurrence that
//! moves (rotations about the occurrence's center). Occurrences with a
//! `fixed` mate stay; when none has one, the lowest pid among the mated
//! occurrences is grounded. Residuals are in millimeters (directions
//! weighed by the assembly's size). The solver is the one of docs/03
//! §11.2: damped least squares pulled toward the current placements, then
//! exact Gauss–Newton steps; the Jacobian's rank gives the freedom left.

use std::collections::BTreeMap;

use glam::{DAffine3, DQuat, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::targets_of;
use crate::commands::params::{Target, target_container};
use crate::commands::transaction::AttrTarget;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ContainerRef, Document, Entities, EntityView, Query, Value};
use crate::geom::CurveDescriptor;
use crate::ids::{EntityId, Pid};
use crate::sketch::{rank, solve_dense};
use crate::units::{Angle, Length};

/// The key of a container's mates in the `planyrith` dictionary.
pub const KEY: &str = "mates";
/// The key of the placements an exploded view keeps to collapse it.
pub const EXPLODED: &str = "exploded";

/// What a mate requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MateKind {
    /// The occurrence `a` stays where it is.
    Fixed,
    /// Faces against each other (planes facing), lines on one line, a line
    /// in a plane.
    Coincident,
    /// Facing planes `length` apart, or parallel lines `length` apart.
    Distance,
    /// Directions parallel.
    Parallel,
    /// Directions at a right angle.
    Perpendicular,
    /// Directions at `angle`.
    Angle,
    /// Two axes on one line (circle or arc edges).
    Concentric,
    /// Circular cylinders tangent to a plane or another circular cylinder.
    Tangent,
}

/// One side of a mate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MateRef {
    /// The group or instance.
    pub occurrence: Pid,
    /// A face or edge inside it (`None` for `fixed`).
    pub element: Option<Pid>,
}

impl std::fmt::Display for MateRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.element {
            Some(e) => write!(f, "{}:{}", self.occurrence.0, e.0),
            None => write!(f, "{}", self.occurrence.0),
        }
    }
}

impl std::str::FromStr for MateRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let num = |t: &str| {
            t.trim()
                .trim_start_matches('@')
                .parse::<u64>()
                .map(Pid)
                .map_err(|_| format!("`{s}`: write <occurrence pid>:<face or edge pid>"))
        };
        match s.split_once(':') {
            Some((o, e)) => Ok(MateRef {
                occurrence: num(o)?,
                element: Some(num(e)?),
            }),
            None => Ok(MateRef {
                occurrence: num(s)?,
                element: None,
            }),
        }
    }
}

impl Serialize for MateRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MateRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for MateRef {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MateRef".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^@?[0-9]+(:@?[0-9]+)?$",
            "description": "<occurrence pid>:<face or edge pid inside it>, or <occurrence pid>"
        })
    }
}

/// One mate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Mate {
    /// Its number in the container.
    pub id: u32,
    /// What it requires.
    pub kind: MateKind,
    /// The first side.
    pub a: MateRef,
    /// The second side (not for `fixed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b: Option<MateRef>,
    /// The distance, mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_mm: Option<f64>,
    /// The angle, radians.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_rad: Option<f64>,
    /// Planes facing the same way, or internal/opposite-side tangent contact.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub aligned: bool,
}

impl Mate {
    fn check(&self) -> Result<(), String> {
        match self.kind {
            MateKind::Fixed => {
                if self.b.is_some() {
                    return Err("fixed takes one occurrence".into());
                }
            }
            _ => {
                let b = self.b.ok_or("this mate needs a second side")?;
                if self.a.element.is_none() || b.element.is_none() {
                    return Err("name a face or edge on each side (<occurrence>:<element>)".into());
                }
                if self.a.occurrence == b.occurrence {
                    return Err("a mate joins two different occurrences".into());
                }
            }
        }
        if self.kind == MateKind::Distance
            && !self.length_mm.is_some_and(|l| l.is_finite() && l >= 0.0)
        {
            return Err("give a distance of zero or more".into());
        }
        if self.kind == MateKind::Angle
            && !self
                .angle_rad
                .is_some_and(|a| (0.0..=std::f64::consts::PI).contains(&a))
        {
            return Err("give an angle from 0° to 180°".into());
        }
        if self.kind == MateKind::Tangent && (self.length_mm.is_some() || self.angle_rad.is_some())
        {
            return Err(
                "tangent uses aligned for internal/opposite contact, not length or angle".into(),
            );
        }
        Ok(())
    }
}

fn attr_target(c: ContainerRef) -> AttrTarget {
    match c {
        ContainerRef::Model => AttrTarget::Document,
        ContainerRef::Definition(d) => AttrTarget::Definition(d),
    }
}

fn attrs_of(doc: &Document, c: ContainerRef) -> Option<&crate::document::AttributeDictionaries> {
    match c {
        ContainerRef::Model => Some(&doc.attrs),
        ContainerRef::Definition(d) => doc.definition(d).map(|def| &def.attrs),
    }
}

/// The mates of a container.
pub fn mates_of(doc: &Document, c: ContainerRef) -> Vec<Mate> {
    match attrs_of(doc, c).and_then(|a| a.get(crate::sketch::DICT, KEY)) {
        Some(Value::Str(json)) => serde_json::from_str(json).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn store(tx: &mut Transaction<'_>, c: ContainerRef, list: &[Mate]) -> Result<(), CommandError> {
    let value = if list.is_empty() {
        None
    } else {
        Some(Value::Str(
            serde_json::to_string(list).map_err(|e| CommandError::Internal(e.to_string()))?,
        ))
    };
    tx.set_attribute(attr_target(c), crate::sketch::DICT, KEY, value)
}

/// A side's geometry in its occurrence's coordinates.
#[derive(Debug, Clone, Copy)]
struct Feature {
    point: DVec3,
    dir: DVec3,
    plane: bool,
    // Radius in assembly coordinates, only when the circular reference's
    // occurrence preserves its circle through a similarity placement.
    radius: Option<f64>,
}

impl Feature {
    fn direction(self, placement: &DAffine3) -> DVec3 {
        let matrix = if self.plane {
            placement.matrix3.inverse().transpose()
        } else {
            placement.matrix3
        };
        (matrix * self.dir).normalize_or_zero()
    }
}

fn feature(doc: &Document, entities: &Entities, r: MateRef) -> Result<Feature, String> {
    let occurrence = entities
        .id_of(r.occurrence)
        .ok_or_else(|| format!("{} is not here", r.occurrence.0))?;
    let def = entities
        .definition_of(occurrence)
        .and_then(|d| doc.definition(d))
        .ok_or_else(|| format!("{} is not a group or component", r.occurrence.0))?;
    let Some(element) = r.element else {
        return Ok(Feature {
            point: DVec3::ZERO,
            dir: DVec3::Z,
            plane: true,
            radius: None,
        });
    };
    let inner = &def.entities;
    let mesh = inner.mesh();
    match inner.id_of(element) {
        Some(EntityId::Face(f)) => {
            let face = mesh.face_ref(f).map_err(|e| e.to_string())?;
            let points = mesh.loop_positions(face.outer).map_err(|e| e.to_string())?;
            let c = points.iter().copied().sum::<DVec3>() / points.len().max(1) as f64;
            Ok(Feature {
                point: c,
                dir: face.plane.normal,
                plane: true,
                radius: None,
            })
        }
        Some(EntityId::Edge(e)) => {
            let edge = mesh.edge(e).ok_or("the edge is gone")?;
            if let Some(c) = edge.curve.and_then(|c| mesh.curve(c))
                && let CurveDescriptor::Circle { center, normal, .. }
                | CurveDescriptor::Arc { center, normal, .. } = c
            {
                let placement = entities
                    .transform_of(occurrence)
                    .ok_or("the occurrence has no placement")?;
                let radius = match c.transformed(&placement) {
                    Some(
                        CurveDescriptor::Circle { radius, .. }
                        | CurveDescriptor::Arc { radius, .. },
                    ) => Some(radius),
                    _ => None,
                };
                return Ok(Feature {
                    point: *center,
                    dir: normal.normalize(),
                    plane: false,
                    radius,
                });
            }
            let s = mesh.edge_segment(e).map_err(|e| e.to_string())?;
            Ok(Feature {
                point: s.a,
                dir: (s.b - s.a).normalize(),
                plane: false,
                radius: None,
            })
        }
        _ => Err(format!(
            "{} names no face or edge in {}",
            element.0, r.occurrence.0
        )),
    }
}

/// The solver's verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct AssemblySolution {
    /// New placements of the occurrences that move.
    pub placements: BTreeMap<EntityId, DAffine3>,
    /// Degrees of freedom left.
    pub degrees_of_freedom: usize,
    /// The largest remaining error, mm.
    pub error_mm: f64,
}

/// Solves `mates` in `entities`.
pub fn solve(
    doc: &Document,
    entities: &Entities,
    mates: &[Mate],
) -> Result<AssemblySolution, String> {
    // The occurrences mated, and which stay.
    let mut occurrences: Vec<Pid> = Vec::new();
    let mut fixed: Vec<Pid> = Vec::new();
    for m in mates {
        for r in std::iter::once(m.a).chain(m.b) {
            if !occurrences.contains(&r.occurrence) {
                occurrences.push(r.occurrence);
            }
        }
        if m.kind == MateKind::Fixed {
            fixed.push(m.a.occurrence);
        }
    }
    occurrences.sort();
    if fixed.is_empty()
        && let Some(first) = occurrences.first()
    {
        fixed.push(*first);
    }
    let free: Vec<Pid> = occurrences
        .iter()
        .copied()
        .filter(|o| !fixed.contains(o))
        .collect();
    let mut base: BTreeMap<Pid, (EntityId, DAffine3, DVec3)> = BTreeMap::new();
    let mut size: f64 = 1.0;
    for &o in &occurrences {
        let id = entities
            .id_of(o)
            .ok_or_else(|| format!("{} is not here", o.0))?;
        let t = entities
            .transform_of(id)
            .ok_or_else(|| format!("{} has no placement", o.0))?;
        let def = entities
            .definition_of(id)
            .and_then(|d| doc.definition(d))
            .ok_or_else(|| format!("{} is not a group or component", o.0))?;
        let bb = def.entities.local_aabb(&doc.definitions, false);
        let center = if bb.is_empty() {
            t.translation
        } else {
            size = size.max((bb.max - bb.min).length());
            t.transform_point3(bb.center())
        };
        base.insert(o, (id, t, center));
    }
    let mut sides: Vec<(Feature, Feature)> = Vec::new();
    for m in mates {
        let fa = feature(doc, entities, m.a)?;
        let fb = match m.b {
            Some(b) => feature(doc, entities, b)?,
            None => fa,
        };
        if m.kind == MateKind::Tangent {
            let circular_pair = fa.radius.is_some() && fb.radius.is_some();
            let plane_circle =
                (fa.plane && fb.radius.is_some()) || (fb.plane && fa.radius.is_some());
            if !circular_pair && !plane_circle {
                return Err("tangent needs a plane and circular axis, or two circular axes with similarity placements (no distorted radii)".into());
            }
            if [fa.radius, fb.radius]
                .into_iter()
                .flatten()
                .any(|r| !r.is_finite() || r <= doc.info.tolerances.length)
            {
                return Err("tangent radii must exceed the length tolerance".into());
            }
            if circular_pair
                && m.aligned
                && (fa.radius.unwrap_or(0.0) - fb.radius.unwrap_or(0.0)).abs()
                    <= doc.info.tolerances.length
            {
                return Err("internal tangency needs unequal radii".into());
            }
        }
        sides.push((fa, fb));
    }
    let n = free.len() * 6;
    let index: BTreeMap<Pid, usize> = free.iter().enumerate().map(|(i, p)| (*p, i)).collect();
    let pose = |o: Pid, x: &[f64]| -> DAffine3 {
        let (_, t0, c) = base[&o];
        match index.get(&o) {
            None => t0,
            Some(&i) => {
                let r = DVec3::new(x[6 * i], x[6 * i + 1], x[6 * i + 2]);
                let t = DVec3::new(x[6 * i + 3], x[6 * i + 4], x[6 * i + 5]);
                DAffine3::from_translation(t + c)
                    * DAffine3::from_quat(DQuat::from_scaled_axis(r))
                    * DAffine3::from_translation(-c)
                    * t0
            }
        }
    };
    let residuals = |x: &[f64]| -> Vec<f64> {
        let mut out = Vec::new();
        for (m, (fa, fb)) in mates.iter().zip(&sides) {
            if m.kind == MateKind::Fixed {
                continue;
            }
            let (ta, tb) = (
                pose(m.a.occurrence, x),
                pose(m.b.map_or(m.a.occurrence, |b| b.occurrence), x),
            );
            let (pa, da) = (ta.transform_point3(fa.point), fa.direction(&ta));
            let (pb, db) = (tb.transform_point3(fb.point), fb.direction(&tb));
            let push3 = |out: &mut Vec<f64>, v: DVec3| out.extend([v.x, v.y, v.z]);
            match m.kind {
                MateKind::Fixed => {}
                MateKind::Coincident | MateKind::Distance => {
                    let gap = m.length_mm.unwrap_or(0.0);
                    if fa.plane && fb.plane {
                        let want = if m.aligned { db - da } else { db + da };
                        push3(&mut out, want * size);
                        out.push((pb - pa).dot(da) - gap);
                    } else if !fa.plane && !fb.plane {
                        push3(&mut out, da.cross(db) * size);
                        let off = (pb - pa).cross(da);
                        if m.kind == MateKind::Distance {
                            out.push(off.length() - gap);
                        } else {
                            push3(&mut out, off);
                        }
                    } else {
                        // A line in (or `gap` off) a plane.
                        let (pl, n, pt, d) = if fa.plane {
                            (pa, da, pb, db)
                        } else {
                            (pb, db, pa, da)
                        };
                        out.push(d.dot(n) * size);
                        out.push((pt - pl).dot(n) - gap);
                    }
                }
                MateKind::Concentric => {
                    push3(&mut out, da.cross(db) * size);
                    push3(&mut out, (pb - pa).cross(da));
                }
                MateKind::Parallel => push3(&mut out, da.cross(db) * size),
                MateKind::Perpendicular => out.push(da.dot(db) * size),
                MateKind::Angle => {
                    let got = libm::atan2(da.cross(db).length(), da.dot(db));
                    out.push((got - m.angle_rad.unwrap_or(0.0)) * size);
                }
                MateKind::Tangent => {
                    if fa.plane || fb.plane {
                        let (plane_point, normal, center, axis, radius) = if fa.plane {
                            (pa, fa.direction(&ta), pb, db, fb.radius.unwrap_or(0.0))
                        } else {
                            (pb, fb.direction(&tb), pa, da, fa.radius.unwrap_or(0.0))
                        };
                        out.push(axis.dot(normal) * size);
                        out.push(
                            (center - plane_point).dot(normal)
                                - if m.aligned { -radius } else { radius },
                        );
                    } else {
                        push3(&mut out, da.cross(db) * size);
                        let gap = if m.aligned {
                            (fa.radius.unwrap_or(0.0) - fb.radius.unwrap_or(0.0)).abs()
                        } else {
                            fa.radius.unwrap_or(0.0) + fb.radius.unwrap_or(0.0)
                        };
                        out.push((pb - pa).cross(da).length() - gap);
                    }
                }
            }
        }
        out
    };
    let jacobian = |x: &[f64], rows: usize| -> Vec<Vec<f64>> {
        let mut jac = vec![vec![0.0; n]; rows];
        let mut w = x.to_vec();
        for k in 0..n {
            let h = if k % 6 < 3 { 1e-7 } else { 1e-7 * size };
            let keep = w[k];
            w[k] = keep + h;
            let plus = residuals(&w);
            w[k] = keep - h;
            let minus = residuals(&w);
            w[k] = keep;
            for (row, (a, b)) in jac.iter_mut().zip(plus.iter().zip(&minus)) {
                row[k] = (a - b) / (2.0 * h);
            }
        }
        jac
    };
    let max_abs = |r: &[f64]| r.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let mut x = vec![0.0; n];
    // Exact symmetries have a zero centered derivative: choose a geometric
    // direction before the first Jacobian rather than leaving the solver stuck.
    for (m, (fa, fb)) in mates.iter().zip(&sides) {
        if m.kind != MateKind::Tangent {
            continue;
        }
        let Some(b) = m.b else {
            continue;
        };
        let (ta, tb) = (pose(m.a.occurrence, &x), pose(b.occurrence, &x));
        if fa.plane || fb.plane {
            let (normal, axis, circle_occ, plane_occ) = if fa.plane {
                (
                    fa.direction(&ta),
                    fb.direction(&tb),
                    b.occurrence,
                    m.a.occurrence,
                )
            } else {
                (
                    fb.direction(&tb),
                    fa.direction(&ta),
                    m.a.occurrence,
                    b.occurrence,
                )
            };
            if axis.dot(normal).abs() >= 1.0 - doc.info.tolerances.angle {
                let rotation_axis = crate::math::Plane::from_point_normal(DVec3::ZERO, normal)
                    .frame()
                    .u;
                if let Some(&i) = index.get(&circle_occ).or_else(|| index.get(&plane_occ)) {
                    let rotation = rotation_axis * std::f64::consts::FRAC_PI_2;
                    for k in 0..3 {
                        x[6 * i + k] += rotation[k];
                    }
                }
            }
        } else {
            let (pa, pb, axis) = (
                ta.transform_point3(fa.point),
                tb.transform_point3(fb.point),
                fa.direction(&ta),
            );
            if (pb - pa).cross(axis).length() <= doc.info.tolerances.length {
                let radius_a = fa.radius.unwrap_or(0.0);
                let radius_b = fb.radius.unwrap_or(0.0);
                let gap = if m.aligned {
                    (radius_a - radius_b).abs()
                } else {
                    radius_a + radius_b
                };
                let direction = crate::math::Plane::from_point_normal(DVec3::ZERO, axis)
                    .frame()
                    .u;
                let moving = index
                    .get(&b.occurrence)
                    .map(|i| (*i, 1.0))
                    .or_else(|| index.get(&m.a.occurrence).map(|i| (*i, -1.0)));
                if let Some((i, sign)) = moving {
                    for k in 0..3 {
                        x[6 * i + 3 + k] += direction[k] * gap * sign;
                    }
                }
            }
        }
    }
    let mut r = residuals(&x);
    let mu = 1e-8;
    let mut lambda = 1e-3;
    for _ in 0..200 {
        if n == 0 || max_abs(&r) < 1e-10 * size {
            break;
        }
        let jac = jacobian(&x, r.len());
        let cost = |x: &[f64], r: &[f64]| -> f64 {
            r.iter().map(|v| v * v).sum::<f64>() + mu * x.iter().map(|v| v * v).sum::<f64>()
        };
        let before = cost(&x, &r);
        let mut improved = false;
        for _ in 0..12 {
            let mut a = vec![vec![0.0; n]; n];
            let mut g = vec![0.0; n];
            for (row, ri) in jac.iter().zip(&r) {
                for i in 0..n {
                    g[i] += row[i] * ri;
                    for j in 0..n {
                        a[i][j] += row[i] * row[j];
                    }
                }
            }
            for i in 0..n {
                g[i] += mu * x[i];
                a[i][i] += mu + lambda * (1.0 + a[i][i]);
            }
            let Some(step) = solve_dense(a, g.iter().map(|v| -v).collect()) else {
                lambda *= 10.0;
                continue;
            };
            let next: Vec<f64> = x.iter().zip(&step).map(|(a, b)| a + b).collect();
            let rn = residuals(&next);
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
    for _ in 0..30 {
        if n == 0 || max_abs(&r) < 1e-10 * size {
            break;
        }
        let jac = jacobian(&x, r.len());
        let m = r.len();
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
        let next: Vec<f64> = (0..n)
            .map(|k| x[k] + (0..m).map(|i| jac[i][k] * y[i]).sum::<f64>())
            .collect();
        let rn = residuals(&next);
        if max_abs(&rn) >= max_abs(&r) {
            break;
        }
        x = next;
        r = rn;
    }
    let error = max_abs(&r);
    if error > 1e-6 * size {
        return Err(format!("the mates conflict; largest error {error:.4} mm"));
    }
    let used = if r.is_empty() || n == 0 {
        0
    } else {
        rank(jacobian(&x, r.len()))
    };
    let mut placements = BTreeMap::new();
    for &o in &free {
        let (id, t0, _) = base[&o];
        let t = pose(o, &x);
        let moved = (t.translation - t0.translation).length() > 1e-9
            || (t.matrix3 - t0.matrix3)
                .to_cols_array()
                .iter()
                .any(|v| v.abs() > 1e-12);
        if moved {
            placements.insert(id, t);
        }
    }
    Ok(AssemblySolution {
        placements,
        degrees_of_freedom: n - used.min(n),
        error_mm: error,
    })
}

fn apply(
    tx: &mut Transaction<'_>,
    target: &Target,
    list: &[Mate],
) -> Result<AssemblySolution, CommandError> {
    let solution = {
        let entities = tx
            .document()
            .container(target.container)
            .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
        solve(tx.document(), entities, list).map_err(CommandError::NotApplicable)?
    };
    for (&id, &t) in &solution.placements {
        tx.set_transform(target.container, id, t)?;
    }
    store(tx, target.container, list)?;
    Ok(solution)
}

/// What a mate command gave.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MateOutput {
    /// The mate added or changed.
    pub id: Option<u32>,
    /// Occurrences moved.
    pub moved: usize,
    /// Degrees of freedom left.
    pub degrees_of_freedom: usize,
    /// Mates in the container.
    pub mates: usize,
}

fn output(id: Option<u32>, s: &AssemblySolution, mates: usize) -> MateOutput {
    MateOutput {
        id,
        moved: s.placements.len(),
        degrees_of_freedom: s.degrees_of_freedom,
        mates,
    }
}

fn meta(name: &str, label: &str, view_only: bool) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        label: label.into(),
        category: CommandCategory::Organize,
        view_only,
        steps: Vec::new(),
    }
}

/// `mate-add` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MateAddParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// What to require.
    pub kind: MateKind,
    /// The first side (`<occurrence pid>:<face or edge pid>`).
    pub a: MateRef,
    /// The second side.
    #[serde(default)]
    pub b: Option<MateRef>,
    /// The distance of `distance`.
    #[serde(default)]
    pub length: Option<Length>,
    /// The angle of `angle`.
    #[serde(default)]
    pub angle: Option<Angle>,
    /// Planes facing the same way, or internal/opposite-side tangent contact.
    #[serde(default)]
    pub aligned: bool,
}

/// The `mate-add` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct MateAdd;

impl Command for MateAdd {
    type Params = MateAddParams;
    type Output = MateOutput;

    fn name(&self) -> &'static str {
        "mate-add"
    }

    fn validate(&self, _: &Document, p: &MateAddParams) -> Result<(), CommandError> {
        Mate {
            id: 0,
            kind: p.kind,
            a: p.a,
            b: p.b,
            length_mm: p.length.map(|l| l.as_mm()),
            angle_rad: p.angle.map(|a| a.as_radians()),
            aligned: p.aligned,
        }
        .check()
        .map_err(|e| CommandError::invalid_param("kind", e))
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MateAddParams,
    ) -> Result<MateOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let mut list = mates_of(tx.document(), target.container);
        let id = list.iter().map(|m| m.id).max().map_or(1, |m| m + 1);
        list.push(Mate {
            id,
            kind: p.kind,
            a: p.a,
            b: p.b,
            length_mm: p.length.map(|l| l.as_mm()),
            angle_rad: p.angle.map(|a| a.as_radians()),
            aligned: p.aligned,
        });
        let n = list.len();
        let s = apply(tx, &target, &list)?;
        Ok(output(Some(id), &s, n))
    }

    fn meta(&self) -> CommandMeta {
        meta("mate-add", "Add Mate", false)
    }
}

/// `mate-edit` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MateEditParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The mate's number.
    pub id: u32,
    /// A new distance.
    #[serde(default)]
    pub length: Option<Length>,
    /// A new angle.
    #[serde(default)]
    pub angle: Option<Angle>,
    /// Facing the same way, or not.
    #[serde(default)]
    pub aligned: Option<bool>,
}

/// The `mate-edit` command: drives a mate (a new distance or angle).
#[derive(Debug, Default, Clone, Copy)]
pub struct MateEdit;

impl Command for MateEdit {
    type Params = MateEditParams;
    type Output = MateOutput;

    fn name(&self) -> &'static str {
        "mate-edit"
    }

    fn validate(&self, _: &Document, p: &MateEditParams) -> Result<(), CommandError> {
        if p.length.is_none() && p.angle.is_none() && p.aligned.is_none() {
            return Err(CommandError::invalid_param(
                "length",
                "give length, angle or aligned",
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MateEditParams,
    ) -> Result<MateOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let mut list = mates_of(tx.document(), target.container);
        let m = list
            .iter_mut()
            .find(|m| m.id == p.id)
            .ok_or_else(|| CommandError::invalid_param("id", format!("no mate {}", p.id)))?;
        if let Some(l) = p.length {
            m.length_mm = Some(l.as_mm());
        }
        if let Some(a) = p.angle {
            m.angle_rad = Some(a.as_radians());
        }
        if let Some(al) = p.aligned {
            m.aligned = al;
        }
        m.check()
            .map_err(|e| CommandError::invalid_param("length", e))?;
        let n = list.len();
        let s = apply(tx, &target, &list)?;
        Ok(output(Some(p.id), &s, n))
    }

    fn meta(&self) -> CommandMeta {
        meta("mate-edit", "Edit Mate", false)
    }
}

/// `mate-delete` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MateDeleteParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The mates' numbers.
    #[serde(default)]
    pub ids: Vec<u32>,
    /// Every mate of the container.
    #[serde(default)]
    pub all: bool,
}

/// The `mate-delete` command (placements stay as they are).
#[derive(Debug, Default, Clone, Copy)]
pub struct MateDelete;

impl Command for MateDelete {
    type Params = MateDeleteParams;
    type Output = MateOutput;

    fn name(&self) -> &'static str {
        "mate-delete"
    }

    fn validate(&self, _: &Document, p: &MateDeleteParams) -> Result<(), CommandError> {
        if p.ids.is_empty() && !p.all {
            return Err(CommandError::invalid_param("ids", "give ids or all"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MateDeleteParams,
    ) -> Result<MateOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let mut list = mates_of(tx.document(), target.container);
        for id in &p.ids {
            if !list.iter().any(|m| m.id == *id) {
                return Err(CommandError::invalid_param("ids", format!("no mate {id}")));
            }
        }
        list.retain(|m| !p.all && !p.ids.contains(&m.id));
        store(tx, target.container, &list)?;
        Ok(MateOutput {
            id: None,
            moved: 0,
            degrees_of_freedom: 0,
            mates: list.len(),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("mate-delete", "Delete Mates", false)
    }
}

/// `mate-list` and `mate-solve` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MateListParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
}

/// `mate-list` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MateListOutput {
    /// The container's mates.
    pub mates: Vec<Mate>,
    /// `none`, `under`, `fully` or `conflict`.
    pub state: String,
    /// Degrees of freedom left.
    pub degrees_of_freedom: Option<usize>,
    /// Why the mates cannot be met.
    pub conflict: Option<String>,
}

/// The `mate-list` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct MateList;

impl Command for MateList {
    type Params = MateListParams;
    type Output = MateListOutput;

    fn name(&self) -> &'static str {
        "mate-list"
    }

    fn validate(&self, _: &Document, _: &MateListParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MateListParams,
    ) -> Result<MateListOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let mates = mates_of(tx.document(), target.container);
        let entities = tx
            .document()
            .container(target.container)
            .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
        let (state, dof, conflict) = if mates.is_empty() {
            ("none".to_string(), None, None)
        } else {
            match solve(tx.document(), entities, &mates) {
                Ok(s) if s.degrees_of_freedom == 0 => ("fully".into(), Some(0), None),
                Ok(s) => ("under".into(), Some(s.degrees_of_freedom), None),
                Err(e) => ("conflict".into(), None, Some(e)),
            }
        };
        Ok(MateListOutput {
            mates,
            state,
            degrees_of_freedom: dof,
            conflict,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("mate-list", "List Mates", true)
    }
}

/// The `mate-solve` command: meets the mates again after occurrences were
/// moved by hand.
#[derive(Debug, Default, Clone, Copy)]
pub struct MateSolve;

impl Command for MateSolve {
    type Params = MateListParams;
    type Output = MateOutput;

    fn name(&self) -> &'static str {
        "mate-solve"
    }

    fn validate(&self, _: &Document, _: &MateListParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MateListParams,
    ) -> Result<MateOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let list = mates_of(tx.document(), target.container);
        let n = list.len();
        let s = apply(tx, &target, &list)?;
        Ok(output(None, &s, n))
    }

    fn meta(&self) -> CommandMeta {
        meta("mate-solve", "Solve Mates", false)
    }
}

// ----- exploded views -------------------------------------------------------------------

fn factor() -> f64 {
    1.0
}

/// `explode-view` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExplodeViewParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The occurrences to spread (every group and component by default).
    #[serde(default)]
    pub targets: Option<Query>,
    /// How far: each moves away from the assembly's center by this times
    /// its own distance from it.
    #[serde(default = "factor")]
    pub factor: f64,
    /// Put everything back where it was before exploding.
    #[serde(default)]
    pub collapse: bool,
}

/// `explode-view` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExplodeViewOutput {
    /// Occurrences moved.
    pub moved: usize,
}

/// The `explode-view` command: spreads an assembly's parts apart, keeping
/// their placements to collapse it again.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExplodeView;

impl Command for ExplodeView {
    type Params = ExplodeViewParams;
    type Output = ExplodeViewOutput;

    fn name(&self) -> &'static str {
        "explode-view"
    }

    fn validate(&self, _: &Document, p: &ExplodeViewParams) -> Result<(), CommandError> {
        if !(p.factor.is_finite() && (0.0..=100.0).contains(&p.factor)) {
            return Err(CommandError::OutOfRange {
                name: "factor".into(),
                reason: "must be between 0 and 100".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ExplodeViewParams,
    ) -> Result<ExplodeViewOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let saved: BTreeMap<u64, [f64; 12]> = attrs_of(tx.document(), target.container)
            .and_then(|a| a.get(crate::sketch::DICT, EXPLODED))
            .and_then(|v| match v {
                Value::Str(s) => serde_json::from_str(s).ok(),
                _ => None,
            })
            .unwrap_or_default();
        if p.collapse {
            let mut moved = 0;
            for (pid, cols) in &saved {
                let id = tx
                    .document()
                    .container(target.container)
                    .and_then(|c| c.id_of(Pid(*pid)));
                if let Some(id) = id {
                    tx.set_transform(target.container, id, DAffine3::from_cols_array(cols))?;
                    moved += 1;
                }
            }
            tx.set_attribute(
                attr_target(target.container),
                crate::sketch::DICT,
                EXPLODED,
                None,
            )?;
            return Ok(ExplodeViewOutput { moved });
        }
        let ids: Vec<EntityId> = match &p.targets {
            Some(q) => targets_of(tx, &target, "targets", q)?,
            None => tx
                .document()
                .container(target.container)
                .map(|c| c.iter().collect())
                .unwrap_or_default(),
        }
        .into_iter()
        .filter(|id| matches!(id, EntityId::Group(_) | EntityId::Instance(_)))
        .collect();
        let doc = tx.document();
        let entities = doc
            .container(target.container)
            .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
        let mut centers = Vec::new();
        for &id in &ids {
            let (Some(t), Some(def)) = (
                entities.transform_of(id),
                entities.definition_of(id).and_then(|d| doc.definition(d)),
            ) else {
                continue;
            };
            let bb = def.entities.local_aabb(&doc.definitions, false);
            if bb.is_empty() {
                continue;
            }
            let pid = entities.pid_of(id).map_or(0, |p| p.0);
            centers.push((id, pid, t, t.transform_point3(bb.center())));
        }
        if centers.is_empty() {
            return Ok(ExplodeViewOutput { moved: 0 });
        }
        let middle = centers.iter().map(|c| c.3).sum::<DVec3>() / centers.len() as f64;
        let mut saved = saved;
        let mut moved = 0;
        for (id, pid, t, c) in centers {
            saved.entry(pid).or_insert_with(|| t.to_cols_array());
            let shift = (c - middle) * p.factor;
            if shift.length() > 1e-9 {
                tx.set_transform(target.container, id, DAffine3::from_translation(shift) * t)?;
                moved += 1;
            }
        }
        let json =
            serde_json::to_string(&saved).map_err(|e| CommandError::Internal(e.to_string()))?;
        tx.set_attribute(
            attr_target(target.container),
            crate::sketch::DICT,
            EXPLODED,
            Some(Value::Str(json)),
        )?;
        Ok(ExplodeViewOutput { moved })
    }

    fn meta(&self) -> CommandMeta {
        meta("explode-view", "Explode View", false)
    }
}

// ----- bill of materials ----------------------------------------------------------------

/// `bill-of-materials` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BomParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Count nested components too (every level), not just the top one.
    #[serde(default)]
    pub nested: bool,
}

/// One line of the bill of materials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BomRow {
    /// Item number, from 1.
    pub item: u32,
    /// The component's (or group's) name.
    pub name: String,
    /// How many.
    pub quantity: u32,
    /// The definition's description.
    pub description: String,
    /// The part number (attribute `properties` / `part_number`), if any.
    pub part_number: Option<String>,
}

/// `bill-of-materials` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BomOutput {
    /// Rows by name.
    pub rows: Vec<BomRow>,
}

/// The `bill-of-materials` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct BillOfMaterials;

fn count(
    doc: &Document,
    entities: &Entities,
    nested: bool,
    into: &mut BTreeMap<String, (u32, String, Option<String>)>,
) {
    for id in entities.iter() {
        let Some(d) = entities.definition_of(id) else {
            continue;
        };
        let Some(def) = doc.definition(d) else {
            continue;
        };
        let name = match entities.view(id) {
            Some(EntityView::Group(g)) if !g.name.is_empty() => g.name.clone(),
            _ => def.name.clone(),
        };
        let part_number = match def.attrs.get("properties", "part_number") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        };
        let row = into
            .entry(name)
            .or_insert((0, def.description.clone(), part_number));
        row.0 += 1;
        if nested {
            count(doc, &def.entities, true, into);
        }
    }
}

/// The bill of materials of a container: one row per name, in name order
/// (docs/05 §5.17; sheet tables and balloons use it, docs/15 §9.4).
pub fn bom_rows(doc: &Document, container: ContainerRef, nested: bool) -> Vec<BomRow> {
    let Some(entities) = doc.container(container) else {
        return Vec::new();
    };
    let mut rows = BTreeMap::new();
    count(doc, entities, nested, &mut rows);
    rows.into_iter()
        .enumerate()
        .map(|(i, (name, (quantity, description, part_number)))| BomRow {
            item: i as u32 + 1,
            name,
            quantity,
            description,
            part_number,
        })
        .collect()
}

impl Command for BillOfMaterials {
    type Params = BomParams;
    type Output = BomOutput;

    fn name(&self) -> &'static str {
        "bill-of-materials"
    }

    fn validate(&self, _: &Document, _: &BomParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: BomParams) -> Result<BomOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        Ok(BomOutput {
            rows: bom_rows(tx.document(), target.container, p.nested),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("bill-of-materials", "Bill of Materials", true)
    }
}

/// Registers the assembly commands.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(MateAdd);
    registry.register(MateEdit);
    registry.register(MateDelete);
    registry.register(MateList);
    registry.register(MateSolve);
    registry.register(ExplodeView);
    registry.register(BillOfMaterials);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use crate::ids::{EntityId, Pid};
    use glam::DVec3;
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn engine() -> Engine {
        Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
    }

    /// A 100 mm cube component placed at `at`; returns its occurrence pid.
    fn cube(e: &mut Engine, at: [f64; 3], name: &str) -> u64 {
        e.execute_erased(
            "rectangle",
            json!({ "corner_a": at, "corner_b": [at[0] + 100.0, at[1] + 100.0, at[2]] }),
        )
        .unwrap();
        e.execute_erased(
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 100 }),
        )
        .unwrap();
        let out = e
            .execute_erased(
                "make-component",
                json!({ "targets": "edge & !instance | face & !instance", "name": name }),
            )
            .unwrap();
        out["instance"].as_u64().unwrap()
    }

    /// The pid, inside occurrence `occ`, of the face whose normal is `n`.
    fn face(e: &Engine, occ: u64, n: DVec3) -> u64 {
        let doc = e.document();
        let id = doc.model.id_of(crate::ids::Pid(occ)).unwrap();
        let def = doc
            .definition(doc.model.definition_of(id).unwrap())
            .unwrap();
        let mesh = def.entities.mesh();
        for (f, face) in mesh.faces() {
            if face.plane.normal.distance(n) < 1e-9 {
                return def.entities.pid_of(EntityId::Face(f)).unwrap().0;
            }
        }
        panic!("no face");
    }

    fn center(e: &Engine, occ: u64) -> DVec3 {
        let doc = e.document();
        let id = doc.model.id_of(crate::ids::Pid(occ)).unwrap();
        doc.model
            .transform_of(id)
            .unwrap()
            .transform_point3(DVec3::new(50.0, 50.0, 50.0))
    }

    fn add(e: &mut Engine, p: Value) -> Value {
        e.execute_erased("mate-add", p).unwrap()
    }

    #[test]
    fn cubes_mated_face_to_face_then_driven() {
        let mut e = engine();
        let base = cube(&mut e, [0.0, 0.0, 0.0], "Base");
        let top = cube(&mut e, [300.0, 200.0, 40.0], "Top");
        // The top's bottom on the base's top, sides flush.
        let side = |e: &Engine, occ: u64, n: DVec3| format!("{occ}:{}", face(e, occ, n));
        let (base_top, top_bottom) = (side(&e, base, DVec3::Z), side(&e, top, -DVec3::Z));
        let (base_x, top_x) = (side(&e, base, DVec3::X), side(&e, top, DVec3::X));
        let (base_y, top_y) = (side(&e, base, DVec3::Y), side(&e, top, DVec3::Y));
        add(&mut e, json!({ "kind": "fixed", "a": base.to_string() }));
        add(
            &mut e,
            json!({ "kind": "coincident", "a": base_top, "b": top_bottom }),
        );
        add(
            &mut e,
            json!({ "kind": "coincident", "a": base_x, "b": top_x, "aligned": true }),
        );
        let out = add(
            &mut e,
            json!({ "kind": "distance", "a": base_y, "b": top_y,
                                      "aligned": true, "length": 0 }),
        );
        assert_eq!(out["degrees_of_freedom"], 0, "{out}");
        let c = center(&e, top);
        assert!(c.distance(DVec3::new(50.0, 50.0, 150.0)) < 1e-6, "{c}");
        // Driving the distance slides it along the green axis.
        let id = out["id"].as_u64().unwrap();
        e.execute_erased("mate-edit", json!({ "id": id, "length": 30 }))
            .unwrap();
        let c = center(&e, top);
        assert!(c.distance(DVec3::new(50.0, 80.0, 150.0)) < 1e-6, "{c}");
        let list = e.execute_erased("mate-list", json!({})).unwrap();
        assert_eq!(list["state"], "fully");
        // The base never moved.
        assert!(center(&e, base).distance(DVec3::splat(50.0)) < 1e-9);
        // A conflicting mate is refused.
        let err = e
            .execute_erased(
                "mate-add",
                json!({ "kind": "distance",
                "a": base_top, "b": top_bottom, "length": 10 }),
            )
            .unwrap_err();
        assert!(err.to_string().contains("conflict"), "{err}");
    }

    #[test]
    fn exploding_collapsing_and_the_bill_of_materials() {
        let mut e = engine();
        let a = cube(&mut e, [0.0, 0.0, 0.0], "Leg");
        e.execute_erased(
            "move",
            json!({ "targets": format!("@{a}"), "delta": [200, 0, 0], "copy": true }),
        )
        .unwrap();
        cube(&mut e, [100.0, 0.0, 100.0], "Top");
        let before: Vec<_> = e
            .document()
            .model
            .iter()
            .filter_map(|id| e.document().model.transform_of(id))
            .collect();
        let out = e
            .execute_erased("explode-view", json!({ "factor": 1.0 }))
            .unwrap();
        assert!(out["moved"].as_u64().unwrap() >= 2);
        e.execute_erased("explode-view", json!({ "collapse": true }))
            .unwrap();
        let after: Vec<_> = e
            .document()
            .model
            .iter()
            .filter_map(|id| e.document().model.transform_of(id))
            .collect();
        assert_eq!(before, after);
        let bom = e.execute_erased("bill-of-materials", json!({})).unwrap();
        let rows = bom["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{bom}");
        assert_eq!(rows[0]["name"], "Leg");
        assert_eq!(rows[0]["quantity"], 2);
        assert_eq!(rows[1]["quantity"], 1);
    }

    fn circle(e: &mut Engine, center: [f64; 3], radius: f64, name: &str) -> (u64, u64) {
        e.execute_erased(
            "circle",
            json!({"center":center,"radius":radius,"segments":12}),
        )
        .unwrap();
        let out = e
            .execute_erased("make-component", json!({"targets":"edge|face","name":name}))
            .unwrap();
        let occurrence = out["instance"].as_u64().unwrap();
        let doc = e.document();
        let id = doc.model.id_of(Pid(occurrence)).unwrap();
        let def = doc
            .definition(doc.model.definition_of(id).unwrap())
            .unwrap();
        let edge = def
            .entities
            .mesh()
            .edges()
            .find(|(_, e)| e.curve.is_some())
            .unwrap()
            .0;
        (occurrence, def.entities.pid_of(edge.into()).unwrap().0)
    }

    fn placed_circle(e: &Engine, occurrence: u64, element: u64) -> (DVec3, DVec3, f64) {
        let entities = &e.document().model;
        let feature = super::feature(
            e.document(),
            entities,
            super::MateRef {
                occurrence: Pid(occurrence),
                element: Some(Pid(element)),
            },
        )
        .unwrap();
        let transform = entities
            .transform_of(entities.id_of(Pid(occurrence)).unwrap())
            .unwrap();
        (
            transform.transform_point3(feature.point),
            feature.direction(&transform),
            feature.radius.unwrap_or(0.0),
        )
    }

    #[test]
    fn tangent_circular_axes_touch_a_plane_on_either_side() {
        let mut e = engine();
        let base = cube(&mut e, [0.0; 3], "Base");
        let plane = face(&e, base, DVec3::X);
        let (part, edge) = circle(&mut e, [250.0, 50.0, 30.0], 20.0, "Roller");
        add(&mut e, json!({"kind":"fixed","a":base.to_string()}));
        let out = add(
            &mut e,
            json!({"kind":"tangent","a":format!("{base}:{plane}"),"b":format!("{part}:{edge}")}),
        );
        assert_eq!(out["degrees_of_freedom"], 4);
        assert!((placed_circle(&e, part, edge).0.x - 120.0).abs() < 1e-6);
        e.execute_erased("mate-edit", json!({"id":out["id"],"aligned":true}))
            .unwrap();
        assert!((placed_circle(&e, part, edge).0.x - 80.0).abs() < 1e-6);
        let opts = crate::io::native::WriteOptions {
            deterministic: true,
            include_history: false,
            ..Default::default()
        };
        let data =
            crate::io::native::write_document(e.document(), &[], &opts, &crate::io::NoProgress)
                .unwrap();
        let loaded = crate::io::native::read_document(&data, &crate::io::NoProgress).unwrap();
        assert_eq!(
            data,
            crate::io::native::write_document(&loaded.document, &[], &opts, &crate::io::NoProgress)
                .unwrap()
        );
    }

    #[test]
    fn tangent_axes_external_internal_and_coaxial_starts() {
        for center in [[80.0, 0.0, 30.0], [0.0, 0.0, 0.0]] {
            let mut e = engine();
            let (a, ae) = circle(&mut e, [0.0; 3], 20.0, "Outer");
            let (b, be) = circle(&mut e, center, 10.0, "Inner");
            let out = add(
                &mut e,
                json!({"kind":"tangent","a":format!("{a}:{ae}"),"b":format!("{b}:{be}")}),
            );
            assert_eq!(out["degrees_of_freedom"], 3);
            let (pa, axis, _) = placed_circle(&e, a, ae);
            let (pb, _, _) = placed_circle(&e, b, be);
            assert!(((pb - pa).cross(axis).length() - 30.0).abs() < 1e-6);
            e.execute_erased("mate-edit", json!({"id":out["id"],"aligned":true}))
                .unwrap();
            let (pb, _, _) = placed_circle(&e, b, be);
            assert!(((pb - pa).cross(axis).length() - 10.0).abs() < 1e-6);
        }
    }

    #[test]
    fn tangent_plane_symmetric_start_and_scaled_or_distorted_radii() {
        let mut e = engine();
        let base = cube(&mut e, [0.0; 3], "Base");
        let plane = face(&e, base, DVec3::Z);
        let (part, edge) = circle(&mut e, [50.0, 50.0, 250.0], 20.0, "Roller");
        e.execute_erased(
            "scale",
            json!({"targets":format!("@{part}"),"factors":[-2,2,2],"pivot":[50,50,250]}),
        )
        .unwrap();
        let out = add(
            &mut e,
            json!({"kind":"tangent","a":format!("{base}:{plane}"),"b":format!("{part}:{edge}")}),
        );
        assert_eq!(out["degrees_of_freedom"], 4);
        let (p, axis, radius) = placed_circle(&e, part, edge);
        assert!((radius - 40.0).abs() < 1e-6);
        assert!((p.z - 140.0).abs() < 1e-6, "{p}");
        assert!(axis.z.abs() < 1e-6);
        e.execute_erased("mate-delete", json!({"all":true}))
            .unwrap();
        e.execute_erased(
            "scale",
            json!({"targets":format!("@{part}"),"factors":[2,1,1]}),
        )
        .unwrap();
        let before = crate::io::export::json::document_to_json(e.document()).unwrap();
        let err = e.execute_erased("mate-add",json!({"kind":"tangent","a":format!("{base}:{plane}"),"b":format!("{part}:{edge}")})).unwrap_err();
        assert!(err.to_string().contains("similarity"), "{err}");
        assert_eq!(
            before,
            crate::io::export::json::document_to_json(e.document()).unwrap()
        );
    }

    #[test]
    fn tangent_conflicts_and_invalid_contacts_leave_the_document_unchanged() {
        let mut e = engine();
        let (a, ae) = circle(&mut e, [0.0; 3], 20.0, "A");
        let (b, be) = circle(&mut e, [80.0, 0.0, 0.0], 20.0, "B");
        for occurrence in [a, b] {
            add(&mut e, json!({"kind":"fixed","a":occurrence.to_string()}));
        }
        for aligned in [false, true] {
            let before = crate::io::export::json::document_to_json(e.document()).unwrap();
            assert!(
                e.execute_erased(
                    "mate-add",
                    json!({"kind":"tangent",
                "a":format!("{a}:{ae}"),"b":format!("{b}:{be}"),"aligned":aligned})
                )
                .is_err()
            );
            assert_eq!(
                before,
                crate::io::export::json::document_to_json(e.document()).unwrap()
            );
        }
        let c = cube(&mut e, [200.0, 0.0, 0.0], "C");
        let d = cube(&mut e, [400.0, 0.0, 0.0], "D");
        let cf = face(&e, c, DVec3::X);
        let df = face(&e, d, DVec3::X);
        let before = crate::io::export::json::document_to_json(e.document()).unwrap();
        let err = e
            .execute_erased(
                "mate-add",
                json!({"kind":"tangent",
            "a":format!("{c}:{cf}"),"b":format!("{d}:{df}")}),
            )
            .unwrap_err();
        assert!(err.to_string().contains("circular"), "{err}");
        assert_eq!(
            before,
            crate::io::export::json::document_to_json(e.document()).unwrap()
        );
    }
}
