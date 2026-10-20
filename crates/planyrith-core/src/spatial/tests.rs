//! Tests for docs/03-geometry-kernel.md §6 (spatial indexes) and the picking
//! semantics of docs/06 §8, on the fixtures of [`super::fixtures`].

use std::collections::BTreeSet;

use super::fixtures::{Fixture, cube, eid, fid, square, vid};
use super::*;
use crate::ids::Pid;
use crate::math::Plane;

fn tol() -> Tolerances {
    Tolerances::default()
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn face_hits(hits: &[LocalHit]) -> Vec<FaceId> {
    hits.iter()
        .filter_map(|h| match h.kind {
            HitKind::Face(f) => Some(f),
            _ => None,
        })
        .collect()
}

#[test]
fn empty_indexes() {
    let c = ContainerIndex::new();
    assert_eq!(c.edge_count() + c.face_count() + c.vertex_count(), 0);
    let mesh = HalfEdgeMesh::new();
    let everything = Aabb::new(DVec3::splat(-1e6), DVec3::splat(1e6));
    assert!(c.edges_in_aabb(&everything).is_empty());
    assert!(c.faces_in_aabb(&everything).is_empty());
    let ray = Ray::new(DVec3::ZERO, DVec3::X);
    assert!(
        c.ray_cast_local(&mesh, &ray, &HitFilter::default(), &tol())
            .is_empty()
    );
    assert!(c.nearest_vertex(&mesh, DVec3::ZERO, 10.0).is_none());
    let d = DocumentIndex::new();
    assert_eq!(d.occurrence_count(), 0);
    assert!(d.container(ContainerRef::Model).is_none());
    assert!(d.aabb_query(&everything).is_empty());
    assert_eq!(HitFilter::default().proximity, 0.0);
}

#[test]
fn build_indexes_every_element() {
    let mesh = cube(10.0).mesh();
    let index = ContainerIndex::build(&mesh, &tol());
    assert_eq!(index.vertex_count(), 8);
    assert_eq!(index.edge_count(), 12);
    assert_eq!(index.face_count(), 6);
    let everything = mesh.aabb().expanded(1.0);
    assert_eq!(index.edges_in_aabb(&everything).len(), 12);
    assert_eq!(index.faces_in_aabb(&everything).len(), 6);
    assert_eq!(index.vertices_in_aabb(&everything).len(), 8);
}

#[test]
fn ray_through_cube_hits_two_faces_in_order() {
    let mesh = cube(10.0).mesh();
    let index = ContainerIndex::build(&mesh, &tol());
    let ray = Ray::new(DVec3::new(-10.0, 5.0, 5.0), DVec3::X);
    let hits = index.ray_cast_local(&mesh, &ray, &HitFilter::default(), &tol());
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert_eq!(hits[0].kind, HitKind::Face(fid(5)));
    assert!(approx(hits[0].t, 10.0));
    assert_eq!(hits[0].point, DVec3::new(0.0, 5.0, 5.0));
    assert_eq!(hits[1].kind, HitKind::Face(fid(3)));
    assert!(approx(hits[1].t, 20.0));
    assert_eq!(hits[1].point, DVec3::new(10.0, 5.0, 5.0));

    // Pointing away: nothing.
    let away = Ray::new(DVec3::new(-10.0, 5.0, 5.0), -DVec3::X);
    assert!(
        index
            .ray_cast_local(&mesh, &away, &HitFilter::default(), &tol())
            .is_empty()
    );
    // Starting inside: only the exit face.
    let inside = Ray::new(DVec3::splat(5.0), DVec3::Z);
    let hits = index.ray_cast_local(&mesh, &inside, &HitFilter::default(), &tol());
    assert_eq!(face_hits(&hits), vec![fid(1)]);
    assert!(approx(hits[0].t, 5.0));
    // Missing the cube through the gap next to it.
    let miss = Ray::new(DVec3::new(-10.0, 15.0, 5.0), DVec3::X);
    assert!(
        index
            .ray_cast_local(&mesh, &miss, &HitFilter::default(), &tol())
            .is_empty()
    );
}

#[test]
fn max_t_and_kind_filters_apply() {
    let mesh = cube(10.0).mesh();
    let index = ContainerIndex::build(&mesh, &tol());
    let ray = Ray::new(DVec3::new(-10.0, 5.0, 5.0), DVec3::X);
    let near = HitFilter {
        max_t: Some(15.0),
        ..HitFilter::default()
    };
    let hits = index.ray_cast_local(&mesh, &ray, &near, &tol());
    assert_eq!(face_hits(&hits), vec![fid(5)]);
    let no_faces = HitFilter {
        faces: false,
        ..HitFilter::default()
    };
    assert!(
        index
            .ray_cast_local(&mesh, &ray, &no_faces, &tol())
            .is_empty()
    );
}

#[test]
fn picking_tolerance_prefers_vertex_then_edge_then_face() {
    let mesh = cube(10.0).mesh();
    let index = ContainerIndex::build(&mesh, &tol());
    // Aim just inside the top-front-left corner (vertex 4 at (0, 0, 10)).
    let ray = Ray::new(DVec3::new(-10.0, 0.3, 9.7), DVec3::X);

    let wide = HitFilter {
        proximity: 0.5,
        ..HitFilter::default()
    };
    let hits = index.ray_cast_local(&mesh, &ray, &wide, &tol());
    assert_eq!(hits[0].kind, HitKind::Vertex(vid(4)), "{hits:?}");
    assert_eq!(hits[0].point, DVec3::new(0.0, 0.0, 10.0));
    assert!(approx(hits[0].t, 10.0));
    // Then the edges within 0.5 (ids ascending), then the left face; the exit
    // corner (vertex 5 at (10, 0, 10)) forms the same pattern at t = 20.
    let kinds: Vec<HitKind> = hits.iter().map(|h| h.kind).collect();
    assert_eq!(
        kinds,
        vec![
            HitKind::Vertex(vid(4)),
            HitKind::Edge(eid(4)),
            HitKind::Edge(eid(7)),
            HitKind::Edge(eid(8)),
            HitKind::Face(fid(5)),
            HitKind::Vertex(vid(5)),
            HitKind::Edge(eid(5)),
            HitKind::Edge(eid(9)),
            HitKind::Face(fid(3)),
        ]
    );
    // Edge hits snap to the edge itself; the parallel edge 4–5 snaps to its
    // end nearest the ray origin.
    assert_eq!(hits[3].point, DVec3::new(0.0, 0.0, 9.7));
    assert_eq!(hits[1].point, DVec3::new(0.0, 0.0, 10.0));
    assert!(approx(hits[hits.len() - 1].t, 20.0));
    assert!(approx(hits[5].t, 20.0));

    // Tighter: the vertex (0.42 away) drops out, edges at 0.3 still win over the face.
    let tight = HitFilter {
        proximity: 0.35,
        ..HitFilter::default()
    };
    let hits = index.ray_cast_local(&mesh, &ray, &tight, &tol());
    let kinds: Vec<HitKind> = hits.iter().map(|h| h.kind).collect();
    assert_eq!(
        kinds,
        vec![
            HitKind::Edge(eid(7)),
            HitKind::Edge(eid(8)),
            HitKind::Face(fid(5)),
            HitKind::Edge(eid(5)),
            HitKind::Edge(eid(9)),
            HitKind::Face(fid(3)),
        ]
    );

    // No proximity: only faces survive.
    let exact = index.ray_cast_local(&mesh, &ray, &HitFilter::default(), &tol());
    assert_eq!(face_hits(&exact), vec![fid(5), fid(3)]);
    assert_eq!(exact.len(), 2);
}

#[test]
fn hidden_edges_are_skipped_unless_requested() {
    let mut fixture = cube(10.0);
    fixture.hidden_edges = vec![8];
    let mesh = fixture.mesh();
    let index = ContainerIndex::build(&mesh, &tol());
    let ray = Ray::new(DVec3::new(-10.0, 0.3, 5.0), DVec3::X);
    let filter = HitFilter {
        proximity: 0.35,
        faces: false,
        vertices: false,
        ..HitFilter::default()
    };
    // Edge 8 (x = 0) is hidden; its opposite edge 9 (x = 10) is still reported.
    let hits = index.ray_cast_local(&mesh, &ray, &filter, &tol());
    let kinds: Vec<HitKind> = hits.iter().map(|h| h.kind).collect();
    assert_eq!(kinds, vec![HitKind::Edge(eid(9))]);
    let shown = HitFilter {
        include_hidden: true,
        ..filter
    };
    let hits = index.ray_cast_local(&mesh, &ray, &shown, &tol());
    let kinds: Vec<HitKind> = hits.iter().map(|h| h.kind).collect();
    assert_eq!(kinds, vec![HitKind::Edge(eid(8)), HitKind::Edge(eid(9))]);
}

#[test]
fn segment_query_finds_crossing_edges() {
    let mesh = square(10.0).mesh();
    let index = ContainerIndex::build(&mesh, &tol());
    let segment = Segment::new(DVec3::new(5.0, -1.0, 0.0), DVec3::new(5.0, 11.0, 0.0));
    let hits = index.segment_intersections(&mesh, &segment, &tol());
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert_eq!(hits[0].edge, eid(0));
    assert!(approx(hits[0].t_segment, 1.0 / 12.0));
    assert!(approx(hits[0].t_edge, 0.5));
    assert!(hits[0].point.distance(DVec3::new(5.0, 0.0, 0.0)) < 1e-9);
    assert_eq!(hits[1].edge, eid(2));
    assert!(approx(hits[1].t_segment, 11.0 / 12.0));
    assert!(hits[1].point.distance(DVec3::new(5.0, 10.0, 0.0)) < 1e-9);

    // A diagonal through two corners touches both edges at each corner.
    let diagonal = Segment::new(DVec3::new(-1.0, -1.0, 0.0), DVec3::new(11.0, 11.0, 0.0));
    let hits = index.segment_intersections(&mesh, &diagonal, &tol());
    let edges: Vec<EdgeId> = hits.iter().map(|h| h.edge).collect();
    assert_eq!(edges, vec![eid(0), eid(3), eid(1), eid(2)]);
    assert!(approx(hits[0].t_segment, hits[1].t_segment));

    // Above the plane: nothing.
    let lifted = Segment::new(DVec3::new(5.0, -1.0, 1.0), DVec3::new(5.0, 11.0, 1.0));
    assert!(
        index
            .segment_intersections(&mesh, &lifted, &tol())
            .is_empty()
    );
    // Just within tolerance: counts.
    let grazing = Segment::new(DVec3::new(5.0, -1.0, 5e-4), DVec3::new(5.0, 11.0, 5e-4));
    assert_eq!(
        index.segment_intersections(&mesh, &grazing, &tol()).len(),
        2
    );
}

#[test]
fn aabb_queries_return_sorted_ids() {
    let mesh = cube(10.0).mesh();
    let index = ContainerIndex::build(&mesh, &tol());
    let top_corner = Aabb::new(DVec3::new(9.0, 9.0, 9.0), DVec3::new(11.0, 11.0, 11.0));
    assert_eq!(index.vertices_in_aabb(&top_corner), vec![vid(6)]);
    assert_eq!(
        index.edges_in_aabb(&top_corner),
        vec![eid(5), eid(6), eid(10)]
    );
    assert_eq!(
        index.faces_in_aabb(&top_corner),
        vec![fid(1), fid(3), fid(4)]
    );
    let outside = Aabb::new(DVec3::splat(20.0), DVec3::splat(30.0));
    assert!(index.edges_in_aabb(&outside).is_empty());
    assert!(index.faces_in_aabb(&Aabb::EMPTY).is_empty());
}

#[test]
fn nearest_queries_break_ties_by_id() {
    let mesh = cube(10.0).mesh();
    let index = ContainerIndex::build(&mesh, &tol());
    // Equidistant from vertices 0 and 1: the lower id wins.
    let (v, d) = index
        .nearest_vertex(&mesh, DVec3::new(5.0, -1.0, 0.0), 10.0)
        .unwrap();
    assert_eq!(v, vid(0));
    assert!(approx(d, 26.0f64.sqrt()));
    assert!(
        index
            .nearest_vertex(&mesh, DVec3::new(5.0, -1.0, 0.0), 2.0)
            .is_none()
    );
    let (v, d) = index
        .nearest_vertex(&mesh, DVec3::new(10.1, 10.0, 10.0), 1.0)
        .unwrap();
    assert_eq!(v, vid(6));
    assert!(approx(d, 0.1));

    // A probe above the top face centre is equidistant from the four top edges.
    let probe = Segment::new(DVec3::new(5.0, 5.0, 12.0), DVec3::new(5.0, 5.0, 13.0));
    let (e, d) = index
        .nearest_edge_to_segment(&mesh, &probe, 10.0, &tol())
        .unwrap();
    assert_eq!(e, eid(4));
    assert!(approx(d, 29.0f64.sqrt()));
    assert!(
        index
            .nearest_edge_to_segment(&mesh, &probe, 1.0, &tol())
            .is_none()
    );
    // Clearly closest to the vertical edge 1–5.
    let probe = Segment::new(DVec3::new(10.5, 0.0, 2.0), DVec3::new(10.5, 0.0, 8.0));
    let (e, d) = index
        .nearest_edge_to_segment(&mesh, &probe, 10.0, &tol())
        .unwrap();
    assert_eq!(e, eid(9));
    assert!(approx(d, 0.5));
}

#[test]
fn apply_deltas_refits_moved_vertex_incrementally() {
    let mut fixture = cube(10.0);
    let before = fixture.mesh();
    let mut index = ContainerIndex::build(&before, &tol());

    fixture.vertices[6] = DVec3::new(10.0, 10.0, 20.0);
    let after = fixture.mesh();
    let deltas = [MeshDelta::VertexMoved {
        id: vid(6),
        from: DVec3::splat(10.0),
        to: DVec3::new(10.0, 10.0, 20.0),
    }];
    index.apply_deltas(&after, &deltas, &tol());
    // 1 vertex + 3 edges + 3 faces of 26 leaves: below the rebuild threshold.
    assert_eq!(index.changed_since_build, 7);

    let raised = Aabb::new(DVec3::new(9.0, 9.0, 15.0), DVec3::new(11.0, 11.0, 21.0));
    assert_eq!(index.vertices_in_aabb(&raised), vec![vid(6)]);
    assert_eq!(index.edges_in_aabb(&raised), vec![eid(5), eid(6), eid(10)]);
    assert_eq!(index.faces_in_aabb(&raised), vec![fid(1), fid(3), fid(4)]);
    assert_eq!(
        index.nearest_vertex(&after, DVec3::new(10.0, 10.0, 19.0), 2.0),
        Some((vid(6), 1.0))
    );

    // The same result as a fresh build.
    let fresh = ContainerIndex::build(&after, &tol());
    assert_eq!(fresh.edges_in_aabb(&raised), index.edges_in_aabb(&raised));
    let probe = Aabb::new(DVec3::splat(-1.0), DVec3::splat(21.0));
    assert_eq!(fresh.faces_in_aabb(&probe), index.faces_in_aabb(&probe));
}

#[test]
fn apply_deltas_removes_and_adds_leaves() {
    let wire = Fixture {
        vertices: vec![
            DVec3::ZERO,
            DVec3::new(10.0, 0.0, 0.0),
            DVec3::new(10.0, 10.0, 0.0),
            DVec3::new(0.0, 10.0, 0.0),
            DVec3::new(0.0, 20.0, 0.0),
            DVec3::new(10.0, 20.0, 0.0),
            DVec3::new(20.0, 20.0, 0.0),
            DVec3::new(30.0, 20.0, 0.0),
        ],
        edges: vec![(0, 1), (1, 2), (2, 3), (3, 4), (4, 5), (5, 6), (6, 7)],
        faces: Vec::new(),
        hidden_edges: Vec::new(),
    };
    let before = wire.mesh();
    let mut index = ContainerIndex::build(&before, &tol());
    assert_eq!(index.edge_count(), 7);

    // Drop the last edge (ids of the others are unchanged in the fixture).
    let mut shorter = wire.clone();
    shorter.edges.pop();
    let after = shorter.mesh();
    let deltas = [MeshDelta::EdgeRemoved {
        id: eid(6),
        a: DVec3::new(20.0, 20.0, 0.0),
        b: DVec3::new(30.0, 20.0, 0.0),
        flags: EdgeFlags::NONE,
        curve: None,
    }];
    index.apply_deltas(&after, &deltas, &tol());
    assert_eq!(index.edge_count(), 6);
    let far = Aabb::new(DVec3::new(25.0, 19.0, -1.0), DVec3::new(31.0, 21.0, 1.0));
    assert!(index.edges_in_aabb(&far).is_empty());
    assert_eq!(index.changed_since_build, 1);

    // Many changes at once cross the rebuild threshold and reset the counter.
    let deltas: Vec<MeshDelta> = (0..6)
        .map(|k| MeshDelta::EdgeFlagsChanged {
            id: eid(k),
            from: EdgeFlags::NONE,
            to: EdgeFlags::SOFT,
        })
        .chain((0..8).map(|v| MeshDelta::VertexMoved {
            id: vid(v),
            from: DVec3::ZERO,
            to: DVec3::ZERO,
        }))
        .collect();
    index.apply_deltas(&after, &deltas, &tol());
    assert_eq!(index.changed_since_build, 0);
    assert_eq!(index.edge_count(), 6);
    assert_eq!(index.vertex_count(), 8);

    // Deltas that do not touch geometry are no-ops.
    index.apply_deltas(&after, &deltas[..6], &tol());
    assert_eq!(index.changed_since_build, 0);
    index.apply_deltas(&after, &[], &tol());
}

#[test]
fn small_trees_build_and_query() {
    // One face / four edges and a two-edge wire exercise the one- and two-leaf trees.
    let square = square(4.0).mesh();
    let index = ContainerIndex::build(&square, &tol());
    assert_eq!(index.face_count(), 1);
    let down = Ray::new(DVec3::new(1.0, 1.0, 5.0), -DVec3::Z);
    let hits = index.ray_cast_local(&square, &down, &HitFilter::default(), &tol());
    assert_eq!(face_hits(&hits), vec![fid(0)]);
    assert!(approx(hits[0].t, 5.0));
    let outside = Ray::new(DVec3::new(5.0, 1.0, 5.0), -DVec3::Z);
    assert!(
        index
            .ray_cast_local(&square, &outside, &HitFilter::default(), &tol())
            .is_empty()
    );

    let wire = Fixture {
        vertices: vec![
            DVec3::ZERO,
            DVec3::new(10.0, 0.0, 0.0),
            DVec3::new(10.0, 10.0, 0.0),
        ],
        edges: vec![(0, 1), (1, 2)],
        faces: Vec::new(),
        hidden_edges: Vec::new(),
    };
    let mesh = wire.mesh();
    let index = ContainerIndex::build(&mesh, &tol());
    assert_eq!(
        (index.vertex_count(), index.edge_count(), index.face_count()),
        (3, 2, 0)
    );
    let near_first = Aabb::new(DVec3::new(4.0, -1.0, -1.0), DVec3::new(6.0, 1.0, 1.0));
    assert_eq!(index.edges_in_aabb(&near_first), vec![eid(0)]);
    assert_eq!(
        index.nearest_edge_to_segment(
            &mesh,
            &Segment::new(DVec3::new(12.0, 5.0, 0.0), DVec3::new(13.0, 5.0, 0.0)),
            5.0,
            &tol()
        ),
        Some((eid(1), 2.0))
    );
    let across = Segment::new(DVec3::new(5.0, -1.0, 0.0), DVec3::new(5.0, 1.0, 0.0));
    assert_eq!(index.segment_intersections(&mesh, &across, &tol()).len(), 1);
}

/// A synthetic scene: one mesh shown through several occurrences.
struct Scene {
    tol: Tolerances,
    mesh: HalfEdgeMesh,
    occurrences: Vec<Occurrence>,
    hidden_entities: BTreeSet<EntityId>,
    sections: Vec<Plane>,
}

impl Scene {
    fn new(mesh: HalfEdgeMesh) -> Self {
        Scene {
            tol: Tolerances::default(),
            mesh,
            occurrences: vec![Occurrence {
                path: InstancePath::root(),
                container: ContainerRef::Model,
                world: DAffine3::IDENTITY,
                hidden: false,
            }],
            hidden_entities: BTreeSet::new(),
            sections: Vec::new(),
        }
    }

    fn with_instance(mut self, pid: u64, world: DAffine3, hidden: bool) -> Self {
        self.occurrences.push(Occurrence {
            path: InstancePath::root().child(EntityKey::Pid(Pid::new(pid))),
            container: ContainerRef::Model,
            world,
            hidden,
        });
        self
    }
}

impl InstanceProvider for Scene {
    fn tolerances(&self) -> &Tolerances {
        &self.tol
    }

    fn occurrences(&self) -> Vec<Occurrence> {
        self.occurrences.clone()
    }

    fn mesh(&self, container: ContainerRef) -> Option<&HalfEdgeMesh> {
        (container == ContainerRef::Model).then_some(&self.mesh)
    }

    fn is_entity_hidden(&self, _container: ContainerRef, entity: EntityId) -> bool {
        self.hidden_entities.contains(&entity)
    }

    fn active_section_planes(&self, _container: ContainerRef) -> Vec<Plane> {
        self.sections.clone()
    }
}

fn instance_path(pid: u64) -> InstancePath {
    InstancePath::root().child(EntityKey::Pid(Pid::new(pid)))
}

#[test]
fn document_index_casts_through_transformed_occurrences() {
    let scaled = DAffine3::from_translation(DVec3::new(100.0, 0.0, 0.0))
        * DAffine3::from_scale(DVec3::splat(0.5));
    let scene = Scene::new(cube(10.0).mesh()).with_instance(7, scaled, false);
    let index = DocumentIndex::build_from(&scene);
    assert_eq!(index.occurrence_count(), 2);
    assert!(index.container(ContainerRef::Model).is_some());

    let ray = Ray::new(DVec3::new(-10.0, 2.0, 2.0), DVec3::X);
    let hits = index.ray_cast_from(&scene, &ray, &HitFilter::default());
    let summary: Vec<(InstancePath, HitKind, f64)> = hits
        .iter()
        .map(|h| (h.path.instances.clone(), h.kind, h.t))
        .collect();
    assert_eq!(summary.len(), 4, "{summary:?}");
    assert_eq!(summary[0].0, InstancePath::root());
    assert_eq!(summary[0].1, HitKind::Face(fid(5)));
    assert!(approx(summary[0].2, 10.0));
    assert_eq!(summary[1].1, HitKind::Face(fid(3)));
    assert!(approx(summary[1].2, 20.0));
    assert_eq!(summary[2].0, instance_path(7));
    assert_eq!(summary[2].1, HitKind::Face(fid(5)));
    assert!(approx(summary[2].2, 110.0));
    assert!(hits[2].point.distance(DVec3::new(100.0, 2.0, 2.0)) < 1e-9);
    assert_eq!(summary[3].1, HitKind::Face(fid(3)));
    assert!(approx(summary[3].2, 115.0));
    assert_eq!(hits[0].path.target, EntityKey::Id(EntityId::Face(fid(5))));

    // Proximity is converted into the scaled instance's frame and confirmed in world units.
    let corner = Ray::new(DVec3::new(90.0, 0.3, 4.7), DVec3::X);
    let filter = HitFilter {
        proximity: 0.5,
        ..HitFilter::default()
    };
    let hits = index.ray_cast_from(&scene, &corner, &filter);
    assert_eq!(hits[0].kind, HitKind::Vertex(vid(4)), "{hits:?}");
    assert_eq!(hits[0].path.instances, instance_path(7));
    assert_eq!(hits[0].path.target, EntityKey::Id(EntityId::Edge(eid(4))));
    assert!(hits[0].point.distance(DVec3::new(100.0, 0.0, 5.0)) < 1e-9);
    assert!(approx(hits[0].t, 10.0));
    assert!(hits.iter().all(|h| h.path.instances == instance_path(7)));
    // Vertex 4 is 0.42 away in world units; a world proximity of 0.35 excludes it
    // even though the local (scaled) tolerance is conservative.
    let tight = HitFilter {
        proximity: 0.35,
        ..HitFilter::default()
    };
    let hits = index.ray_cast_from(&scene, &corner, &tight);
    assert_eq!(hits[0].kind, HitKind::Edge(eid(7)), "{hits:?}");

    // Broad phase.
    let around_b = Aabb::new(DVec3::new(99.0, -1.0, -1.0), DVec3::new(106.0, 6.0, 6.0));
    let leaves = index.aabb_query(&around_b);
    assert_eq!(leaves.len(), 1);
    assert_eq!(leaves[0].path, instance_path(7));
    assert_eq!(
        index
            .aabb_query(&Aabb::new(DVec3::splat(-5.0), DVec3::splat(200.0)))
            .len(),
        2
    );

    // Deterministic: a second cast is identical.
    assert_eq!(
        index.ray_cast_from(&scene, &ray, &HitFilter::default()),
        index.ray_cast_from(&scene, &ray, &HitFilter::default())
    );
}

#[test]
fn document_index_respects_hidden_and_sections() {
    let moved = DAffine3::from_translation(DVec3::new(50.0, 0.0, 0.0));
    let mut scene = Scene::new(cube(10.0).mesh()).with_instance(3, moved, true);
    scene.hidden_entities.insert(EntityId::Face(fid(5)));
    let index = DocumentIndex::build_from(&scene);
    let ray = Ray::new(DVec3::new(-10.0, 5.0, 5.0), DVec3::X);

    let hits = index.ray_cast_from(&scene, &ray, &HitFilter::default());
    // The hidden instance is skipped; the hidden left face of the root is skipped.
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].kind, HitKind::Face(fid(3)));
    assert_eq!(hits[0].path.instances, InstancePath::root());

    let shown = HitFilter {
        include_hidden: true,
        ..HitFilter::default()
    };
    let hits = index.ray_cast_from(&scene, &ray, &shown);
    assert_eq!(hits.len(), 4);
    assert_eq!(hits[2].path.instances, instance_path(3));
    assert!(approx(hits[2].t, 60.0));

    // An active section plane at x = 5 facing +X cuts everything beyond it.
    scene.sections = vec![Plane::new(DVec3::X, -5.0)];
    let hits = index.ray_cast_from(&scene, &ray, &shown);
    let kinds: Vec<HitKind> = hits.iter().map(|h| h.kind).collect();
    assert_eq!(kinds, vec![HitKind::Face(fid(5)), HitKind::Face(fid(5))]);
    let ignore_sections = HitFilter {
        respect_sections: false,
        ..shown
    };
    assert_eq!(index.ray_cast_from(&scene, &ray, &ignore_sections).len(), 4);
}

