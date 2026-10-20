//! Implements docs/03-geometry-kernel.md §4.2 (add edge), §4.4 (split by
//! chord), §4.6 (erase edge, merge), §4.7 (erase face) and §4.8 (transform
//! vertices with merging and auto-fold). The public wrappers in `mesh.rs` run
//! these inside a transaction (apply-or-rollback).

use std::collections::{BTreeMap, BTreeSet};

use glam::{DAffine3, DVec2, DVec3};

use super::{
    AddEdgeResult, EdgeFlags, EraseEdgeResult, EraseFaceResult, HalfEdgeMesh, TransformOptions,
    TransformResult, broken,
};
use crate::geom::planar;
use crate::geom::tessellate::tessellate_polygon_2d;
use crate::geom::{DeltaSink, FaceSides, GeometryError, MeshDelta};
use crate::ids::{EdgeId, FaceId, HalfEdgeId, VertexId};
use crate::math::intersect::{SegmentContact, segment_segment};
use crate::math::{Plane, Segment, polygon};
use crate::tolerance::Tolerances;

/// Flags of the hidden fold edges inserted by auto-fold (docs/03 §4.8 step 2).
const FOLD_FLAGS: EdgeFlags =
    EdgeFlags(EdgeFlags::SOFT.0 | EdgeFlags::SMOOTH.0 | EdgeFlags::HIDDEN.0);

/// A face detached while its vertices move: loops as vertex sequences.
struct DetachedFace {
    f: FaceId,
    cycles: Vec<Vec<VertexId>>,
    plane: Plane,
    sides: FaceSides,
}

impl HalfEdgeMesh {
    // ----- §4.2 add edge ----------------------------------------------------------

    pub(super) fn add_edge_inner(
        &mut self,
        a: DVec3,
        b: DVec3,
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
    ) -> Result<AddEdgeResult, GeometryError> {
        if !tol.in_range(a) || !tol.in_range(b) {
            return Err(GeometryError::OutOfRange);
        }
        // Step 1: snap endpoints to existing vertices.
        let pa = match self.find_vertex_near(a, tol) {
            Some(v) => self.position(v)?,
            None => a,
        };
        let pb = match self.find_vertex_near(b, tol) {
            Some(v) => self.position(v)?,
            None => b,
        };
        if tol.same_point(pa, pb) {
            return Err(GeometryError::Degenerate("edge endpoints coincide".into()));
        }
        let seg = Segment::new(pa, pb);
        let bbox = seg.aabb().expanded(tol.length);
        let mut result = AddEdgeResult::default();
        let mut touched: BTreeSet<VertexId> = BTreeSet::new();

        // Step 2a: split existing edges the new segment meets in their interior.
        let mut splits: BTreeMap<EdgeId, Vec<DVec3>> = BTreeMap::new();
        for e in self.edges_near(&bbox)? {
            let es = self.edge_segment(e)?;
            match segment_segment(&seg, &es, tol) {
                SegmentContact::None => {}
                SegmentContact::Point { point, .. } => {
                    let q = if tol.same_point(point, pa) {
                        pa
                    } else if tol.same_point(point, pb) {
                        pb
                    } else {
                        point
                    };
                    if tol.point_inside_segment(q, &es) {
                        splits.entry(e).or_default().push(q);
                    }
                }
                SegmentContact::Overlap { .. } => {
                    for q in [pa, pb] {
                        if tol.point_inside_segment(q, &es) {
                            splits.entry(e).or_default().push(q);
                        }
                    }
                }
            }
        }
        for (e, mut points) in splits {
            let es = self.edge_segment(e)?;
            points.sort_by(|x, y| es.closest_param(*x).total_cmp(&es.closest_param(*y)));
            points.dedup_by(|x, y| tol.same_point(*x, *y));
            let mut current = e;
            for q in points {
                if !tol.point_inside_segment(q, &self.edge_segment(current)?) {
                    continue;
                }
                let (m, parts) = self.split_edge_at(current, q, tol, d)?;
                touched.insert(m);
                result.split_edges.push((current, parts));
                current = parts[1];
            }
        }

        // Step 2b: the chain of vertices along [a, b].
        let va = self.find_or_add_vertex(pa, tol, d)?;
        let vb = self.find_or_add_vertex(pb, tol, d)?;
        let mut chain: Vec<(f64, VertexId)> = Vec::new();
        for (v, vert) in self.vertices() {
            if v == va || v == vb {
                continue;
            }
            if bbox.contains_point(vert.position) && seg.contains_point(vert.position, tol) {
                chain.push((seg.closest_param(vert.position), v));
            }
        }
        chain.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        let mut vertices = vec![va];
        vertices.extend(chain.into_iter().map(|(_, v)| v));
        vertices.push(vb);

        for w in vertices.windows(2) {
            let (x, y) = (w[0], w[1]);
            touched.insert(x);
            touched.insert(y);
            let e = match self.edge_between(x, y) {
                Some(e) => e,
                None => self.new_edge(x, y, EdgeFlags::NONE, d)?,
            };
            result.edges.push(e);
        }

        // Steps 3–4: face finding. Every chain edge is a seed, so drawing over
        // an existing edge re-creates a face the user erased there.
        let touched: Vec<VertexId> = touched.into_iter().collect();
        let found = self.find_faces_seeded(&touched, &result.edges, tol, d)?;
        result.new_faces = found.new_faces;
        result.split_faces = found.split_faces;
        Ok(result)
    }

