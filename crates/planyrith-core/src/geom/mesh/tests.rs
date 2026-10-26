//! Tests for docs/03-geometry-kernel.md §3–§4 and docs/09 §2, §6 (mesh owner).

use glam::{DAffine3, DVec3};
use proptest::prelude::*;

use super::*;
use crate::geom::{MeshDelta, NoDeltas};

fn tol() -> Tolerances {
    Tolerances::default()
}

fn p(x: f64, y: f64, z: f64) -> DVec3 {
    DVec3::new(x, y, z)
}

/// Adds a closed polyline through `points`.
fn polygon(mesh: &mut HalfEdgeMesh, points: &[DVec3], d: &mut dyn DeltaSink) -> Vec<AddEdgeResult> {
    (0..points.len())
        .map(|i| {
            mesh.add_edge(points[i], points[(i + 1) % points.len()], &tol(), d)
                .unwrap()
        })
        .collect()
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<DVec3> {
    vec![
        p(x0, y0, 0.0),
        p(x1, y0, 0.0),
        p(x1, y1, 0.0),
        p(x0, y1, 0.0),
    ]
}

/// Builds an axis-aligned box from its 12 edges.
fn box_mesh(size: f64, d: &mut dyn DeltaSink) -> HalfEdgeMesh {
    let mut mesh = HalfEdgeMesh::new();
    let c = |i: u8| {
        p(
            if i & 1 != 0 { size } else { 0.0 },
            if i & 2 != 0 { size } else { 0.0 },
            if i & 4 != 0 { size } else { 0.0 },
        )
    };
    let pairs = [
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
    ];
    for (a, b) in pairs {
        mesh.add_edge(c(a), c(b), &tol(), d).unwrap();
    }
    mesh
}

/// An id-independent description of the mesh: sorted vertices, edges (with
/// flags) and faces (loops rotated to start at their smallest position).
fn canonical(mesh: &HalfEdgeMesh) -> String {
    // `+ 0.0` turns -0.0 into 0.0 so signs of zero do not matter.
    let fmt = |v: DVec3| format!("({:.6},{:.6},{:.6})", v.x + 0.0, v.y + 0.0, v.z + 0.0);
    let mut vertices: Vec<String> = mesh.vertices().map(|(_, v)| fmt(v.position)).collect();
    vertices.sort();
    let mut edges: Vec<String> = mesh
        .edges()
        .map(|(e, edge)| {
            let s = mesh.edge_segment(e).unwrap();
            let (a, b) = (fmt(s.a), fmt(s.b));
            let (a, b) = if a < b { (a, b) } else { (b, a) };
            format!("{a}-{b}:{}", edge.flags.bits())
        })
        .collect();
    edges.sort();
    let loop_str = |l: LoopId| {
        let pts: Vec<String> = mesh
            .loop_positions(l)
            .unwrap()
            .into_iter()
            .map(fmt)
            .collect();
        let start = (0..pts.len()).min_by_key(|&i| pts[i].clone()).unwrap();
        let mut rotated = pts[start..].to_vec();
        rotated.extend_from_slice(&pts[..start]);
        rotated.join(" ")
    };
    let mut faces: Vec<String> = mesh
        .faces()
        .map(|(_, face)| {
            let mut holes: Vec<String> = face.holes.iter().map(|&h| loop_str(h)).collect();
            holes.sort();
            format!(
                "[{}] holes {:?} n {}",
                loop_str(face.outer),
                holes,
                fmt(face.plane.normal)
            )
        })
        .collect();
    faces.sort();
    format!("V {vertices:?}\nE {edges:?}\nF {faces:?}")
}

fn undo(mesh: &mut HalfEdgeMesh, deltas: &[MeshDelta]) {
    let inverse: Vec<MeshDelta> = deltas.iter().rev().map(MeshDelta::inverse).collect();
    mesh.replay_all(&inverse, &mut ReplayIds::new(), &tol())
        .unwrap();
}

fn redo(mesh: &mut HalfEdgeMesh, deltas: &[MeshDelta]) {
    mesh.replay_all(deltas, &mut ReplayIds::new(), &tol())
        .unwrap();
}

fn assert_valid(mesh: &HalfEdgeMesh) {
    let violations = mesh.validate(&tol());
    assert!(violations.is_empty(), "{violations:#?}");
}

// ----- types ------------------------------------------------------------------------

#[test]
fn edge_flags_set_operations() {
    let f = EdgeFlags::SOFT | EdgeFlags::SMOOTH;
    assert!(f.contains(EdgeFlags::SOFT));
    assert!(!f.contains(EdgeFlags::HIDDEN));
    assert_eq!(f.with(EdgeFlags::HIDDEN, true).bits(), 0b0111);
    assert_eq!(f.difference(EdgeFlags::SOFT), EdgeFlags::SMOOTH);
    assert_eq!(EdgeFlags::default(), EdgeFlags::NONE);
}

#[test]
fn directed_edge_index_round_trips() {
    let d = DirectedEdgeIndex::reversed(42);
    assert!(d.is_reversed());
    assert_eq!(d.index(), 42);
    assert!(!DirectedEdgeIndex::forward(42).is_reversed());
}

#[test]
fn empty_mesh_accessors() {
    let mesh = HalfEdgeMesh::new();
    assert!(mesh.is_empty());
    assert_eq!(
        mesh.vertex_count() + mesh.edge_count() + mesh.face_count(),
        0
    );
    assert!(mesh.aabb().is_empty());
    assert!(mesh.vertex_star(VertexId::default()).is_empty());
    assert!(matches!(
        mesh.edge_endpoints(EdgeId::default()),
        Err(GeometryError::NotFound(_))
    ));
    assert!(mesh.validate(&tol()).is_empty());
    assert!(!mesh.is_solid());
}

#[test]
fn mesh_serializes_in_named_field_mode() {
    let mut mesh = box_mesh(10.0, &mut NoDeltas);
    let bytes = rmp_serde::to_vec_named(&mesh).unwrap();
    let back: HalfEdgeMesh = rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(canonical(&back), canonical(&mesh));
    mesh = HalfEdgeMesh::new();
    let bytes = rmp_serde::to_vec_named(&mesh).unwrap();
    let back: HalfEdgeMesh = rmp_serde::from_slice(&bytes).unwrap();
    assert!(back.is_empty());
}

// ----- §4.1 / §4.2 ------------------------------------------------------------------

#[test]
fn add_vertex_merges_within_tolerance() {
    let mut mesh = HalfEdgeMesh::new();
    let mut d = Vec::new();
    let a = mesh.add_vertex(p(1.0, 2.0, 3.0), &tol(), &mut d).unwrap();
    let b = mesh
        .add_vertex(p(1.0005, 2.0, 3.0), &tol(), &mut d)
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(d.len(), 1);
    let c = mesh.add_vertex(p(1.01, 2.0, 3.0), &tol(), &mut d).unwrap();
    assert_ne!(a, c);
    assert_eq!(
        mesh.add_vertex(p(2e9, 0.0, 0.0), &tol(), &mut d),
        Err(GeometryError::OutOfRange)
    );
}

#[test]
fn add_edge_rejects_degenerate_and_rolls_back() {
    let mut mesh = HalfEdgeMesh::new();
    let mut d = Vec::new();
    let err = mesh
        .add_edge(p(0.0, 0.0, 0.0), p(0.0005, 0.0, 0.0), &tol(), &mut d)
        .unwrap_err();
    assert_eq!(err.code(), "E1301");
    assert!(mesh.is_empty());
    assert!(d.is_empty());
}

#[test]
fn rectangle_closes_into_one_upward_face() {
    let mut mesh = HalfEdgeMesh::new();
    let results = polygon(&mut mesh, &rect(0.0, 0.0, 100.0, 50.0), &mut NoDeltas);
    assert!(results[..3].iter().all(|r| r.new_faces.is_empty()));
    assert_eq!(results[3].new_faces.len(), 1);
    let f = results[3].new_faces[0];
    assert_eq!(mesh.face_count(), 1);
    assert!(tol().same_direction(mesh.face_normal(f).unwrap(), DVec3::Z));
    assert!((mesh.face_area(f).unwrap() - 5000.0).abs() < 1e-9);
    assert_eq!(mesh.face_vertices(f).unwrap().len(), 4);
    assert!(mesh.face_neighbors(f).unwrap().is_empty());
    assert_valid(&mesh);
}

#[test]
fn chord_splits_a_face_and_keeps_its_id_for_one_piece() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 100.0, 50.0), &mut NoDeltas);
    let f = mesh.faces().next().unwrap().0;
    let mut d = Vec::new();
    let r = mesh
        .add_edge(p(40.0, 0.0, 0.0), p(40.0, 50.0, 0.0), &tol(), &mut d)
        .unwrap();
    assert_eq!(r.split_edges.len(), 2);
    assert_eq!(r.split_faces.len(), 1);
    let (old, a, b) = r.split_faces[0];
    assert_eq!((old, a), (f, f));
    assert_eq!(mesh.face_count(), 2);
    let areas = [mesh.face_area(a).unwrap(), mesh.face_area(b).unwrap()];
    assert!((areas[0] - 3000.0).abs() < 1e-9, "{areas:?}");
    assert!((areas[1] - 2000.0).abs() < 1e-9, "{areas:?}");
    assert!(d.iter().any(|x| matches!(x, MeshDelta::FaceSplit { .. })));
    assert_eq!(mesh.face_neighbors(a).unwrap(), vec![b]);
    assert_valid(&mesh);
}

