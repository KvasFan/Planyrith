//! Fits source face planes and retains warped polygons as facets (docs/09 §5).
use super::{Result, conflicts::Conflicts};
use planyrith_core::{
    geom::{
        EdgeFlags,
        mesh::{DirectedEdgeIndex, IndexedEdge, IndexedFace, IndexedMesh},
        tessellate::tessellate_polygon_2d,
    },
    math::Plane,
    tolerance::Tolerances,
};
use std::collections::{BTreeMap, BTreeSet};

/// What [`prepare`] did to the source faces.
pub(super) struct Prepared {
    /// The source face of each prepared face.
    pub sources: Vec<usize>,
    /// Warped source faces retained as facets.
    pub warped: usize,
    /// Self-crossing or self-touching source faces retained as facets.
    pub crossing: usize,
    /// Facets too thin for the kernel, retained as boundary wires.
    pub degenerate: usize,
    /// Conflicts of the prepared mesh.
    pub conflicts: Conflicts,
}

type EdgeIndex = BTreeMap<(u32, u32), u32>;

fn rings(mesh: &IndexedMesh, face: &IndexedFace) -> Vec<Vec<u32>> {
    face.loops
        .iter()
        .map(|ring| {
            ring.iter()
                .map(|edge| {
                    let e = mesh.edges[edge.index() as usize];
                    if edge.is_reversed() { e.b } else { e.a }
                })
                .collect()
        })
        .collect()
}

