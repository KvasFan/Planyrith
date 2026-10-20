//! Implements docs/03-geometry-kernel.md §4.3 (face finding), §4.4 (split by
//! chord) and §4.5 (holes): planar graph embedding, minimal cycle enumeration,
//! loop orientation.
//!
//! Face finding works on the *arrangement* of the coplanar edges around the
//! touched vertices (docs/03 Decisions):
//! 1. collect the in-plane edges connected to the touched vertices, the
//!    coplanar faces that use them or contain them, and any in-plane geometry
//!    enclosed by a bounded region (closure, §4.3 step 3 "encloses existing
//!    geometry");
//! 2. trace every half-edge cycle with the "next edge clockwise from the
//!    twin" rule; an edge whose two half-edges lie in the same cycle has the
//!    same region on both sides (an antenna, a bridge, a wire edge lying on a
//!    face) and is dropped until no such edge remains;
//! 3. counter-clockwise cycles bound regions; each clockwise cycle is a hole
//!    of the smallest region that strictly contains it, or the unbounded
//!    outside;
//! 4. every region is mapped to the existing face that contains an interior
//!    sample point. A face whose region is unchanged is left alone; one that
//!    now covers several regions is split (it keeps its id for the piece that
//!    keeps its outer boundary, else the largest); a region covered by no face
//!    becomes a new face only when its outer boundary uses a *seed* edge (an
//!    edge the caller just added), so adding an unrelated edge never recreates
//!    a face the user erased.
//!
//! This one mechanism covers the three cases of §4.3–§4.5: a new loop in
//! empty space, a chord across a face, and a closed loop inside a face (the
//! face gains a hole and the inside becomes a new face).

#![warn(unused_variables, dead_code)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use glam::{DVec2, DVec3};

use crate::geom::tessellate::tessellate_polygon_2d;
use crate::geom::{DeltaSink, FaceSides, GeometryError, HalfEdgeMesh, MeshDelta};
use crate::ids::{EdgeId, FaceId, HalfEdgeId, VertexId};
use crate::math::polygon::{self, PointInPolygon};
use crate::math::{Aabb, Plane, PlaneFrame};
use crate::tolerance::Tolerances;

/// Result of [`find_faces`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FaceFindingResult {
    /// Faces created from newly closed coplanar loops, and the inner faces of
    /// hole situations.
    pub new_faces: Vec<FaceId>,
    /// Faces split by a chord: `(old, new_a, new_b)` (docs/03 §4.4). `new_a`
    /// is `old`, which keeps its id for one piece.
    pub split_faces: Vec<(FaceId, FaceId, FaceId)>,
    /// Hole situations: `(outer face, inner face)` (docs/03 §4.5).
    pub holes: Vec<(FaceId, FaceId)>,
}

impl FaceFindingResult {
    fn extend(&mut self, other: FaceFindingResult) {
        self.new_faces.extend(other.new_faces);
        self.split_faces.extend(other.split_faces);
        self.holes.extend(other.holes);
    }
}

/// Orientation of a closed 2D polygon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopOrientation {
    /// Positive signed area.
    CounterClockwise,
    /// Negative signed area.
    Clockwise,
    /// `|area| <= tol.length_sq`.
    Degenerate,
}

/// Face finding on the touched vertices after an edge was added or moved
/// (docs/03 §4.3 steps 1–4). Every edge incident to a touched vertex counts as
/// a seed (may close a new face). Atomic: on failure the mesh is unchanged.
pub fn find_faces(
    mesh: &mut HalfEdgeMesh,
    touched: &[VertexId],
    tol: &Tolerances,
    sink: &mut dyn DeltaSink,
) -> Result<FaceFindingResult, GeometryError> {
    let seeds = incident_edges(mesh, touched)?;
    mesh.transact(tol, sink, |m, d| {
        find_faces_seeded(m, touched, &seeds, tol, d)
    })
}

/// Candidate planes spanned by consecutive edge pairs around each touched
/// vertex, deduplicated with `tol.is_coplanar` (docs/03 §4.3 step 1).
///
/// The draft considers every pair of edges at a vertex (stars are small) and
/// prefers the stored plane of an existing face that contains the edges.
pub fn candidate_planes(mesh: &HalfEdgeMesh, touched: &[VertexId], tol: &Tolerances) -> Vec<Plane> {
    let seeds = incident_edges(mesh, touched).unwrap_or_default();
    let touched: BTreeSet<VertexId> = touched.iter().copied().collect();
    candidate_planes_seeded(mesh, &touched, &seeds.into_iter().collect(), tol).unwrap_or_default()
}

/// Minimal cycles of the coplanar subgraph connected to `seeds`, walking
/// "turn left-most" in the plane frame; the unbounded outer cycle is excluded
/// unless it is the only one (docs/03 §4.3 step 2).
///
/// Returns the outer boundary of every bounded region, counter-clockwise in
/// `plane`'s frame (a single closed loop yields its inside).
pub fn minimal_cycles(
    mesh: &HalfEdgeMesh,
    plane: &Plane,
    seeds: &[VertexId],
    tol: &Tolerances,
) -> Result<Vec<Vec<HalfEdgeId>>, GeometryError> {
    let frame = plane.frame();
    let mut edges = BTreeSet::new();
    let mut seen: BTreeSet<VertexId> = BTreeSet::new();
    let mut queue: VecDeque<VertexId> = VecDeque::new();
    for &v in seeds {
        if on_plane(mesh, plane, v, tol)? && seen.insert(v) {
            queue.push_back(v);
        }
    }
    while let Some(v) = queue.pop_front() {
        for &h in mesh.vertex_star(v) {
            let w = mesh.dest(h)?;
            if on_plane(mesh, plane, w, tol)? {
                edges.insert(mesh.he(h)?.edge);
                if seen.insert(w) {
                    queue.push_back(w);
                }
            }
        }
    }
    Ok(arrangement_regions(mesh, &frame, &edges, tol)?
        .into_iter()
        .map(|r| r.outer)
        .collect())
}

