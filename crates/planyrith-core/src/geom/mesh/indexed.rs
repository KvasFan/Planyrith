//! Implements docs/09-file-formats.md §2 (the slot-layout-independent mesh
//! form used by `document.msgpack`) and §6 (the importer fast path that builds
//! a mesh from triangles, merging coplanar neighbors and softening edges).

use std::collections::{BTreeMap, BTreeSet};

use crate::fastmap::{FastMap, FastSet};

use glam::DVec3;
use slotmap::SecondaryMap;

use super::{
    DirectedEdgeIndex, EdgeFlags, HalfEdgeMesh, IndexedEdge, IndexedFace, IndexedIds, IndexedMesh,
    TriangleMergeOptions, TriangleMergeResult, broken,
};
use crate::geom::planar::boundary_cycles;
use crate::geom::{DeltaSink, FaceSides, GeometryError, NoDeltas};
use crate::ids::{CurveId, EdgeId, FaceId, HalfEdgeId, VertexId};
use crate::math::{Plane, polygon};
use crate::tolerance::Tolerances;

/// docs/09 §2: rebuild the half-edge structure, recompute missing planes,
/// validate every invariant.
pub(super) fn from_indexed(
    indexed: &IndexedMesh,
    tol: &Tolerances,
) -> Result<HalfEdgeMesh, GeometryError> {
    build_from_indexed(indexed, tol, true)
}

fn build_from_indexed(
    indexed: &IndexedMesh,
    tol: &Tolerances,
    check: bool,
) -> Result<HalfEdgeMesh, GeometryError> {
    let mut mesh = HalfEdgeMesh::new();
    append_indexed(&mut mesh, indexed, tol, &mut NoDeltas)?;
    let violations = if check {
        mesh.validate(tol)
    } else {
        Vec::new()
    };
    if let Some(first) = violations.first() {
        return Err(broken(format!(
            "loaded mesh violates invariant {}: {} ({} violation(s))",
            first.invariant,
            first.detail,
            violations.len()
        )));
    }
    Ok(mesh)
}

/// Adds the vertices, edges, curves and faces of `indexed` to `mesh` as they
/// are (no merging with existing elements), recording deltas; returns the new
/// ids by local index (`curves` lists the descriptors that kept edges).
/// Not atomic on its own: callers run it inside `transact` or on a scratch mesh.
pub(super) fn append_indexed(
    mesh: &mut HalfEdgeMesh,
    indexed: &IndexedMesh,
    tol: &Tolerances,
    d: &mut dyn DeltaSink,
) -> Result<IndexedIds, GeometryError> {
    let mut vertices = Vec::with_capacity(indexed.vertices.len());
    for &p in &indexed.vertices {
        if !tol.in_range(p) {
            return Err(GeometryError::OutOfRange);
        }
        vertices.push(mesh.new_vertex(p, d));
    }
    let vertex = |i: u32| -> Result<VertexId, GeometryError> {
        vertices
            .get(i as usize)
            .copied()
            .ok_or_else(|| broken(format!("vertex index {i} out of range")))
    };

    let mut edges = Vec::with_capacity(indexed.edges.len());
    for edge in &indexed.edges {
        let e = mesh.new_edge(vertex(edge.a)?, vertex(edge.b)?, edge.flags, d)?;
        edges.push(e);
    }

    let mut curves: Vec<CurveId> = Vec::with_capacity(indexed.curves.len());
    for (i, descriptor) in indexed.curves.iter().enumerate() {
        let members: Vec<EdgeId> = indexed
            .edges
            .iter()
            .zip(&edges)
            .filter(|(edge, _)| edge.curve == Some(i as u32))
            .map(|(_, &e)| e)
            .collect();
        let members = chain_order(mesh, &members)?;
        if members.is_empty() {
            // A descriptor without edges carries no geometry; drop it.
            continue;
        }
        let c = mesh.add_curve_inner(descriptor.clone(), &members, false, d)?;
        curves.push(c);
    }
    for edge in &indexed.edges {
        if let Some(c) = edge.curve
            && c as usize >= indexed.curves.len()
        {
            return Err(broken(format!("curve index {c} out of range")));
        }
    }

    let mut faces = Vec::with_capacity(indexed.faces.len());
    for face in &indexed.faces {
        let mut cycles = Vec::with_capacity(face.loops.len());
        for lp in &face.loops {
            let mut cycle = Vec::with_capacity(lp.len());
            for de in lp {
                let e = edges
                    .get(de.index() as usize)
                    .copied()
                    .ok_or_else(|| broken(format!("edge index {} out of range", de.index())))?;
                let (h, t) = mesh.edge_half_edges(e)?;
                cycle.push(if de.is_reversed() { t } else { h });
            }
            cycles.push(cycle);
        }
        let (outer, holes) = cycles
            .split_first()
            .ok_or_else(|| GeometryError::Degenerate("face without loops".into()))?;
        let plane = match face.plane {
            Some(plane) => plane,
            None => Plane::from_polygon(&mesh.cycle_positions(outer)?, tol)
                .ok_or_else(|| GeometryError::Degenerate("face outer loop has zero area".into()))?,
        };
        faces.push(mesh.create_face(outer, holes, plane, face.sides.clone(), tol, d)?);
    }
    Ok(IndexedIds {
        vertices,
        edges,
        faces,
        curves,
    })
}

