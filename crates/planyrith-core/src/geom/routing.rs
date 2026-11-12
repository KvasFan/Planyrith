//! Implements docs/03-geometry-kernel.md §4.22 and docs/05 §8.19:
//! hollow tubes on tangent circular elbows and concentric fittings.

use glam::{DQuat, DVec3};

use crate::geom::surface::{indexed, pieces, soften};
use crate::geom::{FaceSides, GeometryError, HalfEdgeMesh};
use crate::math::Segment;
use crate::tolerance::Tolerances;

/// Outer diameter × wall thickness presets, in millimeters.
pub const SIZES: [&str; 8] = [
    "10x1", "12x1", "16x1", "20x1.5", "25x2", "32x2", "40x2", "50x2",
];

/// Dimensions of a round hollow section, in millimeters.
#[derive(Debug, Clone, Copy)]
pub struct TubeSize {
    /// Outer diameter.
    pub diameter: f64,
    /// Wall thickness.
    pub wall: f64,
}

fn bad(message: &str) -> GeometryError {
    GeometryError::NotApplicable(message.into())
}

impl TubeSize {
    /// Looks up an explicit outer diameter × wall preset.
    pub fn preset(name: &str) -> Result<Self, GeometryError> {
        let (diameter, wall) = match name {
            "10x1" => (10.0, 1.0),
            "12x1" => (12.0, 1.0),
            "16x1" => (16.0, 1.0),
            "20x1.5" => (20.0, 1.5),
            "25x2" => (25.0, 2.0),
            "32x2" => (32.0, 2.0),
            "40x2" => (40.0, 2.0),
            "50x2" => (50.0, 2.0),
            _ => {
                return Err(bad(
                    "unknown tube size; use an outer diameter × wall preset",
                ));
            }
        };
        Ok(Self { diameter, wall })
    }

    /// Rejects a collapsed wall or bore at the document tolerance.
    pub fn check(self, tol: &Tolerances) -> Result<(), GeometryError> {
        if !self.diameter.is_finite()
            || !self.wall.is_finite()
            || self.wall <= tol.length
            || self.diameter * 0.5 - self.wall <= tol.length
        {
            return Err(bad(
                "tube wall and inner radius must exceed the length tolerance",
            ));
        }
        Ok(())
    }
}

/// Sampling and bend dimensions for a route.
#[derive(Debug, Clone, Copy)]
pub struct RouteOptions {
    /// Hollow section.
    pub size: TubeSize,
    /// Centerline bend radius, mm.
    pub bend_radius: f64,
    /// Facets around the tube (8–128).
    pub segments: u32,
    /// Chords per elbow (2–64).
    pub bend_segments: u32,
}

impl RouteOptions {
    /// Checks section, sampling and bend radius before constructing geometry.
    pub fn check(self, tol: &Tolerances) -> Result<(), GeometryError> {
        self.size.check(tol)?;
        if !self.bend_radius.is_finite()
            || self.bend_radius <= self.size.diameter * 0.5 + tol.length
            || !(8..=128).contains(&self.segments)
            || !(2..=64).contains(&self.bend_segments)
        {
            return Err(bad(
                "bend radius must exceed the outer radius; use 8–128 sides and 2–64 elbow chords",
            ));
        }
        Ok(())
    }
}

/// A generated tube and its analytic centerline measurements.
pub struct RouteMesh {
    /// Closed wall mesh, with an open bore.
    pub mesh: HalfEdgeMesh,
    /// Centerline length including analytic circular arcs, mm.
    pub length_mm: f64,
    /// Circular elbows made.
    pub elbows: usize,
}

#[derive(Clone, Copy)]
struct Station {
    point: DVec3,
    tangent: DVec3,
    diameter: f64,
}

