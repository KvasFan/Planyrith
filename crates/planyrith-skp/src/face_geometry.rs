//! Fits source face planes and retains warped polygons as facets (docs/09 §5).
use super::Result;
use planyrith_core::{
    geom::{
        EdgeFlags,
        mesh::{DirectedEdgeIndex, IndexedEdge, IndexedMesh},
        tessellate::tessellate_polygon_2d,
    },
    math::Plane,
    tolerance::Tolerances,
};
use std::collections::BTreeMap;

pub(super) fn prepare(
    mesh: &mut IndexedMesh,
    tol: &Tolerances,
) -> Result<(Vec<usize>, usize, usize)> {
    let mut sources = Vec::new();
    let mut split = 0;
    let mut degenerate = 0;
    let mut edges = mesh
        .edges
        .iter()
        .enumerate()
        .map(|(i, e)| ((e.a.min(e.b), e.a.max(e.b)), i as u32))
        .collect::<BTreeMap<_, _>>();
    for (source, mut face) in std::mem::take(&mut mesh.faces).into_iter().enumerate() {
        let rings = face
            .loops
            .iter()
            .map(|ring| {
                ring.iter()
                    .map(|edge| {
                        let e = mesh.edges[edge.index() as usize];
                        if edge.is_reversed() { e.b } else { e.a }
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
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
        let frame = fitted.frame();
        let projected = rings
            .iter()
            .map(|ring| {
                ring.iter()
                    .map(|i| frame.to_2d(mesh.vertices[*i as usize]))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let triangles = tessellate_polygon_2d(&projected[0], &projected[1..])?;
        if triangles.is_empty() {
            return Err("warped source face could not be triangulated".into());
        }
        let vertices = rings.into_iter().flatten().collect::<Vec<_>>();
        for triangle in triangles {
            let mut ids = triangle.map(|i| vertices[i as usize]);
            let [a, b, c] = ids.map(|i| mesh.vertices[i as usize]);
            if (b - a).cross(c - a).dot(fitted.normal) < 0.0 {
                ids.swap(1, 2);
            }
            let [a, b, c] = ids.map(|i| mesh.vertices[i as usize]);
            let normal = (b - a).cross(c - a);
            if normal.length() * 0.5 <= tol.length_sq {
                // Keep source boundary edges/vertices as wire geometry. The
                // kernel cannot represent a surface below its area tolerance.
                degenerate += 1;
                continue;
            }
            let plane = Plane::from_point_normal(a, normal);
            let mut piece = face.clone();
            piece.plane = Some(plane);
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
            mesh.faces.push(piece);
            sources.push(source);
        }
        split += 1;
    }
    Ok((sources, split, degenerate))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use glam::DVec3;
    use planyrith_core::geom::{FaceSides, mesh::IndexedFace};

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
        let (sources, split, degenerate) = prepare(&mut mesh, &Tolerances::default()).unwrap();
        assert_eq!(split, 1);
        assert_eq!(degenerate, 0);
        assert_eq!(mesh.vertices, vertices);
        assert_eq!(sources, vec![0; mesh.faces.len()]);
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
