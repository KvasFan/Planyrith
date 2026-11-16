//! Implements docs/05-modeling-tools.md §4.4 "Moving to contact"
//! (milestone 1.37): how far objects can move along a straight path before
//! they touch another object.
//!
//! Under a translation, two polyhedra first touch where a vertex of one
//! meets a face of the other, or an edge of one meets an edge of the other.
//! Each moving object's vertices are cast along the move against the other
//! objects' triangles, their vertices back against its triangles, and its
//! edges swept against theirs; the earliest contact that moves *into* the
//! other object (against a face's outward normal, or into the outward cone
//! of an edge) stops the move. Faces of open meshes count from both sides.
//! Contacts already present at the start stop the move only when it goes
//! into them, so objects resting on each other can still slide apart or
//! along each other.

use glam::DVec3;

use crate::document::{Document, InstancePath};
use crate::geom::tessellate::tessellate_face;
use crate::io::render_mesh::DrawItem;
use crate::math::Aabb;
use crate::tolerance::Tolerances;

/// One object's geometry in world coordinates.
pub(crate) struct Body {
    path: InstancePath,
    closed: bool,
    vertices: Vec<DVec3>,
    /// Triangles with their face's outward unit normal.
    triangles: Vec<([DVec3; 3], DVec3)>,
    /// Edges with the sum of their faces' outward unit normals.
    edges: Vec<(DVec3, DVec3, DVec3)>,
    bounds: Aabb,
}

impl Body {
    /// The body of a draw item.
    pub(crate) fn of(doc: &Document, item: &DrawItem, tol: &Tolerances) -> Option<Body> {
        let mesh = doc.container(item.container)?.mesh();
        if mesh.face_count() == 0 {
            return None;
        }
        let w = item.world;
        let normal_matrix = w.matrix3.inverse().transpose();
        let world_normal = |n: DVec3| (normal_matrix * n).normalize_or_zero();
        let vertices: Vec<DVec3> = mesh
            .vertices()
            .map(|(_, v)| w.transform_point3(v.position))
            .collect();
        let mut triangles = Vec::new();
        for (f, face) in mesh.faces() {
            let n = world_normal(face.plane.normal);
            let Ok(t) = tessellate_face(mesh, f, tol) else {
                continue;
            };
            for tri in &t.triangles {
                let p = |i: u32| w.transform_point3(t.positions[i as usize]);
                triangles.push(([p(tri[0]), p(tri[1]), p(tri[2])], n));
            }
        }
        let mut edges = Vec::new();
        for (e, _) in mesh.edges() {
            let Ok(s) = mesh.edge_segment(e) else {
                continue;
            };
            let mut out = DVec3::ZERO;
            for f in mesh.edge_faces(e).ok()?.into_iter().flatten() {
                if let Some(face) = mesh.face(f) {
                    out += world_normal(face.plane.normal);
                }
            }
            edges.push((w.transform_point3(s.a), w.transform_point3(s.b), out));
        }
        let bounds = Aabb::from_points(vertices.iter().copied());
        Some(Body {
            path: item.path.clone(),
            closed: mesh.is_solid(),
            vertices,
            triangles,
            edges,
            bounds,
        })
    }
}

/// Where a move first touches.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Contact {
    /// The share of the move made before touching (0 to 1).
    pub fraction: f64,
    /// The moving object that touches.
    pub moving: InstancePath,
    /// The object it touches.
    pub against: InstancePath,
}

/// `a` grown by `by` in every direction.
fn grown(a: &Aabb, by: f64) -> Aabb {
    Aabb::from_points([a.min - DVec3::splat(by), a.max + DVec3::splat(by)])
}

/// `a` and where it ends after moving by `d`.
fn swept(a: &Aabb, d: DVec3) -> Aabb {
    Aabb::from_points([a.min, a.max, a.min + d, a.max + d])
}

fn boxes_meet(a: &Aabb, b: &Aabb) -> bool {
    a.min.x <= b.max.x
        && b.min.x <= a.max.x
        && a.min.y <= b.max.y
        && b.min.y <= a.max.y
        && a.min.z <= b.max.z
        && b.min.z <= a.max.z
}

/// The share `t` of `d` at which the ray `o + t·d` crosses triangle `tri`.
fn ray_triangle(o: DVec3, d: DVec3, tri: &[DVec3; 3], slack: f64) -> Option<f64> {
    let (e1, e2) = (tri[1] - tri[0], tri[2] - tri[0]);
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-18 {
        return None;
    }
    let s = o - tri[0];
    let u = s.dot(p) / det;
    let q = s.cross(e1);
    let v = d.dot(q) / det;
    let eps = 1e-9;
    if u < -eps || v < -eps || u + v > 1.0 + eps {
        return None;
    }
    let t = e2.dot(q) / det;
    (t >= -slack && t <= 1.0).then_some(t)
}

