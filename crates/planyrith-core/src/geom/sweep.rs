//! Implements docs/03-geometry-kernel.md §4.11 (sweep): extrudes a profile face
//! along a path of edges with minimum-rotation frames and mitered corners.
//!
//! Owner key: `later` (milestone 0.8).

use glam::{DAffine3, DMat3, DQuat, DVec3};

use crate::geom::{DeltaSink, GeometryError, HalfEdgeMesh};
use crate::ids::{EdgeId, FaceId};
use crate::tolerance::Tolerances;

/// Options for [`sweep_profile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepOptions {
    /// Delete the profile face (it becomes interior) (default `true`).
    pub delete_profile: bool,
    /// Miter the profile at path corners (default `true`).
    pub miter_corners: bool,
}

impl Default for SweepOptions {
    fn default() -> Self {
        SweepOptions {
            delete_profile: true,
            miter_corners: true,
        }
    }
}

/// Result of [`sweep_profile`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SweepResult {
    /// Faces created (side quads and end caps).
    pub faces: Vec<FaceId>,
    /// `true` when the path was closed and a ring was produced.
    pub closed: bool,
}

/// Minimum-rotation frames along a polyline (one per vertex), Frenet-free to
/// avoid twisting (docs/03 §4.11). For closed paths the last frame is
/// reconciled with the first.
pub fn path_frames(points: &[DVec3], closed: bool) -> Vec<DAffine3> {
    if points.len() < 2 {
        return Vec::new();
    }
    let tangents = (0..points.len())
        .map(|i| {
            if i + 1 < points.len() {
                (points[i + 1] - points[i]).normalize_or_zero()
            } else if closed {
                (points[0] - points[i]).normalize_or_zero()
            } else {
                (points[i] - points[i - 1]).normalize_or_zero()
            }
        })
        .collect::<Vec<_>>();
    if tangents.iter().any(|t| t.length_squared() < 0.5) {
        return Vec::new();
    }
    let t = tangents[0];
    let axis = [DVec3::X, DVec3::Y, DVec3::Z]
        .into_iter()
        .min_by(|a, b| a.dot(t).abs().total_cmp(&b.dot(t).abs()))
        .unwrap_or(DVec3::X);
    let mut u = axis.cross(t).normalize();
    let mut frames = Vec::new();
    for i in 0..points.len() {
        if i > 0 {
            u = DQuat::from_rotation_arc(tangents[i - 1], tangents[i]) * u;
        }
        let t = tangents[i];
        u = (u - t * u.dot(t)).normalize_or_zero();
        frames.push(DAffine3::from_mat3_translation(
            DMat3::from_cols(u, t.cross(u), t),
            points[i],
        ));
    }
    if closed {
        let last = frames.len() - 1;
        let transported =
            DQuat::from_rotation_arc(tangents[last], tangents[0]) * frames[last].matrix3.x_axis;
        let target = frames[0].matrix3.x_axis;
        let angle = libm::atan2(
            tangents[0].dot(transported.cross(target)),
            transported.dot(target),
        );
        let lengths = points
            .windows(2)
            .map(|w| w[0].distance(w[1]))
            .collect::<Vec<_>>();
        let total = lengths.iter().sum::<f64>() + points[last].distance(points[0]);
        let mut distance = 0.0;
        for i in 1..frames.len() {
            distance += lengths[i - 1];
            let rotation = DMat3::from_quat(DQuat::from_axis_angle(
                tangents[i],
                angle * distance / total,
            ));
            frames[i].matrix3 = rotation * frames[i].matrix3;
        }
    }
    frames
}

