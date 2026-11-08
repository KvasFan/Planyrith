//! Implements docs/03-geometry-kernel.md §4.17 Loft: a solid through two or
//! more closed profiles in order, and cubic Bézier sampling for the
//! `bezier` drawing tool (§4.18).

use glam::DVec3;

use crate::geom::boolean::{Piece, stitch};
use crate::geom::planar::reverse_faces;
use crate::geom::{EdgeFlags, FaceSides, GeometryError, HalfEdgeMesh, NoDeltas};
use crate::tolerance::Tolerances;

/// Points of a cubic Bézier spline: `control` holds `3n + 1` points (each
/// span: start, two handles, end; spans share ends), `segments` straight
/// pieces per span. Returns the polyline from the first to the last point.
pub fn bezier_points(control: &[DVec3], segments: u32) -> Result<Vec<DVec3>, GeometryError> {
    if control.len() < 4 || !(control.len() - 1).is_multiple_of(3) {
        return Err(GeometryError::NotApplicable(
            "a Bézier spline needs 3n + 1 points: start, two handles and an end per span".into(),
        ));
    }
    let steps = segments.clamp(1, 256);
    let mut out = vec![control[0]];
    for span in control.windows(4).step_by(3) {
        let [p0, p1, p2, p3] = [span[0], span[1], span[2], span[3]];
        for k in 1..=steps {
            let t = f64::from(k) / f64::from(steps);
            let u = 1.0 - t;
            out.push(
                p0 * (u * u * u)
                    + p1 * (3.0 * u * u * t)
                    + p2 * (3.0 * u * t * t)
                    + p3 * (t * t * t),
            );
        }
    }
    Ok(out)
}

fn newell(points: &[DVec3]) -> DVec3 {
    let mut n = DVec3::ZERO;
    for (i, a) in points.iter().enumerate() {
        n += a.cross(points[(i + 1) % points.len()]);
    }
    n
}

fn centroid(points: &[DVec3]) -> DVec3 {
    points.iter().copied().sum::<DVec3>() / points.len().max(1) as f64
}

/// `points` (a closed loop) with extra points on its edges until it has
/// `count`: each edge gets a share proportional to its length (largest
/// remainders first, then the earlier edge), so the corners stay.
fn resample(points: &[DVec3], count: usize) -> Vec<DVec3> {
    let n = points.len();
    if n >= count {
        return points.to_vec();
    }
    let lengths: Vec<f64> = (0..n)
        .map(|i| points[i].distance(points[(i + 1) % n]))
        .collect();
    let total: f64 = lengths.iter().sum::<f64>().max(f64::MIN_POSITIVE);
    let extra = count - n;
    let exact: Vec<f64> = lengths.iter().map(|l| l / total * extra as f64).collect();
    let mut shares: Vec<usize> = exact.iter().map(|x| x.floor() as usize).collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        let (ra, rb) = (exact[a] - exact[a].floor(), exact[b] - exact[b].floor());
        rb.total_cmp(&ra).then(a.cmp(&b))
    });
    let given: usize = shares.iter().sum();
    for &i in order.iter().take(extra - given) {
        shares[i] += 1;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..n {
        let (a, b) = (points[i], points[(i + 1) % n]);
        out.push(a);
        for k in 1..=shares[i] {
            out.push(a.lerp(b, k as f64 / (shares[i] + 1) as f64));
        }
    }
    out
}