/// Orders curve members into a chain (from an end for open chains, from the
/// lowest edge id for closed ones). Members that do not form one chain keep
/// their input order.
fn chain_order(mesh: &HalfEdgeMesh, members: &[EdgeId]) -> Result<Vec<EdgeId>, GeometryError> {
    if members.len() < 2 {
        return Ok(members.to_vec());
    }
    let mut at: BTreeMap<VertexId, Vec<EdgeId>> = BTreeMap::new();
    for &e in members {
        let (a, b) = mesh.edge_endpoints(e)?;
        at.entry(a).or_default().push(e);
        at.entry(b).or_default().push(e);
    }
    if at.values().any(|es| es.len() > 2) {
        return Ok(members.to_vec());
    }
    // Start at an end vertex (degree 1) of the first member in input order, if any.
    let mut start_vertex = None;
    for &e in members {
        let (a, b) = mesh.edge_endpoints(e)?;
        for v in [a, b] {
            if at.get(&v).is_some_and(|es| es.len() == 1) {
                start_vertex = Some((v, e));
                break;
            }
        }
        if start_vertex.is_some() {
            break;
        }
    }
    let (mut v, mut e) = match start_vertex {
        Some(found) => found,
        None => {
            let e = members.iter().copied().min().unwrap_or(members[0]);
            (mesh.edge_endpoints(e)?.0, e)
        }
    };
    let mut out = Vec::with_capacity(members.len());
    let mut used = BTreeSet::new();
    loop {
        out.push(e);
        used.insert(e);
        let (a, b) = mesh.edge_endpoints(e)?;
        v = if a == v { b } else { a };
        let next = at
            .get(&v)
            .and_then(|es| es.iter().copied().find(|x| !used.contains(x)));
        match next {
            Some(n) => e = n,
            None => break,
        }
    }
    if out.len() == members.len() {
        Ok(out)
    } else {
        Ok(members.to_vec())
    }
}

/// Builds without the final `validate` and with planarity relaxed, for
/// loading documents whose own tolerances are not known yet (the file form
/// of `Entities`). Structural errors (bad indices, open loops, inverted
/// orientation) still fail.
pub(crate) fn from_indexed_lenient(indexed: &IndexedMesh) -> Result<HalfEdgeMesh, GeometryError> {
    let tol = Tolerances {
        planarity: 1e6,
        ..Tolerances::default()
    };
    build_from_indexed(indexed, &tol, false)
}

