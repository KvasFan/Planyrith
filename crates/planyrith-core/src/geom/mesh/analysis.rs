//! Implements docs/03-geometry-kernel.md §3.1 (invariant validation) and §4.13
//! (solid classification and volume) for the half-edge mesh.
//!
//! `validate` checks, in order: structural consistency (reported as
//! invariant 0), twin pairs (1), face planarity/orientation/area (2), vertex
//! separation (3) and edge–edge intersections (4). Invariant 5 (at most two
//! faces per edge) holds by construction: a half-edge has one `face` slot.
//! The "edges never pass through a face interior" half of invariant 4 is not
//! checked: wire edges lying on a face are allowed (docs/03 Decisions).

use std::collections::{BTreeMap, BTreeSet};

use slotmap::Key;

use super::{HalfEdgeMesh, MeshViolation};
use crate::geom::GeometryError;
use crate::ids::{EdgeId, FaceId};
use crate::math::intersect::{SegmentContact, segment_segment};
use crate::math::polygon;
use crate::tolerance::Tolerances;

impl HalfEdgeMesh {
    /// All invariant checks; `intersections` adds the O(E log E + k) edge
    /// crossing sweep of invariant 4 (skipped by the post-operation check in
    /// test builds because Move legitimately skips it, docs/03 §4.8 step 3).
    pub(super) fn validate_inner(
        &self,
        tol: &Tolerances,
        intersections: bool,
    ) -> Vec<MeshViolation> {
        let mut out = Vec::new();
        self.check_structure(&mut out);
        if out.is_empty() {
            self.check_faces(tol, &mut out);
            self.check_vertex_separation(tol, &mut out);
            if intersections {
                self.check_edge_crossings(tol, &mut out);
            }
        }
        out
    }

    fn check_structure(&self, out: &mut Vec<MeshViolation>) {
        let mut push =
            |invariant: u8, detail: String| out.push(MeshViolation { invariant, detail });

        for (v, _) in &self.vertices {
            if !self.vertex_edges.contains_key(v) {
                push(0, format!("{v} has no star entry"));
            }
        }
        for (v, star) in &self.vertex_edges {
            if !self.vertices.contains_key(v) {
                push(0, format!("star entry for missing {v}"));
            }
            for &h in star {
                match self.half_edges.get(h) {
                    Some(he) if he.origin == v => {}
                    _ => push(
                        0,
                        format!("star of {v} lists {h}, which does not start there"),
                    ),
                }
            }
        }

        let mut face_half_edges: BTreeMap<FaceId, usize> = BTreeMap::new();
        for (h, he) in &self.half_edges {
            let Some(twin) = self.half_edges.get(he.twin) else {
                push(1, format!("{h} has no twin"));
                continue;
            };
            if twin.twin != h || he.twin == h || twin.edge != he.edge {
                push(1, format!("{h} and {} are not twins", he.twin));
            }
            match self.edges.get(he.edge) {
                Some(edge) if edge.he == h || edge.he == he.twin => {}
                _ => push(1, format!("{h} does not belong to {}", he.edge)),
            }
            if !self.vertex_star(he.origin).contains(&h) {
                push(0, format!("{h} is missing from the star of {}", he.origin));
            }
            if let Some(f) = he.face {
                *face_half_edges.entry(f).or_default() += 1;
                if !self.faces.contains_key(f) {
                    push(0, format!("{h} borders missing {f}"));
                    continue;
                }
                let (Some(next), Some(prev)) =
                    (self.half_edges.get(he.next), self.half_edges.get(he.prev))
                else {
                    push(0, format!("{h} has a dangling next/prev"));
                    continue;
                };
                if next.prev != h || prev.next != h {
                    push(0, format!("{h}: next/prev are not inverse"));
                }
                if next.origin != twin.origin {
                    push(0, format!("{h}: next does not start at its end vertex"));
                }
                if next.face != he.face {
                    push(0, format!("{h}: next lies in another face"));
                }
            }
        }

        for (e, edge) in &self.edges {
            match self.half_edges.get(edge.he) {
                Some(he) if he.edge == e => {}
                _ => push(1, format!("{e} has no half-edge of its own")),
            }
            if let Some(c) = edge.curve {
                let listed = self
                    .curve_members
                    .get(c)
                    .is_some_and(|members| members.contains(&e));
                if !self.curves.contains_key(c) || !listed {
                    push(0, format!("{e} names {c}, which does not list it"));
                }
            }
        }
        for (c, _) in &self.curves {
            for &e in self.curve_members.get(c).map(Vec::as_slice).unwrap_or(&[]) {
                if self.edges.get(e).and_then(|edge| edge.curve) != Some(c) {
                    push(0, format!("{c} lists {e}, which is not a member"));
                }
            }
        }

        let mut loop_half_edges: BTreeMap<FaceId, usize> = BTreeMap::new();
        for (f, face) in &self.faces {
            if face.outer.is_null() {
                push(0, format!("{f} is detached"));
                continue;
            }
            for &l in std::iter::once(&face.outer).chain(face.holes.iter()) {
                match self.loops.get(l) {
                    Some(lp) if lp.face == f => {}
                    _ => {
                        push(0, format!("{f} lists {l}, which does not belong to it"));
                        continue;
                    }
                }
                match self.loop_half_edges(l) {
                    Ok(cycle) => {
                        *loop_half_edges.entry(f).or_default() += cycle.len();
                        if cycle
                            .iter()
                            .any(|&h| self.half_edges.get(h).and_then(|he| he.face) != Some(f))
                        {
                            push(0, format!("{l} contains a half-edge of another face"));
                        }
                    }
                    Err(err) => push(0, format!("{l}: {err}")),
                }
            }
        }
        if face_half_edges != loop_half_edges {
            push(
                0,
                "some half-edges claim a face without being in its loops".into(),
            );
        }
        for (l, lp) in &self.loops {
            let listed = self
                .faces
                .get(lp.face)
                .is_some_and(|face| face.outer == l || face.holes.contains(&l));
            if !listed {
                push(0, format!("{l} is not listed by {}", lp.face));
            }
        }
    }