/// The share `t` of `d` at which edge `a0–a1`, moving by `t·d`, crosses edge
/// `b0–b1`.
fn edge_edge(a0: DVec3, a1: DVec3, b0: DVec3, b1: DVec3, d: DVec3, slack: f64) -> Option<f64> {
    let (ea, eb) = (a1 - a0, b1 - b0);
    // s·ea − u·eb + t·d = b0 − a0
    let r = b0 - a0;
    let m = glam::DMat3::from_cols(ea, -eb, d);
    let det = m.determinant();
    let scale = ea.length() * eb.length() * d.length();
    if scale <= 0.0 || det.abs() < 1e-12 * scale {
        return None;
    }
    let x = m.inverse() * r;
    let (s, u, t) = (x.x, x.y, x.z);
    let eps = 1e-9;
    ((-eps..=1.0 + eps).contains(&s) && (-eps..=1.0 + eps).contains(&u) && t >= -slack && t <= 1.0)
        .then_some(t)
}

/// The earliest share of `d` at which `a` (moving) touches `b` (still).
fn first_touch(a: &Body, b: &Body, d: DVec3, tol: &Tolerances) -> Option<f64> {
    let reach = grown(&swept(&a.bounds, d), tol.length);
    if !boxes_meet(&reach, &b.bounds) {
        return None;
    }
    let len = d.length();
    let slack = tol.length / len;
    let into = |n: DVec3, closed: bool| -> bool {
        let along = d.dot(n) / len;
        if closed {
            along < -1e-9
        } else {
            along.abs() > 1e-9
        }
    };
    let mut best: Option<f64> = None;
    let mut take = |t: f64| {
        if best.is_none_or(|b| t < b) {
            best = Some(t);
        }
    };
    // Vertices of the moving object against the other's faces.
    for v in a.vertices.iter().filter(|v| b_near(&b.bounds, **v, d, tol)) {
        for (tri, n) in &b.triangles {
            if into(*n, b.closed)
                && let Some(t) = ray_triangle(*v, d, tri, slack)
            {
                take(t);
            }
        }
    }
    // The other's vertices against the moving object's faces (moving back).
    for v in b
        .vertices
        .iter()
        .filter(|v| b_near(&a.bounds, **v, -d, tol))
    {
        for (tri, n) in &a.triangles {
            if into(-*n, a.closed)
                && let Some(t) = ray_triangle(*v, -d, tri, slack)
            {
                take(t);
            }
        }
    }
    // Edges against edges.
    for (a0, a1, _) in &a.edges {
        let ea = Aabb::from_points([*a0, *a1, *a0 + d, *a1 + d]);
        if !boxes_meet(&grown(&ea, tol.length), &b.bounds) {
            continue;
        }
        for (b0, b1, out) in &b.edges {
            if !boxes_meet(&grown(&ea, tol.length), &Aabb::from_points([*b0, *b1])) {
                continue;
            }
            let Some(mut n) = (*a1 - *a0).cross(*b1 - *b0).try_normalize() else {
                continue;
            };
            if b.closed {
                // Point the normal out of the still object at its edge.
                let side = n.dot(*out);
                if side.abs() < 1e-9 {
                    continue;
                }
                if side < 0.0 {
                    n = -n;
                }
            }
            if into(n, b.closed)
                && let Some(t) = edge_edge(*a0, *a1, *b0, *b1, d, slack)
            {
                take(t);
            }
        }
    }
    best.map(|t| t.max(0.0))
}

/// Whether a vertex moving by `d` can reach box `b`.
fn b_near(b: &Aabb, v: DVec3, d: DVec3, tol: &Tolerances) -> bool {
    boxes_meet(&grown(&Aabb::from_points([v, v + d]), tol.length), b)
}