#[test]
fn crossing_line_splits_edges_and_face() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 100.0, 50.0), &mut NoDeltas);
    let r = mesh
        .add_edge(
            p(50.0, -20.0, 0.0),
            p(50.0, 80.0, 0.0),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
    assert_eq!(r.edges.len(), 3);
    assert_eq!(r.split_faces.len(), 1);
    assert_eq!(mesh.face_count(), 2);
    assert_eq!(mesh.edge_count(), 4 + 2 + 3);
    assert_valid(&mesh);
}

#[test]
fn closed_loop_inside_a_face_makes_a_hole_and_an_inner_face() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 100.0, 100.0), &mut NoDeltas);
    let outer = mesh.faces().next().unwrap().0;
    let results = polygon(&mut mesh, &rect(20.0, 20.0, 40.0, 40.0), &mut NoDeltas);
    let inner = results[3].new_faces[0];
    assert_eq!(mesh.face_count(), 2);
    assert_eq!(mesh.face(outer).unwrap().holes.len(), 1);
    assert!((mesh.face_area(outer).unwrap() - 9600.0).abs() < 1e-9);
    assert!((mesh.face_area(inner).unwrap() - 400.0).abs() < 1e-9);
    assert!(tol().same_direction(mesh.face_normal(inner).unwrap(), DVec3::Z));
    assert_valid(&mesh);

    // Erasing the inner face leaves a real hole.
    mesh.erase_face(inner, false, &tol(), &mut NoDeltas)
        .unwrap();
    assert_eq!(mesh.face(outer).unwrap().holes.len(), 1);
    assert_valid(&mesh);
}

#[test]
fn loop_around_an_existing_face_encloses_it_as_a_hole() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(20.0, 20.0, 40.0, 40.0), &mut NoDeltas);
    let small = mesh.faces().next().unwrap().0;
    let results = polygon(&mut mesh, &rect(0.0, 0.0, 100.0, 100.0), &mut NoDeltas);
    let big = results[3].new_faces[0];
    assert_eq!(mesh.face_count(), 2);
    assert!(mesh.face(small).is_some());
    assert_eq!(mesh.face(big).unwrap().holes.len(), 1);
    assert!((mesh.face_area(big).unwrap() - 9600.0).abs() < 1e-9);
    assert_valid(&mesh);
}

#[test]
fn dangling_line_on_a_face_does_not_split_it() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 100.0, 100.0), &mut NoDeltas);
    let r = mesh
        .add_edge(p(0.0, 50.0, 0.0), p(30.0, 50.0, 0.0), &tol(), &mut NoDeltas)
        .unwrap();
    assert!(r.split_faces.is_empty() && r.new_faces.is_empty());
    assert_eq!(mesh.face_count(), 1);
    assert_valid(&mesh);
    // Completing the chord splits it.
    let r = mesh
        .add_edge(
            p(30.0, 50.0, 0.0),
            p(100.0, 50.0, 0.0),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
    assert_eq!(r.split_faces.len(), 1);
    assert_eq!(mesh.face_count(), 2);
    assert_valid(&mesh);
}