/// Outgoing half-edges of `v` sorted by angle in `plane`'s 2D frame
/// (docs/03 §4.3 "angularly sorted star"). Edges not in the plane are skipped.
pub fn sort_star_by_angle(
    mesh: &HalfEdgeMesh,
    v: VertexId,
    plane: &Plane,
    tol: &Tolerances,
) -> Result<Vec<HalfEdgeId>, GeometryError> {
    let frame = plane.frame();
    let origin = frame.to_2d(mesh.position(v)?);
    let mut star = Vec::new();
    for &h in mesh.vertex_star(v) {
        let w = mesh.dest(h)?;
        if on_plane(mesh, plane, w, tol)? {
            let angle = sort_angle(frame.to_2d(mesh.position(w)?) - origin);
            star.push((angle, h));
        }
    }
    star.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    Ok(star.into_iter().map(|(_, h)| h).collect())
}

/// Default front orientation for a new face: `+Z` when the plane is not
/// vertical, else `+Y`, else `+X` (docs/03 §4.3 step 3).
pub fn default_face_normal(plane: &Plane, tol: &Tolerances) -> DVec3 {
    let n = plane.normal;
    if n.z.abs() > tol.angle {
        if n.z > 0.0 { n } else { -n }
    } else if n.y.abs() > tol.angle {
        if n.y > 0.0 { n } else { -n }
    } else if n.x >= 0.0 {
        n
    } else {
        -n
    }
}

/// Shoelace signed area of a closed 2D polygon (positive = counter-clockwise).
pub fn signed_area_2d(points: &[DVec2]) -> f64 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        sum += a.x * b.y - b.x * a.y;
    }
    sum * 0.5
}

/// Orientation from the signed area with the degeneracy threshold `length²`
/// (docs/03 §2: faces with area < length² are degenerate).
pub fn loop_orientation(points: &[DVec2], tol: &Tolerances) -> LoopOrientation {
    let area = signed_area_2d(points);
    if area.abs() <= tol.length_sq {
        LoopOrientation::Degenerate
    } else if area > 0.0 {
        LoopOrientation::CounterClockwise
    } else {
        LoopOrientation::Clockwise
    }
}

/// Point-in-polygon test (even-odd rule) in 2D; points on the boundary within
/// `tol.length` count as inside (docs/03 §4.2 step 3).
pub fn point_in_polygon_2d(p: DVec2, polygon: &[DVec2], tol: &Tolerances) -> bool {
    polygon::point_in_ring(p, polygon, tol).is_inside_or_on()
}

/// `true` when polygon `inner` lies entirely inside `outer` (used to assign
/// holes after a split, docs/03 §4.4, and to detect hole situations, §4.5).
pub fn polygon_contains_polygon_2d(outer: &[DVec2], inner: &[DVec2], tol: &Tolerances) -> bool {
    polygon::ring_contains_ring(outer, inner, tol)
}

// ----- crate-internal machinery ---------------------------------------------------

/// Angle of a 2D direction in `(-π, π]`, the sort key of angular stars.
pub(crate) fn sort_angle(d: DVec2) -> f64 {
    libm::atan2(d.y, d.x)
}

/// `v` lies within `planarity` of `plane`.
fn on_plane(
    mesh: &HalfEdgeMesh,
    plane: &Plane,
    v: VertexId,
    tol: &Tolerances,
) -> Result<bool, GeometryError> {
    Ok(tol.point_on_plane(plane, mesh.position(v)?))
}

/// Every edge incident to one of `vertices` (sorted, deduplicated).
fn incident_edges(
    mesh: &HalfEdgeMesh,
    vertices: &[VertexId],
) -> Result<Vec<EdgeId>, GeometryError> {
    let mut out = BTreeSet::new();
    for &v in vertices {
        for &h in mesh.vertex_star(v) {
            out.insert(mesh.he(h)?.edge);
        }
    }
    Ok(out.into_iter().collect())
}

/// Face finding where only regions bounded by a `seeds` edge may become new
/// faces (the crate-internal entry used by `add_edge`). Not atomic.
pub(crate) fn find_faces_seeded(
    mesh: &mut HalfEdgeMesh,
    touched: &[VertexId],
    seeds: &[EdgeId],
    tol: &Tolerances,
    d: &mut dyn DeltaSink,
) -> Result<FaceFindingResult, GeometryError> {
    let seeds: BTreeSet<EdgeId> = seeds
        .iter()
        .copied()
        .filter(|&e| mesh.edge(e).is_some())
        .collect();
    let touched: BTreeSet<VertexId> = touched
        .iter()
        .copied()
        .filter(|&v| mesh.vertex(v).is_some())
        .collect();
    let mut result = FaceFindingResult::default();
    for plane in candidate_planes_seeded(mesh, &touched, &seeds, tol)? {
        let mut start = Vec::new();
        for &v in &touched {
            if on_plane(mesh, &plane, v, tol)? {
                start.push(v);
            }
        }
        let Some(&first) = start.first() else {
            continue;
        };
        let frame = plane.frame_at(mesh.position(first)?);
        let graph = collect_plane_graph(mesh, &plane, &frame, &start, tol)?;
        let found = rebuild_plane(
            mesh,
            &RebuildInput {
                plane,
                frame,
                edges: &graph.edges,
                faces: &graph.faces,
                seeds: &seeds,
            },
            tol,
            d,
        )?;
        result.extend(found);
    }
    Ok(result)
}