/// docs/09 §2 in a canonical order independent of slot layout: edges listed
/// in `edge_order` first, then the remaining edges in slot order; faces
/// likewise; vertices in order of first use by those edges, then isolated
/// vertices in slot order; curves in order of their first member edge.
pub(crate) fn to_indexed_ordered(
    mesh: &HalfEdgeMesh,
    edge_order: &[EdgeId],
    face_order: &[FaceId],
) -> (IndexedMesh, IndexedIds) {
    let mut edges: Vec<EdgeId> = Vec::with_capacity(mesh.edges.len());
    let mut seen = BTreeSet::new();
    for &e in edge_order
        .iter()
        .chain(mesh.edges.keys().collect::<Vec<_>>().iter())
    {
        if mesh.edges.contains_key(e) && seen.insert(e) {
            edges.push(e);
        }
    }
    let mut faces: Vec<FaceId> = Vec::with_capacity(mesh.faces.len());
    let mut seen_faces = BTreeSet::new();
    for &f in face_order
        .iter()
        .chain(mesh.faces.keys().collect::<Vec<_>>().iter())
    {
        if mesh.faces.contains_key(f) && seen_faces.insert(f) {
            faces.push(f);
        }
    }
    let mut vertices: Vec<VertexId> = Vec::with_capacity(mesh.vertices.len());
    let mut seen_vertices = BTreeSet::new();
    for &e in &edges {
        if let Ok((a, b)) = mesh.edge_endpoints(e) {
            for v in [a, b] {
                if seen_vertices.insert(v) {
                    vertices.push(v);
                }
            }
        }
    }
    for v in mesh.vertices.keys() {
        if seen_vertices.insert(v) {
            vertices.push(v);
        }
    }
    let mut curves: Vec<CurveId> = Vec::new();
    for &e in &edges {
        if let Some(c) = mesh.edges.get(e).and_then(|x| x.curve)
            && !curves.contains(&c)
        {
            curves.push(c);
        }
    }
    for c in mesh.curves.keys() {
        if !curves.contains(&c) {
            curves.push(c);
        }
    }
    emit_indexed(mesh, &vertices, &edges, &faces, &curves)
}

/// docs/09 §2: slot order for vertices, edges, faces and curves.
pub(super) fn to_indexed(mesh: &HalfEdgeMesh) -> (IndexedMesh, IndexedIds) {
    let vertices: Vec<VertexId> = mesh.vertices.keys().collect();
    let edges: Vec<EdgeId> = mesh.edges.keys().collect();
    let faces: Vec<FaceId> = mesh.faces.keys().collect();
    let curves: Vec<CurveId> = mesh.curves.keys().collect();
    emit_indexed(mesh, &vertices, &edges, &faces, &curves)
}

/// The indexed form for explicit element orders.
fn emit_indexed(
    mesh: &HalfEdgeMesh,
    vertex_order: &[VertexId],
    edge_order: &[EdgeId],
    face_order: &[FaceId],
    curve_order: &[CurveId],
) -> (IndexedMesh, IndexedIds) {
    let mut ids = IndexedIds::default();
    let mut out = IndexedMesh::default();

    let mut vertex_index: SecondaryMap<VertexId, u32> = SecondaryMap::new();
    for &v in vertex_order {
        let Some(vert) = mesh.vertices.get(v) else {
            continue;
        };
        vertex_index.insert(v, out.vertices.len() as u32);
        out.vertices.push(vert.position);
        ids.vertices.push(v);
    }

    let mut curve_index: SecondaryMap<CurveId, u32> = SecondaryMap::new();
    for &c in curve_order {
        let Some(descriptor) = mesh.curves.get(c) else {
            continue;
        };
        curve_index.insert(c, out.curves.len() as u32);
        out.curves.push(descriptor.clone());
        ids.curves.push(c);
    }

    let mut edge_index: SecondaryMap<EdgeId, u32> = SecondaryMap::new();
    for &e in edge_order {
        let Some(edge) = mesh.edges.get(e) else {
            continue;
        };
        let (Some(he), Some(twin)) = (
            mesh.half_edges.get(edge.he),
            mesh.half_edges
                .get(edge.he)
                .and_then(|he| mesh.half_edges.get(he.twin)),
        ) else {
            continue;
        };
        let (Some(&a), Some(&b)) = (vertex_index.get(he.origin), vertex_index.get(twin.origin))
        else {
            continue;
        };
        edge_index.insert(e, out.edges.len() as u32);
        out.edges.push(IndexedEdge {
            a,
            b,
            flags: edge.flags,
            curve: edge.curve.and_then(|c| curve_index.get(c).copied()),
        });
        ids.edges.push(e);
    }

    for &f in face_order {
        let Some(face) = mesh.faces.get(f) else {
            continue;
        };
        let mut loops = Vec::with_capacity(1 + face.holes.len());
        for &l in std::iter::once(&face.outer).chain(face.holes.iter()) {
            let Ok(cycle) = mesh.loop_half_edges(l) else {
                continue;
            };
            let directed: Vec<DirectedEdgeIndex> = cycle
                .iter()
                .filter_map(|&h| {
                    let he = mesh.half_edges.get(h)?;
                    let index = *edge_index.get(he.edge)?;
                    let forward = mesh.edges.get(he.edge)?.he == h;
                    Some(if forward {
                        DirectedEdgeIndex::forward(index)
                    } else {
                        DirectedEdgeIndex::reversed(index)
                    })
                })
                .collect();
            loops.push(directed);
        }
        out.faces.push(IndexedFace {
            loops,
            plane: Some(face.plane),
            sides: face.sides.clone(),
        });
        ids.faces.push(f);
    }
    (out, ids)
}