#[test]
fn redrawing_an_edge_recreates_an_erased_face_but_a_stray_edge_does_not() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 10.0, 10.0), &mut NoDeltas);
    let f = mesh.faces().next().unwrap().0;
    mesh.erase_face(f, false, &tol(), &mut NoDeltas).unwrap();
    let r = mesh
        .add_edge(
            p(10.0, 10.0, 0.0),
            p(20.0, 20.0, 0.0),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
    assert!(r.new_faces.is_empty());
    assert_eq!(mesh.face_count(), 0);
    let r = mesh
        .add_edge(p(0.0, 0.0, 0.0), p(10.0, 0.0, 0.0), &tol(), &mut NoDeltas)
        .unwrap();
    assert_eq!(r.new_faces.len(), 1);
    assert_valid(&mesh);
}

#[test]
fn box_from_twelve_edges_is_a_consistent_solid() {
    let mesh = box_mesh(10.0, &mut NoDeltas);
    assert_eq!(mesh.face_count(), 6);
    assert_eq!(mesh.edge_count(), 12);
    assert!(mesh.is_solid());
    assert!((mesh.volume().unwrap().abs() - 1000.0).abs() < 1e-6);
    assert_valid(&mesh);
}

#[test]
fn door_opening_on_a_wall_splits_it_into_two_faces() {
    let mut mesh = HalfEdgeMesh::new();
    // A wall in the XZ plane.
    let wall = vec![
        p(0.0, 0.0, 0.0),
        p(100.0, 0.0, 0.0),
        p(100.0, 0.0, 50.0),
        p(0.0, 0.0, 50.0),
    ];
    polygon(&mut mesh, &wall, &mut NoDeltas);
    let f = mesh.faces().next().unwrap().0;
    assert!(tol().is_parallel(mesh.face_normal(f).unwrap(), DVec3::Y));
    for (a, b) in [
        (p(40.0, 0.0, 0.0), p(40.0, 0.0, 30.0)),
        (p(40.0, 0.0, 30.0), p(60.0, 0.0, 30.0)),
    ] {
        let r = mesh.add_edge(a, b, &tol(), &mut NoDeltas).unwrap();
        assert!(r.split_faces.is_empty());
    }
    let r = mesh
        .add_edge(p(60.0, 0.0, 30.0), p(60.0, 0.0, 0.0), &tol(), &mut NoDeltas)
        .unwrap();
    assert_eq!(r.split_faces.len(), 1);
    assert_eq!(mesh.face_count(), 2);
    // The wall (the larger piece) keeps its id.
    assert!((mesh.face_area(f).unwrap() - (5000.0 - 600.0)).abs() < 1e-9);
    assert_valid(&mesh);
}

#[test]
fn collinear_overlap_reuses_the_existing_edge() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 100.0, 50.0), &mut NoDeltas);
    let r = mesh
        .add_edge(p(50.0, 0.0, 0.0), p(150.0, 0.0, 0.0), &tol(), &mut NoDeltas)
        .unwrap();
    assert_eq!(r.split_edges.len(), 1);
    assert_eq!(r.edges.len(), 2);
    assert_eq!(mesh.edge_count(), 6);
    assert_eq!(mesh.face_count(), 1);
    assert_valid(&mesh);
}

#[test]
fn adjacent_rectangles_share_an_edge_and_merge_when_it_is_erased() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 10.0, 10.0), &mut NoDeltas);
    let a = mesh.faces().next().unwrap().0;
    let results = polygon(&mut mesh, &rect(10.0, 0.0, 20.0, 10.0), &mut NoDeltas);
    let b = results
        .iter()
        .flat_map(|r| r.new_faces.clone())
        .next()
        .unwrap();
    assert!(mesh.face(a).is_some());
    assert_eq!(mesh.face_neighbors(a).unwrap(), vec![b]);
    let shared = mesh
        .edge_between(
            mesh.find_vertex_near(p(10.0, 0.0, 0.0), &tol()).unwrap(),
            mesh.find_vertex_near(p(10.0, 10.0, 0.0), &tol()).unwrap(),
        )
        .unwrap();
    let r = mesh.erase_edge(shared, &tol(), &mut NoDeltas).unwrap();
    assert!(r.faces_merged.is_some());
    assert_eq!(mesh.face_count(), 1);
    let f = mesh.faces().next().unwrap().0;
    assert!((mesh.face_area(f).unwrap() - 200.0).abs() < 1e-9);
    assert_valid(&mesh);
}

#[test]
fn tilted_rectangle_faces_upward() {
    let mut mesh = HalfEdgeMesh::new();
    let pts = vec![
        p(0.0, 0.0, 0.0),
        p(10.0, 0.0, 10.0),
        p(10.0, 10.0, 10.0),
        p(0.0, 10.0, 0.0),
    ];
    polygon(&mut mesh, &pts, &mut NoDeltas);
    let f = mesh.faces().next().unwrap().0;
    assert!(mesh.face_normal(f).unwrap().z > 0.0);
    assert!((mesh.face_area(f).unwrap() - 100.0 * 2f64.sqrt()).abs() < 1e-9);
    assert_valid(&mesh);
}

#[test]
fn erasing_a_hole_edge_heals_the_hole() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 100.0, 100.0), &mut NoDeltas);
    let outer = mesh.faces().next().unwrap().0;
    polygon(&mut mesh, &rect(20.0, 20.0, 40.0, 40.0), &mut NoDeltas);
    let hole_edge = mesh
        .edge_between(
            mesh.find_vertex_near(p(20.0, 20.0, 0.0), &tol()).unwrap(),
            mesh.find_vertex_near(p(40.0, 20.0, 0.0), &tol()).unwrap(),
        )
        .unwrap();
    let r = mesh.erase_edge(hole_edge, &tol(), &mut NoDeltas).unwrap();
    assert!(r.faces_merged.is_some());
    assert_eq!(mesh.face_count(), 1);
    assert!(mesh.face(outer).unwrap().holes.is_empty());
    assert!((mesh.face_area(outer).unwrap() - 10000.0).abs() < 1e-9);
    // The other three hole edges stay as wire edges lying on the face.
    assert_eq!(mesh.edge_count(), 7);
    assert_valid(&mesh);
}