/// docs/03 §4.3 step 1: planes of existing faces that contain a seed edge at a
/// touched vertex first (their fit is the most accurate), then planes spanned
/// by two edges at a touched vertex where at least one is a seed.
fn candidate_planes_seeded(
    mesh: &HalfEdgeMesh,
    touched: &BTreeSet<VertexId>,
    seeds: &BTreeSet<EdgeId>,
    tol: &Tolerances,
) -> Result<Vec<Plane>, GeometryError> {
    let mut planes: Vec<Plane> = Vec::new();
    let mut add = |p: Plane| {
        if !planes.iter().any(|q| tol.is_coplanar(q, &p)) {
            planes.push(p);
        }
    };
    // Per touched vertex: (outgoing half-edge, its edge, far end position).
    type Star = Vec<(HalfEdgeId, EdgeId, DVec3)>;
    let mut stars: Vec<(VertexId, Star)> = Vec::new();
    for &v in touched {
        let mut star = Vec::new();
        for &h in mesh.vertex_star(v) {
            star.push((h, mesh.he(h)?.edge, mesh.position(mesh.dest(h)?)?));
        }
        star.sort_by_key(|x| x.0);
        stars.push((v, star));
    }
    for (v, star) in &stars {
        let pv = mesh.position(*v)?;
        let mut faces = Vec::new();
        for &(g, _, _) in star {
            for side in [g, mesh.twin_of(g)?] {
                if let Some(f) = mesh.he(side)?.face {
                    faces.push(f);
                }
            }
        }
        faces.sort();
        faces.dedup();
        for f in faces {
            let plane = mesh.face_ref(f)?.plane;
            let holds_seed = star.iter().any(|&(_, e, pw)| {
                seeds.contains(&e)
                    && tol.point_on_plane(&plane, pv)
                    && tol.point_on_plane(&plane, pw)
            });
            if holds_seed {
                add(plane);
            }
        }
    }
    for (v, star) in &stars {
        let pv = mesh.position(*v)?;
        for i in 0..star.len() {
            for j in i + 1..star.len() {
                if !seeds.contains(&star[i].1) && !seeds.contains(&star[j].1) {
                    continue;
                }
                if let Some(p) = Plane::from_points(pv, star[i].2, star[j].2, tol) {
                    add(p);
                }
            }
        }
    }
    Ok(planes)
}

/// The geometry face finding looks at in one plane.
#[derive(Debug, Default)]
struct PlaneGraph {
    edges: BTreeSet<EdgeId>,
    faces: BTreeSet<FaceId>,
    vertices: BTreeSet<VertexId>,
}