    // ----- §4.6 erase edge / merge --------------------------------------------------

    pub(super) fn erase_edge_inner(
        &mut self,
        e: EdgeId,
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
    ) -> Result<EraseEdgeResult, GeometryError> {
        let (a, b) = self.edge_endpoints(e)?;
        let [f1, f2] = self.edge_faces(e)?;
        let mut result = EraseEdgeResult::default();
        let mut edge_gone = false;
        match (f1, f2) {
            (Some(x), Some(y))
                if x != y
                    && tol.is_coplanar(&self.face_ref(x)?.plane, &self.face_ref(y)?.plane) =>
            {
                // The larger face survives (ties: the face left of `edge.he`),
                // so healing a drawn opening keeps the wall's id and material
                // (docs/03 Decisions).
                let (kept, removed) = if self.face_area(y)? > self.face_area(x)? {
                    (y, x)
                } else {
                    (x, y)
                };
                self.merge_inner(kept, removed, &[e], tol, d)?;
                result.faces_merged = Some((kept, removed));
                edge_gone = true;
            }
            (Some(x), Some(y)) if x != y => {
                let (first, second) = if x < y { (x, y) } else { (y, x) };
                self.remove_face(first, d)?;
                self.remove_face(second, d)?;
                result.faces_removed = vec![first, second];
            }
            (Some(x), _) | (None, Some(x)) => {
                self.remove_face(x, d)?;
                result.faces_removed.push(x);
            }
            (None, None) => {}
        }
        if !edge_gone {
            self.delete_edge(e, d)?;
        }
        for v in [a, b] {
            if self.delete_if_isolated(v, d)? {
                result.vertices_removed.push(v);
            }
        }
        Ok(result)
    }

    pub(super) fn merge_faces_inner(
        &mut self,
        kept: FaceId,
        removed: FaceId,
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
    ) -> Result<FaceId, GeometryError> {
        if kept == removed {
            return Err(GeometryError::NotApplicable(
                "cannot merge a face with itself".into(),
            ));
        }
        let (pk, pr) = (self.face_ref(kept)?.plane, self.face_ref(removed)?.plane);
        if !tol.is_coplanar_same_side(&pk, &pr) {
            return Err(GeometryError::NotApplicable(
                "only coplanar faces facing the same way can be merged".into(),
            ));
        }
        let kept_edges: BTreeSet<EdgeId> = self.face_edges(kept)?.into_iter().collect();
        let shared: Vec<EdgeId> = self
            .face_edges(removed)?
            .into_iter()
            .filter(|e| kept_edges.contains(e))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if shared.is_empty() {
            return Err(GeometryError::NotApplicable(
                "the faces share no edge".into(),
            ));
        }
        let mut ends = Vec::new();
        for &e in &shared {
            let (a, b) = self.edge_endpoints(e)?;
            ends.extend([a, b]);
        }
        self.merge_inner(kept, removed, &shared, tol, d)?;
        for v in ends {
            self.delete_if_isolated(v, d)?;
        }
        Ok(kept)
    }