#[test]
fn redrawing_a_box_edge_restores_the_missing_top() {
    let mut mesh = box_mesh(10.0, &mut NoDeltas);
    let top = mesh
        .faces()
        .find(|(_, f)| tol().is_parallel(f.plane.normal, DVec3::Z) && f.plane.d.abs() > 5.0)
        .map(|(id, _)| id)
        .unwrap();
    mesh.erase_face(top, false, &tol(), &mut NoDeltas).unwrap();
    assert!(!mesh.is_solid());
    let r = mesh
        .add_edge(p(0.0, 0.0, 10.0), p(10.0, 0.0, 10.0), &tol(), &mut NoDeltas)
        .unwrap();
    assert_eq!(r.new_faces.len(), 1);
    assert!(mesh.is_solid());
    assert_valid(&mesh);
}

#[test]
fn grid_of_squares_finds_every_cell() {
    let n = 12;
    let mut mesh = HalfEdgeMesh::new();
    for i in 0..=n {
        let c = i as f64 * 10.0;
        let end = n as f64 * 10.0;
        mesh.add_edge(p(c, 0.0, 0.0), p(c, end, 0.0), &tol(), &mut NoDeltas)
            .unwrap();
        mesh.add_edge(p(0.0, c, 0.0), p(end, c, 0.0), &tol(), &mut NoDeltas)
            .unwrap();
    }
    assert_eq!(mesh.face_count(), n * n);
    assert_eq!(mesh.vertex_count(), (n + 1) * (n + 1));
    assert_valid(&mesh);
}

// ----- §4.6 / §4.7 erase ----------------------------------------------------------

#[test]
fn erasing_a_chord_merges_back_into_the_larger_face() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 100.0, 50.0), &mut NoDeltas);
    let before = mesh.face_count();
    let r = mesh
        .add_edge(p(40.0, 0.0, 0.0), p(40.0, 50.0, 0.0), &tol(), &mut NoDeltas)
        .unwrap();
    let chord = r.edges[0];
    let (keeper, _, smaller) = r.split_faces[0];
    let er = mesh.erase_edge(chord, &tol(), &mut NoDeltas).unwrap();
    // The larger piece (which kept the original id) survives.
    assert_eq!(er.faces_merged, Some((keeper, smaller)));
    assert_eq!(mesh.face_count(), before);
    let f = mesh.faces().next().unwrap().0;
    assert!((mesh.face_area(f).unwrap() - 5000.0).abs() < 1e-9);
    // The split vertices stay on the boundary (collinear vertices are kept).
    assert_eq!(mesh.face_vertices(f).unwrap().len(), 6);
    assert_valid(&mesh);
}

#[test]
fn erasing_a_boundary_edge_deletes_the_face() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 10.0, 10.0), &mut NoDeltas);
    let e = mesh.edges().next().unwrap().0;
    let r = mesh.erase_edge(e, &tol(), &mut NoDeltas).unwrap();
    assert_eq!(r.faces_removed.len(), 1);
    assert_eq!(mesh.face_count(), 0);
    assert_eq!(mesh.edge_count(), 3);
    assert!(r.vertices_removed.is_empty());
    assert_valid(&mesh);
}

#[test]
fn erasing_a_box_edge_deletes_both_faces() {
    let mut mesh = box_mesh(10.0, &mut NoDeltas);
    let e = mesh.edges().next().unwrap().0;
    let r = mesh.erase_edge(e, &tol(), &mut NoDeltas).unwrap();
    assert_eq!(r.faces_removed.len(), 2);
    assert_eq!(mesh.face_count(), 4);
    assert!(!mesh.is_solid());
    assert_valid(&mesh);
}

#[test]
fn erase_face_with_orphans_removes_everything() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 10.0, 10.0), &mut NoDeltas);
    let f = mesh.faces().next().unwrap().0;
    let r = mesh.erase_face(f, true, &tol(), &mut NoDeltas).unwrap();
    assert_eq!(r.edges_removed.len(), 4);
    assert!(mesh.is_empty());
}

#[test]
fn explicit_split_and_merge() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 100.0, 50.0), &mut NoDeltas);
    let f = mesh.faces().next().unwrap().0;
    // A chord between two existing corners, added without face finding.
    let a = mesh.find_vertex_near(p(0.0, 0.0, 0.0), &tol()).unwrap();
    let c = mesh.find_vertex_near(p(100.0, 50.0, 0.0), &tol()).unwrap();
    let chord = mesh.new_edge(a, c, EdgeFlags::NONE, &mut NoDeltas).unwrap();
    let [k, g] = mesh
        .split_face_by_chord(f, &[chord], &tol(), &mut NoDeltas)
        .unwrap();
    assert_eq!(k, f);
    assert_eq!(mesh.face_count(), 2);
    assert!((mesh.face_area(g).unwrap() - 2500.0).abs() < 1e-9);
    assert_valid(&mesh);

    let kept = mesh.merge_faces(k, g, &tol(), &mut NoDeltas).unwrap();
    assert_eq!(kept, f);
    assert_eq!(mesh.face_count(), 1);
    assert_eq!(mesh.edge_count(), 4);
    assert_valid(&mesh);
}

// ----- §4.8 transform ---------------------------------------------------------------

#[test]
fn moving_the_top_of_a_box_keeps_it_solid() {
    let mut mesh = box_mesh(10.0, &mut NoDeltas);
    let top: Vec<VertexId> = mesh
        .vertices()
        .filter(|(_, v)| v.position.z > 5.0)
        .map(|(id, _)| id)
        .collect();
    let mut d = Vec::new();
    let r = mesh
        .transform_vertices(
            &top,
            &DAffine3::from_translation(p(0.0, 0.0, 5.0)),
            TransformOptions::default(),
            &tol(),
            &mut d,
        )
        .unwrap();
    assert_eq!(r.merged_vertices, 0);
    assert!(r.folded_faces.is_empty());
    assert!(mesh.is_solid());
    assert!((mesh.volume().unwrap().abs() - 1500.0).abs() < 1e-6);
    assert_valid(&mesh);
    let after = canonical(&mesh);
    undo(&mut mesh, &d);
    assert!((mesh.volume().unwrap().abs() - 1000.0).abs() < 1e-6);
    redo(&mut mesh, &d);
    assert_eq!(canonical(&mesh), after);
}