/// docs/03 §4.3 step 2–3 closure: in-plane edges connected to `start`,
/// coplanar faces using them or containing one of their vertices (with all
/// of those faces' loops), and in-plane geometry enclosed by a bounded region.
fn collect_plane_graph(
    mesh: &HalfEdgeMesh,
    plane: &Plane,
    frame: &PlaneFrame,
    start: &[VertexId],
    tol: &Tolerances,
) -> Result<PlaneGraph, GeometryError> {
    let mut graph = PlaneGraph::default();
    let mut queue: VecDeque<VertexId> = VecDeque::new();
    for &v in start {
        if graph.vertices.insert(v) {
            queue.push_back(v);
        }
    }
    let add_face = |graph: &mut PlaneGraph,
                    queue: &mut VecDeque<VertexId>,
                    f: FaceId|
     -> Result<(), GeometryError> {
        if graph.faces.insert(f) {
            for l in mesh.face_loops(f)? {
                for h in mesh.loop_half_edges(l)? {
                    let he = mesh.he(h)?;
                    graph.edges.insert(he.edge);
                    if graph.vertices.insert(he.origin) {
                        queue.push_back(he.origin);
                    }
                }
            }
        }
        Ok(())
    };
    loop {
        while let Some(v) = queue.pop_front() {
            for &h in mesh.vertex_star(v) {
                let w = mesh.dest(h)?;
                if !on_plane(mesh, plane, w, tol)? {
                    continue;
                }
                let he = mesh.he(h)?;
                graph.edges.insert(he.edge);
                if graph.vertices.insert(w) {
                    queue.push_back(w);
                }
                let twin_face = mesh.he(he.twin)?.face;
                for f in [he.face, twin_face].into_iter().flatten() {
                    if !graph.faces.contains(&f) && tol.is_coplanar(&mesh.face_ref(f)?.plane, plane)
                    {
                        add_face(&mut graph, &mut queue, f)?;
                    }
                }
            }
        }

        let mut points = Vec::with_capacity(graph.vertices.len());
        for &v in &graph.vertices {
            points.push(mesh.position(v)?);
        }
        let bbox = Aabb::from_points(points.iter().copied()).expanded(tol.planarity);
        let mut grew = false;
        for f in mesh.coplanar_faces_near(plane, &bbox, tol)? {
            if graph.faces.contains(&f) {
                continue;
            }
            let (outer, holes) = face_polygon(mesh, f, frame)?;
            let hole_refs: Vec<&[DVec2]> = holes.iter().map(Vec::as_slice).collect();
            let contains = points.iter().any(|&p| {
                polygon::point_in_polygon(frame.to_2d(p), &outer, &hole_refs, tol)
                    != PointInPolygon::Outside
            });
            if contains {
                add_face(&mut graph, &mut queue, f)?;
                grew = true;
            }
        }
        if !grew && !graph.edges.is_empty() {
            let rings: Vec<Vec<DVec2>> = arrangement_regions(mesh, frame, &graph.edges, tol)?
                .iter()
                .map(|r| cycle_points_2d(mesh, frame, &r.outer))
                .collect::<Result<_, _>>()?;
            if !rings.is_empty() {
                for (v, vert) in mesh.vertices() {
                    if graph.vertices.contains(&v)
                        || !bbox.contains_point(vert.position)
                        || !tol.point_on_plane(plane, vert.position)
                    {
                        continue;
                    }
                    let p = frame.to_2d(vert.position);
                    if rings
                        .iter()
                        .any(|r| polygon::point_in_ring(p, r, tol) == PointInPolygon::Inside)
                    {
                        graph.vertices.insert(v);
                        queue.push_back(v);
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            return Ok(graph);
        }
    }
}

/// Inputs of [`rebuild_plane`].
struct RebuildInput<'a> {
    plane: Plane,
    frame: PlaneFrame,
    edges: &'a BTreeSet<EdgeId>,
    faces: &'a BTreeSet<FaceId>,
    seeds: &'a BTreeSet<EdgeId>,
}

/// docs/03 §4.3 step 3–4, §4.4, §4.5: reconcile the arrangement's regions with
/// the involved faces (see the module documentation).
fn rebuild_plane(
    mesh: &mut HalfEdgeMesh,
    input: &RebuildInput<'_>,
    tol: &Tolerances,
    d: &mut dyn DeltaSink,
) -> Result<FaceFindingResult, GeometryError> {
    let frame = &input.frame;
    let regions = arrangement_regions(mesh, frame, input.edges, tol)?;

    let mut polygons = Vec::with_capacity(input.faces.len());
    for &f in input.faces {
        let (outer, holes) = face_polygon(mesh, f, frame)?;
        polygons.push((f, outer, holes));
    }
    let mut pieces: BTreeMap<FaceId, Vec<usize>> = BTreeMap::new();
    let mut unowned: Vec<usize> = Vec::new();
    for (i, region) in regions.iter().enumerate() {
        let Some(sample) = region_sample(mesh, frame, region, tol)? else {
            continue;
        };
        let owner = polygons.iter().find(|(_, outer, holes)| {
            let hole_refs: Vec<&[DVec2]> = holes.iter().map(Vec::as_slice).collect();
            polygon::point_in_polygon(sample, outer, &hole_refs, tol) == PointInPolygon::Inside
        });
        match owner {
            Some((f, _, _)) => pieces.entry(*f).or_default().push(i),
            None => unowned.push(i),
        }
    }

    // Faces whose region changed.
    let mut changed = Vec::new();
    for (f, list) in pieces {
        let face = mesh.face_ref(f)?;
        let flip = face.plane.normal.dot(frame.normal) < 0.0;
        if list.len() == 1 {
            let (outer, holes) = orient(mesh, &regions[list[0]], flip)?;
            let now: BTreeSet<HalfEdgeId> = outer
                .iter()
                .chain(holes.iter().flatten())
                .copied()
                .collect();
            let before: BTreeSet<HalfEdgeId> = mesh.face_cycles(f)?.into_iter().flatten().collect();
            if now == before {
                continue;
            }
        }
        let old_outer: BTreeSet<HalfEdgeId> =
            mesh.loop_half_edges(face.outer)?.into_iter().collect();
        changed.push((f, list, flip, face.plane, face.sides.clone(), old_outer));
    }
    for (f, ..) in &changed {
        mesh.detach_face(*f, d)?;
    }

    let mut result = FaceFindingResult::default();
    for (f, list, flip, old_plane, sides, old_outer) in changed {
        let mut oriented = Vec::with_capacity(list.len());
        for &i in &list {
            let (outer, holes) = orient(mesh, &regions[i], flip)?;
            oriented.push((regions[i].area, outer, holes));
        }
        // The piece that keeps the whole old outer boundary keeps the id (hole
        // situation); otherwise the largest piece does (ties: first region).
        let keeper = oriented
            .iter()
            .position(|(_, outer, _)| old_outer.iter().all(|h| outer.contains(h)))
            .unwrap_or_else(|| {
                let mut best = 0;
                for (k, piece) in oriented.iter().enumerate() {
                    if piece.0 > oriented[best].0 {
                        best = k;
                    }
                }
                best
            });
        let (_, k_outer, k_holes) = oriented.remove(keeper);
        let plane = piece_plane(mesh, &old_plane, &k_outer, &k_holes, tol)?;
        mesh.attach_face(f, &k_outer, &k_holes, plane, sides.clone(), tol, d)?;
        let hole_sets: Vec<BTreeSet<HalfEdgeId>> = k_holes
            .iter()
            .map(|h| h.iter().copied().collect())
            .collect();
        for (_, outer, holes) in oriented {
            let plane = piece_plane(mesh, &old_plane, &outer, &holes, tol)?;
            let g = mesh.create_face(&outer, &holes, plane, sides.clone(), tol, d)?;
            let mut twins = BTreeSet::new();
            for &h in &outer {
                twins.insert(mesh.twin_of(h)?);
            }
            if hole_sets.contains(&twins) {
                result.holes.push((f, g));
                result.new_faces.push(g);
            } else {
                result.split_faces.push((f, f, g));
                d.record(MeshDelta::FaceSplit {
                    old: f,
                    new: [f, g],
                });
            }
        }
    }

    // Regions no face covers: new faces when bounded by a seed edge.
    let want = default_face_normal(&input.plane, tol);
    let preferred_flip = want.dot(frame.normal) < 0.0;
    for i in unowned {
        let region = &regions[i];
        let mut seeded = false;
        for &h in &region.outer {
            if input.seeds.contains(&mesh.he(h)?.edge) {
                seeded = true;
                break;
            }
        }
        if !seeded {
            continue;
        }
        if let Some(g) = create_region_face(mesh, region, preferred_flip, tol, d)? {
            result.new_faces.push(g);
        }
    }
    Ok(result)
}

/// Creates the face of an unowned region (docs/03 §4.3 step 3). The front
/// follows the default rule when its half-edges are free, else the opposite
/// one. A half-edge carries one face, so faces sharing an edge must agree on
/// orientation; when both orientations are taken (e.g. the last side of a box
/// whose floor and top both face +Z) the smaller edge-connected group of
/// conflicting faces is re-oriented first, unless that group also holds a face
/// the new one must agree with (docs/03 Decisions). `None` when no orientation
/// works or the region is not planar.
fn create_region_face(
    mesh: &mut HalfEdgeMesh,
    region: &Region,
    preferred_flip: bool,
    tol: &Tolerances,
    d: &mut dyn DeltaSink,
) -> Result<Option<FaceId>, GeometryError> {
    struct Candidate {
        outer: Vec<HalfEdgeId>,
        holes: Vec<Vec<HalfEdgeId>>,
        users: BTreeSet<FaceId>,
        allies: BTreeSet<FaceId>,
    }
    let mut options = Vec::with_capacity(2);
    for flip in [preferred_flip, !preferred_flip] {
        let (outer, holes) = orient(mesh, region, flip)?;
        let mut users = BTreeSet::new();
        let mut allies = BTreeSet::new();
        for &h in outer.iter().chain(holes.iter().flatten()) {
            if let Some(f) = mesh.he(h)?.face {
                users.insert(f);
            }
            if let Some(f) = mesh.he(mesh.twin_of(h)?)?.face {
                allies.insert(f);
            }
        }
        options.push(Candidate {
            outer,
            holes,
            users,
            allies,
        });
    }
    let create = |mesh: &mut HalfEdgeMesh,
                  outer: &[HalfEdgeId],
                  holes: &[Vec<HalfEdgeId>],
                  d: &mut dyn DeltaSink|
     -> Result<Option<FaceId>, GeometryError> {
        let Some(plane) = fit_plane(mesh, outer, holes, tol)? else {
            return Ok(None);
        };
        Ok(mesh
            .create_face(outer, holes, plane, FaceSides::default(), tol, d)
            .ok())
    };
    if let Some(free) = options.iter().find(|o| o.users.is_empty()) {
        return create(mesh, &free.outer, &free.holes, d);
    }
    let mut best: Option<(usize, usize, BTreeSet<FaceId>)> = None;
    for (i, option) in options.iter().enumerate() {
        let group = face_group(mesh, &option.users)?;
        if option.allies.iter().any(|f| group.contains(f)) {
            continue;
        }
        if best.as_ref().is_none_or(|b| group.len() < b.0) {
            best = Some((group.len(), i, group));
        }
    }
    let Some((_, i, group)) = best else {
        return Ok(None);
    };
    reverse_faces(mesh, &group, tol, d)?;
    create(mesh, &options[i].outer, &options[i].holes, d)
}

/// All faces edge-connected to `seeds` (across any shared edge).
pub(crate) fn face_group(
    mesh: &HalfEdgeMesh,
    seeds: &BTreeSet<FaceId>,
) -> Result<BTreeSet<FaceId>, GeometryError> {
    let mut group = seeds.clone();
    let mut queue: VecDeque<FaceId> = seeds.iter().copied().collect();
    while let Some(f) = queue.pop_front() {
        for g in mesh.face_neighbors(f)? {
            if group.insert(g) {
                queue.push_back(g);
            }
        }
    }
    Ok(group)
}

/// Reverses faces in place (ids kept): loops run through the twins, the plane
/// flips, and front/back appearance swap so each side keeps its material.
pub(crate) fn reverse_faces(
    mesh: &mut HalfEdgeMesh,
    faces: &BTreeSet<FaceId>,
    tol: &Tolerances,
    d: &mut dyn DeltaSink,
) -> Result<(), GeometryError> {
    let mut saved = Vec::with_capacity(faces.len());
    for &f in faces {
        let cycles = mesh.face_cycles(f)?;
        let face = mesh.face_ref(f)?;
        let sides = FaceSides {
            material_front: face.sides.material_back,
            material_back: face.sides.material_front,
            uv_front: face.sides.uv_back.clone(),
            uv_back: face.sides.uv_front.clone(),
        };
        saved.push((f, cycles, face.plane.flipped(), sides));
    }
    for &(f, ..) in &saved {
        mesh.detach_face(f, d)?;
    }
    for (f, cycles, plane, sides) in saved {
        let reversed = cycles
            .iter()
            .map(|c| c.iter().rev().map(|&h| mesh.twin_of(h)).collect())
            .collect::<Result<Vec<Vec<HalfEdgeId>>, _>>()?;
        let (outer, holes) = reversed
            .split_first()
            .ok_or_else(|| GeometryError::Invariant(format!("{f} has no loops")))?;
        mesh.attach_face(f, outer, holes, plane, sides, tol, d)?;
    }
    Ok(())
}

/// Reconciles faces with the arrangement of `edges` in `plane` without the
/// closure step (used by `split_face_by_chord`): `faces` are the faces that
/// may change, `seeds` the edges that may bound new faces.
pub(crate) fn rebuild_faces(
    mesh: &mut HalfEdgeMesh,
    plane: Plane,
    edges: &BTreeSet<EdgeId>,
    faces: &BTreeSet<FaceId>,
    seeds: &BTreeSet<EdgeId>,
    tol: &Tolerances,
    d: &mut dyn DeltaSink,
) -> Result<FaceFindingResult, GeometryError> {
    let frame = plane.frame();
    rebuild_plane(
        mesh,
        &RebuildInput {
            plane,
            frame,
            edges,
            faces,
            seeds,
        },
        tol,
        d,
    )
}

/// Traces the boundary of a region given as directed boundary edges (region
/// on the left): the successor of `a → b` is the boundary edge leaving `b`
/// reached first when sweeping clockwise from `b → a`, which also resolves
/// pinch vertices. Returns `None` when the edges do not form closed cycles.
pub(crate) fn boundary_cycles<K: Ord + Copy>(
    boundary: &[(K, K)],
    point: impl Fn(K) -> DVec2,
) -> Option<Vec<Vec<K>>> {
    let mut outgoing: BTreeMap<K, Vec<K>> = BTreeMap::new();
    for &(a, b) in boundary {
        outgoing.entry(a).or_default().push(b);
    }
    let successor = |a: K, b: K| -> Option<K> {
        let pb = point(b);
        let back = sort_angle(point(a) - pb);
        let turn = |c: K| (back - sort_angle(point(c) - pb)).rem_euclid(std::f64::consts::TAU);
        outgoing
            .get(&b)?
            .iter()
            .copied()
            .min_by(|&c1, &c2| turn(c1).total_cmp(&turn(c2)).then(c1.cmp(&c2)))
    };
    let mut used: BTreeSet<(K, K)> = BTreeSet::new();
    let mut cycles = Vec::new();
    for &(a0, b0) in boundary {
        if used.contains(&(a0, b0)) {
            continue;
        }
        let mut cycle = Vec::new();
        let (mut a, mut b) = (a0, b0);
        loop {
            if !used.insert((a, b)) || cycle.len() > boundary.len() {
                return None;
            }
            cycle.push(a);
            let c = successor(a, b)?;
            if (b, c) == (a0, b0) {
                break;
            }
            (a, b) = (b, c);
        }
        cycles.push(cycle);
    }
    Some(cycles)
}

/// Plane for a piece of an existing face: the old plane when every vertex is
/// still within planarity of it (stable across splits), else a fresh fit.
pub(crate) fn piece_plane(
    mesh: &HalfEdgeMesh,
    old: &Plane,
    outer: &[HalfEdgeId],
    holes: &[Vec<HalfEdgeId>],
    tol: &Tolerances,
) -> Result<Plane, GeometryError> {
    let mut points = mesh.cycle_positions(outer)?;
    for hole in holes {
        points.extend(mesh.cycle_positions(hole)?);
    }
    if tol.points_on_plane(old, &points) {
        return Ok(*old);
    }
    fit_plane(mesh, outer, holes, tol)?
        .ok_or_else(|| GeometryError::NotApplicable("face piece is not planar".into()))
}

/// Newell plane of the outer cycle (orientation follows its winding), when
/// every loop vertex lies within planarity of it.
pub(crate) fn fit_plane(
    mesh: &HalfEdgeMesh,
    outer: &[HalfEdgeId],
    holes: &[Vec<HalfEdgeId>],
    tol: &Tolerances,
) -> Result<Option<Plane>, GeometryError> {
    let outer_points = mesh.cycle_positions(outer)?;
    let Some(plane) = Plane::from_polygon(&outer_points, tol) else {
        return Ok(None);
    };
    if !tol.points_on_plane(&plane, &outer_points) {
        return Ok(None);
    }
    for hole in holes {
        if !tol.points_on_plane(&plane, &mesh.cycle_positions(hole)?) {
            return Ok(None);
        }
    }
    Ok(Some(plane))
}

/// A bounded region of a planar arrangement: an outer cycle
/// (counter-clockwise in the frame) and hole cycles (clockwise).
#[derive(Debug, Clone)]
pub(crate) struct Region {
    /// Outer boundary, counter-clockwise in the frame.
    pub(crate) outer: Vec<HalfEdgeId>,
    /// Holes, clockwise in the frame.
    pub(crate) holes: Vec<Vec<HalfEdgeId>>,
    /// Area of the region (outer minus holes), mm².
    pub(crate) area: f64,
}

/// The region's cycles in the requested orientation: as traced (`flip =
/// false`, front along the frame normal) or reversed through the twins.
pub(crate) fn orient(
    mesh: &HalfEdgeMesh,
    region: &Region,
    flip: bool,
) -> Result<(Vec<HalfEdgeId>, Vec<Vec<HalfEdgeId>>), GeometryError> {
    if !flip {
        return Ok((region.outer.clone(), region.holes.clone()));
    }
    let reverse = |cycle: &[HalfEdgeId]| -> Result<Vec<HalfEdgeId>, GeometryError> {
        cycle.iter().rev().map(|&h| mesh.twin_of(h)).collect()
    };
    let outer = reverse(&region.outer)?;
    let holes = region
        .holes
        .iter()
        .map(|h| reverse(h))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((outer, holes))
}

/// Bounded regions of the arrangement formed by `edges` in `frame`
/// (module documentation, steps 2–3).
pub(crate) fn arrangement_regions(
    mesh: &HalfEdgeMesh,
    frame: &PlaneFrame,
    edges: &BTreeSet<EdgeId>,
    tol: &Tolerances,
) -> Result<Vec<Region>, GeometryError> {
    let mut active = edges.clone();
    loop {
        let cycles = trace_cycles(mesh, frame, &active)?;
        let mut interior = BTreeSet::new();
        for cycle in &cycles {
            let mut seen = BTreeSet::new();
            for &h in cycle {
                let e = mesh.he(h)?.edge;
                if !seen.insert(e) {
                    interior.insert(e);
                }
            }
        }
        if interior.is_empty() {
            return build_regions(mesh, frame, cycles, tol);
        }
        for e in interior {
            active.remove(&e);
        }
    }
}

/// All half-edge cycles of the arrangement: the successor of `u → v` is the
/// outgoing edge at `v` immediately clockwise from `v → u`, so every bounded
/// face is traced counter-clockwise and every component's outside clockwise.
fn trace_cycles(
    mesh: &HalfEdgeMesh,
    frame: &PlaneFrame,
    edges: &BTreeSet<EdgeId>,
) -> Result<Vec<Vec<HalfEdgeId>>, GeometryError> {
    let mut points: BTreeMap<VertexId, DVec2> = BTreeMap::new();
    let mut outgoing: BTreeMap<VertexId, Vec<(f64, HalfEdgeId)>> = BTreeMap::new();
    for &e in edges {
        let (h, t) = mesh.edge_half_edges(e)?;
        for x in [h, t] {
            let o = mesh.he(x)?.origin;
            let w = mesh.dest(x)?;
            for v in [o, w] {
                if let std::collections::btree_map::Entry::Vacant(slot) = points.entry(v) {
                    slot.insert(frame.to_2d(mesh.position(v)?));
                }
            }
            let angle = sort_angle(points[&w] - points[&o]);
            outgoing.entry(o).or_default().push((angle, x));
        }
    }
    let mut slot_of: BTreeMap<HalfEdgeId, usize> = BTreeMap::new();
    for list in outgoing.values_mut() {
        list.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        for (i, &(_, h)) in list.iter().enumerate() {
            slot_of.insert(h, i);
        }
    }
    let next = |h: HalfEdgeId| -> Result<HalfEdgeId, GeometryError> {
        let twin = mesh.twin_of(h)?;
        let v = mesh.he(twin)?.origin;
        let list = outgoing
            .get(&v)
            .ok_or_else(|| GeometryError::Invariant("arrangement vertex without edges".into()))?;
        let i = slot_of
            .get(&twin)
            .copied()
            .ok_or_else(|| GeometryError::Invariant("arrangement half-edge not indexed".into()))?;
        Ok(list[(i + list.len() - 1) % list.len()].1)
    };
    let total = slot_of.len();
    let mut visited = BTreeSet::new();
    let mut cycles = Vec::new();
    for &h0 in slot_of.keys() {
        if visited.contains(&h0) {
            continue;
        }
        let mut cycle = Vec::new();
        let mut h = h0;
        loop {
            if !visited.insert(h) || cycle.len() > total {
                return Err(GeometryError::Invariant(
                    "arrangement walk did not close".into(),
                ));
            }
            cycle.push(h);
            h = next(h)?;
            if h == h0 {
                break;
            }
        }
        cycles.push(cycle);
    }
    Ok(cycles)
}

/// Classifies traced cycles into regions with holes (module documentation,
/// step 3).
fn build_regions(
    mesh: &HalfEdgeMesh,
    frame: &PlaneFrame,
    cycles: Vec<Vec<HalfEdgeId>>,
    tol: &Tolerances,
) -> Result<Vec<Region>, GeometryError> {
    let rings = cycles
        .iter()
        .map(|c| cycle_points_2d(mesh, frame, c))
        .collect::<Result<Vec<_>, _>>()?;
    let areas: Vec<f64> = rings.iter().map(|r| polygon::signed_area(r)).collect();
    let positive: Vec<usize> = (0..cycles.len())
        .filter(|&i| areas[i] > tol.length_sq)
        .collect();
    let mut holes_of: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for n in (0..cycles.len()).filter(|&i| areas[i] < -tol.length_sq) {
        let mut best: Option<usize> = None;
        for &p in &positive {
            if areas[p] + tol.length_sq < -areas[n] {
                continue;
            }
            if best.is_some_and(|b| areas[p] >= areas[b]) {
                continue;
            }
            if ring_strictly_inside(&rings[n], &rings[p], tol) {
                best = Some(p);
            }
        }
        if let Some(b) = best {
            holes_of.entry(b).or_default().push(n);
        }
    }
    Ok(positive
        .into_iter()
        .map(|p| {
            let holes = holes_of.remove(&p).unwrap_or_default();
            Region {
                outer: cycles[p].clone(),
                area: areas[p] + holes.iter().map(|&n| areas[n]).sum::<f64>(),
                holes: holes.into_iter().map(|n| cycles[n].clone()).collect(),
            }
        })
        .collect())
}

/// `inner` lies inside `outer`, decided by the first vertex (then edge
/// midpoint) of `inner` that is not on `outer`'s boundary.
fn ring_strictly_inside(inner: &[DVec2], outer: &[DVec2], tol: &Tolerances) -> bool {
    let midpoints = (0..inner.len()).map(|i| (inner[i] + inner[(i + 1) % inner.len()]) * 0.5);
    for p in inner.iter().copied().chain(midpoints) {
        match polygon::point_in_ring(p, outer, tol) {
            PointInPolygon::Inside => return true,
            PointInPolygon::Outside => return false,
            PointInPolygon::Boundary => {}
        }
    }
    false
}

/// Origin positions of a cycle in a frame.
fn cycle_points_2d(
    mesh: &HalfEdgeMesh,
    frame: &PlaneFrame,
    cycle: &[HalfEdgeId],
) -> Result<Vec<DVec2>, GeometryError> {
    Ok(frame.project_all(&mesh.cycle_positions(cycle)?))
}

/// A face's loops in a frame: outer, holes.
pub(crate) fn face_polygon(
    mesh: &HalfEdgeMesh,
    f: FaceId,
    frame: &PlaneFrame,
) -> Result<(Vec<DVec2>, Vec<Vec<DVec2>>), GeometryError> {
    let mut loops = mesh
        .face_cycles(f)?
        .into_iter()
        .map(|c| cycle_points_2d(mesh, frame, &c))
        .collect::<Result<Vec<_>, _>>()?;
    if loops.is_empty() {
        return Err(GeometryError::Invariant(format!("{f} has no loops")));
    }
    let outer = loops.remove(0);
    Ok((outer, loops))
}

/// `p` is strictly inside the polygon `outer` minus `holes`.
pub(crate) fn strictly_inside(
    p: DVec2,
    outer: &[DVec2],
    holes: &[Vec<DVec2>],
    tol: &Tolerances,
) -> bool {
    let hole_refs: Vec<&[DVec2]> = holes.iter().map(Vec::as_slice).collect();
    polygon::point_in_polygon(p, outer, &hole_refs, tol) == PointInPolygon::Inside
}

/// A point strictly inside a region: the centroid of its largest ear-clipping
/// triangle, falling back to points just left of each outer edge's midpoint.
pub(crate) fn region_sample(
    mesh: &HalfEdgeMesh,
    frame: &PlaneFrame,
    region: &Region,
    tol: &Tolerances,
) -> Result<Option<DVec2>, GeometryError> {
    let outer = cycle_points_2d(mesh, frame, &region.outer)?;
    let holes = region
        .holes
        .iter()
        .map(|h| cycle_points_2d(mesh, frame, h))
        .collect::<Result<Vec<_>, _>>()?;
    let hole_refs: Vec<&[DVec2]> = holes.iter().map(Vec::as_slice).collect();
    let inside =
        |p: DVec2| polygon::point_in_polygon(p, &outer, &hole_refs, tol) == PointInPolygon::Inside;

    if let Ok(triangles) = tessellate_polygon_2d(&outer, &holes) {
        let all: Vec<DVec2> = outer
            .iter()
            .chain(holes.iter().flatten())
            .copied()
            .collect();
        let mut best: Option<(f64, DVec2)> = None;
        for t in triangles {
            let [a, b, c] = t.map(|i| all[i as usize]);
            let area = ((b - a).perp_dot(c - a) * 0.5).abs();
            let centroid = (a + b + c) / 3.0;
            if best.is_none_or(|(ba, _)| area > ba) && inside(centroid) {
                best = Some((area, centroid));
            }
        }
        if let Some((_, p)) = best {
            return Ok(Some(p));
        }
    }
    let n = outer.len();
    for i in 0..n {
        let (a, b) = (outer[i], outer[(i + 1) % n]);
        let len = a.distance(b);
        if len <= tol.length {
            continue;
        }
        let left = (b - a).perp() / len;
        for scale in [0.25, 0.01, 1e-4] {
            let p = (a + b) * 0.5 + left * (len * scale).max(tol.length * 4.0);
            if inside(p) {
                return Ok(Some(p));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_area_and_orientation() {
        let ccw = [
            DVec2::new(0.0, 0.0),
            DVec2::new(10.0, 0.0),
            DVec2::new(10.0, 10.0),
            DVec2::new(0.0, 10.0),
        ];
        assert_eq!(signed_area_2d(&ccw), 100.0);
        let tol = Tolerances::default();
        assert_eq!(
            loop_orientation(&ccw, &tol),
            LoopOrientation::CounterClockwise
        );
        let mut cw = ccw;
        cw.reverse();
        assert_eq!(loop_orientation(&cw, &tol), LoopOrientation::Clockwise);
        assert_eq!(
            loop_orientation(&ccw[..2], &tol),
            LoopOrientation::Degenerate
        );
    }

    #[test]
    fn default_normal_prefers_up_then_y_then_x() {
        let tol = Tolerances::default();
        assert_eq!(
            default_face_normal(&Plane::new(-DVec3::Z, 0.0), &tol),
            DVec3::Z
        );
        assert_eq!(
            default_face_normal(&Plane::new(-DVec3::Y, 0.0), &tol),
            DVec3::Y
        );
        assert_eq!(
            default_face_normal(&Plane::new(-DVec3::X, 0.0), &tol),
            DVec3::X
        );
    }
}