    /// Merges `removed` into `kept` (same plane), deleting `remove_edges`
    /// (which must lie between them). `kept` keeps its id, plane and sides;
    /// other edges they shared stay as wire edges lying on the merged face.
    fn merge_inner(
        &mut self,
        kept: FaceId,
        removed: FaceId,
        remove_edges: &[EdgeId],
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
    ) -> Result<(), GeometryError> {
        let face = self.face_ref(kept)?;
        let (plane, sides) = (face.plane, face.sides.clone());
        let frame = plane.frame();
        let mut graph: BTreeSet<EdgeId> = self.face_edges(kept)?.into_iter().collect();
        graph.extend(self.face_edges(removed)?);
        for e in remove_edges {
            graph.remove(e);
        }
        let old = [
            planar::face_polygon(self, kept, &frame)?,
            planar::face_polygon(self, removed, &frame)?,
        ];

        self.detach_face(kept, d)?;
        self.detach_face(removed, d)?;
        for &e in remove_edges {
            self.delete_edge(e, d)?;
        }

        let regions = planar::arrangement_regions(self, &frame, &graph, tol)?;
        let mut chosen = Vec::new();
        for (i, region) in regions.iter().enumerate() {
            let Some(sample) = planar::region_sample(self, &frame, region, tol)? else {
                continue;
            };
            if old
                .iter()
                .any(|(o, h)| planar::strictly_inside(sample, o, h, tol))
            {
                chosen.push(i);
            }
        }
        chosen.sort_by(|&x, &y| regions[y].area.total_cmp(&regions[x].area).then(x.cmp(&y)));
        let Some((&first, rest)) = chosen.split_first() else {
            return Err(broken("merging produced no region"));
        };
        let (outer, holes) = planar::orient(self, &regions[first], false)?;
        let piece = planar::piece_plane(self, &plane, &outer, &holes, tol)?;
        self.attach_face(kept, &outer, &holes, piece, sides.clone(), tol, d)?;
        for &i in rest {
            let (outer, holes) = planar::orient(self, &regions[i], false)?;
            let piece = planar::piece_plane(self, &plane, &outer, &holes, tol)?;
            self.create_face(&outer, &holes, piece, sides.clone(), tol, d)?;
        }
        self.discard_detached_face(removed)?;
        d.record(MeshDelta::FacesMerged { kept, removed });
        Ok(())
    }

    // ----- §4.4 split by chord -------------------------------------------------------

    pub(super) fn split_face_by_chord_inner(
        &mut self,
        f: FaceId,
        chord: &[EdgeId],
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
    ) -> Result<[FaceId; 2], GeometryError> {
        let plane = self.face_ref(f)?.plane;
        if chord.is_empty() {
            return Err(GeometryError::NotApplicable("empty chord".into()));
        }
        let mut edges: BTreeSet<EdgeId> = self.face_edges(f)?.into_iter().collect();
        for &e in chord {
            if self.edge_faces(e)? != [None, None] {
                return Err(GeometryError::NotApplicable(format!(
                    "{e} already borders a face"
                )));
            }
            let s = self.edge_segment(e)?;
            if !tol.point_on_plane(&plane, s.a) || !tol.point_on_plane(&plane, s.b) {
                return Err(GeometryError::NotApplicable(format!(
                    "{e} does not lie in the face plane"
                )));
            }
            edges.insert(e);
        }
        let faces = BTreeSet::from([f]);
        let found = planar::rebuild_faces(self, plane, &edges, &faces, &BTreeSet::new(), tol, d)?;
        match found.split_faces.as_slice() {
            [(_, a, b)] if found.holes.is_empty() => Ok([*a, *b]),
            _ => Err(GeometryError::NotApplicable(
                "the chord does not split the face into two".into(),
            )),
        }
    }

    // ----- §4.7 erase face -------------------------------------------------------------

    pub(super) fn erase_face_inner(
        &mut self,
        f: FaceId,
        erase_orphans: bool,
        d: &mut dyn DeltaSink,
    ) -> Result<EraseFaceResult, GeometryError> {
        let edges: BTreeSet<EdgeId> = self.face_edges(f)?.into_iter().collect();
        self.remove_face(f, d)?;
        let mut result = EraseFaceResult::default();
        if erase_orphans {
            for e in edges {
                if self.edge(e).is_none() || self.edge_faces(e)? != [None, None] {
                    continue;
                }
                let (a, b) = self.edge_endpoints(e)?;
                self.delete_edge(e, d)?;
                result.edges_removed.push(e);
                self.delete_if_isolated(a, d)?;
                self.delete_if_isolated(b, d)?;
            }
        }
        Ok(result)
    }