/// Disjoint-set forest over triangle indices (path halving, union by index so
/// representatives are deterministic: the smallest index of each set).
struct UnionFind(Vec<usize>);

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind((0..n).collect())
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.0[i] != i {
            self.0[i] = self.0[self.0[i]];
            i = self.0[i];
        }
        i
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            self.0[hi] = lo;
        }
    }
}

/// docs/09 §6 importer fast path. With `keys` (one per triangle), only
/// triangles of the same key merge (materials of imported meshes).
pub(super) fn from_triangles_merged(
    positions: &[DVec3],
    triangles: &[[u32; 3]],
    keys: Option<&[u32]>,
    opts: TriangleMergeOptions,
    tol: &Tolerances,
) -> Result<TriangleMergeResult, GeometryError> {
    // 1. Weld input positions by tolerance (grid of cell size `length`, the
    //    27 neighbor cells cover every point within `length`).
    let mut grid: FastMap<[i64; 3], Vec<usize>> = FastMap::default();
    let mut weld: Vec<usize> = Vec::with_capacity(positions.len());
    let mut reps: Vec<DVec3> = Vec::new();
    let cell = |p: DVec3| -> [i64; 3] {
        let q = p / tol.length;
        [q.x.floor() as i64, q.y.floor() as i64, q.z.floor() as i64]
    };
    for &p in positions {
        if !tol.in_range(p) {
            return Err(GeometryError::OutOfRange);
        }
        let c = cell(p);
        let mut found = None;
        'search: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(list) = grid.get(&[c[0] + dx, c[1] + dy, c[2] + dz]) {
                        for &r in list {
                            if tol.same_point(reps[r], p) {
                                found = Some(r);
                                break 'search;
                            }
                        }
                    }
                }
            }
        }
        let r = match found {
            Some(r) => r,
            None => {
                reps.push(p);
                grid.entry(c).or_default().push(reps.len() - 1);
                reps.len() - 1
            }
        };
        weld.push(r);
    }

    // 2. Accept triangles: skip degenerate ones, fix inconsistent winding by
    //    flipping, drop the ones that would make an edge non-manifold.
    let mut degenerate_triangles = 0;
    let mut non_manifold_edges = 0;
    let mut used_directed: FastSet<(usize, usize)> = FastSet::default();
    let mut tris: Vec<[usize; 3]> = Vec::with_capacity(triangles.len());
    // The input triangle of each accepted one.
    let mut source: Vec<usize> = Vec::with_capacity(triangles.len());
    for (input, t) in triangles.iter().enumerate() {
        let mut idx = [0usize; 3];
        for k in 0..3 {
            idx[k] = *weld
                .get(t[k] as usize)
                .ok_or_else(|| broken(format!("triangle index {} out of range", t[k])))?;
        }
        let [a, b, c] = idx;
        if a == b || b == c || a == c || tol.collinear(reps[a], reps[b], reps[c]) {
            degenerate_triangles += 1;
            continue;
        }
        let free = |tri: [usize; 3], used: &FastSet<(usize, usize)>| {
            (0..3).all(|k| !used.contains(&(tri[k], tri[(k + 1) % 3])))
        };
        let chosen = if free(idx, &used_directed) {
            Some(idx)
        } else if free([a, c, b], &used_directed) {
            Some([a, c, b])
        } else {
            None
        };
        match chosen {
            Some(tri) => {
                for k in 0..3 {
                    used_directed.insert((tri[k], tri[(k + 1) % 3]));
                }
                tris.push(tri);
                source.push(input);
            }
            None => non_manifold_edges += 1,
        }
    }

    // 3. Triangle planes and adjacency across shared undirected edges.
    let planes: Vec<Option<Plane>> = tris
        .iter()
        .map(|t| Plane::from_points(reps[t[0]], reps[t[1]], reps[t[2]], tol))
        .collect();
    let mut by_directed: FastMap<(usize, usize), usize> = FastMap::default();
    by_directed.reserve(tris.len() * 3);
    for (i, t) in tris.iter().enumerate() {
        for k in 0..3 {
            by_directed.insert((t[k], t[(k + 1) % 3]), i);
        }
    }
    // Representatives are the smallest index of each set, so the order of
    // the unions does not matter.
    let mut groups = UnionFind::new(tris.len());
    if opts.merge_coplanar {
        let key = |t: usize| keys.and_then(|k| k.get(source[t]).copied());
        for (i, t) in tris.iter().enumerate() {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                if let Some(&j) = by_directed.get(&(b, a))
                    && j > i
                    && key(i) == key(j)
                    && let (Some(pi), Some(pj)) = (planes[i], planes[j])
                    && tol.is_coplanar_same_side(&pi, &pj)
                {
                    groups.union(i, j);
                }
            }
        }
    }

    // 4. Build each group's boundary loops; fall back to single triangles for
    //    groups whose boundary is not one planar region.
    // By representative (the set's smallest triangle), in index order.
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); tris.len()];
    for i in 0..tris.len() {
        let r = groups.find(i);
        members[r].push(i);
    }
    // Each face with the accepted triangles it covers.
    let mut faces: Vec<(Vec<Vec<usize>>, Plane, Vec<usize>)> = Vec::new();
    let mut merged_faces = 0;
    for tri_ids in members.iter().filter(|m| !m.is_empty()) {
        if tri_ids.len() > 1
            && let Some((loops, plane)) = group_face(&reps, &tris, tri_ids, tol)
        {
            merged_faces += tri_ids.len() - 1;
            faces.push((loops, plane, tri_ids.clone()));
            continue;
        }
        for &i in tri_ids {
            if let Some(plane) = planes[i] {
                faces.push((vec![tris[i].to_vec()], plane, vec![i]));
            }
        }
    }

    // 5. Assemble the mesh: only vertices and edges that some face uses.
    let mut mesh = HalfEdgeMesh::new();
    let d = &mut NoDeltas;
    let mut vertex_ids: Vec<Option<VertexId>> = vec![None; reps.len()];
    // Edges in sorted (lower, higher) vertex order.
    let mut edge_set: Vec<(usize, usize)> = Vec::new();
    for (loops, _, _) in &faces {
        for lp in loops {
            for k in 0..lp.len() {
                let (a, b) = (lp[k], lp[(k + 1) % lp.len()]);
                edge_set.push((a.min(b), a.max(b)));
            }
        }
    }
    edge_set.sort_unstable();
    edge_set.dedup();
    for &(a, b) in &edge_set {
        let mut id = |v: usize| *vertex_ids[v].get_or_insert_with(|| mesh.new_vertex(reps[v], d));
        let (va, vb) = (id(a), id(b));
        mesh.new_edge(va, vb, EdgeFlags::NONE, d)?;
    }
    let mut triangle_faces: Vec<Option<FaceId>> = vec![None; triangles.len()];
    for (loops, plane, covered) in faces {
        let mut cycles: Vec<Vec<HalfEdgeId>> = Vec::with_capacity(loops.len());
        for lp in &loops {
            let mut cycle = Vec::with_capacity(lp.len());
            for k in 0..lp.len() {
                let (Some(a), Some(b)) = (vertex_ids[lp[k]], vertex_ids[lp[(k + 1) % lp.len()]])
                else {
                    return Err(broken("missing vertex while importing"));
                };
                cycle.push(
                    mesh.half_edge_from_to(a, b)
                        .ok_or_else(|| broken("missing half-edge while importing"))?,
                );
            }
            cycles.push(cycle);
        }
        let Some((outer, holes)) = cycles.split_first() else {
            continue;
        };
        match mesh.create_face(outer, holes, plane, FaceSides::default(), tol, d) {
            Ok(f) => {
                for t in covered {
                    triangle_faces[source[t]] = Some(f);
                }
            }
            Err(_) => degenerate_triangles += 1,
        }
    }

    // 6. Soften edges between faces meeting below the soften angle.
    let soften_cos = libm::cos(opts.soften_angle.as_radians());
    let edge_ids: Vec<EdgeId> = mesh.edges.keys().collect();
    for e in edge_ids {
        if let [Some(fa), Some(fb)] = mesh.edge_faces(e)?
            && fa != fb
        {
            let (na, nb) = (mesh.face_normal(fa)?, mesh.face_normal(fb)?);
            if na.dot(nb) >= soften_cos {
                mesh.set_edge_flags(e, EdgeFlags::SOFT | EdgeFlags::SMOOTH, d)?;
            }
        }
    }

    Ok(TriangleMergeResult {
        mesh,
        merged_faces,
        non_manifold_edges,
        degenerate_triangles,
        triangle_faces,
    })
}

