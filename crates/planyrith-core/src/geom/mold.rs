//! Implements docs/03-geometry-kernel.md §4.23 and docs/05 §8.18:
//! a compensated part, two boolean-cut blanks and a planar parting surface.

use glam::{DAffine3, DVec2, DVec3};

use crate::geom::boolean::{BooleanOp, boolean, piece, stitch};
use crate::geom::loft::loft;
use crate::geom::mesh::{IndexedEdge, IndexedMesh, transform_indexed};
use crate::geom::section::slice;
use crate::geom::{EdgeFlags, FaceSides, GeometryError, HalfEdgeMesh};
use crate::math::Plane;
use crate::tolerance::Tolerances;

/// The meshes and measurements of a planar two-half mold.
pub struct MoldMesh {
    /// Material on the negative side of the parting plane.
    pub core: HalfEdgeMesh,
    /// Material on the positive side of the parting plane.
    pub cavity: HalfEdgeMesh,
    /// Wire edges along the compensated part's section.
    pub parting_line: HalfEdgeMesh,
    /// Blank's plane outside the compensated part, with section holes.
    pub parting_surface: HalfEdgeMesh,
    /// Compensated part volume, mm³.
    pub compensated_volume_mm3: f64,
    /// Total section perimeter, mm.
    pub parting_length_mm: f64,
    /// Whether either boolean used the optional B-rep backend.
    pub fallback_used: bool,
}

fn bad(message: &str) -> GeometryError {
    GeometryError::NotApplicable(message.into())
}