#[test]
fn document_index_applies_mesh_deltas_per_container() {
    let mut fixture = cube(10.0);
    let mut scene = Scene::new(fixture.mesh());
    let mut index = DocumentIndex::build_from(&scene);
    fixture.vertices[6] = DVec3::new(10.0, 10.0, 20.0);
    scene.mesh = fixture.mesh();
    let deltas = [Delta::Mesh {
        container: ContainerRef::Model,
        delta: MeshDelta::VertexMoved {
            id: vid(6),
            from: DVec3::splat(10.0),
            to: DVec3::new(10.0, 10.0, 20.0),
        },
    }];
    index.apply_deltas_from(&scene, &deltas);
    let raised = Aabb::new(DVec3::new(9.0, 9.0, 15.0), DVec3::new(11.0, 11.0, 21.0));
    let container = index.container(ContainerRef::Model).unwrap();
    assert_eq!(container.vertices_in_aabb(&raised), vec![vid(6)]);
    // The occurrence box follows the mesh.
    assert_eq!(index.aabb_query(&raised).len(), 1);
    let ray = Ray::new(DVec3::new(5.0, 5.0, 30.0), -DVec3::Z);
    let hits = index.ray_cast_from(&scene, &ray, &HitFilter::default());
    assert_eq!(hits.first().map(|h| h.kind), Some(HitKind::Face(fid(1))));
}