/// A solid through `profiles` (closed outer loops, in order). Profiles get
/// as many points as the largest (§4.17), turn the same way about the
/// loft's direction, and start where their points line up best; the sides
/// are quads, split into triangles where not flat; the first and last
/// profiles cap the ends.
pub fn loft(
    profiles: &[Vec<DVec3>],
    sides: FaceSides,
    tol: &Tolerances,
) -> Result<HalfEdgeMesh, GeometryError> {
    if profiles.len() < 2 || profiles.iter().any(|p| p.len() < 3) {
        return Err(GeometryError::NotApplicable(
            "a loft needs two or more closed profiles".into(),
        ));
    }
    let count = profiles.iter().map(Vec::len).max().unwrap_or(3);
    let first = centroid(&profiles[0]);
    let last = centroid(&profiles[profiles.len() - 1]);
    let axis = (last - first).normalize_or_zero();
    if axis == DVec3::ZERO {
        return Err(GeometryError::NotApplicable(
            "the profiles must not share their center; move one along the loft".into(),
        ));
    }
    let mut loops: Vec<Vec<DVec3>> = Vec::with_capacity(profiles.len());
    for profile in profiles {
        let mut p = resample(profile, count);
        if newell(&p).dot(axis) < 0.0 {
            p.reverse();
        }
        if let Some(prev) = loops.last() {
            let shift = centroid(&p) - centroid(prev);
            let best = (0..count)
                .min_by(|&a, &b| {
                    let cost = |k: usize| -> f64 {
                        (0..count)
                            .map(|j| (p[(j + k) % count] - shift).distance_squared(prev[j]))
                            .sum()
                    };
                    cost(a).total_cmp(&cost(b)).then(a.cmp(&b))
                })
                .unwrap_or(0);
            p.rotate_left(best);
        }
        loops.push(p);
    }
    let mut pieces = vec![
        Piece {
            loops: vec![loops[0].iter().rev().copied().collect()],
            sides: sides.clone(),
        },
        Piece {
            loops: vec![loops[loops.len() - 1].clone()],
            sides: sides.clone(),
        },
    ];
    for pair in loops.windows(2) {
        let (p, q) = (&pair[0], &pair[1]);
        for j in 0..count {
            let k = (j + 1) % count;
            let quad = [p[j], p[k], q[k], q[j]];
            let n = newell(&quad).normalize_or_zero();
            let flat = n != DVec3::ZERO
                && quad
                    .iter()
                    .all(|c| (n.dot(*c - quad[0])).abs() <= tol.planarity);
            if flat {
                pieces.push(Piece {
                    loops: vec![quad.to_vec()],
                    sides: sides.clone(),
                });
            } else {
                for tri in [[p[j], p[k], q[k]], [p[j], q[k], q[j]]] {
                    if newell(&tri).length() > tol.length_sq {
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
            "the loft does not close; check that the profiles do not cross".into(),
        ));
    }
    if solid.volume()? < 0.0 {
        let all = solid.faces().map(|(f, _)| f).collect();
        reverse_faces(&mut solid, &all, tol, &mut NoDeltas)?;
    }
    // Side pieces meeting at a shallow angle read as one smooth surface;
    // the ends keep their outlines.
    let caps = [newell(&loops[0]), newell(&loops[loops.len() - 1])].map(|n| n.normalize_or_zero());
    let is_cap = |n: DVec3| caps.iter().any(|c| c.dot(n).abs() > 1.0 - 1e-6);
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

/// cos 20° (the importers' soften angle): side pieces closer than this are
/// smoothed together.
const SMOOTH_COS: f64 = 0.939_692_621;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tol() -> Tolerances {
        Tolerances::default()
    }

    fn square(z: f64, s: f64) -> Vec<DVec3> {
        vec![
            DVec3::new(-s, -s, z),
            DVec3::new(s, -s, z),
            DVec3::new(s, s, z),
            DVec3::new(-s, s, z),
        ]
    }

    #[test]
    fn two_equal_squares_make_a_box() {
        let solid = loft(
            &[square(0.0, 50.0), square(100.0, 50.0)],
            FaceSides::default(),
            &tol(),
        )
        .unwrap();
        assert_eq!(solid.face_count(), 6);
        assert!((solid.volume().unwrap() - 1_000_000.0).abs() < 1e-6);
    }

    #[test]
    fn a_square_to_a_smaller_one_is_a_frustum_whatever_the_winding() {
        let mut top = square(100.0, 25.0);
        top.reverse();
        top.rotate_left(1);
        let solid = loft(&[square(0.0, 50.0), top], FaceSides::default(), &tol()).unwrap();
        assert_eq!(solid.face_count(), 6, "aligned corners keep flat sides");
        // Frustum: h/3 (A1 + A2 + √(A1 A2)).
        let expected = 100.0 / 3.0 * (10_000.0 + 2_500.0 + 5_000.0);
        assert!((solid.volume().unwrap() - expected).abs() < 1e-6);
    }

    #[test]
    fn a_square_to_a_round_profile_resamples_the_square() {
        let octagon: Vec<DVec3> = (0..24)
            .map(|i| {
                let a = std::f64::consts::PI / 12.0 * f64::from(i);
                DVec3::new(40.0 * libm::cos(a), 40.0 * libm::sin(a), 80.0)
            })
            .collect();
        let solid = loft(&[square(0.0, 40.0), octagon], FaceSides::default(), &tol()).unwrap();
        assert!(solid.is_solid());
        assert!(solid.volume().unwrap() > 0.0);
        let soft = solid
            .edges()
            .filter(|(_, e)| e.flags.contains(EdgeFlags::SOFT))
            .count();
        assert!(soft > 0, "the twisted sides are smoothed");
        let box_ = loft(
            &[square(0.0, 50.0), square(100.0, 50.0)],
            FaceSides::default(),
            &tol(),
        )
        .unwrap();
        assert!(
            box_.edges()
                .all(|(_, e)| !e.flags.contains(EdgeFlags::SOFT))
        );
    }

    #[test]
    fn bezier_spans_pass_through_their_ends() {
        let c = [
            DVec3::ZERO,
            DVec3::new(0.0, 100.0, 0.0),
            DVec3::new(100.0, 100.0, 0.0),
            DVec3::new(100.0, 0.0, 0.0),
        ];
        let p = bezier_points(&c, 10).unwrap();
        assert_eq!(p.len(), 11);
        assert_eq!(p[0], c[0]);
        assert!(p[10].distance(c[3]) < 1e-12);
        assert!(
            (p[5].y - 75.0).abs() < 1e-9,
            "the middle is at 3/4 of the handles"
        );
        assert!(bezier_points(&c[..3], 4).is_err());
    }
}