/// Facets of a polygon with holes, keeping its vertices: `None` when it
/// cannot be triangulated. Returns the facets and the slivers left out.
fn facets(
    mesh: &mut IndexedMesh,
    edges: &mut EdgeIndex,
    face: &IndexedFace,
    rings: &[Vec<u32>],
    plane: Plane,
    tol: &Tolerances,
) -> Option<(Vec<IndexedFace>, usize)> {
    let frame = plane.frame();
    let projected = rings
        .iter()
        .map(|ring| {
            ring.iter()
                .map(|i| frame.to_2d(mesh.vertices[*i as usize]))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let triangles = tessellate_polygon_2d(&projected[0], &projected[1..]).ok()?;
    if triangles.is_empty() {
        return None;
    }
    let vertices = rings.iter().flatten().copied().collect::<Vec<_>>();
    let mut pieces = Vec::new();
    let mut slivers = 0;
    for triangle in triangles {
        let mut ids = triangle.map(|i| vertices[i as usize]);
        let [a, b, c] = ids.map(|i| mesh.vertices[i as usize]);
        if (b - a).cross(c - a).dot(plane.normal) < 0.0 {
            ids.swap(1, 2);
        }
        let [a, b, c] = ids.map(|i| mesh.vertices[i as usize]);
        let normal = (b - a).cross(c - a);
        if normal.length() * 0.5 <= tol.length_sq {
            // Keep source boundary edges/vertices as wire geometry. The
            // kernel cannot represent a surface below its area tolerance.
            slivers += 1;
            continue;
        }
        let mut piece = face.clone();
        piece.plane = Some(Plane::from_point_normal(a, normal));
        let mut ring = Vec::new();
        for i in 0..3 {
            let (a, b) = (ids[i], ids[(i + 1) % 3]);
            let index = *edges.entry((a.min(b), a.max(b))).or_insert_with(|| {
                let index = mesh.edges.len() as u32;
                mesh.edges.push(IndexedEdge {
                    a,
                    b,
                    flags: EdgeFlags::SOFT | EdgeFlags::SMOOTH,
                    curve: None,
                });
                index
            });
            ring.push(if mesh.edges[index as usize].a == a {
                DirectedEdgeIndex::forward(index)
            } else {
                DirectedEdgeIndex::reversed(index)
            });
        }
        piece.loops = vec![ring];
        pieces.push(piece);
    }
    Some((pieces, slivers))
}

fn self_conflicting(face: &IndexedFace, conflicts: &Conflicts) -> bool {
    let uses = face.loops.iter().flatten().copied().collect::<Vec<_>>();
    let edges = uses
        .iter()
        .map(|e| e.index() as usize)
        .collect::<BTreeSet<_>>();
    // A ring can pass along one edge twice, which the kernel cannot hold.
    uses.len() != uses.iter().copied().collect::<BTreeSet<_>>().len()
        || edges
            .iter()
            .any(|e| !conflicts.edges[*e].is_disjoint(&edges))
}

pub(super) fn prepare(mesh: &mut IndexedMesh, tol: &Tolerances) -> Result<Prepared> {
    let mut sources = Vec::new();
    let mut warped = 0;
    let mut crossing = 0;
    let mut degenerate = 0;
    let mut edges = mesh
        .edges
        .iter()
        .enumerate()
        .map(|(i, e)| ((e.a.min(e.b), e.a.max(e.b)), i as u32))
        .collect::<EdgeIndex>();
    for (source, mut face) in std::mem::take(&mut mesh.faces).into_iter().enumerate() {
        let rings = rings(mesh, &face);
        let points = rings
            .iter()
            .flatten()
            .map(|i| mesh.vertices[*i as usize])
            .collect::<Vec<_>>();
        let original = face.plane.ok_or("source face plane missing")?;
        if points.iter().all(|p| original.contains(*p, tol)) {
            sources.push(source);
            mesh.faces.push(face);
            continue;
        }
        let mut fitted = Plane::fit(&points, tol).ok_or("source face is degenerate")?;
        if fitted.normal.dot(original.normal) < 0.0 {
            fitted.normal = -fitted.normal;
            fitted.d = -fitted.d;
        }
        if points.iter().all(|p| fitted.contains(*p, tol)) {
            face.plane = Some(fitted);
            sources.push(source);
            mesh.faces.push(face);
            continue;
        }
        let (pieces, slivers) = facets(mesh, &mut edges, &face, &rings, fitted, tol)
            .ok_or("warped source face could not be triangulated")?;
        degenerate += slivers;
        sources.extend(std::iter::repeat_n(source, pieces.len()));
        mesh.faces.extend(pieces);
        warped += 1;
    }
    // No grouping separates edges of one face: a ring crossing itself or
    // another ring becomes facets, and a facet whose vertex lies on its
    // opposite side keeps only its wires. Each pass only shrinks faces.
    let conflicts = loop {
        let conflicts = super::conflicts::detect(mesh, tol);
        if !mesh.faces.iter().any(|f| self_conflicting(f, &conflicts)) {
            break conflicts;
        }
        let faces = std::mem::take(&mut mesh.faces);
        let old_sources = std::mem::take(&mut sources);
        for (face, source) in faces.into_iter().zip(old_sources) {
            if !self_conflicting(&face, &conflicts) {
                mesh.faces.push(face);
                sources.push(source);
                continue;
            }
            let rings = rings(mesh, &face);
            let split = (face.loops.iter().map(Vec::len).sum::<usize>() > 3)
                .then_some(face.plane)
                .flatten()
                .and_then(|plane| facets(mesh, &mut edges, &face, &rings, plane, tol));
            let Some((pieces, slivers)) = split else {
                degenerate += 1;
                continue;
            };
            degenerate += slivers;
            sources.extend(std::iter::repeat_n(source, pieces.len()));
            mesh.faces.extend(pieces);
            crossing += 1;
        }
    };
    Ok(Prepared {
        sources,
        warped,
        crossing,
        degenerate,
        conflicts,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use glam::DVec3;
    use planyrith_core::geom::FaceSides;

    fn loops(vertices: Vec<DVec3>, rings: &[&[u32]]) -> IndexedMesh {
        let mut mesh = IndexedMesh {
            vertices,
            ..Default::default()
        };
        let mut loops = Vec::new();
        for ring in rings {
            let mut directed = Vec::new();
            for (i, a) in ring.iter().enumerate() {
                directed.push(DirectedEdgeIndex::forward(mesh.edges.len() as u32));
                mesh.edges.push(IndexedEdge {
                    a: *a,
                    b: ring[(i + 1) % ring.len()],
                    flags: EdgeFlags::NONE,
                    curve: None,
                });
            }
            loops.push(directed);
        }
        mesh.faces.push(IndexedFace {
            loops,
            plane: Some(Plane::XY),
            sides: FaceSides::default(),
        });
        mesh
    }

    #[test]
    fn a_sliver_above_area_tolerance_keeps_only_its_wires() {
        // The apex lies on the opposite side within the length tolerance.
        let mut mesh = loops(
            vec![
                DVec3::new(0.0, 0.0, 0.0),
                DVec3::new(13.0, 0.0, 0.0),
                DVec3::new(6.0, 9e-5, 0.0),
            ],
            &[&[0, 1, 2]],
        );
        let prepared = prepare(&mut mesh, &Tolerances::default()).unwrap();
        assert_eq!(prepared.degenerate, 1);
        assert!(mesh.faces.is_empty());
        assert_eq!(mesh.edges.len(), 3);
        assert!(prepared.sources.is_empty());
    }

    #[test]
    fn a_ring_passing_along_one_edge_twice_is_rebuilt() {
        let mut mesh = IndexedMesh {
            vertices: vec![
                DVec3::new(0.0, 0.0, 0.0),
                DVec3::new(10.0, 0.0, 0.0),
                DVec3::new(10.0, 10.0, 0.0),
            ],
            edges: (0..3)
                .map(|i| IndexedEdge {
                    a: i,
                    b: (i + 1) % 3,
                    flags: EdgeFlags::NONE,
                    curve: None,
                })
                .collect(),
            ..Default::default()
        };
        let uses = [0, 1, 2, 0, 1, 2].map(DirectedEdgeIndex::forward);
        mesh.faces.push(IndexedFace {
            loops: vec![uses.to_vec()],
            plane: Some(Plane::XY),
            sides: FaceSides::default(),
        });
        prepare(&mut mesh, &Tolerances::default()).unwrap();
        for face in &mesh.faces {
            let distinct = face.loops[0].iter().collect::<BTreeSet<_>>();
            assert_eq!(distinct.len(), face.loops[0].len());
        }
    }

    #[test]
    fn crossing_holes_become_facets_with_their_source_vertices() {
        let square = |x: f64, y: f64, s: f64| {
            [
                DVec3::new(x, y, 0.0),
                DVec3::new(x, y + s, 0.0),
                DVec3::new(x + s, y + s, 0.0),
                DVec3::new(x + s, y, 0.0),
            ]
        };
        let vertices = [
            vec![
                DVec3::new(-50.0, -50.0, 0.0),
                DVec3::new(50.0, -50.0, 0.0),
                DVec3::new(50.0, 50.0, 0.0),
                DVec3::new(-50.0, 50.0, 0.0),
            ],
            square(-10.0, -10.0, 15.0).to_vec(),
            square(-5.0, -5.0, 15.0).to_vec(),
        ]
        .concat();
        let mut mesh = loops(
            vertices.clone(),
            &[&[0, 1, 2, 3], &[4, 5, 6, 7], &[8, 9, 10, 11]],
        );
        let prepared = prepare(&mut mesh, &Tolerances::default()).unwrap();
        assert_eq!(mesh.vertices, vertices);
        assert_eq!(prepared.sources, vec![0; mesh.faces.len()]);
        assert!(
            mesh.faces
                .iter()
                .all(|f| f.loops.len() == 1 && f.loops[0].len() == 3)
        );
        assert!(prepared.crossing + prepared.degenerate >= 1);
    }

    #[test]
    fn warped_polygon_keeps_its_hole_and_exact_vertex_positions() {
        let vertices = vec![
            DVec3::new(-10.0, -10.0, 0.0),
            DVec3::new(10.0, -10.0, 0.0),
            DVec3::new(10.0, 10.0, 1.0),
            DVec3::new(-10.0, 10.0, 0.0),
            DVec3::new(-3.0, -3.0, 0.0),
            DVec3::new(-3.0, 3.0, 0.0),
            DVec3::new(3.0, 3.0, 0.0),
            DVec3::new(3.0, -3.0, 0.0),
        ];
        let edges = (0..8)
            .map(|i| IndexedEdge {
                a: i,
                b: (i / 4) * 4 + (i + 1) % 4,
                flags: EdgeFlags::NONE,
                curve: None,
            })
            .collect();
        let mut mesh = IndexedMesh {
            vertices: vertices.clone(),
            edges,
            faces: vec![IndexedFace {
                loops: vec![
                    (0..4).map(DirectedEdgeIndex::forward).collect(),
                    (4..8).map(DirectedEdgeIndex::forward).collect(),
                ],
                plane: Some(Plane::XY),
                sides: FaceSides::default(),
            }],
            ..Default::default()
        };
        let prepared = prepare(&mut mesh, &Tolerances::default()).unwrap();
        assert_eq!(prepared.warped, 1);
        assert_eq!(prepared.degenerate, 0);
        assert_eq!(mesh.vertices, vertices);
        assert_eq!(prepared.sources, vec![0; mesh.faces.len()]);
        let mut area = 0.0;
        for face in &mesh.faces {
            assert_eq!(face.loops.len(), 1);
            let points = face.loops[0]
                .iter()
                .map(|e| {
                    let edge = mesh.edges[e.index() as usize];
                    mesh.vertices[if e.is_reversed() { edge.b } else { edge.a } as usize]
                })
                .collect::<Vec<_>>();
            let [a, b, c] = [points[0], points[1], points[2]];
            let centroid = (a + b + c) / 3.0;
            assert!(centroid.x.abs() >= 3.0 || centroid.y.abs() >= 3.0);
            area += (b - a).truncate().perp_dot((c - a).truncate()) * 0.5;
        }
        assert!((area - 364.0).abs() < 1e-9);
    }
}