#[test]
fn document_index_builds_from_a_document_root() {
    let mut doc = Document::new();
    *doc.model.mesh_mut() = cube(10.0).mesh();
    let index = DocumentIndex::build(&doc);
    assert_eq!(index.occurrence_count(), 1);
    assert_eq!(
        index.container(ContainerRef::Model).map(|c| c.face_count()),
        Some(6)
    );
    let ray = Ray::new(DVec3::new(5.0, 5.0, 30.0), -DVec3::Z);
    let hits = index.ray_cast(&doc, &ray, &HitFilter::default());
    let kinds: Vec<HitKind> = hits.iter().map(|h| h.kind).collect();
    assert_eq!(kinds, vec![HitKind::Face(fid(1)), HitKind::Face(fid(0))]);
    assert!(approx(hits[0].t, 20.0));
    assert_eq!(
        hits[0].path,
        EntityPath::in_model(EntityKey::Id(EntityId::Face(fid(1))))
    );

    // An empty document indexes nothing.
    let empty = DocumentIndex::build(&Document::new());
    assert_eq!(empty.occurrence_count(), 0);
    assert!(empty.container(ContainerRef::Model).is_some());

    // Delta plumbing through the document wrapper is a no-op for non-mesh deltas.
    let mut index = index;
    index.apply_deltas(&doc, &[]);
    assert_eq!(index.occurrence_count(), 1);
}

#[test]
fn hit_kind_helpers() {
    assert_eq!(HitKind::Vertex(vid(0)).priority(), 0);
    assert_eq!(HitKind::Edge(eid(0)).priority(), 1);
    assert_eq!(HitKind::Face(fid(0)).priority(), 2);
    assert_eq!(HitKind::Vertex(vid(0)).entity(), None);
    assert_eq!(HitKind::Edge(eid(2)).entity(), Some(EntityId::Edge(eid(2))));
    assert!(HitKind::Edge(eid(1)).rank() < HitKind::Edge(eid(2)).rank());
}