/// The first contact of `moving` objects with `still` ones along `d`.
pub(crate) fn first_contact(
    moving: &[Body],
    still: &[Body],
    d: DVec3,
    tol: &Tolerances,
) -> Option<Contact> {
    if d.length() <= tol.length {
        return None;
    }
    let mut best: Option<Contact> = None;
    for a in moving {
        for b in still {
            if let Some(t) = first_touch(a, b, d, tol)
                && best.as_ref().is_none_or(|c| t < c.fraction)
            {
                best = Some(Contact {
                    fraction: t,
                    moving: a.path.clone(),
                    against: b.path.clone(),
                });
            }
        }
    }
    best
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use serde_json::{Value, json};
    use std::sync::Arc;

    /// A box group named `name` from `lo` to `hi`.
    fn block(e: &mut Engine, name: &str, lo: [f64; 3], hi: [f64; 3]) {
        e.execute_erased(
            "rectangle",
            json!({ "corner_a": [lo[0], lo[1], lo[2]], "corner_b": [hi[0], hi[1], lo[2]] }),
        )
        .unwrap();
        e.execute_erased(
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": hi[2] - lo[2] }),
        )
        .unwrap();
        e.execute_erased(
            "make-group",
            json!({ "targets": "edge | face", "name": name }),
        )
        .unwrap();
    }

    fn origin_of(e: &Engine, name: &str) -> glam::DVec3 {
        let doc = e.document();
        doc.model
            .groups()
            .find(|(_, g)| g.name == name)
            .unwrap()
            .1
            .transform
            .translation
    }

    fn mv(e: &mut Engine, name: &str, delta: [f64; 3]) -> Value {
        e.execute_erased(
            "move",
            json!({ "targets": format!("group & name:{name}"), "delta": delta, "stop_at_contact": true }),
        )
        .unwrap()
    }

    #[test]
    fn a_box_stops_against_another() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        block(&mut e, "A", [0.0, 0.0, 0.0], [100.0, 100.0, 100.0]);
        block(&mut e, "B", [300.0, 20.0, 20.0], [400.0, 80.0, 80.0]);
        let out = mv(&mut e, "A", [500.0, 0.0, 0.0]);
        let c = &out["contact"];
        assert!(
            (c["distance_mm"].as_f64().unwrap() - 200.0).abs() < 1e-6,
            "{out}"
        );
        assert!((c["fraction"].as_f64().unwrap() - 0.4).abs() < 1e-9);
        assert_eq!(
            (c["moving"].as_str(), c["against"].as_str()),
            (Some("A"), Some("B"))
        );
        assert!((origin_of(&e, "A").x - 200.0).abs() < 1e-6);
        // Touching, it cannot go further; away it moves freely.
        let out = mv(&mut e, "A", [10.0, 0.0, 0.0]);
        assert!(
            out["contact"]["distance_mm"].as_f64().unwrap().abs() < 1e-6,
            "{out}"
        );
        let out = mv(&mut e, "A", [-50.0, 0.0, 0.0]);
        assert!(out.get("contact").is_none(), "{out}");
        assert!((origin_of(&e, "A").x - 150.0).abs() < 1e-6);
        // Undo is one step per move.
        e.undo().unwrap();
        assert!((origin_of(&e, "A").x - 200.0).abs() < 1e-6);
        // Copies do not stop.
        let err = e
            .execute_erased(
                "move",
                json!({ "targets": "group & name:A", "delta": [1, 0, 0], "copy": true, "stop_at_contact": true }),
            )
            .unwrap_err();
        assert!(err.to_string().contains("copies"), "{err}");
    }

    #[test]
    fn resting_objects_slide_but_do_not_sink() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        block(
            &mut e,
            "Floor",
            [-1000.0, -1000.0, -10.0],
            [1000.0, 1000.0, 0.0],
        );
        block(&mut e, "Box", [0.0, 0.0, 0.0], [100.0, 100.0, 100.0]);
        let out = mv(&mut e, "Box", [0.0, 300.0, 0.0]);
        assert!(out.get("contact").is_none(), "{out}");
        let out = mv(&mut e, "Box", [0.0, 0.0, -50.0]);
        assert!(
            out["contact"]["fraction"].as_f64().unwrap().abs() < 1e-9,
            "{out}"
        );
        assert!(origin_of(&e, "Box").z.abs() < 1e-9);
        // Lifting it off is free.
        let out = mv(&mut e, "Box", [0.0, 0.0, 40.0]);
        assert!(out.get("contact").is_none(), "{out}");
    }

    #[test]
    fn crossed_ridges_touch_edge_to_edge() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        block(&mut e, "Low", [0.0, 0.0, 0.0], [100.0, 100.0, 100.0]);
        block(&mut e, "High", [0.0, 0.0, 250.0], [100.0, 100.0, 350.0]);
        let quarter = std::f64::consts::FRAC_PI_4;
        // A ridge along red at the top of Low, one along green at the
        // bottom of High.
        e.execute_erased(
            "rotate",
            json!({ "targets": "group & name:Low", "center": [50, 50, 50], "axis": [1, 0, 0], "angle": quarter }),
        )
        .unwrap();
        e.execute_erased(
            "rotate",
            json!({ "targets": "group & name:High", "center": [50, 50, 300], "axis": [0, 1, 0], "angle": quarter }),
        )
        .unwrap();
        let out = mv(&mut e, "High", [0.0, 0.0, -300.0]);
        let gap = (300.0 - 50.0 * 2f64.sqrt()) - (50.0 + 50.0 * 2f64.sqrt());
        let got = out["contact"]["distance_mm"].as_f64().unwrap();
        assert!((got - gap).abs() < 1e-6, "{got} vs {gap}: {out}");
        // Now touching: measured interference is none.
        let check = e.execute_erased("interference-check", json!({})).unwrap();
        assert_eq!(check["interferences"], json!([]), "{check}");
    }
}