    // ----- §4.8 transform vertices --------------------------------------------------

    pub(super) fn transform_vertices_inner(
        &mut self,
        set: &[VertexId],
        t: &DAffine3,
        opts: TransformOptions,
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
    ) -> Result<TransformResult, GeometryError> {
        let moved: BTreeSet<VertexId> = set.iter().copied().collect();
        let mut target: BTreeMap<VertexId, DVec3> = BTreeMap::new();
        for &v in &moved {
            let p = t.transform_point3(self.position(v)?);
            if !tol.in_range(p) {
                return Err(GeometryError::OutOfRange);
            }
            target.insert(v, p);
        }
        let mut result = TransformResult::default();

        // Step 1: merges, onto vertices that stay first, then among the moved
        // ones (the lowest id of a coinciding group survives).
        let mut fixed: Vec<(DVec3, VertexId)> = self
            .vertices()
            .filter(|(v, _)| !moved.contains(v))
            .map(|(v, x)| (x.position, v))
            .collect();
        fixed.sort_by(|a, b| a.0.x.total_cmp(&b.0.x).then(a.1.cmp(&b.1)));
        let mut merge_into: BTreeMap<VertexId, VertexId> = BTreeMap::new();
        let mut kept: Vec<(DVec3, VertexId)> = Vec::new();
        for (&v, &p) in &target {
            if let Some(w) = nearest_sorted(&fixed, p, tol) {
                merge_into.insert(v, w);
            } else if let Some(&(_, w)) = kept.iter().find(|(q, _)| tol.same_point(*q, p)) {
                merge_into.insert(v, w);
            } else {
                kept.push((p, v));
            }
        }

        // Detach every face with a moving vertex; it is re-attached afterwards.
        let mut affected: BTreeSet<FaceId> = BTreeSet::new();
        for &v in &moved {
            for &h in self.vertex_star(v) {
                if let Some(f) = self.he(h)?.face {
                    affected.insert(f);
                }
            }
        }
        let mut detached = Vec::with_capacity(affected.len());
        for f in affected {
            let cycles = self
                .face_cycles(f)?
                .into_iter()
                .map(|c| {
                    c.into_iter()
                        .map(|h| self.he(h).map(|x| x.origin))
                        .collect()
                })
                .collect::<Result<Vec<Vec<VertexId>>, _>>()?;
            let face = self.face_ref(f)?;
            detached.push(DetachedFace {
                f,
                cycles,
                plane: face.plane,
                sides: face.sides.clone(),
            });
            self.detach_face(f, d)?;
        }

        // Merging vertices give up their edges and disappear before the batch
        // move, so replay can still find every vertex by position.
        let mut rewired: Vec<(VertexId, VertexId, EdgeFlags, EdgeId)> = Vec::new();
        if !merge_into.is_empty() {
            let mut doomed = BTreeSet::new();
            for &v in merge_into.keys() {
                for &h in self.vertex_star(v) {
                    doomed.insert(self.he(h)?.edge);
                }
            }
            for e in doomed {
                let (a, b) = self.edge_endpoints(e)?;
                let flags = self.edge(e).map(|x| x.flags).unwrap_or_default();
                rewired.push((a, b, flags, e));
                self.delete_edge(e, d)?;
            }
            for &v in merge_into.keys() {
                self.delete_vertex(v, d)?;
                result.merged_vertices += 1;
            }
        }

        let mut moves = Vec::new();
        for &(p, v) in &kept {
            moves.push((v, self.position(v)?, p));
            self.set_position_raw(v, p)?;
        }
        if !moves.is_empty() {
            d.record(MeshDelta::VerticesMoved { moves });
        }

        let map = |v: VertexId| merge_into.get(&v).copied().unwrap_or(v);
        for (a, b, flags, old) in rewired {
            let (a2, b2) = (map(a), map(b));
            if a2 == b2 {
                result.collapsed_edges.push(old);
            } else if self.edge_between(a2, b2).is_none() {
                self.new_edge(a2, b2, flags, d)?;
            }
        }

        // Step 2: re-attach, refitting planes; auto-fold non-planar faces.
        for face in detached {
            self.reattach_moved_face(face, &map, opts, tol, d, &mut result)?;
        }

        // Step 3: optional intersection check.
        if opts.check_intersections {
            let mut edges = BTreeSet::new();
            for &(_, v) in &kept {
                for &h in self.vertex_star(v) {
                    edges.insert(self.he(h)?.edge);
                }
            }
            for &w in merge_into.values() {
                for &h in self.vertex_star(w) {
                    edges.insert(self.he(h)?.edge);
                }
            }
            let (touched, pieces) = self.integrate_edges(edges.into_iter().collect(), tol, d)?;
            self.find_faces_seeded(&touched, &pieces, tol, d)?;
        }
        Ok(result)
    }

