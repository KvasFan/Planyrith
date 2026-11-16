//! Implements docs/05-modeling-tools.md §5.16 "Persistent references"
//! (milestone 1.36): the entities a feature's parameters name by pid keep
//! naming the same geometry after its part regenerates.
//!
//! Regenerating a part recreates its contents, so their pids change. When a
//! feature is added, each pid its parameters name (`{"Pid": n}`, `@n` in a
//! query or path, `n.a`/`n.b` ends of edges) that exists in the part gets a
//! shape: a face's plane, center and area, an edge's ends, a group's or
//! component's place. Before the feature runs again, each shape is matched
//! to the best entity of the same kind in the part as it is then; the
//! parameters are rewritten to the matched pids and the shapes refreshed.

use std::collections::{BTreeMap, BTreeSet};

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::document::Entities;
use crate::ids::{EntityId, Pid};

/// The shape of an entity a feature names (milestone 1.36).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RefShape {
    /// A face: its unit normal, its plane's offset along it, its outer
    /// loop's center and its area.
    Face {
        /// Unit normal.
        normal: [f64; 3],
        /// `normal · p` for points `p` of the plane, mm.
        offset: f64,
        /// Center of the outer loop, mm.
        center: [f64; 3],
        /// Area, mm².
        area: f64,
    },
    /// An edge: its two ends.
    Edge {
        /// First end, mm.
        a: [f64; 3],
        /// Second end, mm.
        b: [f64; 3],
    },
    /// A group or component instance: where its origin is placed.
    Object {
        /// `group` or `instance`.
        object: String,
        /// Its origin in the part, mm.
        center: [f64; 3],
    },
}

/// How well a candidate matches: lower is better; `None` when it cannot.
fn score(want: &RefShape, have: &RefShape) -> Option<f64> {
    let v = DVec3::from_array;
    match (want, have) {
        (
            RefShape::Face {
                normal: n0,
                offset: o0,
                center: c0,
                area: a0,
            },
            RefShape::Face {
                normal,
                offset,
                center,
                area,
            },
        ) => (v(*n0).dot(v(*normal)) >= 0.999).then(|| {
            v(*c0).distance(v(*center))
                + (o0 - offset).abs()
                + (a0.max(0.0).sqrt() - area.max(0.0).sqrt()).abs()
        }),
        (RefShape::Edge { a: a0, b: b0 }, RefShape::Edge { a, b }) => {
            let (d0, d) = (v(*b0) - v(*a0), v(*b) - v(*a));
            let parallel = d0.normalize_or_zero().dot(d.normalize_or_zero()).abs() >= 0.999;
            parallel.then(|| {
                ((v(*a0) + v(*b0)) * 0.5).distance((v(*a) + v(*b)) * 0.5)
                    + (d0.length() - d.length()).abs()
            })
        }
        (
            RefShape::Object {
                object: k0,
                center: c0,
            },
            RefShape::Object { object, center },
        ) => (k0 == object).then(|| v(*c0).distance(v(*center))),
        _ => None,
    }
}

/// Where a shape is, to report how far a match moved.
fn center_of(s: &RefShape) -> DVec3 {
    match s {
        RefShape::Face { center, .. } | RefShape::Object { center, .. } => {
            DVec3::from_array(*center)
        }
        RefShape::Edge { a, b } => (DVec3::from_array(*a) + DVec3::from_array(*b)) * 0.5,
    }
}

/// The shape of entity `id` of `entities`.
pub(crate) fn shape_of(entities: &Entities, id: EntityId) -> Option<RefShape> {
    let mesh = entities.mesh();
    match id {
        EntityId::Face(f) => {
            let face = mesh.face(f)?;
            let n = face.plane.normal;
            let pts = mesh.loop_positions(face.outer).ok()?;
            let first = *pts.first()?;
            // Area-weighted center of the outer loop (fan from its first
            // corner; signed areas keep concave loops right).
            let (mut weight, mut sum) = (0.0, DVec3::ZERO);
            for w in pts.windows(2).skip(1) {
                let a = 0.5 * (w[0] - first).cross(w[1] - first).dot(n);
                weight += a;
                sum += (first + w[0] + w[1]) / 3.0 * a;
            }
            let center = if weight.abs() > 1e-12 {
                sum / weight
            } else {
                pts.iter().copied().sum::<DVec3>() / pts.len() as f64
            };
            Some(RefShape::Face {
                normal: n.to_array(),
                offset: n.dot(first),
                center: center.to_array(),
                area: mesh.face_area(f).ok()?,
            })
        }
        EntityId::Edge(e) => {
            let s = mesh.edge_segment(e).ok()?;
            Some(RefShape::Edge {
                a: s.a.to_array(),
                b: s.b.to_array(),
            })
        }
        EntityId::Group(_) | EntityId::Instance(_) => Some(RefShape::Object {
            object: if matches!(id, EntityId::Group(_)) {
                "group".into()
            } else {
                "instance".into()
            },
            center: entities.transform_of(id)?.translation.to_array(),
        }),
        _ => None,
    }
}

/// Keys whose strings are free text, never references.
const TEXT_KEYS: [&str; 6] = ["text", "name", "label", "description", "formula", "value"];

