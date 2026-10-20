//! Test fixtures for docs/03-geometry-kernel.md §6: tiny indexed meshes
//! (a cube, a square face, wire edges) turned into a [`HalfEdgeMesh`] through
//! its serde form, because the kernel's constructive operations
//! (`add_edge`, `from_indexed`, docs/03 §4) belong to the `mesh` owner and are
//! not implemented yet. Ids are predictable: local index `i` becomes slot
//! `i + 1` with generation `1` (slot `0` is the slot-map sentinel).
//!
//! Owner key: `spatial` (test-only).

use glam::DVec3;
use serde_json::{Value, json};
use slotmap::KeyData;

use crate::geom::{EdgeFlags, HalfEdgeMesh};
use crate::ids::{EdgeId, FaceId, VertexId};

/// Slot-map key for local index `i` (slot `i + 1`, generation `1`).
fn key(i: usize) -> Value {
    json!({ "idx": i + 1, "version": 1 })
}

/// Key data for local index `i`, as the mesh will report it.
fn key_data(i: usize) -> KeyData {
    KeyData::from_ffi((1u64 << 32) | (i as u64 + 1))
}

/// Vertex id of local vertex `i`.
pub(super) fn vid(i: usize) -> VertexId {
    key_data(i).into()
}

/// Edge id of local edge `i`.
pub(super) fn eid(i: usize) -> EdgeId {
    key_data(i).into()
}

/// Face id of local face `i`.
pub(super) fn fid(i: usize) -> FaceId {
    key_data(i).into()
}

/// An indexed mesh description (docs/09 §2 flavor): faces are vertex loops,
/// counter-clockwise seen from the front; every loop edge must exist in `edges`.
#[derive(Debug, Clone, Default)]
pub(super) struct Fixture {
    pub vertices: Vec<DVec3>,
    pub edges: Vec<(usize, usize)>,
    pub faces: Vec<Vec<usize>>,
    /// Local edge indices that carry `EdgeFlags::HIDDEN`.
    pub hidden_edges: Vec<usize>,
}

/// Serializes a `DVec3` the way `glam` does (a 3-tuple).
fn vec3(p: DVec3) -> Value {
    json!([p.x, p.y, p.z])
}

/// A slot map / secondary map body: the vacant sentinel plus occupied slots.
fn slots(values: impl IntoIterator<Item = Value>) -> Value {
    let mut out = vec![json!({ "value": null, "version": 0 })];
    out.extend(
        values
            .into_iter()
            .map(|v| json!({ "value": v, "version": 1 })),
    );
    Value::Array(out)
}

