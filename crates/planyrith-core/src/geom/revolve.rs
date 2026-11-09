//! Implements docs/03-geometry-kernel.md §4.19 Revolve: a solid swept by a
//! planar profile turning about an axis in its plane.

use glam::DVec3;

use crate::geom::boolean::{Piece, stitch};
use crate::geom::planar::reverse_faces;
use crate::geom::{EdgeFlags, FaceSides, GeometryError, HalfEdgeMesh, NoDeltas};
use crate::math::transform::rotation_about;
use crate::tolerance::Tolerances;
use crate::units::Angle;

/// cos 20°: neighboring side pieces closer than this read as one surface.
const SMOOTH_COS: f64 = 0.939_692_621;

/// The profile `loops` (outer first, then holes, as a face stores them)
/// turned by `angle` radians (up to a full turn) about the axis through
/// `origin` along `axis`, in `segments` steps per full turn. The axis must
/// lie in the profile's plane and the profile on one side of it; points on
/// the axis are allowed. A partial turn is closed by the profile at both
/// ends.
pub fn revolve(
    loops: &[Vec<DVec3>],
    origin: DVec3,
    axis: DVec3,
    angle: f64,
    segments: u32,
    sides: FaceSides,
    tol: &Tolerances,
) -> Result<HalfEdgeMesh, GeometryError> {
    let axis = axis
        .try_normalize()
        .ok_or_else(|| GeometryError::NotApplicable("the axis has no direction".into()))?;
    let outer = loops
        .first()
        .filter(|l| l.len() >= 3)
        .ok_or_else(|| GeometryError::NotApplicable("the profile needs a closed loop".into()))?;
    let full = std::f64::consts::TAU;
    if !(angle.is_finite() && angle > tol.angle && angle <= full + tol.angle) {
        return Err(GeometryError::NotApplicable(
            "the angle must be more than zero and at most a full turn".into(),
        ));
    }
    let closed = angle >= full - tol.angle;
    // The profile's plane must hold the axis, and the profile one side of it.
    let mut normal = DVec3::ZERO;
    for (i, a) in outer.iter().enumerate() {
        normal += a.cross(outer[(i + 1) % outer.len()]);
    }
    let normal = normal
        .try_normalize()
        .ok_or_else(|| GeometryError::NotApplicable("the profile has no area".into()))?;
    if normal.dot(axis).abs() > 1e-6 {
        return Err(GeometryError::NotApplicable(
            "the axis must lie in the profile's plane".into(),
        ));
    }
    if (origin - outer[0]).dot(normal).abs() > tol.planarity {
        return Err(GeometryError::NotApplicable(
            "the axis must lie in the profile's plane".into(),
        ));
    }
    let across = normal.cross(axis);
    let side: Vec<f64> = loops
        .iter()
        .flatten()
        .map(|p| (*p - origin).dot(across))
        .collect();
    let (lo, hi) = side
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), s| {
            (lo.min(*s), hi.max(*s))
        });
    if lo < -tol.length && hi > tol.length {
        return Err(GeometryError::NotApplicable(
            "the profile crosses the axis".into(),
        ));
    }
    if hi.abs().max(lo.abs()) <= tol.length {
        return Err(GeometryError::NotApplicable(
            "the profile lies on the axis".into(),
        ));
    }
    let per_turn = segments.clamp(3, 720);
    let steps = ((f64::from(per_turn) * angle / full).ceil() as u32).max(1);
    let ring = |k: u32| -> Vec<Vec<DVec3>> {
        let t = rotation_about(
            origin,
            axis,
            Angle::radians(angle * f64::from(k) / f64::from(steps)),
        );
        loops
            .iter()
            .map(|l| l.iter().map(|p| t.transform_point3(*p)).collect())
            .collect()
    };
    let rings: Vec<Vec<Vec<DVec3>>> = (0..=steps).map(ring).collect();
    let mut pieces = Vec::new();
    let mut caps = Vec::new();
    if !closed {
        let first: Vec<Vec<DVec3>> = rings[0]
            .iter()
            .map(|l| l.iter().rev().copied().collect())
            .collect();
        caps.push(normal);
        caps.push(rotation_about(origin, axis, Angle::radians(angle)).transform_vector3(normal));
        pieces.push(Piece {
            loops: first,
            sides: sides.clone(),
        });
        pieces.push(Piece {
            loops: rings[steps as usize].clone(),
            sides: sides.clone(),
        });
    }
    for k in 0..steps as usize {
        let (p, q) = (&rings[k], &rings[k + 1]);
        for (lp, lq) in p.iter().zip(q) {
            let n = lp.len();
            for j in 0..n {
                let i = (j + 1) % n;
                let mut quad: Vec<DVec3> = Vec::with_capacity(4);
                for c in [lp[j], lp[i], lq[i], lq[j]] {
                    if quad.last().is_none_or(|l| !tol.same_point(*l, c)) {
                        quad.push(c);
                    }
                }
                while quad.len() > 1 && tol.same_point(quad[0], quad[quad.len() - 1]) {
                    quad.pop();
                }
                if quad.len() < 3 {
                    continue;
                }
                let mut area = DVec3::ZERO;
                for (a, b) in quad.iter().zip(quad.iter().cycle().skip(1)) {
                    area += a.cross(*b);
                }
                if area.length() <= tol.length_sq {
                    continue;
                }
                let nn = area.normalize();
                let flat = quad
                    .iter()
                    .all(|c| nn.dot(*c - quad[0]).abs() <= tol.planarity);
                if flat {
                    pieces.push(Piece {
                        loops: vec![quad],
                        sides: sides.clone(),
                    });
                } else {
                    for tri in [[quad[0], quad[1], quad[2]], [quad[0], quad[2], quad[3]]] {
                        pieces.push(Piece {
                            loops: vec![tri.to_vec()],
                            sides: sides.clone(),
                        });
                    }
                }
            }
        }
    }
    let mut solid = stitch(&pieces, tol)?;
    if !solid.is_solid() {
        return Err(GeometryError::NonRobust(
            "the revolved profile does not close a solid".into(),
        ));
    }
    if solid.volume()? < 0.0 {
        let all = solid.faces().map(|(f, _)| f).collect();
        reverse_faces(&mut solid, &all, tol, &mut NoDeltas)?;
    }
    let is_cap = |n: DVec3| caps.iter().any(|c: &DVec3| c.dot(n).abs() > 1.0 - 1e-6);
    let mut soft = Vec::new();
    for (e, _) in solid.edges() {
        let [Some(fa), Some(fb)] = solid.edge_faces(e)? else {
            continue;
        };
        let (na, nb) = (solid.face_normal(fa)?, solid.face_normal(fb)?);
        if !is_cap(na) && !is_cap(nb) && na.dot(nb) > SMOOTH_COS {
            soft.push(e);
        }
    }
    for e in soft {
        solid.set_edge_flags(e, EdgeFlags::SOFT | EdgeFlags::SMOOTH, &mut NoDeltas)?;
    }
    Ok(solid)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tol() -> Tolerances {
        Tolerances::default()
    }

    /// A rectangle in the XZ plane from x = r0 to r1, z = 0 to h.
    fn rect(r0: f64, r1: f64, h: f64) -> Vec<DVec3> {
        vec![
            DVec3::new(r0, 0.0, 0.0),
            DVec3::new(r1, 0.0, 0.0),
            DVec3::new(r1, 0.0, h),
            DVec3::new(r0, 0.0, h),
        ]
    }

    fn turn() -> f64 {
        std::f64::consts::TAU
    }

    #[test]
    fn a_rectangle_on_the_axis_makes_a_cylinder() {
        let n = 64;
        let solid = revolve(
            &[rect(0.0, 100.0, 50.0)],
            DVec3::ZERO,
            DVec3::Z,
            turn(),
            n,
            FaceSides::default(),
            &tol(),
        )
        .unwrap();
        // A regular 64-gon prism.
        let area = 0.5 * f64::from(n) * 100.0 * 100.0 * libm::sin(turn() / f64::from(n));
        assert!((solid.volume().unwrap() - area * 50.0).abs() < 1e-3);
    }

    #[test]
    fn a_rectangle_off_the_axis_makes_a_ring_and_a_half_turn_is_capped() {
        let ring = revolve(
            &[rect(50.0, 100.0, 20.0)],
            DVec3::ZERO,
            DVec3::Z,
            turn(),
            32,
            FaceSides::default(),
            &tol(),
        )
        .unwrap();
        assert!(ring.is_solid());
        let half = revolve(
            &[rect(50.0, 100.0, 20.0)],
            DVec3::ZERO,
            DVec3::Z,
            turn() / 2.0,
            32,
            FaceSides::default(),
            &tol(),
        )
        .unwrap();
        assert!(half.is_solid());
        assert!((half.volume().unwrap() * 2.0 - ring.volume().unwrap()).abs() < 1e-3);
    }

    #[test]
    fn a_triangle_touching_the_axis_makes_a_cone() {
        let tri = vec![
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(100.0, 0.0, 0.0),
            DVec3::new(0.0, 0.0, 100.0),
        ];
        let cone = revolve(
            &[tri],
            DVec3::ZERO,
            DVec3::Z,
            turn(),
            48,
            FaceSides::default(),
            &tol(),
        )
        .unwrap();
        assert!(cone.is_solid());
        assert!(cone.volume().unwrap() > 0.0);
    }

    #[test]
    fn a_profile_across_the_axis_or_out_of_plane_is_refused() {
        let across = rect(-50.0, 50.0, 10.0);
        assert!(
            revolve(
                &[across],
                DVec3::ZERO,
                DVec3::Z,
                turn(),
                24,
                FaceSides::default(),
                &tol()
            )
            .is_err()
        );
        assert!(
            revolve(
                &[rect(10.0, 50.0, 10.0)],
                DVec3::ZERO,
                DVec3::Y,
                turn(),
                24,
                FaceSides::default(),
                &tol()
            )
            .is_err()
        );
    }
}