    fn reattach_moved_face(
        &mut self,
        face: DetachedFace,
        map: &dyn Fn(VertexId) -> VertexId,
        opts: TransformOptions,
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
        result: &mut TransformResult,
    ) -> Result<(), GeometryError> {
        let mut cycles: Vec<Vec<VertexId>> = face
            .cycles
            .iter()
            .map(|c| dedup_cyclic(c.iter().map(|&v| map(v)).collect()))
            .collect();
        let outer = cycles.remove(0);
        let holes: Vec<Vec<VertexId>> = cycles.into_iter().filter(|c| c.len() >= 3).collect();
        if outer.len() < 3 {
            return self.discard_detached_face(face.f);
        }
        let (Some(outer_h), Some(holes_h)) = (
            self.cycle_of(&outer),
            holes
                .iter()
                .map(|h| self.cycle_of(h))
                .collect::<Option<Vec<_>>>(),
        ) else {
            return self.discard_detached_face(face.f);
        };
        for &h in outer_h.iter().chain(holes_h.iter().flatten()) {
            if self.he(h)?.face.is_some() {
                // Collapsed onto another face: drop it (degenerate, docs/03 §2).
                return self.discard_detached_face(face.f);
            }
        }
        match planar::fit_plane(self, &outer_h, &holes_h, tol)? {
            Some(plane) => {
                if self
                    .attach_face(face.f, &outer_h, &holes_h, plane, face.sides, tol, d)
                    .is_err()
                {
                    self.discard_detached_face(face.f)?;
                }
                Ok(())
            }
            None if opts.auto_fold => self.fold_face(face.f, &outer, &holes, &face, tol, d, result),
            None => Err(GeometryError::NotApplicable(
                "moving these vertices would make a face non-planar".into(),
            )),
        }
    }

    /// Half-edges along a vertex cycle, if every edge exists.
    fn cycle_of(&self, vertices: &[VertexId]) -> Option<Vec<HalfEdgeId>> {
        let n = vertices.len();
        (0..n)
            .map(|i| self.half_edge_from_to(vertices[i], vertices[(i + 1) % n]))
            .collect()
    }