/// Sweeps `profile` along the connected edge chain `path` (docs/03 §4.11).
/// The profile must be perpendicular to the first path segment within
/// `tol.angle` (`NotApplicable` otherwise, `E1201`).
pub fn sweep_profile(
    mesh: &mut HalfEdgeMesh,
    profile: FaceId,
    path: &[EdgeId],
    opts: SweepOptions,
    tol: &Tolerances,
    sink: &mut dyn DeltaSink,
) -> Result<SweepResult, GeometryError> {
    use crate::geom::boolean::{Piece, stitch};
    use crate::geom::{EdgeFlags, NoDeltas};
    use std::collections::BTreeSet;

    let path_set: BTreeSet<_> = path.iter().copied().collect();
    if path_set.is_empty() {
        return Err(GeometryError::NotApplicable(
            "sweep needs a connected path".into(),
        ));
    }
    let mut adjacency = std::collections::BTreeMap::<_, Vec<_>>::new();
    for &e in &path_set {
        let (a, b) = mesh.edge_endpoints(e)?;
        adjacency.entry(a).or_default().push((e, b));
        adjacency.entry(b).or_default().push((e, a));
    }
    if adjacency.values().any(|n| n.len() > 2) {
        return Err(GeometryError::NotApplicable("sweep path branches".into()));
    }
    let ends = adjacency
        .iter()
        .filter(|(_, n)| n.len() == 1)
        .map(|(&v, _)| v)
        .collect::<Vec<_>>();
    let closed = ends.is_empty();
    if !closed && ends.len() != 2 {
        return Err(GeometryError::NotApplicable(
            "sweep path is disconnected".into(),
        ));
    }
    let plane = mesh.face_ref(profile)?.plane;
    let candidates = if closed {
        adjacency.keys().copied().collect::<Vec<_>>()
    } else {
        ends
    };
    let center = mesh.face_aabb(profile)?.center();
    let start = *candidates
        .iter()
        .min_by(|&&a, &&b| {
            plane
                .signed_distance(mesh.position(a).unwrap_or(DVec3::ZERO))
                .abs()
                .total_cmp(
                    &plane
                        .signed_distance(mesh.position(b).unwrap_or(DVec3::ZERO))
                        .abs(),
                )
                .then_with(|| {
                    mesh.position(a)
                        .unwrap_or(DVec3::ZERO)
                        .distance_squared(center)
                        .total_cmp(
                            &mesh
                                .position(b)
                                .unwrap_or(DVec3::ZERO)
                                .distance_squared(center),
                        )
                })
        })
        .ok_or_else(|| GeometryError::NotApplicable("empty path".into()))?;
    if let Some(neighbors) = adjacency.get_mut(&start) {
        neighbors.sort_by(|(_, a), (_, b)| {
            let origin = mesh.position(start).unwrap_or(DVec3::ZERO);
            let alignment = |v| {
                (mesh.position(v).unwrap_or(origin) - origin)
                    .normalize_or_zero()
                    .dot(plane.normal)
                    .abs()
            };
            alignment(*b).total_cmp(&alignment(*a))
        });
    }
    let mut vertices = vec![start];
    let mut used = BTreeSet::new();
    let mut current = start;
    loop {
        let next = adjacency
            .get(&current)
            .and_then(|n| n.iter().find(|(e, _)| !used.contains(e)))
            .copied();
        let Some((e, v)) = next else {
            break;
        };
        used.insert(e);
        if v == start {
            break;
        }
        vertices.push(v);
        current = v;
    }
    if used.len() != path_set.len() {
        return Err(GeometryError::NotApplicable(
            "sweep path is disconnected".into(),
        ));
    }
    let mut points = vertices
        .iter()
        .map(|&v| mesh.position(v))
        .collect::<Result<Vec<_>, _>>()?;
    // Drawing the guide can split a straight segment at unrelated boundary
    // vertices. Those subdivisions are not sweep corners or extra frames.
    loop {
        let end = if closed {
            points.len()
        } else {
            points.len().saturating_sub(1)
        };
        let redundant = (1..end).find(|&i| {
            let (a, b, c) = (points[i - 1], points[i], points[(i + 1) % points.len()]);
            tol.collinear(a, b, c) && (b - a).dot(c - b) > 0.0
        });
        let Some(i) = redundant else {
            break;
        };
        points.remove(i);
    }
    let frames = path_frames(&points, closed);
    if frames.len() != points.len() {
        return Err(GeometryError::Degenerate(
            "sweep path has repeated points".into(),
        ));
    }
    let tangent = frames[0].matrix3.z_axis;
    if plane.normal.dot(tangent).abs() < std::f64::consts::FRAC_1_SQRT_2
        || !tol.point_on_plane(&plane, points[0])
    {
        return Err(GeometryError::NotApplicable("profile must meet the path start and deviate less than 45 degrees from its perpendicular plane".into()));
    }
    let loops = mesh
        .face_loops(profile)?
        .into_iter()
        .map(|l| mesh.loop_positions(l))
        .collect::<Result<Vec<_>, _>>()?;
    let sides = mesh.face_ref(profile)?.sides.clone();
    // Project a slightly oblique profile along the first tangent, then carry
    // its coordinates in the minimum-rotation frame.
    let local = loops
        .iter()
        .map(|lp| {
            lp.iter()
                .map(|p| {
                    let q = *p - points[0];
                    frames[0].matrix3.transpose() * (q - tangent * q.dot(tangent))
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut rings = Vec::new();
    for i in 0..points.len() {
        let outgoing = frames[i].matrix3.z_axis;
        let bisector = if (i > 0 || closed) && (i + 1 < points.len() || closed) {
            let prev = (points[i] - points[(i + points.len() - 1) % points.len()]).normalize();
            (prev + outgoing).try_normalize().ok_or_else(|| {
                GeometryError::NotApplicable("sweep path reverses direction".into())
            })?
        } else {
            outgoing
        };
        if outgoing.dot(bisector).abs() < 1e-6 {
            return Err(GeometryError::NotApplicable(
                "sweep corner is too sharp".into(),
            ));
        }
        rings.push(
            local
                .iter()
                .map(|lp| {
                    lp.iter()
                        .map(|&p| {
                            let mut q = frames[i].matrix3 * p;
                            if opts.miter_corners {
                                q -= outgoing * q.dot(bisector) / outgoing.dot(bisector);
                            }
                            points[i] + q
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>(),
        );
    }
    let mut pieces = Vec::new();
    let mut fold_seams = Vec::new();
    let count = if closed {
        points.len()
    } else {
        points.len() - 1
    };
    for i in 0..count {
        let j = (i + 1) % points.len();
        for (l, lp) in local.iter().enumerate() {
            for k in 0..lp.len() {
                let next = (k + 1) % lp.len();
                let quad = vec![
                    rings[i][l][k],
                    rings[j][l][k],
                    rings[j][l][next],
                    rings[i][l][next],
                ];
                if crate::math::Plane::from_points(quad[0], quad[1], quad[2], tol)
                    .is_some_and(|plane| tol.points_on_plane(&plane, &quad))
                {
                    pieces.push(Piece {
                        loops: vec![quad],
                        sides: sides.clone(),
                    });
                } else {
                    fold_seams.push(crate::math::Segment::new(quad[0], quad[2]));
                    for indices in [[0, 1, 2], [0, 2, 3]] {
                        pieces.push(Piece {
                            loops: vec![indices.into_iter().map(|i| quad[i]).collect()],
                            sides: sides.clone(),
                        });
                    }
                }
            }
        }
    }
    if !closed {
        pieces.push(Piece {
            loops: rings[0].clone(),
            sides: sides.clone(),
        });
        let mut end = rings[points.len() - 1].clone();
        for lp in &mut end {
            lp.reverse();
        }
        pieces.push(Piece { loops: end, sides });
    }
    let mut swept = stitch(&pieces, tol)?;
    let fold_edges = swept
        .edges()
        .filter_map(|(e, _)| {
            swept
                .edge_segment(e)
                .ok()
                .filter(|s| {
                    fold_seams.iter().any(|fold| {
                        tol.point_on_segment(s.a, fold) && tol.point_on_segment(s.b, fold)
                    })
                })
                .map(|_| e)
        })
        .collect::<Vec<_>>();
    for edge in fold_edges {
        swept.set_edge_flags(
            edge,
            EdgeFlags::SOFT | EdgeFlags::SMOOTH | EdgeFlags::HIDDEN,
            &mut NoDeltas,
        )?;
    }
    if swept.volume()? < 0.0 {
        let faces = swept.faces().map(|(f, _)| f).collect();
        crate::geom::planar::reverse_faces(&mut swept, &faces, tol, &mut NoDeltas)?;
    }
    // Smooth longitudinal seams inherited from a curved profile.
    for (l, lp) in loops.iter().enumerate() {
        for k in 0..lp.len() {
            let curved = mesh.face_edges(profile)?.iter().any(|&e| {
                mesh.edge(e).is_some_and(|e| e.curve.is_some())
                    && mesh
                        .edge_segment(e)
                        .is_ok_and(|s| tol.same_point(s.a, lp[k]) || tol.same_point(s.b, lp[k]))
            });
            if !curved {
                continue;
            }
            for i in 0..count {
                let j = (i + 1) % points.len();
                let ids = swept
                    .edges()
                    .filter_map(|(e, _)| {
                        swept
                            .edge_segment(e)
                            .ok()
                            .filter(|s| {
                                (tol.same_point(s.a, rings[i][l][k])
                                    && tol.same_point(s.b, rings[j][l][k]))
                                    || (tol.same_point(s.b, rings[i][l][k])
                                        && tol.same_point(s.a, rings[j][l][k]))
                            })
                            .map(|_| e)
                    })
                    .collect::<Vec<_>>();
                for e in ids {
                    swept.set_edge_flags(e, EdgeFlags::SOFT | EdgeFlags::SMOOTH, &mut NoDeltas)?;
                }
            }
        }
    }
    let (indexed, _) = swept.to_indexed();
    mesh.transact(tol, sink, |m, d| {
        // A planar closed path may have formed a face while it was drawn.
        // Consume only faces bounded entirely by this path (the path guide).
        let guides = m
            .faces()
            .filter_map(|(f, _)| {
                if f != profile
                    && m.face_edges(f).is_ok_and(|edges| {
                        !edges.is_empty() && edges.iter().all(|e| path_set.contains(e))
                    })
                {
                    Some(f)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for guide in guides {
            m.erase_face(guide, true, tol, d)?;
        }
        if opts.delete_profile {
            m.erase_face(profile, true, tol, d)?;
        }
        for e in &path_set {
            if m.edge(*e).is_some() && m.edge_faces(*e)? == [None, None] {
                m.erase_edge(*e, tol, d)?;
            }
        }
        let ids = m.insert_disjoint(&indexed, tol, d)?.ok_or_else(|| {
            GeometryError::NotApplicable(
                "sweep touches existing geometry; use a separate group".into(),
            )
        })?;
        Ok(SweepResult {
            faces: ids.faces,
            closed,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{EdgeFlags, NoDeltas};

    #[test]
    fn ordinary_closed_path_face_is_consumed_and_nonplanar_paths_close() {
        let tol = Tolerances::default();
        for closed in [true, false] {
            let mut mesh = HalfEdgeMesh::new();
            let profile = [
                DVec3::new(0.0, -5.0, -5.0),
                DVec3::new(0.0, 5.0, -5.0),
                DVec3::new(0.0, 5.0, 5.0),
                DVec3::new(0.0, -5.0, 5.0),
            ];
            for i in 0..4 {
                mesh.add_edge(profile[i], profile[(i + 1) % 4], &tol, &mut NoDeltas)
                    .unwrap();
            }
            let face = mesh.faces().next().unwrap().0;
            let points = if closed {
                vec![
                    DVec3::ZERO,
                    DVec3::X * 100.0,
                    DVec3::new(100.0, 100.0, 0.0),
                    DVec3::Y * 100.0,
                ]
            } else {
                vec![
                    DVec3::ZERO,
                    DVec3::X * 100.0,
                    DVec3::new(100.0, 100.0, 50.0),
                    DVec3::new(150.0, 150.0, 100.0),
                ]
            };
            let mut path = Vec::new();
            for i in 0..if closed {
                points.len()
            } else {
                points.len() - 1
            } {
                path.extend(
                    mesh.add_edge(
                        points[i],
                        points[(i + 1) % points.len()],
                        &tol,
                        &mut NoDeltas,
                    )
                    .unwrap()
                    .edges,
                );
            }
            if closed {
                let guide = mesh
                    .faces()
                    .find(|(_, f)| f.plane.normal.z.abs() > 0.5)
                    .unwrap()
                    .0;
                path = mesh.loop_edges(mesh.face(guide).unwrap().outer).unwrap();
            }
            sweep_profile(
                &mut mesh,
                face,
                &path,
                Default::default(),
                &tol,
                &mut NoDeltas,
            )
            .unwrap_or_else(|e| panic!("closed={closed}: {e}"));
            assert!(mesh.is_solid());
            assert!(mesh.validate(&tol).is_empty());
            assert!(mesh.volume().unwrap() > 0.0);
        }
    }

    #[test]
    fn circular_profile_along_closed_square_is_a_closed_ring() {
        let tol = Tolerances::default();
        let mut mesh = HalfEdgeMesh::new();
        let n = 16;
        let profile = (0..n)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / n as f64;
                DVec3::new(0.0, 10.0 * libm::cos(a), 10.0 * libm::sin(a))
            })
            .collect::<Vec<_>>();
        for i in 0..n {
            mesh.add_edge(profile[i], profile[(i + 1) % n], &tol, &mut NoDeltas)
                .unwrap();
        }
        let face = mesh.faces().next().unwrap().0;
        let points = [
            DVec3::ZERO,
            DVec3::X * 100.0,
            DVec3::new(100.0, 100.0, 0.0),
            DVec3::Y * 100.0,
        ];
        let vertices = points.map(|p| mesh.new_vertex(p, &mut NoDeltas));
        let path = (0..4)
            .map(|i| {
                mesh.new_edge(
                    vertices[i],
                    vertices[(i + 1) % 4],
                    EdgeFlags::NONE,
                    &mut NoDeltas,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let r = sweep_profile(
            &mut mesh,
            face,
            &path,
            Default::default(),
            &tol,
            &mut NoDeltas,
        )
        .unwrap();
        assert!(r.closed);
        assert_eq!(r.faces.len(), 4 * n);
        assert!(mesh.is_solid());
        assert!(mesh.validate(&tol).is_empty());
        assert!(mesh.volume().unwrap() > 100_000.0);
    }
}