#[test]
fn lifting_one_corner_auto_folds_the_quad() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 10.0, 10.0), &mut NoDeltas);
    let v = mesh.find_vertex_near(p(10.0, 10.0, 0.0), &tol()).unwrap();
    let r = mesh
        .transform_vertices(
            &[v],
            &DAffine3::from_translation(p(0.0, 0.0, 3.0)),
            TransformOptions::default(),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
    assert_eq!(mesh.face_count(), 2);
    assert_eq!(r.fold_edges.len(), 1);
    let fold = mesh.edge(r.fold_edges[0]).unwrap();
    assert!(
        fold.flags
            .contains(EdgeFlags::HIDDEN | EdgeFlags::SOFT | EdgeFlags::SMOOTH)
    );
    // Shortest diagonal: both diagonals have equal plan length, the one not
    // touching the lifted corner is shorter in 3D.
    let (a, b) = mesh.edge_endpoints(r.fold_edges[0]).unwrap();
    assert!(a != v && b != v);
    assert_valid(&mesh);

    // Without auto-fold the move is refused and nothing changes.
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 10.0, 10.0), &mut NoDeltas);
    let before = canonical(&mesh);
    let w = mesh.find_vertex_near(p(0.0, 0.0, 0.0), &tol()).unwrap();
    let err = mesh.transform_vertices(
        &[w],
        &DAffine3::from_translation(p(0.0, 0.0, 2.0)),
        TransformOptions {
            auto_fold: false,
            check_intersections: false,
        },
        &tol(),
        &mut NoDeltas,
    );
    assert!(err.is_err());
    assert_eq!(canonical(&mesh), before);
}

#[test]
fn moving_a_vertex_onto_another_merges_and_collapses() {
    let mut mesh = HalfEdgeMesh::new();
    // A pentagon-ish loop: moving the extra vertex onto its neighbor leaves a quad.
    let pts = vec![
        p(0.0, 0.0, 0.0),
        p(5.0, 0.0, 0.0),
        p(10.0, 0.0, 0.0),
        p(10.0, 10.0, 0.0),
        p(0.0, 10.0, 0.0),
    ];
    polygon(&mut mesh, &pts, &mut NoDeltas);
    let mid = mesh.find_vertex_near(p(5.0, 0.0, 0.0), &tol()).unwrap();
    let mut d = Vec::new();
    let r = mesh
        .transform_vertices(
            &[mid],
            &DAffine3::from_translation(p(5.0, 0.0, 0.0)),
            TransformOptions::default(),
            &tol(),
            &mut d,
        )
        .unwrap();
    assert_eq!(r.merged_vertices, 1);
    assert_eq!(r.collapsed_edges.len(), 1);
    assert_eq!(mesh.vertex_count(), 4);
    assert_eq!(mesh.edge_count(), 4);
    assert_eq!(mesh.face_count(), 1);
    assert_valid(&mesh);
    undo(&mut mesh, &d);
    assert_eq!(mesh.vertex_count(), 5);
    assert_valid(&mesh);
}

#[test]
fn rotating_a_square_onto_itself_replays_exactly() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(-5.0, -5.0, 5.0, 5.0), &mut NoDeltas);
    // Make the vertices distinguishable through an extra edge.
    mesh.add_edge(p(5.0, 5.0, 0.0), p(8.0, 8.0, 0.0), &tol(), &mut NoDeltas)
        .unwrap();
    let before = canonical(&mesh);
    let square: Vec<VertexId> = mesh
        .vertices()
        .filter(|(_, v)| v.position.x.abs() < 6.0)
        .map(|(id, _)| id)
        .collect();
    let mut d = Vec::new();
    mesh.transform_vertices(
        &square,
        &DAffine3::from_rotation_z(std::f64::consts::FRAC_PI_2),
        TransformOptions::default(),
        &tol(),
        &mut d,
    )
    .unwrap();
    let after = canonical(&mesh);
    assert_ne!(before, after);
    undo(&mut mesh, &d);
    assert_eq!(canonical(&mesh), before);
    redo(&mut mesh, &d);
    assert_eq!(canonical(&mesh), after);
}

// ----- curves ------------------------------------------------------------------------

#[test]
fn curves_track_members_and_drop_when_broken() {
    let mut mesh = HalfEdgeMesh::new();
    let n = 8;
    let pts: Vec<DVec3> = (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            p(10.0 * libm::cos(a), 10.0 * libm::sin(a), 0.0)
        })
        .collect();
    let results = polygon(&mut mesh, &pts, &mut NoDeltas);
    let members: Vec<EdgeId> = results.iter().map(|r| r.edges[0]).collect();
    let c = mesh
        .add_curve(
            CurveDescriptor::Circle {
                center: DVec3::ZERO,
                normal: DVec3::Z,
                radius: 10.0,
                segments: n,
            },
            &members,
            &mut NoDeltas,
        )
        .unwrap();
    assert_eq!(mesh.curve_edges(c).unwrap(), members);
    assert!(mesh.curve_is_intact(c).unwrap());
    assert_valid(&mesh);

    // A line through the circle splits two member edges: the descriptor drops.
    mesh.add_edge(p(-20.0, 0.5, 0.0), p(20.0, 0.5, 0.0), &tol(), &mut NoDeltas)
        .unwrap();
    assert_eq!(mesh.curve_count(), 0);
    assert!(mesh.edges().all(|(_, e)| e.curve.is_none()));
    assert_valid(&mesh);
}

// ----- docs/09 §2 / §6 ----------------------------------------------------------------

