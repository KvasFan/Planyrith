//! Implements docs/03-geometry-kernel.md §4.14 Solid check and repair: what
//! keeps a container's mesh from being a solid (the printability check of
//! docs/05 §8.1) and the repairs that need no judgement — filling holes,
//! removing wire edges and turning inside-out shells right side out.

use std::collections::BTreeSet;

use glam::{DVec2, DVec3};

use crate::geom::boolean::{Classification, classify_point, piece, stitch};
use crate::geom::planar::reverse_faces;
use crate::geom::{DeltaSink, FaceSides, GeometryError, HalfEdgeMesh};
use crate::ids::{EdgeId, FaceId, HalfEdgeId, VertexId};
use crate::math::Plane;
use crate::tolerance::Tolerances;

/// Why a mesh is or is not a solid.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SolidDiagnosis {
    /// Faces in the mesh.
    pub faces: usize,
    /// Edge-connected groups of faces.
    pub shells: usize,
    /// Edges with a face on one side only.
    pub open_edges: Vec<EdgeId>,
    /// Closed chains of open edges, as the free half-edges in order (a
    /// face filling one would run along them).
    pub holes: Vec<Vec<HalfEdgeId>>,
    /// Edges with no face on either side.
    pub wire_edges: Vec<EdgeId>,
    /// Vertices where faces meet only at the point (two fans).
    pub pinched_vertices: Vec<VertexId>,
    /// Closed shells facing inward that are not voids inside another shell.
    pub inside_out: Vec<BTreeSet<FaceId>>,
    /// Closed shells facing inward inside another shell (hollows; fine).
    pub voids: usize,
}

impl SolidDiagnosis {
    /// `true` when the mesh encloses a volume with every face facing out.
    pub fn is_printable(&self) -> bool {
        self.faces > 0
            && self.open_edges.is_empty()
            && self.wire_edges.is_empty()
            && self.pinched_vertices.is_empty()
            && self.inside_out.is_empty()
    }
}

/// What [`repair`] changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepairReport {
    /// Holes closed with new faces.
    pub holes_filled: usize,
    /// Faces added to close them.
    pub faces_added: usize,
    /// Holes that could not be closed.
    pub holes_left: usize,
    /// Wire edges removed.
    pub wire_edges_removed: usize,
    /// Shells turned right side out.
    pub shells_reversed: usize,
}

/// Which repairs [`repair`] makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairOptions {
    /// Close holes: flat ones with one face, others with triangles.
    pub fill_holes: bool,
    /// Remove edges that bound no face.
    pub remove_wire_edges: bool,
    /// Reverse closed shells that face inward and are not voids.
    pub reverse_inside_out: bool,
}

impl Default for RepairOptions {
    fn default() -> Self {
        RepairOptions {
            fill_holes: true,
            remove_wire_edges: true,
            reverse_inside_out: true,
        }
    }
}

/// Diagnoses `mesh` (docs/03 §4.14).
pub fn diagnose(mesh: &HalfEdgeMesh, tol: &Tolerances) -> Result<SolidDiagnosis, GeometryError> {
    let mut out = SolidDiagnosis {
        faces: mesh.face_count(),
        ..SolidDiagnosis::default()
    };
    let mut free: BTreeSet<HalfEdgeId> = BTreeSet::new();
    for (e, edge) in mesh.edges() {
        let he = mesh.he(edge.he)?;
        let twin = mesh.he(he.twin)?;
        match (he.face, twin.face) {
            (None, None) => out.wire_edges.push(e),
            (Some(_), None) => {
                out.open_edges.push(e);
                free.insert(he.twin);
            }
            (None, Some(_)) => {
                out.open_edges.push(e);
                free.insert(edge.he);
            }
            (Some(_), Some(_)) => {}
        }
    }
    out.holes = trace_holes(mesh, &free)?;
    for (v, _) in mesh.vertices() {
        if fans_at(mesh, v)? > 1 {
            out.pinched_vertices.push(v);
        }
    }
    let shells = shells(mesh)?;
    out.shells = shells.len();
    // Closed shells as stand-alone meshes with their signed volumes.
    let mut closed = Vec::new();
    for shell in shells {
        let open = shell.iter().try_fold(false, |open, &f| {
            Ok::<_, GeometryError>(
                open || mesh
                    .face_edges(f)?
                    .into_iter()
                    .any(|e| out.open_edges.contains(&e)),
            )
        })?;
        if open {
            continue;
        }
        let pieces = shell
            .iter()
            .map(|&f| piece(mesh, f, false))
            .collect::<Result<Vec<_>, _>>()?;
        let Ok(alone) = stitch(&pieces, tol) else {
            continue;
        };
        let Ok(volume) = alone.volume() else {
            continue;
        };
        closed.push((shell, alone, volume));
    }
    for (i, (shell, alone, volume)) in closed.iter().enumerate() {
        if *volume >= 0.0 {
            continue;
        }
        let Some(sample) = alone.vertices().next().map(|(_, v)| v.position) else {
            continue;
        };
        let inside_another = closed.iter().enumerate().any(|(j, (_, other, v))| {
            j != i
                && *v > 0.0
                && matches!(
                    classify_point(other, sample, tol),
                    Ok(Classification::Inside)
                )
        });
        if inside_another {
            out.voids += 1;
        } else {
            out.inside_out.push(shell.clone());
        }
    }
    Ok(out)
}

