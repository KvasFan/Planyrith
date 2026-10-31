//! Exports analytic curves and welded sampled chains (docs/09 §5).
use super::{Result, encode::Pids};
use openskp::GeometryRun;
use planyrith_core::{
    geom::{CurveDescriptor, HalfEdgeMesh},
    ids::CurveId,
    math::Plane,
};
use std::collections::BTreeMap;

pub(super) fn append(
    mesh: &HalfEdgeMesh,
    run: &mut GeometryRun,
    pids: &mut Pids,
) -> Result<BTreeMap<CurveId, u32>> {
    let mut curves = BTreeMap::new();
    for (id, curve) in mesh.curves() {
        let members = mesh.edges().filter(|(_, e)| e.curve == Some(id)).count();
        if members == 0 {
            continue;
        }
        let arc = match curve {
            CurveDescriptor::Arc {
                center,
                normal,
                radius,
                start_angle,
                end_angle,
                ..
            } => {
                let frame = Plane::from_point_normal(*center, *normal).frame();
                Some(openskp::Arc {
                    center_m: (*center / 1000.0).to_array(),
                    normal: normal.to_array(),
                    x_axis_m: (frame.u * (*radius / 1000.0)).to_array(),
                    y_axis_m: (frame.v * (*radius / 1000.0)).to_array(),
                    radius_m: *radius / 1000.0,
                    start_rad: start_angle.as_radians(),
                    end_rad: end_angle.as_radians(),
                })
            }
            CurveDescriptor::Circle {
                center,
                normal,
                radius,
                ..
            } => {
                let frame = Plane::from_point_normal(*center, *normal).frame();
                Some(openskp::Arc {
                    center_m: (*center / 1000.0).to_array(),
                    normal: normal.to_array(),
                    x_axis_m: (frame.u * (*radius / 1000.0)).to_array(),
                    y_axis_m: (frame.v * (*radius / 1000.0)).to_array(),
                    radius_m: *radius / 1000.0,
                    start_rad: 0.0,
                    end_rad: std::f64::consts::TAU,
                })
            }
            _ => None,
        };
        curves.insert(id, u32::try_from(run.curves.len())?);
        run.curves.push(openskp::Curve {
            pid: pids.next()?,
            members: u32::try_from(members)?,
            arc,
        });
    }
    Ok(curves)
}