    fn check_faces(&self, tol: &Tolerances, out: &mut Vec<MeshViolation>) {
        for (f, face) in &self.faces {
            let frame = face.plane.frame();
            let mut area = 0.0;
            for (i, &l) in std::iter::once(&face.outer)
                .chain(face.holes.iter())
                .enumerate()
            {
                let Ok(points) = self.loop_positions(l) else {
                    continue;
                };
                if !tol.points_on_plane(&face.plane, &points) {
                    out.push(MeshViolation {
                        invariant: 2,
                        detail: format!("{l} of {f} is not within planarity of the face plane"),
                    });
                }
                let signed = polygon::signed_area(&frame.project_all(&points));
                let want_ccw = i == 0;
                if (signed > 0.0) != want_ccw {
                    out.push(MeshViolation {
                        invariant: 2,
                        detail: format!(
                            "{l} of {f} is {} seen from the front",
                            if want_ccw {
                                "clockwise"
                            } else {
                                "counter-clockwise"
                            }
                        ),
                    });
                }
                area += if want_ccw {
                    signed.abs()
                } else {
                    -signed.abs()
                };
            }
            if area <= tol.length_sq {
                out.push(MeshViolation {
                    invariant: 2,
                    detail: format!("{f} has zero area"),
                });
            }
            // Holes lie inside the outer loop (invariant 2).
            if let Ok(outer) = self.loop_positions(face.outer) {
                let outer = frame.project_all(&outer);
                for &l in &face.holes {
                    let Ok(points) = self.loop_positions(l) else {
                        continue;
                    };
                    if points.iter().any(|&p| {
                        polygon::point_in_ring(frame.to_2d(p), &outer, tol)
                            == polygon::PointInPolygon::Outside
                    }) {
                        out.push(MeshViolation {
                            invariant: 2,
                            detail: format!("hole {l} of {f} lies outside its outer loop"),
                        });
                    }
                }
            }
        }
    }