/// Applies `f` to every pid a parameter value names, rewriting it to what
/// `f` returns.
fn visit(params: &Json, key: Option<&str>, f: &mut dyn FnMut(u64) -> u64) -> Json {
    match params {
        Json::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let next = if k == "Pid" {
                    match v.as_u64() {
                        Some(n) => Json::from(f(n)),
                        None => v.clone(),
                    }
                } else {
                    visit(v, Some(k), f)
                };
                out.insert(k.clone(), next);
            }
            Json::Object(out)
        }
        Json::Array(items) => Json::Array(items.iter().map(|v| visit(v, key, f)).collect()),
        Json::String(s) if !s.starts_with('=') && !key.is_some_and(|k| TEXT_KEYS.contains(&k)) => {
            Json::String(rewrite_text(s, f))
        }
        other => other.clone(),
    }
}

/// `s` with each `@n` token, and a whole `n.a`/`n.b`, rewritten by `f`.
fn rewrite_text(s: &str, f: &mut dyn FnMut(u64) -> u64) -> String {
    let bytes = s.as_bytes();
    // A whole end of an edge: `12.a`.
    if let Some((pid, end)) = s.rsplit_once('.')
        && (end == "a" || end == "b")
        && !pid.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
        && let Ok(n) = pid.parse::<u64>()
    {
        return format!("{}.{end}", f(n));
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > start
                && let Ok(n) = s[start..j].parse::<u64>()
            {
                out.push('@');
                out.push_str(&f(n).to_string());
                i = j;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The pids `params` names.
pub(crate) fn named(params: &Json) -> BTreeSet<u64> {
    let mut out = BTreeSet::new();
    visit(params, None, &mut |n| {
        out.insert(n);
        n
    });
    out
}

/// Shapes of the pids `params` names that exist in `entities`; `keep`
/// supplies shapes already known (captured when the feature was made).
pub(crate) fn capture(
    entities: &Entities,
    params: &Json,
    keep: &BTreeMap<u64, RefShape>,
) -> BTreeMap<u64, RefShape> {
    let mut out = BTreeMap::new();
    for n in named(params) {
        if let Some(s) = keep.get(&n) {
            out.insert(n, s.clone());
        } else if let Some(s) = entities.id_of(Pid(n)).and_then(|id| shape_of(entities, id)) {
            out.insert(n, s);
        }
    }
    out
}

/// One reference matched again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RefMatch {
    /// The feature.
    pub feature: u32,
    /// The pid its parameters named.
    pub from: u64,
    /// The pid of the entity matched now.
    pub to: u64,
    /// How far the matched entity's center lies from the recorded one, mm.
    pub moved_mm: f64,
}

/// Matches each recorded shape to an entity of `entities`: the new pid, its
/// shape now, and how far it moved. Fails naming a reference with no match.
pub(crate) fn resolve(
    entities: &Entities,
    refs: &BTreeMap<u64, RefShape>,
) -> Result<BTreeMap<u64, (u64, RefShape, f64)>, String> {
    let mut candidates: Vec<(u64, RefShape)> = entities
        .iter()
        .filter_map(|id| Some((entities.pid_of(id)?.0, shape_of(entities, id)?)))
        .collect();
    candidates.sort_by_key(|(pid, _)| *pid);
    let mut out = BTreeMap::new();
    for (&old, want) in refs {
        let best = candidates
            .iter()
            .filter_map(|(pid, have)| score(want, have).map(|s| (s, *pid, have)))
            .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let Some((_, pid, have)) = best else {
            let what = match want {
                RefShape::Face { .. } => "a face facing the same way",
                RefShape::Edge { .. } => "a parallel edge",
                RefShape::Object { .. } => "an object of the same kind",
            };
            return Err(format!("reference @{old} found no match ({what})"));
        };
        let moved = center_of(want).distance(center_of(have));
        out.insert(old, (pid, have.clone(), moved));
    }
    Ok(out)
}

/// `params` with the pids in `map` replaced.
pub(crate) fn rewrite(params: &Json, map: &BTreeMap<u64, u64>) -> Json {
    visit(params, None, &mut |n| map.get(&n).copied().unwrap_or(n))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn references_are_found_and_rewritten() {
        let params = json!({
            "face": { "Pid": 12 },
            "edges": ["@3", "@4 | @5"],
            "points": ["7.a", "@8.b"],
            "context": "#Model/@1",
            "text": "Room @9",
            "distance": "=h + 1",
        });
        assert_eq!(
            named(&params).into_iter().collect::<Vec<_>>(),
            vec![1, 3, 4, 5, 7, 8, 12]
        );
        let map: BTreeMap<u64, u64> = [(12, 40), (3, 30), (4, 31), (7, 70), (8, 80)].into();
        assert_eq!(
            rewrite(&params, &map),
            json!({
                "face": { "Pid": 40 },
                "edges": ["@30", "@31 | @5"],
                "points": ["70.a", "@80.b"],
                "context": "#Model/@1",
                "text": "Room @9",
                "distance": "=h + 1",
            })
        );
    }
}