/// Builds wall faces between stations, with annular end faces.
fn tube(
    stations: &[Station],
    wall: f64,
    segments: u32,
    tol: &Tolerances,
) -> Result<HalfEdgeMesh, GeometryError> {
    let n = segments as usize;
    let t = stations[0].tangent;
    let axis = [DVec3::X, DVec3::Y, DVec3::Z]
        .into_iter()
        .min_by(|a, b| a.dot(t).abs().total_cmp(&b.dot(t).abs()))
        .unwrap_or(DVec3::X);
    let mut u = axis.cross(t).normalize();
    let mut rings = Vec::with_capacity(stations.len());
    for (i, s) in stations.iter().enumerate() {
        if i > 0 {
            u = DQuat::from_rotation_arc(stations[i - 1].tangent, s.tangent) * u;
        }
        u = (u - s.tangent * u.dot(s.tangent)).normalize();
        let v = s.tangent.cross(u);
        let mut pair = Vec::with_capacity(2);
        for r in [s.diameter * 0.5, s.diameter * 0.5 - wall] {
            let ring: Vec<_> = (0..n)
                .map(|j| {
                    let a = std::f64::consts::TAU * j as f64 / n as f64;
                    s.point + r * (u * libm::cos(a) + v * libm::sin(a))
                })
                .collect();
            if ring.iter().any(|p| !tol.in_range(*p)) {
                return Err(GeometryError::OutOfRange);
            }
            if ring[0].distance(ring[1]) <= tol.length {
                return Err(bad(
                    "tube facets collapse within tolerance; increase the bore or reduce segments",
                ));
            }
            pair.push(ring);
        }
        rings.push(pair);
    }
    let mut faces = Vec::new();
    let sides = FaceSides::default();
    for pair in rings.windows(2) {
        for (k, (a, b)) in pair[0].iter().zip(&pair[1]).enumerate() {
            for j in 0..n {
                let next = (j + 1) % n;
                let mut q = vec![a[j], a[next], b[next], b[j]];
                if k == 1 {
                    q.reverse();
                }
                let polys = pieces(q, tol);
                if polys.is_empty() {
                    return Err(bad("tube wall facets collapse; increase the path spacing"));
                }
                faces.extend(polys.into_iter().map(|p| (vec![p], sides.clone())));
            }
        }
    }
    let last = rings.len() - 1;
    faces.push((
        vec![
            rings[0][0].iter().rev().copied().collect(),
            rings[0][1].clone(),
        ],
        sides.clone(),
    ));
    faces.push((
        vec![
            rings[last][0].clone(),
            rings[last][1].iter().rev().copied().collect(),
        ],
        sides,
    ));
    let mut mesh = indexed(&faces, tol);
    soften(&mut mesh, 0.8);
    let mesh = HalfEdgeMesh::from_indexed(&mesh, tol)?;
    if !mesh.is_solid() || mesh.volume()? <= tol.length * tol.length_sq {
        return Err(bad("tube did not enclose a valid wall volume"));
    }
    Ok(mesh)
}