    /// docs/03 §4.8 step 2: splits a detached, non-planar face into planar
    /// pieces joined by hidden soft fold edges. Quads fold along the shorter
    /// valid diagonal; other faces are ear-clipped in their best-fit plane and
    /// the triangles regrown into maximal planar groups.
    #[allow(clippy::too_many_arguments)]
    fn fold_face(
        &mut self,
        f: FaceId,
        outer: &[VertexId],
        holes: &[Vec<VertexId>],
        face: &DetachedFace,
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
        result: &mut TransformResult,
    ) -> Result<(), GeometryError> {
        let all: Vec<VertexId> = outer
            .iter()
            .chain(holes.iter().flatten())
            .copied()
            .collect();
        let points = all
            .iter()
            .map(|&v| self.position(v))
            .collect::<Result<Vec<_>, _>>()?;
        let fit = Plane::fit(&points, tol)
            .map(|p| {
                if p.normal.dot(face.plane.normal) < 0.0 {
                    p.flipped()
                } else {
                    p
                }
            })
            .unwrap_or(face.plane);
        let frame = fit.frame_at(points[0]);
        let p2: Vec<DVec2> = frame.project_all(&points);

        let cannot = || GeometryError::NotApplicable("auto-fold could not split the face".into());
        let mut triangles: Vec<[usize; 3]> = if holes.is_empty() && outer.len() == 4 {
            let area = |t: [usize; 3]| (p2[t[1]] - p2[t[0]]).perp_dot(p2[t[2]] - p2[t[0]]);
            let options = [
                (points[0].distance(points[2]), [[0, 1, 2], [0, 2, 3]]),
                (points[1].distance(points[3]), [[1, 2, 3], [1, 3, 0]]),
            ];
            let mut valid: Vec<_> = options
                .into_iter()
                .filter(|(_, tris)| tris.iter().all(|&t| area(t) > tol.length_sq))
                .collect();
            valid.sort_by(|a, b| a.0.total_cmp(&b.0));
            valid
                .first()
                .map(|(_, tris)| tris.to_vec())
                .ok_or_else(cannot)?
        } else {
            let outer2: Vec<DVec2> = p2[..outer.len()].to_vec();
            let mut holes2 = Vec::with_capacity(holes.len());
            let mut cursor = outer.len();
            for h in holes {
                holes2.push(p2[cursor..cursor + h.len()].to_vec());
                cursor += h.len();
            }
            tessellate_polygon_2d(&outer2, &holes2)
                .map_err(|_| cannot())?
                .into_iter()
                .map(|t| t.map(|i| i as usize))
                .collect()
        };
        for t in &mut triangles {
            if (p2[t[1]] - p2[t[0]]).perp_dot(p2[t[2]] - p2[t[0]]) < 0.0 {
                t.swap(1, 2);
            }
        }

        // Grow planar groups across shared triangle edges.
        let mut by_edge: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
        for (i, t) in triangles.iter().enumerate() {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                by_edge.entry((a.min(b), a.max(b))).or_default().push(i);
            }
        }
        let mut group_of = vec![usize::MAX; triangles.len()];
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for start in 0..triangles.len() {
            if group_of[start] != usize::MAX {
                continue;
            }
            let t = triangles[start];
            let Some(plane) = Plane::from_points(points[t[0]], points[t[1]], points[t[2]], tol)
            else {
                continue;
            };
            let g = groups.len();
            group_of[start] = g;
            let mut members = vec![start];
            let mut k = 0;
            while k < members.len() {
                let tri = triangles[members[k]];
                k += 1;
                for e in 0..3 {
                    let (a, b) = (tri[e], tri[(e + 1) % 3]);
                    for &n in &by_edge[&(a.min(b), a.max(b))] {
                        let nt = triangles[n];
                        if group_of[n] == usize::MAX
                            && nt.iter().all(|&i| tol.point_on_plane(&plane, points[i]))
                        {
                            group_of[n] = g;
                            members.push(n);
                        }
                    }
                }
            }
            groups.push(members);
        }
        if groups.len() < 2 || group_of.contains(&usize::MAX) {
            return Err(cannot());
        }

        // Fold edges between groups.
        let mut fold_pairs = BTreeSet::new();
        for (&(a, b), tris) in &by_edge {
            if tris.len() == 2 && group_of[tris[0]] != group_of[tris[1]] {
                fold_pairs.insert((a, b));
            }
        }
        for (a, b) in fold_pairs {
            if self.edge_between(all[a], all[b]).is_none() {
                let e = self.new_edge(all[a], all[b], FOLD_FLAGS, d)?;
                result.fold_edges.push(e);
            }
        }