impl Fixture {
    /// Builds the half-edge mesh. Panics on an inconsistent description
    /// (test-only code).
    pub fn mesh(&self) -> HalfEdgeMesh {
        let n_edges = self.edges.len();
        // Half-edge `2k` runs a→b of edge k, `2k + 1` runs b→a.
        let mut he_face: Vec<Option<usize>> = vec![None; 2 * n_edges];
        let mut he_next: Vec<usize> = (0..2 * n_edges).map(|h| h ^ 1).collect();
        let mut he_prev: Vec<usize> = he_next.clone();
        let mut loop_start: Vec<usize> = Vec::with_capacity(self.faces.len());

        for (f, loop_vertices) in self.faces.iter().enumerate() {
            let n = loop_vertices.len();
            assert!(n >= 3, "face {f} needs at least three vertices");
            let mut half_edges = Vec::with_capacity(n);
            for i in 0..n {
                let (a, b) = (loop_vertices[i], loop_vertices[(i + 1) % n]);
                let he = self
                    .edges
                    .iter()
                    .enumerate()
                    .find_map(|(k, &(ea, eb))| {
                        if (ea, eb) == (a, b) {
                            Some(2 * k)
                        } else if (ea, eb) == (b, a) {
                            Some(2 * k + 1)
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| panic!("face {f}: no edge between {a} and {b}"));
                assert!(he_face[he].is_none(), "half-edge {he} used by two faces");
                he_face[he] = Some(f);
                half_edges.push(he);
            }
            for i in 0..n {
                let cur = half_edges[i];
                he_next[cur] = half_edges[(i + 1) % n];
                he_prev[cur] = half_edges[(i + n - 1) % n];
            }
            loop_start.push(half_edges[0]);
        }

        let vertices = slots(
            self.vertices
                .iter()
                .map(|p| json!({ "position": vec3(*p) })),
        );
        let half_edges = slots((0..2 * n_edges).map(|h| {
            let k = h / 2;
            let (a, b) = self.edges[k];
            let origin = if h % 2 == 0 { a } else { b };
            json!({
                "origin": key(origin),
                "twin": key(h ^ 1),
                "next": key(he_next[h]),
                "prev": key(he_prev[h]),
                "edge": key(k),
                "face": he_face[h].map(key),
            })
        }));
        let edges = slots((0..n_edges).map(|k| {
            let flags = if self.hidden_edges.contains(&k) {
                EdgeFlags::HIDDEN.bits()
            } else {
                0
            };
            json!({ "he": key(2 * k), "flags": flags, "curve": null })
        }));
        let faces = slots(self.faces.iter().enumerate().map(|(f, loop_vertices)| {
            let plane = newell_plane(loop_vertices.iter().map(|&i| self.vertices[i]));
            json!({
                "outer": key(f),
                "holes": [],
                "plane": { "normal": vec3(plane.0), "d": plane.1 },
                "sides": {
                    "material_front": null,
                    "material_back": null,
                    "uv_front": null,
                    "uv_back": null,
                },
            })
        }));
        let loops = slots(
            loop_start
                .iter()
                .enumerate()
                .map(|(f, &start)| json!({ "start": key(start), "face": key(f) })),
        );
        let curves = slots(std::iter::empty());
        let vertex_edges = slots((0..self.vertices.len()).map(|v| {
            let star: Vec<Value> = (0..2 * n_edges)
                .filter(|&h| {
                    let (a, b) = self.edges[h / 2];
                    (if h % 2 == 0 { a } else { b }) == v
                })
                .map(key)
                .collect();
            Value::Array(star)
        }));

        let mesh = json!({
            "vertices": vertices,
            "half_edges": half_edges,
            "edges": edges,
            "faces": faces,
            "loops": loops,
            "curves": curves,
            "vertex_edges": vertex_edges,
        });
        serde_json::from_value(mesh).expect("fixture mesh deserializes")
    }
}

/// Newell normal and plane offset of a loop (front = counter-clockwise side).
fn newell_plane(points: impl IntoIterator<Item = DVec3>) -> (DVec3, f64) {
    let pts: Vec<DVec3> = points.into_iter().collect();
    let mut n = DVec3::ZERO;
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        n += a.cross(b);
    }
    let n = n.normalize();
    (n, -n.dot(pts[0]))
}

/// Axis-aligned cube from the origin to `(size, size, size)`.
///
/// Vertices: 0 `(0,0,0)`, 1 `(s,0,0)`, 2 `(s,s,0)`, 3 `(0,s,0)`, 4..7 the same
/// at `z = s`. Edges 0–3 bottom ring, 4–7 top ring, 8–11 verticals `i`–`i+4`.
/// Faces: 0 bottom (−Z), 1 top (+Z), 2 front (−Y), 3 right (+X), 4 back (+Y),
/// 5 left (−X).
pub(super) fn cube(size: f64) -> Fixture {
    let s = size;
    Fixture {
        vertices: vec![
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(s, 0.0, 0.0),
            DVec3::new(s, s, 0.0),
            DVec3::new(0.0, s, 0.0),
            DVec3::new(0.0, 0.0, s),
            DVec3::new(s, 0.0, s),
            DVec3::new(s, s, s),
            DVec3::new(0.0, s, s),
        ],
        edges: vec![
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ],
        faces: vec![
            vec![0, 3, 2, 1],
            vec![4, 5, 6, 7],
            vec![0, 1, 5, 4],
            vec![1, 2, 6, 5],
            vec![2, 3, 7, 6],
            vec![3, 0, 4, 7],
        ],
        hidden_edges: Vec::new(),
    }
}

/// A single square face on the ground plane facing +Z: vertices `(0,0,0)`,
/// `(s,0,0)`, `(s,s,0)`, `(0,s,0)`; edges `i`–`i+1`; face 0.
pub(super) fn square(size: f64) -> Fixture {
    let s = size;
    Fixture {
        vertices: vec![
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(s, 0.0, 0.0),
            DVec3::new(s, s, 0.0),
            DVec3::new(0.0, s, 0.0),
        ],
        edges: vec![(0, 1), (1, 2), (2, 3), (3, 0)],
        faces: vec![vec![0, 1, 2, 3]],
        hidden_edges: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_fixture_deserializes_with_predictable_ids() {
        let mesh = cube(10.0).mesh();
        assert_eq!(mesh.vertex_count(), 8);
        assert_eq!(mesh.edge_count(), 12);
        assert_eq!(mesh.face_count(), 6);
        assert_eq!(
            mesh.vertex(vid(6)).unwrap().position,
            DVec3::new(10.0, 10.0, 10.0)
        );
        assert_eq!(mesh.edge_endpoints(eid(8)).unwrap(), (vid(0), vid(4)));
        let top = mesh.face(fid(1)).unwrap();
        assert!((top.plane.normal - DVec3::Z).length() < 1e-12);
        assert!((top.plane.d + 10.0).abs() < 1e-12);
        let front = mesh.face(fid(2)).unwrap();
        assert!((front.plane.normal + DVec3::Y).length() < 1e-12);
        assert_eq!(mesh.vertex_star(vid(0)).len(), 3);
        assert!(!mesh.aabb().is_empty());
    }

    #[test]
    fn square_fixture_has_open_boundary() {
        let mesh = square(5.0).mesh();
        assert_eq!(mesh.face_count(), 1);
        let edge = mesh.edge(eid(0)).unwrap();
        let he = mesh.half_edge(edge.he).unwrap();
        let twin = mesh.half_edge(he.twin).unwrap();
        assert_eq!(he.face, Some(fid(0)));
        assert_eq!(twin.face, None);
    }
}
