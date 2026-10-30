//! Imports analytic curves and spatial polylines (docs/09 §5).
use glam::{DVec2, DVec3};
use planyrith_core::{geom::CurveDescriptor, math::Plane, units::Angle};
use std::collections::{BTreeMap, BTreeSet};

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
        let i = *adjacent.get(&vertex)?.iter().find(|i| unused.contains(i))?;
        unused.remove(&i);
        let e = members[i];
        vertex = if e.v0 == vertex { e.v1 } else { e.v0 };
        points.push(DVec3::from_array(*run.mesh.vertices.get(vertex as usize)?) * 1000.0);
    }
    Some(CurveDescriptor::Freehand { points })
}