/// Boundary loops (outer first, then holes) and plane of a group of coplanar
/// triangles, or `None` when the boundary is not a single planar region.
fn group_face(
    reps: &[DVec3],
    tris: &[[usize; 3]],
    tri_ids: &[usize],
    tol: &Tolerances,
) -> Option<(Vec<Vec<usize>>, Plane)> {
    let mut ordered: Vec<(usize, usize)> = Vec::with_capacity(tri_ids.len() * 3);
    for &i in tri_ids {
        let t = tris[i];
        for k in 0..3 {
            ordered.push((t[k], t[(k + 1) % 3]));
        }
    }
    ordered.sort_unstable();
    ordered.dedup();
    let directed: FastSet<(usize, usize)> = ordered.iter().copied().collect();
    // In sorted order.
    let boundary: Vec<(usize, usize)> = ordered
        .into_iter()
        .filter(|&(a, b)| !directed.contains(&(b, a)))
        .collect();
    let first = tris[tri_ids[0]];
    let plane0 = Plane::from_points(reps[first[0]], reps[first[1]], reps[first[2]], tol)?;
    let frame = plane0.frame();

    let cycles = boundary_cycles(&boundary, |v| frame.to_2d(reps[v]))?;

    let mut outer: Vec<Vec<usize>> = Vec::new();
    let mut holes: Vec<Vec<usize>> = Vec::new();
    for cycle in cycles {
        let pts: Vec<_> = cycle.iter().map(|&v| frame.to_2d(reps[v])).collect();
        let area = polygon::signed_area(&pts);
        if area > tol.length_sq {
            outer.push(cycle);
        } else if area < -tol.length_sq {
            holes.push(cycle);
        } else {
            return None;
        }
    }
    if outer.len() != 1 {
        return None;
    }
    let outer = outer.remove(0);
    let points: Vec<DVec3> = outer.iter().map(|&v| reps[v]).collect();
    let plane = Plane::from_polygon(&points, tol)?;
    let all_on_plane = outer
        .iter()
        .chain(holes.iter().flatten())
        .all(|&v| tol.point_on_plane(&plane, reps[v]));
    if !all_on_plane {
        return None;
    }
    let mut loops = vec![outer];
    loops.extend(holes);
    Some((loops, plane))
}