        // Pieces: the largest keeps the face id.
        let mut pieces = Vec::with_capacity(groups.len());
        for members in &groups {
            let mut directed = BTreeSet::new();
            for &i in members {
                let t = triangles[i];
                for k in 0..3 {
                    directed.insert((t[k], t[(k + 1) % 3]));
                }
            }
            let boundary: Vec<(usize, usize)> = directed
                .iter()
                .copied()
                .filter(|&(a, b)| !directed.contains(&(b, a)))
                .collect();
            let cycles = planar::boundary_cycles(&boundary, |i| p2[i]).ok_or_else(cannot)?;
            let mut outer_c = Vec::new();
            let mut hole_c = Vec::new();
            for c in cycles {
                let ring: Vec<DVec2> = c.iter().map(|&i| p2[i]).collect();
                let vs: Vec<VertexId> = c.iter().map(|&i| all[i]).collect();
                if polygon::signed_area(&ring) > 0.0 {
                    outer_c.push(vs);
                } else {
                    hole_c.push(vs);
                }
            }
            if outer_c.len() != 1 {
                return Err(cannot());
            }
            let outer_h = self.cycle_of(&outer_c[0]).ok_or_else(cannot)?;
            let holes_h = hole_c
                .iter()
                .map(|h| self.cycle_of(h))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(cannot)?;
            let plane = planar::fit_plane(self, &outer_h, &holes_h, tol)?.ok_or_else(cannot)?;
            let area: f64 = members
                .iter()
                .map(|&i| {
                    let t = triangles[i];
                    ((p2[t[1]] - p2[t[0]]).perp_dot(p2[t[2]] - p2[t[0]]) * 0.5).abs()
                })
                .sum();
            pieces.push((area, outer_h, holes_h, plane));
        }
        let keeper = (0..pieces.len())
            .max_by(|&a, &b| pieces[a].0.total_cmp(&pieces[b].0).then(b.cmp(&a)))
            .ok_or_else(cannot)?;
        let (_, k_outer, k_holes, k_plane) = pieces.remove(keeper);
        self.attach_face(f, &k_outer, &k_holes, k_plane, face.sides.clone(), tol, d)?;
        result.folded_faces.push(f);
        for (_, outer_h, holes_h, plane) in pieces {
            let g = self.create_face(&outer_h, &holes_h, plane, face.sides.clone(), tol, d)?;
            d.record(MeshDelta::FaceSplit {
                old: f,
                new: [f, g],
            });
            result.folded_faces.push(g);
        }
        Ok(())
    }

    /// Splits `edges` and the edges they cross at every interior contact
    /// (docs/03 §4.8 step 3). Returns the touched vertices and the resulting
    /// pieces of `edges`.
    fn integrate_edges(
        &mut self,
        edges: Vec<EdgeId>,
        tol: &Tolerances,
        d: &mut dyn DeltaSink,
    ) -> Result<(Vec<VertexId>, Vec<EdgeId>), GeometryError> {
        let mut work = edges;
        let mut done = Vec::new();
        let mut touched = BTreeSet::new();
        let limit = 4 * (self.edge_count() + work.len()) + 16;
        let mut steps = 0;
        while let Some(e) = work.pop() {
            steps += 1;
            if steps > limit {
                return Err(GeometryError::NonRobust(
                    "intersection check did not converge".into(),
                ));
            }
            if self.edge(e).is_none() {
                continue;
            }
            let seg = self.edge_segment(e)?;
            let (ea, eb) = self.edge_endpoints(e)?;
            let bbox = seg.aabb().expanded(tol.length);
            // A vertex lying inside `e`.
            let mut split_at: Option<DVec3> = None;
            for (v, vert) in self.vertices() {
                if v != ea
                    && v != eb
                    && bbox.contains_point(vert.position)
                    && tol.point_inside_segment(vert.position, &seg)
                {
                    split_at = Some(vert.position);
                    break;
                }
            }
            if split_at.is_none() {
                for g in self.edges_near(&bbox)? {
                    if g == e {
                        continue;
                    }
                    let gs = self.edge_segment(g)?;
                    if let SegmentContact::Point { point, .. } = segment_segment(&seg, &gs, tol)
                        && tol.point_inside_segment(point, &gs)
                    {
                        let (m, parts) = self.split_edge_at(g, point, tol, d)?;
                        touched.insert(m);
                        let _ = parts;
                        split_at = Some(self.position(m)?);
                        break;
                    }
                }
            }
            match split_at {
                Some(p) if tol.point_inside_segment(p, &seg) => {
                    let (m, parts) = self.split_edge_at(e, p, tol, d)?;
                    touched.insert(m);
                    work.extend(parts);
                }
                Some(_) => work.push(e),
                None => {
                    touched.insert(ea);
                    touched.insert(eb);
                    done.push(e);
                }
            }
        }
        done.sort();
        done.dedup();
        Ok((touched.into_iter().collect(), done))
    }
}

/// Removes consecutive duplicates of a cyclic sequence (collapsed edges).
fn dedup_cyclic(mut cycle: Vec<VertexId>) -> Vec<VertexId> {
    cycle.dedup();
    while cycle.len() > 1 && cycle.first() == cycle.last() {
        cycle.pop();
    }
    cycle
}

/// The vertex of an x-sorted list closest to `p` within `tol.length`.
fn nearest_sorted(sorted: &[(DVec3, VertexId)], p: DVec3, tol: &Tolerances) -> Option<VertexId> {
    let lo = sorted.partition_point(|(q, _)| q.x < p.x - tol.length);
    let mut best: Option<(f64, VertexId)> = None;
    for &(q, v) in &sorted[lo..] {
        if q.x > p.x + tol.length {
            break;
        }
        let d2 = q.distance_squared(p);
        if d2 <= tol.length_sq && best.is_none_or(|(bd, _)| d2 < bd) {
            best = Some((d2, v));
        }
    }
    best.map(|(_, v)| v)
}