#[test]
fn indexed_round_trip_is_exact_and_stable() {
    let mut mesh = box_mesh(10.0, &mut NoDeltas);
    let members: Vec<EdgeId> = mesh.edges().take(1).map(|(e, _)| e).collect();
    mesh.add_curve(
        CurveDescriptor::Freehand {
            points: vec![DVec3::ZERO, DVec3::X],
        },
        &members,
        &mut NoDeltas,
    )
    .unwrap();
    let (indexed, ids) = mesh.to_indexed();
    assert_eq!(ids.faces.len(), 6);
    let back = HalfEdgeMesh::from_indexed(&indexed, &tol()).unwrap();
    assert_eq!(canonical(&back), canonical(&mesh));
    assert_eq!(back.curve_count(), 1);
    let (again, _) = back.to_indexed();
    assert_eq!(again, indexed);

    // A missing plane is recomputed from the loop.
    let mut no_planes = indexed.clone();
    for face in &mut no_planes.faces {
        face.plane = None;
    }
    let rebuilt = HalfEdgeMesh::from_indexed(&no_planes, &tol()).unwrap();
    assert_eq!(canonical(&rebuilt), canonical(&mesh));
}

#[test]
fn from_indexed_rejects_broken_input() {
    let indexed = IndexedMesh {
        vertices: vec![DVec3::ZERO, DVec3::X],
        edges: vec![IndexedEdge {
            a: 0,
            b: 5,
            flags: EdgeFlags::NONE,
            curve: None,
        }],
        faces: vec![],
        curves: vec![],
    };
    assert!(HalfEdgeMesh::from_indexed(&indexed, &tol()).is_err());
}

#[test]
fn triangle_soup_cube_merges_into_six_faces() {
    let s = 10.0;
    let positions: Vec<DVec3> = (0..8u8)
        .map(|i| {
            p(
                if i & 1 != 0 { s } else { 0.0 },
                if i & 2 != 0 { s } else { 0.0 },
                if i & 4 != 0 { s } else { 0.0 },
            )
        })
        .collect();
    // Outward-facing triangles.
    let quads = [
        [0, 2, 3, 1],
        [4, 5, 7, 6],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 4, 6, 2],
        [1, 3, 7, 5],
    ];
    let triangles: Vec<[u32; 3]> = quads
        .iter()
        .flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]])
        .collect();
    let r = HalfEdgeMesh::from_triangles_merged(
        &positions,
        &triangles,
        TriangleMergeOptions::default(),
        &tol(),
    )
    .unwrap();
    assert_eq!(r.merged_faces, 6);
    assert_eq!(r.degenerate_triangles, 0);
    assert_eq!(r.mesh.face_count(), 6);
    assert_eq!(r.mesh.edge_count(), 12);
    assert!(r.mesh.is_solid());
    assert!((r.mesh.volume().unwrap() - 1000.0).abs() < 1e-6);
    assert_valid(&r.mesh);
}

#[test]
fn triangle_soup_softens_shallow_edges() {
    // Two triangles meeting at 10°: kept separate, edge softened.
    let a = 10f64.to_radians();
    let positions = vec![
        p(0.0, 0.0, 0.0),
        p(0.0, 10.0, 0.0),
        p(-10.0, 5.0, 0.0),
        p(10.0 * libm::cos(a), 5.0, 10.0 * libm::sin(a)),
    ];
    let r = HalfEdgeMesh::from_triangles_merged(
        &positions,
        &[[0, 1, 2], [1, 0, 3]],
        TriangleMergeOptions::default(),
        &tol(),
    )
    .unwrap();
    assert_eq!(r.mesh.face_count(), 2);
    let soft = r
        .mesh
        .edges()
        .filter(|(_, e)| e.flags.contains(EdgeFlags::SOFT | EdgeFlags::SMOOTH))
        .count();
    assert_eq!(soft, 1);
}

// ----- undo / redo ----------------------------------------------------------------------

#[test]
fn every_operation_replays_backwards_and_forwards() {
    let mut mesh = HalfEdgeMesh::new();
    let mut steps: Vec<(String, Vec<MeshDelta>)> = Vec::new();
    let mut run = |mesh: &mut HalfEdgeMesh, op: &dyn Fn(&mut HalfEdgeMesh, &mut Vec<MeshDelta>)| {
        let before = canonical(mesh);
        let mut d = Vec::new();
        op(mesh, &mut d);
        steps.push((before, d));
    };
    for (a, b) in [
        (p(0.0, 0.0, 0.0), p(100.0, 0.0, 0.0)),
        (p(100.0, 0.0, 0.0), p(100.0, 60.0, 0.0)),
        (p(100.0, 60.0, 0.0), p(0.0, 60.0, 0.0)),
        (p(0.0, 60.0, 0.0), p(0.0, 0.0, 0.0)),
        (p(50.0, -10.0, 0.0), p(50.0, 70.0, 0.0)),
        (p(10.0, 10.0, 0.0), p(20.0, 10.0, 0.0)),
        (p(20.0, 10.0, 0.0), p(20.0, 20.0, 0.0)),
        (p(20.0, 20.0, 0.0), p(10.0, 10.0, 0.0)),
        (p(0.0, 0.0, 0.0), p(0.0, 0.0, 40.0)),
        (p(0.0, 0.0, 40.0), p(0.0, 60.0, 40.0)),
        (p(0.0, 60.0, 40.0), p(0.0, 60.0, 0.0)),
    ] {
        run(&mut mesh, &|m, d| {
            m.add_edge(a, b, &tol(), d).unwrap();
        });
    }
    let chord = mesh
        .edges()
        .find(|(e, _)| {
            let s = mesh.edge_segment(*e).unwrap();
            (s.a.x - 50.0).abs() < 1e-9
                && (s.b.x - 50.0).abs() < 1e-9
                && s.a.y.min(s.b.y) > -1.0
                && s.a.y.max(s.b.y) < 61.0
        })
        .map(|(e, _)| e)
        .unwrap();
    run(&mut mesh, &|m, d| {
        m.erase_edge(chord, &tol(), d).unwrap();
    });
    let face = mesh.faces().next().unwrap().0;
    run(&mut mesh, &|m, d| {
        m.set_face_sides(
            face,
            FaceSides {
                material_front: Some(crate::ids::MaterialId::default()),
                ..FaceSides::default()
            },
            d,
        )
        .unwrap();
    });
    assert_valid(&mesh);
    let end = canonical(&mesh);

    // Undo everything step by step, checking each intermediate state, then
    // redo. One alias table spans the whole history (ids named by later steps
    // resolve to the elements replay re-created).
    let mut ids = ReplayIds::new();
    for (k, (before, d)) in steps.iter().enumerate().rev() {
        let inverse: Vec<MeshDelta> = d.iter().rev().map(MeshDelta::inverse).collect();
        if let Err(err) = mesh.replay_all(&inverse, &mut ids, &tol()) {
            panic!("undo of step {k} failed: {err}");
        }
        assert_eq!(&canonical(&mesh), before);
        assert_valid(&mesh);
    }
    assert!(mesh.is_empty());
    for (k, (_, d)) in steps.iter().enumerate() {
        if let Err(err) = mesh.replay_all(d, &mut ids, &tol()) {
            panic!("redo of step {k} failed: {err}");
        }
    }
    assert_eq!(canonical(&mesh), end);
}

