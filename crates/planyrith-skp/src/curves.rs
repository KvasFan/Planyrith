//! Imports analytic curves and spatial polylines (docs/09 §5).
use glam::{DVec2, DVec3};
use planyrith_core::{geom::CurveDescriptor, math::Plane, units::Angle};
use std::collections::{BTreeMap, BTreeSet};

/// Splits interrupted or branching source memberships into native chains.
pub(super) fn split_chains(mesh: &mut planyrith_core::geom::mesh::IndexedMesh) -> usize {
    let original_count = mesh.curves.len();
    let mut splits = 0;
    let mut groups = vec![Vec::new(); original_count];
    for (i, edge) in mesh.edges.iter().enumerate() {
        if let Some(curve) = edge.curve.and_then(|c| groups.get_mut(c as usize)) {
            curve.push(i);
        }
    }
    for (curve, group) in groups.iter().enumerate() {
        let mut adjacent = BTreeMap::<u32, Vec<usize>>::new();
        let mut unused = BTreeSet::new();
        for &i in group {
            let edge = mesh.edges[i];
            unused.insert(i);
            adjacent.entry(edge.a).or_default().push(i);
            adjacent.entry(edge.b).or_default().push(i);
        }
        let mut chains = Vec::new();
        while let Some(&first) = unused.first() {
            let start = unused
                .iter()
                .find_map(|i| {
                    let e = mesh.edges[*i];
                    [e.a, e.b]
                        .into_iter()
                        .find(|v| adjacent[v].len() != 2)
                        .map(|v| (*i, v))
                })
                .unwrap_or((first, mesh.edges[first].a));
            let (mut edge, mut vertex) = start;
            let mut members = Vec::new();
            let mut vertices = vec![vertex];
            loop {
                unused.remove(&edge);
                members.push(edge);
                let e = mesh.edges[edge];
                vertex = if e.a == vertex { e.b } else { e.a };
                vertices.push(vertex);
                if adjacent[&vertex].len() != 2 {
                    break;
                }
                let Some(next) = adjacent[&vertex]
                    .iter()
                    .find(|e| unused.contains(e))
                    .copied()
                else {
                    break;
                };
                edge = next;
            }
            chains.push((members, vertices));
        }
        if chains.len() <= 1 {
            continue;
        }
        splits += chains.len() - 1;
        let original = mesh.curves[curve].clone();
        for (part, (members, vertices)) in chains.into_iter().enumerate() {
            let points = vertices
                .iter()
                .map(|v| mesh.vertices[*v as usize])
                .collect::<Vec<_>>();
            let descriptor = match original {
                CurveDescriptor::Arc {
                    center,
                    normal,
                    radius,
                    ..
                }
                | CurveDescriptor::Circle {
                    center,
                    normal,
                    radius,
                    ..
                } => {
                    if vertices.first() == vertices.last() {
                        CurveDescriptor::Circle {
                            center,
                            normal,
                            radius,
                            segments: members.len() as u32,
                        }
                    } else {
                        let frame = Plane::from_point_normal(center, normal).frame();
                        let angle = |p: DVec3| {
                            DVec2::X.angle_to(DVec2::new(
                                (p - center).dot(frame.u),
                                (p - center).dot(frame.v),
                            ))
                        };
                        let start = angle(points[0]);
                        let mut end = start;
                        let mut previous = start;
                        for p in &points[1..] {
                            let next = angle(*p);
                            end += (next - previous + std::f64::consts::PI)
                                .rem_euclid(std::f64::consts::TAU)
                                - std::f64::consts::PI;
                            previous = next;
                        }
                        CurveDescriptor::Arc {
                            center,
                            normal,
                            radius,
                            start_angle: Angle::radians(start),
                            end_angle: Angle::radians(end),
                            segments: members.len() as u32,
                        }
                    }
                }
                _ => CurveDescriptor::Freehand { points },
            };
            let index = if part == 0 {
                mesh.curves[curve] = descriptor;
                curve
            } else {
                let index = mesh.curves.len();
                mesh.curves.push(descriptor);
                index
            };
            for edge in members {
                mesh.edges[edge].curve = Some(index as u32);
            }
        }
    }
    splits
}

pub(super) fn descriptor(run: &openskp::GeometryRun, index: usize) -> Option<CurveDescriptor> {
    let members = run
        .mesh
        .edges
        .iter()
        .filter(|e| e.curve == Some(index as u32))
        .collect::<Vec<_>>();
    if members.is_empty() {
        return None;
    }
    if let Some(arc) = run.curves.get(index)?.arc.as_ref() {
        let x = DVec3::from_array(arc.x_axis_m) * 1000.0;
        let y = DVec3::from_array(arc.y_axis_m) * 1000.0;
        let radius = x.length();
        if radius > 1e-3
            && DVec3::from_array(arc.center_m).is_finite()
            && arc.start_rad.is_finite()
            && arc.end_rad.is_finite()
            && (radius - y.length()).abs() < 1e-3
            && x.dot(y).abs() < radius * radius * 1e-6
        {
            let center = DVec3::from_array(arc.center_m) * 1000.0;
            let normal = x.cross(y).normalize_or_zero();
            let frame = Plane::from_point_normal(center, normal).frame();
            let rotation = DVec2::X.angle_to(DVec2::new(x.dot(frame.u), x.dot(frame.v)));
            let span = arc.end_rad - arc.start_rad;
            if span.abs() < 1e-9 || (span.abs() - std::f64::consts::TAU).abs() < 1e-9 {
                return Some(CurveDescriptor::Circle {
                    center,
                    normal,
                    radius,
                    segments: members.len() as u32,
                });
            }
            return Some(CurveDescriptor::Arc {
                center,
                normal,
                radius,
                start_angle: Angle::radians(rotation + arc.start_rad),
                end_angle: Angle::radians(rotation + arc.end_rad),
                segments: members.len() as u32,
            });
        }
    }
    let mut adjacent = BTreeMap::<u32, Vec<usize>>::new();
    for (i, e) in members.iter().enumerate() {
        adjacent.entry(e.v0).or_default().push(i);
        adjacent.entry(e.v1).or_default().push(i);
    }
    let mut vertex = adjacent
        .iter()
        .find(|(_, v)| v.len() == 1)
        .map_or(members[0].v0, |(v, _)| *v);
    let mut unused = (0..members.len()).collect::<BTreeSet<_>>();
    let mut points = vec![DVec3::from_array(*run.mesh.vertices.get(vertex as usize)?) * 1000.0];
    while !unused.is_empty() {
        let Some(&i) = adjacent.get(&vertex)?.iter().find(|i| unused.contains(i)) else {
            // Interrupted memberships are split after indexing, with every
            // source edge retained and each fragment's descriptor rebuilt.
            break;
        };
        unused.remove(&i);
        let e = members[i];
        vertex = if e.v0 == vertex { e.v1 } else { e.v0 };
        points.push(DVec3::from_array(*run.mesh.vertices.get(vertex as usize)?) * 1000.0);
    }
    Some(CurveDescriptor::Freehand { points })
}