/// Splits an enclosing blank on `plane` and subtracts a scaled copy of
/// `part` from each half. The source is never modified. `margin` is the
/// blank's clearance on all six sides, in the plane's orthonormal frame.
pub fn split(
    part: &HalfEdgeMesh,
    plane: Plane,
    margin: f64,
    shrink_factor: f64,
    origin: DVec3,
    tol: &Tolerances,
) -> Result<MoldMesh, GeometryError> {
    if !part.is_solid() || part.volume()? <= tol.length * tol.length_sq {
        return Err(bad("a mold needs a closed, outward-facing solid"));
    }
    if !margin.is_finite()
        || margin <= tol.planarity
        || !shrink_factor.is_finite()
        || shrink_factor <= 0.0
        || !tol.in_range(origin)
        || !plane.normal.is_finite()
        || !plane.d.is_finite()
        || (plane.normal.length() - 1.0).abs() > tol.angle
    {
        return Err(bad(
            "give a finite unit plane, positive shrink factor and a margin above the planarity tolerance",
        ));
    }
    let transform = DAffine3::from_translation(origin)
        * DAffine3::from_scale(DVec3::splat(shrink_factor))
        * DAffine3::from_translation(-origin);
    let (source, _) = part.to_indexed();
    let compensated = HalfEdgeMesh::from_indexed(&transform_indexed(&source, &transform), tol)?;
    let frame = plane.frame();
    let mut low = DVec3::splat(f64::INFINITY);
    let mut high = DVec3::splat(f64::NEG_INFINITY);
    for (_, v) in compensated.vertices() {
        let xy = frame.to_2d(v.position);
        let p = DVec3::new(xy.x, xy.y, plane.signed_distance(v.position));
        low = low.min(p);
        high = high.max(p);
    }
    if low.z >= -tol.planarity || high.z <= tol.planarity {
        return Err(bad("the parting plane must cross the compensated part"));
    }
    low -= DVec3::splat(margin);
    high += DVec3::splat(margin);
    let square = |z: f64| -> Vec<DVec3> {
        [
            DVec2::new(low.x, low.y),
            DVec2::new(high.x, low.y),
            DVec2::new(high.x, high.y),
            DVec2::new(low.x, high.y),
        ]
        .map(|p| frame.from_2d(p) + plane.normal * z)
        .to_vec()
    };
    let mut halves = Vec::with_capacity(2);
    let mut fallback_used = false;
    for (bottom, top) in [(low.z, 0.0), (0.0, high.z)] {
        let blank = loft(&[square(bottom), square(top)], FaceSides::default(), tol)?;
        let result = boolean(
            BooleanOp::Subtract,
            &blank,
            &DAffine3::IDENTITY,
            &compensated,
            &DAffine3::IDENTITY,
            tol,
        )?;
        fallback_used |= result.fallback_used;
        let mut faces = Vec::new();
        for solid in &result.solids {
            for (f, _) in solid.faces() {
                faces.push(piece(solid, f, false)?);
            }
        }
        if faces.is_empty() {
            return Err(bad("the subtraction left an empty mold half"));
        }
        let mesh = stitch(&faces, tol)?;
        if !mesh.is_solid() || mesh.volume()? <= tol.length * tol.length_sq {
            return Err(bad("the subtraction did not leave a solid mold half"));
        }
        halves.push(mesh);
    }
    let cavity = halves.pop().ok_or_else(|| bad("missing cavity"))?;
    let core = halves.pop().ok_or_else(|| bad("missing core"))?;
    let mut surface = Vec::new();
    for (f, face) in core.faces() {
        if face.plane.normal.dot(plane.normal) >= 1.0 - tol.angle
            && core.face_vertices(f)?.iter().all(|v| {
                core.position(*v)
                    .is_ok_and(|p| plane.signed_distance(p).abs() <= tol.planarity)
            })
        {
            surface.push(piece(&core, f, false)?);
        }
    }
    if surface.is_empty() {
        return Err(bad("could not extract the parting surface"));
    }
    let parting_surface = stitch(&surface, tol)?;
    let segments = slice(&compensated, &plane, tol);
    let mut wire = IndexedMesh::default();
    for (a, b) in segments {
        let mut vertex = |p: DVec3| -> u32 {
            if let Some(i) = wire.vertices.iter().position(|q| tol.same_point(p, *q)) {
                i as u32
            } else {
                wire.vertices.push(p);
                (wire.vertices.len() - 1) as u32
            }
        };
        let (a, b) = (vertex(a), vertex(b));
        if a != b
            && !wire
                .edges
                .iter()
                .any(|e| (e.a == a && e.b == b) || (e.a == b && e.b == a))
        {
            wire.edges.push(IndexedEdge {
                a,
                b,
                flags: EdgeFlags::NONE,
                curve: None,
            });
        }
    }
    let mut degree = vec![0; wire.vertices.len()];
    let mut parting_length_mm = 0.0;
    for e in &wire.edges {
        degree[e.a as usize] += 1;
        degree[e.b as usize] += 1;
        parting_length_mm += wire.vertices[e.a as usize].distance(wire.vertices[e.b as usize]);
    }
    if wire.edges.is_empty() || degree.iter().any(|d| *d != 2) {
        return Err(bad(
            "the parting line is not closed; move the plane away from coincident edges",
        ));
    }
    Ok(MoldMesh {
        core,
        cavity,
        parting_line: HalfEdgeMesh::from_indexed(&wire, tol)?,
        parting_surface,
        compensated_volume_mm3: compensated.volume()?,
        parting_length_mm,
        fallback_used,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn block(tol: &Tolerances) -> HalfEdgeMesh {
        let ring = |z| {
            vec![
                DVec3::new(0.0, 0.0, z),
                DVec3::new(100.0, 0.0, z),
                DVec3::new(100.0, 60.0, z),
                DVec3::new(0.0, 60.0, z),
            ]
        };
        loft(&[ring(0.0), ring(40.0)], FaceSides::default(), tol).unwrap()
    }

    #[test]
    fn compensated_block_volume_perimeter_and_parting_hole() {
        let tol = Tolerances::default();
        let part = block(&tol);
        let origin = part.aabb().center();
        let result = split(
            &part,
            Plane::from_point_normal(origin, DVec3::Z),
            10.0,
            1.02,
            origin,
            &tol,
        )
        .unwrap();
        let scaled = 100.0 * 60.0 * 40.0 * 1.02f64.powi(3);
        let blank = (102.0 + 20.0) * (61.2 + 20.0) * (40.8 + 20.0);
        assert!((result.compensated_volume_mm3 - scaled).abs() < 1e-5);
        assert!(
            (result.core.volume().unwrap() + result.cavity.volume().unwrap() - (blank - scaled))
                .abs()
                < 1e-5
        );
        assert!((result.parting_length_mm - 320.0 * 1.02).abs() < tol.length);
        assert_eq!(result.parting_surface.faces().count(), 1);
        assert_eq!(
            result.parting_surface.faces().next().unwrap().1.holes.len(),
            1
        );
        assert_eq!(result.parting_line.faces().count(), 0);
        assert_eq!(result.parting_line.edges().count(), 4);
        for m in [
            &result.core,
            &result.cavity,
            &result.parting_line,
            &result.parting_surface,
        ] {
            assert!(m.validate(&tol).is_empty());
        }
    }

    #[test]
    fn oblique_plane_and_invalid_requests() {
        let tol = Tolerances::default();
        let part = block(&tol);
        let origin = part.aabb().center();
        let result = split(
            &part,
            Plane::from_point_normal(origin, DVec3::new(1.0, 1.0, 2.0)),
            10.0,
            1.0,
            origin,
            &tol,
        )
        .unwrap();
        assert!(result.core.is_solid());
        assert!(result.cavity.is_solid());
        assert!(split(&part, Plane::XY, 10.0, 1.0, origin, &tol).is_err());
        assert!(split(&part, Plane::XY, -10.0, 1.0, origin, &tol).is_err());
        assert!(split(&part, Plane::XY, 10.0, f64::NAN, origin, &tol).is_err());
    }
}