// ----- property tests (docs/03 §10) -------------------------------------------------

/// A random star-shaped polygon: `n` vertices at increasing angles with
/// random radii (always simple).
fn star_polygon() -> impl Strategy<Value = Vec<DVec3>> {
    (3usize..12).prop_flat_map(|n| {
        prop::collection::vec(5.0f64..50.0, n).prop_map(move |radii| {
            radii
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let a = std::f64::consts::TAU * i as f64 / n as f64;
                    p(r * libm::cos(a), r * libm::sin(a), 0.0)
                })
                .collect()
        })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn random_polygon_yields_exactly_one_face(points in star_polygon()) {
        let mut mesh = HalfEdgeMesh::new();
        let mut d = Vec::new();
        polygon(&mut mesh, &points, &mut d);
        prop_assert_eq!(mesh.face_count(), 1);
        prop_assert!(mesh.validate(&tol()).is_empty());
        let f = mesh.faces().next().unwrap().0;
        let expected: f64 = crate::math::polygon::area(
            &points.iter().map(|q| q.truncate()).collect::<Vec<_>>(),
        );
        prop_assert!((mesh.face_area(f).unwrap() - expected).abs() < 1e-6);
        undo(&mut mesh, &d);
        prop_assert!(mesh.is_empty());
    }

    #[test]
    fn random_chord_splits_and_erasing_it_merges(
        points in star_polygon().prop_filter("needs 4+", |p| p.len() >= 4),
        i in 0usize..100,
        j in 0usize..100,
    ) {
        let n = points.len();
        let a = i % n;
        let b = (a + 2 + j % (n - 3)) % n;
        let mut mesh = HalfEdgeMesh::new();
        polygon(&mut mesh, &points, &mut NoDeltas);
        // Star polygons around the origin: a chord through the origin side may
        // leave the polygon, so only check chords that stay inside it.
        let ring: Vec<_> = points.iter().map(|q| q.truncate()).collect();
        let mid = ((points[a] + points[b]) * 0.5).truncate();
        prop_assume!(crate::math::polygon::point_in_ring(mid, &ring, &tol())
            == crate::math::PointInPolygon::Inside);
        let crosses = (0..n).any(|k| {
            let s = Segment::new(points[k], points[(k + 1) % n]);
            let c = crate::math::intersect::segment_segment(&Segment::new(points[a], points[b]), &s, &tol());
            c.is_proper_crossing(&Segment::new(points[a], points[b]), &s, &tol())
        });
        prop_assume!(!crosses);
        // A chord grazing another vertex (within 0.01 mm) would cut off a
        // sliver below the area tolerance: degenerate input, not a split.
        let chord = Segment::new(points[a], points[b]);
        let grazes = (0..n)
            .filter(|&k| k != a && k != b)
            .any(|k| chord.distance_to_point(points[k]) < 0.01);
        prop_assume!(!grazes);
        let r = mesh.add_edge(points[a], points[b], &tol(), &mut NoDeltas).unwrap();
        prop_assert_eq!(mesh.face_count(), 2);
        prop_assert_eq!(r.split_faces.len(), 1);
        prop_assert!(mesh.validate(&tol()).is_empty());
        mesh.erase_edge(r.edges[0], &tol(), &mut NoDeltas).unwrap();
        prop_assert_eq!(mesh.face_count(), 1);
        prop_assert!(mesh.validate(&tol()).is_empty());
    }
}

#[test]
fn tessellation_of_a_face_with_a_hole_covers_its_area() {
    let mut mesh = HalfEdgeMesh::new();
    polygon(&mut mesh, &rect(0.0, 0.0, 100.0, 100.0), &mut NoDeltas);
    let outer = mesh.faces().next().unwrap().0;
    polygon(&mut mesh, &rect(20.0, 20.0, 40.0, 40.0), &mut NoDeltas);
    // A collinear vertex on the boundary must not produce a sliver.
    mesh.add_edge(
        p(50.0, 0.0, 0.0),
        p(50.0, -10.0, 0.0),
        &tol(),
        &mut NoDeltas,
    )
    .unwrap();
    let tess = crate::geom::tessellate::tessellate_face(&mesh, outer, &tol()).unwrap();
    let n = mesh.face_normal(outer).unwrap();
    let mut area = 0.0;
    for t in &tess.triangles {
        let [a, b, c] = t.map(|i| tess.positions[i as usize]);
        let cross = (b - a).cross(c - a);
        assert!(cross.dot(n) > 0.0, "triangle faces backwards");
        area += cross.length() * 0.5;
    }
    assert!((area - mesh.face_area(outer).unwrap()).abs() < 1e-6);
}

// ----- randomized operation sequences (docs/03 §10 "fuzzed sequences") ------------

#[derive(Debug, Clone)]
enum Op {
    Add([i8; 3], [i8; 3]),
    /// Axis-aligned rectangle: corner, extent along the two in-plane axes,
    /// normal axis (0 = x, 1 = y, 2 = z).
    Rect([i8; 3], [i8; 2], u8),
    EraseEdge(usize),
    EraseFace(usize),
    Move(usize, [i8; 3]),
    /// Push/pull face `n` by a multiple of 50 mm.
    Extrude(usize, i8),
    /// Offset face `n` by a multiple of 25 mm.
    Offset(usize, i8),
}