/// A hollow tube on an open polyline, replacing corners with tangent elbows.
/// Nonadjacent sampled spans closer than the outer diameter are rejected.
pub fn route(
    points: &[DVec3],
    options: RouteOptions,
    tol: &Tolerances,
) -> Result<RouteMesh, GeometryError> {
    options.check(tol)?;
    if !(2..=256).contains(&points.len()) {
        return Err(bad("a route needs 2–256 points"));
    }
    if points.iter().any(|p| !tol.in_range(*p)) {
        return Err(GeometryError::OutOfRange);
    }
    if tol.same_point(points[0], points[points.len() - 1]) {
        return Err(bad("a route must be open"));
    }
    let lengths: Vec<f64> = points.windows(2).map(|w| w[0].distance(w[1])).collect();
    if lengths.iter().any(|l| *l <= tol.length) {
        return Err(bad("a route has duplicate consecutive points"));
    }
    let dirs: Vec<DVec3> = points
        .windows(2)
        .map(|w| (w[1] - w[0]).normalize())
        .collect();
    let mut setbacks = vec![0.0; points.len()];
    let mut angles = vec![0.0; points.len()];
    for i in 1..points.len() - 1 {
        let dot = dirs[i - 1].dot(dirs[i]).clamp(-1.0, 1.0);
        let angle = libm::acos(dot);
        if angle <= tol.angle {
            continue;
        }
        if std::f64::consts::PI - angle <= tol.angle {
            return Err(bad("a route cannot reverse at a corner"));
        }
        angles[i] = angle;
        setbacks[i] = options.bend_radius * libm::tan(angle * 0.5);
    }
    for (i, length) in lengths.iter().enumerate() {
        if *length - setbacks[i] - setbacks[i + 1] <= tol.length {
            return Err(bad(
                "elbows do not fit the path; lengthen its segments or reduce the bend radius",
            ));
        }
    }
    let station_count = 2 + angles.iter().filter(|a| **a > tol.angle).count()
        * (options.bend_segments as usize + 1);
    if station_count > 4096 || station_count * options.segments as usize * 2 > 262_144 {
        return Err(bad(
            "route sampling is too large; reduce segments or split the path (4096 stations, 262144 ring vertices maximum)",
        ));
    }
    let diameter = options.size.diameter;
    let mut stations = vec![Station {
        point: points[0],
        tangent: dirs[0],
        diameter,
    }];
    let mut length_mm: f64 = lengths.iter().sum();
    let mut elbows = 0;
    for i in 1..points.len() - 1 {
        let a = angles[i];
        if a <= tol.angle {
            continue;
        }
        elbows += 1;
        length_mm += options.bend_radius * a - 2.0 * setbacks[i];
        let start = points[i] - dirs[i - 1] * setbacks[i];
        let axis = dirs[i - 1].cross(dirs[i]).normalize();
        let center = start + axis.cross(dirs[i - 1]) * options.bend_radius;
        for k in 0..=options.bend_segments {
            let rotation =
                DQuat::from_axis_angle(axis, a * f64::from(k) / f64::from(options.bend_segments));
            stations.push(Station {
                point: center + rotation * (start - center),
                tangent: rotation * dirs[i - 1],
                diameter,
            });
        }
    }
    stations.push(Station {
        point: points[points.len() - 1],
        tangent: dirs[dirs.len() - 1],
        diameter,
    });
    // Skip nearby spans of the same local bend. A nonlocal pair whose
    // intervening arc length exceeds a half circumference can collide.
    let mut distance = vec![0.0];
    for w in stations.windows(2) {
        distance.push(distance[distance.len() - 1] + w[0].point.distance(w[1].point));
    }
    for i in 0..stations.len() - 1 {
        for j in i + 2..stations.len() - 1 {
            if distance[j] - distance[i + 1] <= std::f64::consts::PI * diameter * 0.5 {
                continue;
            }
            let (a, b) = (stations[i].point, stations[i + 1].point);
            let (c, d) = (stations[j].point, stations[j + 1].point);
            let clearance = DVec3::splat(diameter + tol.length);
            if (a.min(b) - clearance).cmpgt(c.max(d)).any()
                || (c.min(d) - clearance).cmpgt(a.max(b)).any()
            {
                continue;
            }
            if Segment::new(stations[i].point, stations[i + 1].point)
                .distance_to_segment(&Segment::new(stations[j].point, stations[j + 1].point), tol)
                <= diameter + tol.length
            {
                return Err(bad(
                    "nonadjacent route spans overlap; separate them by more than the outer diameter",
                ));
            }
        }
    }
    Ok(RouteMesh {
        mesh: tube(&stations, options.size.wall, options.segments, tol)?,
        length_mm,
        elbows,
    })
}