/// Repairs `mesh` (docs/03 §4.14); each repair is atomic, and one that
/// fails leaves its part as it was (a hole that cannot be closed is counted
/// in `holes_left`).
pub fn repair(
    mesh: &mut HalfEdgeMesh,
    options: RepairOptions,
    tol: &Tolerances,
    d: &mut dyn DeltaSink,
) -> Result<RepairReport, GeometryError> {
    let mut report = RepairReport::default();
    if options.remove_wire_edges {
        for e in diagnose(mesh, tol)?.wire_edges {
            if mesh.edge(e).is_some() {
                mesh.erase_edge(e, tol, d)?;
                report.wire_edges_removed += 1;
            }
        }
    }
    if options.fill_holes {
        for hole in diagnose(mesh, tol)?.holes {
            match mesh.transact(tol, d, |m, log| fill_hole(m, &hole, tol, log)) {
                Ok(faces) => {
                    report.holes_filled += 1;
                    report.faces_added += faces;
                }
                Err(_) => report.holes_left += 1,
            }
        }
    }
    if options.reverse_inside_out {
        for shell in diagnose(mesh, tol)?.inside_out {
            mesh.transact(tol, d, |m, log| reverse_faces(m, &shell, tol, log))?;
            report.shells_reversed += 1;
        }
    }
    Ok(report)
}

/// Edge-connected face groups, in face order.
fn shells(mesh: &HalfEdgeMesh) -> Result<Vec<BTreeSet<FaceId>>, GeometryError> {
    let mut seen: BTreeSet<FaceId> = BTreeSet::new();
    let mut out = Vec::new();
    for (f, _) in mesh.faces() {
        if seen.contains(&f) {
            continue;
        }
        let mut shell = BTreeSet::from([f]);
        let mut stack = vec![f];
        while let Some(g) = stack.pop() {
            for h in mesh.face_neighbors(g)? {
                if shell.insert(h) {
                    stack.push(h);
                }
            }
        }
        seen.extend(shell.iter().copied());
        out.push(shell);
    }
    Ok(out)
}