fn lattice() -> impl Strategy<Value = [i8; 3]> {
    (0i8..5, 0i8..5, prop_oneof![4 => Just(0i8), 1 => 0i8..3]).prop_map(|(x, y, z)| [x, y, z])
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (lattice(), lattice()).prop_map(|(a, b)| Op::Add(a, b)),
        2 => (lattice(), (1i8..4, 1i8..4), 0u8..3).prop_map(|(c, (u, v), n)| Op::Rect(c, [u, v], n)),
        2 => (0usize..64).prop_map(Op::EraseEdge),
        1 => (0usize..64).prop_map(Op::EraseFace),
        1 => (0usize..64, (-1i8..2, -1i8..2, -1i8..2)).prop_map(|(v, (x, y, z))| Op::Move(v, [x, y, z])),
        2 => (0usize..64, prop_oneof![-4i8..0, 1i8..5]).prop_map(|(f, d)| Op::Extrude(f, d)),
        1 => (0usize..64, prop_oneof![-2i8..0, 1i8..3]).prop_map(|(f, d)| Op::Offset(f, d)),
    ]
}

fn at(p: [i8; 3]) -> DVec3 {
    p3(
        f64::from(p[0]) * 100.0,
        f64::from(p[1]) * 100.0,
        f64::from(p[2]) * 100.0,
    )
}

fn p3(x: f64, y: f64, z: f64) -> DVec3 {
    DVec3::new(x, y, z)
}

/// One elementary mesh operation of a fuzz [`Op`], with ids resolved.
#[derive(Debug)]
enum Step {
    Add(DVec3, DVec3),
    EraseEdge(EdgeId),
    EraseFace(FaceId, bool),
    Move(VertexId, DVec3),
    Extrude(FaceId, f64),
    Offset(FaceId, f64),
}

fn steps(mesh: &HalfEdgeMesh, op: &Op) -> Vec<Step> {
    let face = |i: usize| {
        mesh.faces()
            .nth(i % mesh.face_count().max(1))
            .map(|(f, _)| f)
    };
    match *op {
        Op::Add(a, b) => vec![Step::Add(at(a), at(b))],
        Op::Rect(c, [u, v], n) => {
            let (iu, iv) = match n {
                0 => (1, 2),
                1 => (0, 2),
                _ => (0, 1),
            };
            let mut corners = [c; 4];
            corners[1][iu] += u;
            corners[2][iu] += u;
            corners[2][iv] += v;
            corners[3][iv] += v;
            (0..4)
                .map(|k| Step::Add(at(corners[k]), at(corners[(k + 1) % 4])))
                .collect()
        }
        Op::EraseEdge(i) => mesh
            .edges()
            .nth(i % mesh.edge_count().max(1))
            .map(|(e, _)| Step::EraseEdge(e))
            .into_iter()
            .collect(),
        Op::EraseFace(i) => face(i)
            .map(|f| Step::EraseFace(f, i % 2 == 0))
            .into_iter()
            .collect(),
        Op::Move(i, delta) => mesh
            .vertices()
            .nth(i % mesh.vertex_count().max(1))
            .map(|(v, _)| Step::Move(v, at(delta)))
            .into_iter()
            .collect(),
        Op::Extrude(i, k) => face(i)
            .map(|f| Step::Extrude(f, f64::from(k) * 50.0))
            .into_iter()
            .collect(),
        Op::Offset(i, k) => face(i)
            .map(|f| Step::Offset(f, f64::from(k) * 25.0))
            .into_iter()
            .collect(),
    }
}

fn apply_step(
    mesh: &mut HalfEdgeMesh,
    step: &Step,
    d: &mut Vec<MeshDelta>,
) -> Result<(), GeometryError> {
    let tol = tol();
    match *step {
        Step::Add(a, b) => mesh.add_edge(a, b, &tol, d).map(|_| ()),
        Step::EraseEdge(e) => mesh.erase_edge(e, &tol, d).map(|_| ()),
        Step::EraseFace(f, edges) => mesh.erase_face(f, edges, &tol, d).map(|_| ()),
        Step::Move(v, delta) => mesh
            .transform_vertices(
                &[v],
                &DAffine3::from_translation(delta),
                TransformOptions {
                    auto_fold: true,
                    check_intersections: true,
                },
                &tol,
                d,
            )
            .map(|_| ()),
        Step::Extrude(f, k) => crate::geom::extrude::extrude_face(
            mesh,
            f,
            crate::units::Length::mm(k),
            crate::geom::extrude::ExtrudeOptions::default(),
            &tol,
            d,
        )
        .map(|_| ()),
        Step::Offset(f, k) => {
            crate::geom::offset::offset_face_boundary(mesh, f, crate::units::Length::mm(k), &tol, d)
                .map(|_| ())
        }
    }
}

proptest! {
    // `PLANYRITH_FUZZ_CASES=20000` runs a long fuzzing session.
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PLANYRITH_FUZZ_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(96)
    ))]

    /// Every operation either succeeds leaving a valid mesh (all invariants,
    /// edge crossings included) or fails leaving the mesh unchanged; undoing
    /// the whole sequence by replay returns to the empty mesh.
    #[test]
    fn random_operation_sequences_keep_the_mesh_valid(ops in prop::collection::vec(op(), 1..24)) {
        let mut mesh = HalfEdgeMesh::new();
        let mut log: Vec<Vec<MeshDelta>> = Vec::new();
        for op in ops {
            for step in steps(&mesh, &op) {
                let before = canonical(&mesh);
                let mut d = Vec::new();
                match apply_step(&mut mesh, &step, &mut d) {
                    Ok(()) => {
                        let violations = mesh.validate(&tol());
                        prop_assert!(violations.is_empty(), "{:?} ({:?}) broke the mesh: {:#?}", op, step, violations);
                        log.push(d);
                    }
                    Err(_) => {
                        prop_assert_eq!(canonical(&mesh), before, "a failed {:?} ({:?}) changed the mesh", op, step);
                        prop_assert!(d.is_empty());
                    }
                }
            }
        }
        let mut ids = ReplayIds::new();
        for d in log.iter().rev() {
            let inverse: Vec<MeshDelta> = d.iter().rev().map(MeshDelta::inverse).collect();
            mesh.replay_all(&inverse, &mut ids, &tol()).unwrap();
        }
        prop_assert!(mesh.is_empty(), "undoing everything leaves {}", canonical(&mesh));
    }
}