/// A hollow straight sleeve or concentric reducer, starting at `position`.
pub fn fitting(
    size: TubeSize,
    outlet_diameter: f64,
    length: f64,
    position: DVec3,
    axis: DVec3,
    tol: &Tolerances,
) -> Result<HalfEdgeMesh, GeometryError> {
    size.check(tol)?;
    TubeSize {
        diameter: outlet_diameter,
        wall: size.wall,
    }
    .check(tol)?;
    if !length.is_finite() || length <= tol.length || !tol.in_range(position) {
        return Err(bad(
            "fitting length must exceed the tolerance and its position must be in range",
        ));
    }
    let tangent = axis
        .try_normalize()
        .filter(|a| a.is_finite())
        .ok_or_else(|| bad("fitting axis must be finite and nonzero"))?;
    tube(
        &[
            Station {
                point: position,
                tangent,
                diameter: size.diameter,
            },
            Station {
                point: position + tangent * length,
                tangent,
                diameter: outlet_diameter,
            },
        ],
        size.wall,
        24,
        tol,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::geom::boolean::{Classification, classify_point};
    use proptest::prelude::*;

    fn options() -> RouteOptions {
        RouteOptions {
            size: TubeSize::preset("20x1.5").unwrap(),
            bend_radius: 30.0,
            segments: 24,
            bend_segments: 8,
        }
    }

    #[test]
    fn elbow_length_bore_and_three_dimensional_path() {
        let tol = Tolerances::default();
        let r = route(
            &[DVec3::ZERO, DVec3::X * 100.0, DVec3::new(100.0, 100.0, 0.0)],
            options(),
            &tol,
        )
        .unwrap();
        assert_eq!(r.elbows, 1);
        assert!((r.length_mm - (140.0 + 30.0 * std::f64::consts::FRAC_PI_2)).abs() < tol.length);
        assert!(r.mesh.is_solid());
        assert!(r.mesh.validate(&tol).is_empty());
        assert_eq!(
            classify_point(&r.mesh, DVec3::new(40.0, 0.0, 0.0), &tol).unwrap(),
            Classification::Outside
        );
        assert_eq!(
            classify_point(&r.mesh, DVec3::new(40.0, 9.0, 0.0), &tol).unwrap(),
            Classification::Inside
        );
        let r = route(
            &[
                DVec3::ZERO,
                DVec3::X * 100.0,
                DVec3::new(100.0, 100.0, 0.0),
                DVec3::splat(100.0),
            ],
            options(),
            &tol,
        )
        .unwrap();
        assert_eq!(r.elbows, 2);
        assert!(r.mesh.validate(&tol).is_empty());
    }

    #[test]
    fn reject_collapsed_and_crossing_paths() {
        let tol = Tolerances::default();
        for path in [
            vec![DVec3::ZERO, DVec3::ZERO],
            vec![DVec3::ZERO, DVec3::X * 100.0, DVec3::ZERO],
            vec![DVec3::ZERO, DVec3::X * 10.0, DVec3::splat(10.0)],
            vec![
                DVec3::ZERO,
                DVec3::X * 100.0,
                DVec3::new(100.0, 100.0, 0.0),
                DVec3::new(-100.0, 100.0, 0.0),
                DVec3::new(-100.0, -100.0, 0.0),
                DVec3::new(50.0, -100.0, 0.0),
                DVec3::new(50.0, 50.0, 0.0),
            ],
        ] {
            assert!(route(&path, options(), &tol).is_err(), "{path:?}");
        }
        let mut o = options();
        o.size.wall = 10.0;
        assert!(route(&[DVec3::ZERO, DVec3::X * 100.0], o, &tol).is_err());
        o = options();
        o.bend_radius = 5.0;
        assert!(route(&[DVec3::ZERO, DVec3::X * 100.0], o, &tol).is_err());
    }

    #[test]
    fn reducer_has_an_open_bore_and_expected_frustum_volume() {
        let tol = Tolerances::default();
        let size = TubeSize::preset("32x2").unwrap();
        let m = fitting(size, 20.0, 40.0, DVec3::ZERO, DVec3::Z, &tol).unwrap();
        let polygon_area = |r: f64| 12.0 * libm::sin(std::f64::consts::TAU / 24.0) * r * r;
        let cone = |r: f64, s: f64| {
            40.0 / 3.0
                * (polygon_area(r)
                    + libm::sqrt(polygon_area(r) * polygon_area(s))
                    + polygon_area(s))
        };
        let expected = cone(16.0, 10.0) - cone(14.0, 8.0);
        assert!((m.volume().unwrap() - expected).abs() < 1e-6);
        assert_eq!(
            classify_point(&m, DVec3::Z * 20.0, &tol).unwrap(),
            Classification::Outside
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]
        #[test]
        fn straight_tubes_match_polygon_annulus_volume(diameter in 5.0f64..80.0, fraction in 0.05f64..0.35, length in 10.0f64..500.0, segments in 8u32..48) {
            let tol = Tolerances::default();
            let wall = diameter * fraction;
            let mut o = options();
            o.size = TubeSize { diameter, wall };
            o.bend_radius = diameter * 1.5;
            o.segments = segments;
            let result = route(&[DVec3::ZERO, DVec3::Z * length], o, &tol).unwrap();
            let outer = diameter * 0.5;
            let inner = outer - wall;
            let area = f64::from(segments) * 0.5 * libm::sin(std::f64::consts::TAU / f64::from(segments)) * (outer * outer - inner * inner);
            prop_assert!((result.mesh.volume().unwrap() - area * length).abs() < area * length * 1e-9);
            prop_assert!(result.mesh.validate(&tol).is_empty());
        }
    }
}