    fn check_vertex_separation(&self, tol: &Tolerances, out: &mut Vec<MeshViolation>) {
        let mut points: Vec<_> = self.vertices.iter().map(|(v, x)| (x.position, v)).collect();
        points.sort_by(|a, b| a.0.x.total_cmp(&b.0.x).then(a.1.cmp(&b.1)));
        for i in 0..points.len() {
            for j in i + 1..points.len() {
                if points[j].0.x - points[i].0.x > tol.length {
                    break;
                }
                if tol.same_point(points[i].0, points[j].0) {
                    out.push(MeshViolation {
                        invariant: 3,
                        detail: format!("{} and {} are within tolerance", points[i].1, points[j].1),
                    });
                }
            }
        }
    }

    fn check_edge_crossings(&self, tol: &Tolerances, out: &mut Vec<MeshViolation>) {
        let mut items: Vec<(f64, f64, EdgeId)> = Vec::with_capacity(self.edges.len());
        for (e, _) in &self.edges {
            if let Ok(seg) = self.edge_segment(e) {
                let bb = seg.aabb();
                items.push((bb.min.x, bb.max.x, e));
            }
        }
        items.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.2.cmp(&b.2)));
        for i in 0..items.len() {
            let (_, max_x, e) = items[i];
            let (Ok(sa), Ok((a0, a1))) = (self.edge_segment(e), self.edge_endpoints(e)) else {
                continue;
            };
            for item in &items[i + 1..] {
                if item.0 > max_x + tol.length {
                    break;
                }
                let g = item.2;
                let (Ok(sb), Ok((b0, b1))) = (self.edge_segment(g), self.edge_endpoints(g)) else {
                    continue;
                };
                if !sa.aabb().expanded(tol.length).intersects(&sb.aabb()) {
                    continue;
                }
                let shared: Vec<_> = [a0, a1]
                    .into_iter()
                    .filter(|v| *v == b0 || *v == b1)
                    .collect();
                let bad = match segment_segment(&sa, &sb, tol) {
                    SegmentContact::None => false,
                    SegmentContact::Overlap { .. } => true,
                    SegmentContact::Point { point, .. } => !shared
                        .iter()
                        .any(|&v| self.position(v).is_ok_and(|p| tol.same_point(p, point))),
                };
                if bad {
                    out.push(MeshViolation {
                        invariant: 4,
                        detail: format!("{e} and {g} intersect away from a shared vertex"),
                    });
                }
            }
        }
    }

    /// docs/03 §4.13: closed, every edge between two distinct faces, every
    /// vertex a single fan of faces, no isolated vertices.
    pub(super) fn is_solid_inner(&self) -> bool {
        if self.faces.is_empty() {
            return false;
        }
        for (_, he) in &self.half_edges {
            let Some(f) = he.face else {
                return false;
            };
            if self.half_edges.get(he.twin).and_then(|t| t.face) == Some(f) {
                return false;
            }
        }
        for (v, _) in &self.vertices {
            let star = self.vertex_star(v);
            let Some(&first) = star.first() else {
                return false;
            };
            // Rotate around v: the outgoing half-edge after h is twin(prev(h)).
            let mut seen = BTreeSet::new();
            let mut h = first;
            loop {
                if !seen.insert(h) || seen.len() > star.len() {
                    return false;
                }
                let Some(prev) = self.half_edges.get(h).map(|x| x.prev) else {
                    return false;
                };
                let Some(next) = self.half_edges.get(prev).map(|x| x.twin) else {
                    return false;
                };
                h = next;
                if h == first {
                    break;
                }
            }
            if seen.len() != star.len() {
                return false;
            }
        }
        true
    }

    /// Divergence theorem over planar faces, relative to the bounding-box
    /// center to limit cancellation: `V = Σ (n · (p − c)) A / 3`.
    pub(super) fn volume_inner(&self) -> Result<f64, GeometryError> {
        if !self.is_solid_inner() {
            return Err(GeometryError::NotApplicable(
                "volume needs a closed solid".into(),
            ));
        }
        let c = self.aabb().center();
        let mut volume = 0.0;
        for (f, face) in &self.faces {
            let height = face.plane.signed_distance(c);
            volume += -height * self.face_area(f)? / 3.0;
        }
        Ok(volume)
    }
}