/// Fans of faces around `v`: outgoing half-edges joined when one face lies
/// between them. Wire edges belong to no fan.
fn fans_at(mesh: &HalfEdgeMesh, v: VertexId) -> Result<usize, GeometryError> {
    let star: Vec<HalfEdgeId> = mesh
        .vertex_star(v)
        .iter()
        .copied()
        .filter(|&h| {
            mesh.he(h)
                .and_then(|he| Ok(he.face.is_some() || mesh.he(he.twin)?.face.is_some()))
                .unwrap_or(false)
        })
        .collect();
    let index = |h: HalfEdgeId| star.iter().position(|&s| s == h);
    let mut parent: Vec<usize> = (0..star.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for (i, &h) in star.iter().enumerate() {
        let he = mesh.he(h)?;
        if he.face.is_none() {
            continue;
        }
        // h and twin(prev(h)) leave v on either side of h's face.
        let other = mesh.twin_of(he.prev)?;
        if let Some(j) = index(other) {
            let (a, b) = (root(&mut parent, i), root(&mut parent, j));
            parent[a] = b;
        }
    }
    let roots: BTreeSet<usize> = (0..star.len()).map(|i| root(&mut parent, i)).collect();
    Ok(roots.len())
}

/// Chains of free half-edges (open sides of open edges) closed into cycles.
/// At a vertex with several ways on, the loop link is preferred, then the
/// lowest id; chains that do not close are left out.
fn trace_holes(
    mesh: &HalfEdgeMesh,
    free: &BTreeSet<HalfEdgeId>,
) -> Result<Vec<Vec<HalfEdgeId>>, GeometryError> {
    let mut used: BTreeSet<HalfEdgeId> = BTreeSet::new();
    let mut out = Vec::new();
    for &start in free {
        if used.contains(&start) {
            continue;
        }
        let origin = mesh.he(start)?.origin;
        let mut cycle = vec![start];
        let mut taken: BTreeSet<HalfEdgeId> = BTreeSet::from([start]);
        let mut current = start;
        let closed = loop {
            let at = mesh.dest(current)?;
            if at == origin {
                break true;
            }
            let link = mesh.he(current)?.next;
            let mut ways: Vec<HalfEdgeId> = mesh
                .vertex_star(at)
                .iter()
                .copied()
                .filter(|h| free.contains(h) && !used.contains(h) && !taken.contains(h))
                .collect();
            ways.sort_unstable();
            let Some(&next) = ways.iter().find(|&&h| h == link).or(ways.first()) else {
                break false;
            };
            cycle.push(next);
            taken.insert(next);
            current = next;
        };
        if closed && cycle.len() >= 3 {
            used.extend(cycle.iter().copied());
            out.push(cycle);
        }
    }
    Ok(out)
}

/// Closes one hole: a flat hole gets one face, any other a fan of
/// triangles from ear clipping in its best-fit plane. Returns faces added.
fn fill_hole(
    mesh: &mut HalfEdgeMesh,
    cycle: &[HalfEdgeId],
    tol: &Tolerances,
    d: &mut dyn DeltaSink,
) -> Result<usize, GeometryError> {
    // The back of a lone face is not a hole: closing it would put a second
    // face on the same corners.
    let mut beside: BTreeSet<FaceId> = BTreeSet::new();
    for &h in cycle {
        if let Some(f) = mesh.he(mesh.twin_of(h)?)?.face {
            beside.insert(f);
        }
    }
    if let [f] = beside.iter().copied().collect::<Vec<_>>()[..]
        && mesh.face_cycles(f)?.iter().map(Vec::len).sum::<usize>() == cycle.len()
    {
        return Err(GeometryError::NotApplicable(
            "a lone face has no inside; push/pull it into a solid".into(),
        ));
    }
    let vertices = cycle
        .iter()
        .map(|&h| mesh.he(h).map(|he| he.origin))
        .collect::<Result<Vec<_>, _>>()?;
    let points = vertices
        .iter()
        .map(|&v| mesh.position(v))
        .collect::<Result<Vec<_>, _>>()?;
    // Newell's normal: the side from which the cycle runs counter-clockwise.
    let mut normal = DVec3::ZERO;
    for (i, a) in points.iter().enumerate() {
        let b = points[(i + 1) % points.len()];
        normal += DVec3::new(
            (a.y - b.y) * (a.z + b.z),
            (a.z - b.z) * (a.x + b.x),
            (a.x - b.x) * (a.y + b.y),
        );
    }
    let normal = normal.normalize_or_zero();
    if normal == DVec3::ZERO {
        return Err(GeometryError::Degenerate(
            "the hole encloses no area".into(),
        ));
    }
    let center = points.iter().copied().sum::<DVec3>() / points.len() as f64;
    let plane = Plane::from_point_normal(center, normal);
    if points
        .iter()
        .all(|&p| plane.signed_distance(p).abs() <= tol.planarity)
    {
        mesh.create_face(cycle, &[], plane, FaceSides::default(), tol, d)?;
        return Ok(1);
    }
    let frame = plane.frame();
    let flat: Vec<DVec2> = points.iter().map(|&p| frame.to_2d(p)).collect();
    let data: Vec<f64> = flat.iter().flat_map(|p| [p.x, p.y]).collect();
    let triangles = earcutr::earcut(&data, &[], 2)
        .map_err(|e| GeometryError::NonRobust(format!("cannot triangulate the hole: {e:?}")))?;
    if triangles.len() / 3 != points.len() - 2 {
        return Err(GeometryError::NonRobust(
            "the hole folds over itself; close it in flat pieces".into(),
        ));
    }
    let mut added = 0;
    for &[mut a, b, mut c] in triangles.as_chunks::<3>().0 {
        let area = (flat[b] - flat[a]).perp_dot(flat[c] - flat[a]);
        if area < 0.0 {
            std::mem::swap(&mut a, &mut c);
        }
        let corners = [vertices[a], vertices[b], vertices[c]];
        let mut loop_edges = Vec::with_capacity(3);
        for k in 0..3 {
            let (from, to) = (corners[k], corners[(k + 1) % 3]);
            let h = match mesh.half_edge_from_to(from, to) {
                Some(h) => h,
                None => {
                    let e = mesh.new_edge(from, to, crate::geom::EdgeFlags::NONE, d)?;
                    let (h, _) = mesh.edge_half_edges(e)?;
                    if mesh.he(h)?.origin == from {
                        h
                    } else {
                        mesh.twin_of(h)?
                    }
                }
            };
            loop_edges.push(h);
        }
        let [pa, pb, pc] = corners.map(|v| mesh.position(v).unwrap_or_default());
        let n = (pb - pa).cross(pc - pa).normalize_or_zero();
        if n == DVec3::ZERO {
            return Err(GeometryError::Degenerate("a sliver in the hole".into()));
        }
        mesh.create_face(
            &loop_edges,
            &[],
            Plane::from_point_normal(pa, n),
            FaceSides::default(),
            tol,
            d,
        )?;
        added += 1;
    }
    Ok(added)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::geom::NoDeltas;

    fn tol() -> Tolerances {
        Tolerances::default()
    }

    /// A box of side `s` at `origin` from its 12 edges.
    fn add_box(mesh: &mut HalfEdgeMesh, origin: DVec3, s: f64) {
        let c = |i: u8| {
            origin
                + DVec3::new(
                    if i & 1 != 0 { s } else { 0.0 },
                    if i & 2 != 0 { s } else { 0.0 },
                    if i & 4 != 0 { s } else { 0.0 },
                )
        };
        for (a, b) in [
            (0, 1),
            (1, 3),
            (3, 2),
            (2, 0),
            (4, 5),
            (5, 7),
            (7, 6),
            (6, 4),
            (0, 4),
            (1, 5),
            (3, 7),
            (2, 6),
        ] {
            mesh.add_edge(c(a), c(b), &tol(), &mut NoDeltas).unwrap();
        }
    }

    /// Faces of `mesh` with every corner inside `[lo, hi]³`.
    fn faces_within(mesh: &HalfEdgeMesh, lo: f64, hi: f64) -> BTreeSet<FaceId> {
        mesh.faces()
            .filter(|(f, _)| {
                mesh.face_vertices(*f).unwrap().iter().all(|&v| {
                    let p = mesh.position(v).unwrap();
                    p.min_element() >= lo - 1e-9 && p.max_element() <= hi + 1e-9
                })
            })
            .map(|(f, _)| f)
            .collect()
    }

    /// Orients the faces of `shell` outward (`out`) or inward.
    fn orient(mesh: &mut HalfEdgeMesh, shell: &BTreeSet<FaceId>, out: bool) {
        let pieces: Vec<_> = shell
            .iter()
            .map(|&f| piece(mesh, f, false).unwrap())
            .collect();
        let volume = stitch(&pieces, &tol()).unwrap().volume().unwrap();
        if (volume > 0.0) != out {
            reverse_faces(mesh, shell, &tol(), &mut NoDeltas).unwrap();
        }
    }

    /// A box of side `s` facing outward.
    fn cube(s: f64) -> HalfEdgeMesh {
        let mut mesh = HalfEdgeMesh::new();
        add_box(&mut mesh, DVec3::ZERO, s);
        let all = mesh.faces().map(|(f, _)| f).collect();
        orient(&mut mesh, &all, true);
        mesh
    }

    fn face_with_normal(mesh: &HalfEdgeMesh, n: DVec3) -> FaceId {
        mesh.faces()
            .find(|(_, f)| f.plane.normal.dot(n) > 0.99)
            .map(|(f, _)| f)
            .unwrap()
    }

    #[test]
    fn a_closed_box_is_printable() {
        let mesh = cube(10.0);
        assert!(mesh.is_solid());
        let report = diagnose(&mesh, &tol()).unwrap();
        assert!(report.is_printable(), "{report:?}");
        assert_eq!((report.faces, report.shells), (6, 1));
    }

    #[test]
    fn a_missing_face_is_one_hole_and_filling_restores_the_solid() {
        let mut mesh = cube(10.0);
        let top = face_with_normal(&mesh, DVec3::Z);
        mesh.erase_face(top, false, &tol(), &mut NoDeltas).unwrap();
        let report = diagnose(&mesh, &tol()).unwrap();
        assert!(!report.is_printable());
        assert_eq!((report.open_edges.len(), report.holes.len()), (4, 1));
        let done = repair(&mut mesh, RepairOptions::default(), &tol(), &mut NoDeltas).unwrap();
        assert_eq!((done.holes_filled, done.faces_added), (1, 1));
        assert!(mesh.is_solid());
        assert!((mesh.volume().unwrap() - 1000.0).abs() < 1e-6);
        assert!(mesh.validate(&tol()).is_empty());
    }

    #[test]
    fn a_bent_hole_closes_with_triangles() {
        let mut mesh = cube(10.0);
        for n in [DVec3::Z, DVec3::X] {
            let f = face_with_normal(&mesh, n);
            mesh.erase_face(f, false, &tol(), &mut NoDeltas).unwrap();
        }
        // The shared edge of the two missing faces is now a wire edge.
        let before = diagnose(&mesh, &tol()).unwrap();
        assert_eq!(before.wire_edges.len(), 1);
        let done = repair(&mut mesh, RepairOptions::default(), &tol(), &mut NoDeltas).unwrap();
        assert_eq!(done.wire_edges_removed, 1);
        assert_eq!(done.holes_filled, 1);
        assert_eq!(
            done.faces_added, 4,
            "a six-sided bent hole takes four triangles"
        );
        assert!(mesh.is_solid(), "{:?}", diagnose(&mesh, &tol()).unwrap());
        // Triangles across the bent hole cut a wedge off the box: at most
        // the prism between the two missing faces (half the box).
        let volume = mesh.volume().unwrap();
        assert!((500.0 - 1e-6..1000.0).contains(&volume), "{volume}");
        assert!(mesh.validate(&tol()).is_empty());
    }

    #[test]
    fn an_inside_out_box_is_reversed_but_a_void_is_kept() {
        let mut mesh = cube(10.0);
        let all: BTreeSet<FaceId> = mesh.faces().map(|(f, _)| f).collect();
        reverse_faces(&mut mesh, &all, &tol(), &mut NoDeltas).unwrap();
        assert!(mesh.volume().unwrap() < 0.0);
        let report = diagnose(&mesh, &tol()).unwrap();
        assert_eq!(report.inside_out.len(), 1);
        let done = repair(&mut mesh, RepairOptions::default(), &tol(), &mut NoDeltas).unwrap();
        assert_eq!(done.shells_reversed, 1);
        assert!(mesh.volume().unwrap() > 0.0);

        // A hollow box: the inner shell faces inward on purpose.
        let mut hollow = cube(30.0);
        add_box(&mut hollow, DVec3::splat(10.0), 10.0);
        let inner = faces_within(&hollow, 10.0, 20.0);
        assert_eq!(inner.len(), 6);
        orient(&mut hollow, &inner, false);
        let report = diagnose(&hollow, &tol()).unwrap();
        assert_eq!((report.shells, report.voids), (2, 1), "{report:?}");
        assert!(report.is_printable());
    }

    #[test]
    fn a_lone_face_is_left_open() {
        let mut mesh = HalfEdgeMesh::new();
        let corners = [
            DVec3::ZERO,
            DVec3::new(10.0, 0.0, 0.0),
            DVec3::new(10.0, 10.0, 0.0),
            DVec3::new(0.0, 10.0, 0.0),
        ];
        for i in 0..4 {
            mesh.add_edge(corners[i], corners[(i + 1) % 4], &tol(), &mut NoDeltas)
                .unwrap();
        }
        assert_eq!(diagnose(&mesh, &tol()).unwrap().holes.len(), 1);
        let done = repair(&mut mesh, RepairOptions::default(), &tol(), &mut NoDeltas).unwrap();
        assert_eq!((done.holes_filled, done.holes_left), (0, 1));
        assert_eq!(mesh.face_count(), 1);
    }

    #[test]
    fn faces_touching_at_a_corner_are_pinched() {
        let mut mesh = cube(10.0);
        add_box(&mut mesh, DVec3::splat(10.0), 10.0);
        let report = diagnose(&mesh, &tol()).unwrap();
        assert_eq!(report.pinched_vertices.len(), 1);
        assert!(!report.is_printable());
        assert_eq!(report.shells, 2);
    }
}
