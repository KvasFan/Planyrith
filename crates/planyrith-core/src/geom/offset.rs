//! Implements docs/03-geometry-kernel.md §4.10 (offset loop): offsets a face
//! boundary in its plane with miter joins and self-intersection clipping.
//!
//! The 2D offset (docs/03 Decisions):
//! 1. every ring is offset along its right-hand normal (out of the region:
//!    outer rings are counter-clockwise, holes clockwise) with miter joins;
//!    corners whose miter would reach farther than [`MITER_LIMIT`] × |d| are
//!    bevelled;
//! 2. the raw offset curves are split at all their intersections into a
//!    planar arrangement; a bounded region of it is kept when its winding
//!    number with respect to the raw curves is at least one and, for inward
//!    offsets, it lies inside the original region at least |d| from its
//!    boundary;
//! 3. the result is the boundary of the kept regions: outer rings
//!    counter-clockwise, holes clockwise. An inward offset past a narrow
//!    part therefore yields several rings, and one past the whole width
//!    yields none (`Degenerate`).
//!
//! Owner key: `later`.

#![warn(unused_variables, dead_code)]

use std::collections::BTreeMap;

use glam::DVec2;

use crate::geom::planar;
use crate::geom::tessellate::tessellate_polygon_2d;
use crate::geom::{DeltaSink, GeometryError, HalfEdgeMesh, MeshDelta};
use crate::ids::{EdgeId, FaceId};
use crate::math::polygon::{self, PointInPolygon};
use crate::tolerance::Tolerances;
use crate::units::Length;

/// Miter joins longer than this many offset distances are bevelled.
pub const MITER_LIMIT: f64 = 4.0;

/// Result of [`offset_face_boundary`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OffsetResult {
    /// Edges created (via `add_edge`, so faces form automatically).
    pub edges: Vec<EdgeId>,
    /// The face bounded by the offset loop, when one formed.
    pub face: Option<FaceId>,
    /// Faces split as a side effect.
    pub faces_split: Vec<FaceId>,
}

/// Offsets every loop of `f` by `d` (positive = outward for the outer loop,
/// inward for holes) and adds the result as edges (docs/03 §4.10). Atomic.
pub fn offset_face_boundary(
    mesh: &mut HalfEdgeMesh,
    f: FaceId,
    d: Length,
    tol: &Tolerances,
    sink: &mut dyn DeltaSink,
) -> Result<OffsetResult, GeometryError> {
    let d = d.as_mm();
    if !d.is_finite() || d.abs() <= tol.length {
        return Err(GeometryError::Degenerate(
            "offset distance must be non-zero".into(),
        ));
    }
    let plane = mesh.face_ref(f)?.plane;
    let frame = plane.frame();
    let rings: Vec<Vec<DVec2>> = mesh
        .face_cycles(f)?
        .iter()
        .map(|c| mesh.cycle_positions(c).map(|p| frame.project_all(&p)))
        .collect::<Result<_, _>>()?;
    let loops = offset_rings_2d(&rings, d, tol)?;
    if loops.is_empty() {
        return Err(GeometryError::Degenerate(
            "the offset collapses the whole face".into(),
        ));
    }
    let loops3: Vec<Vec<glam::DVec3>> = loops.iter().map(|l| frame.unproject_all(l)).collect();
    mesh.transact(tol, sink, |m, log| {
        let mut out = OffsetResult::default();
        let first = log.len();
        for ring in &loops3 {
            for i in 0..ring.len() {
                let r = m.add_edge_inner(ring[i], ring[(i + 1) % ring.len()], tol, log)?;
                for e in r.edges {
                    if !out.edges.contains(&e) {
                        out.edges.push(e);
                    }
                }
                out.faces_split
                    .extend(r.split_faces.iter().map(|(_, _, b)| *b));
            }
        }
        out.edges.retain(|e| m.edge(*e).is_some());
        out.faces_split.retain(|g| m.face(*g).is_some());
        // The face bounded by the first (outer) offset ring.
        let wanted: Vec<_> = loops3[0]
            .iter()
            .filter_map(|&p| m.find_vertex_near(p, tol))
            .collect();
        let mut created = Vec::new();
        for delta in &log[first..] {
            if let MeshDelta::FaceAdded { id, .. } = delta
                && m.face(*id).is_some()
                && !created.contains(id)
            {
                created.push(*id);
            }
        }
        out.face = created.into_iter().find(|g| {
            m.face_vertices(*g)
                .is_ok_and(|vs| vs.len() >= wanted.len() && wanted.iter().all(|v| vs.contains(v)))
        });
        Ok(out)
    })
}

/// Pure 2D polygon offset: miter joins at convex corners, trimming at concave
/// corners, self-intersections clipped (docs/03 §4.10). The polygon may have
/// either orientation; positive `d` grows it. When the result splits into
/// several rings, the largest is returned; use [`offset_rings_2d`] for all.
pub fn offset_polygon_2d(
    polygon: &[DVec2],
    d: f64,
    tol: &Tolerances,
) -> Result<Vec<DVec2>, GeometryError> {
    let mut ring = polygon.to_vec();
    polygon::make_counter_clockwise(&mut ring);
    let loops = offset_rings_2d(&[ring], d, tol)?;
    loops
        .into_iter()
        .filter(|l| polygon::signed_area(l) > 0.0)
        .max_by(|a, b| polygon::area(a).total_cmp(&polygon::area(b)))
        .ok_or_else(|| GeometryError::Degenerate("the offset collapses the polygon".into()))
}

/// Offsets a region given as rings (outer counter-clockwise, holes clockwise)
/// by `d` out of the region (negative = into it); returns the boundary rings
/// of the result (outer counter-clockwise, holes clockwise), possibly none.
pub fn offset_rings_2d(
    rings: &[Vec<DVec2>],
    d: f64,
    tol: &Tolerances,
) -> Result<Vec<Vec<DVec2>>, GeometryError> {
    let rings: Vec<Vec<DVec2>> = rings
        .iter()
        .map(|r| clean_ring(r, tol))
        .filter(|r| r.len() >= 3)
        .collect();
    if rings.is_empty() {
        return Err(GeometryError::Degenerate("nothing to offset".into()));
    }
    let raws: Vec<Vec<DVec2>> = rings.iter().map(|r| raw_offset(r, d)).collect();
    let arrangement = Arrangement::build(&raws, tol);
    let faces = arrangement.bounded_faces(tol);
    let hole_refs: Vec<&[DVec2]> = rings[1..].iter().map(Vec::as_slice).collect();
    let mut kept = vec![false; faces.len()];
    for (k, face) in faces.iter().enumerate() {
        let Some(sample) = arrangement.sample(face) else {
            continue;
        };
        let winding: i32 = raws.iter().map(|r| winding_number(sample, r)).sum();
        if winding < 1 {
            continue;
        }
        if d < 0.0 {
            let inside = polygon::point_in_polygon(sample, &rings[0], &hole_refs, tol)
                == PointInPolygon::Inside;
            let clearance = rings
                .iter()
                .map(|r| polygon::distance_to_ring(sample, r))
                .fold(f64::INFINITY, f64::min);
            if !inside || clearance < d.abs() * (1.0 - 1e-9) - tol.length {
                continue;
            }
        }
        kept[k] = true;
    }
    Ok(arrangement.union_boundary(&faces, &kept, tol))
}

/// Removes repeated and collinear vertices.
fn clean_ring(ring: &[DVec2], tol: &Tolerances) -> Vec<DVec2> {
    let mut out: Vec<DVec2> = Vec::with_capacity(ring.len());
    for &p in ring {
        if out.last().is_none_or(|q| q.distance(p) > tol.length) {
            out.push(p);
        }
    }
    while out.len() > 1 && out[0].distance(out[out.len() - 1]) <= tol.length {
        out.pop();
    }
    let mut changed = true;
    while changed && out.len() >= 3 {
        changed = false;
        for i in 0..out.len() {
            let n = out.len();
            let (a, b, c) = (out[(i + n - 1) % n], out[i], out[(i + 1) % n]);
            let ab = b - a;
            let len = ab.length().max((c - b).length());
            if (ab.perp_dot(c - b)).abs() <= tol.length * len && (c - b).dot(ab) > 0.0 {
                out.remove(i);
                changed = true;
                break;
            }
        }
    }
    out
}

/// Miter offset of one ring along its right-hand normals.
fn raw_offset(ring: &[DVec2], d: f64) -> Vec<DVec2> {
    let n = ring.len();
    let dirs: Vec<DVec2> = (0..n)
        .map(|i| (ring[(i + 1) % n] - ring[i]).normalize_or_zero())
        .collect();
    let normals: Vec<DVec2> = dirs.iter().map(|t| DVec2::new(t.y, -t.x)).collect();
    let mut out = Vec::with_capacity(n + 4);
    for i in 0..n {
        let prev = (i + n - 1) % n;
        let p = ring[i];
        let (a, b) = (p + normals[prev] * d, p + normals[i] * d);
        let cross = dirs[prev].perp_dot(dirs[i]);
        if cross.abs() < 1e-12 {
            out.push(b);
            continue;
        }
        // Intersection of the two offset lines a + s·dirs[prev], b + t·dirs[i].
        let s = (b - a).perp_dot(dirs[i]) / cross;
        let q = a + dirs[prev] * s;
        if q.distance(p) > MITER_LIMIT * d.abs() {
            out.push(a);
            out.push(b);
        } else {
            out.push(q);
        }
    }
    out
}

/// Winding number of `p` with respect to a closed ring.
fn winding_number(p: DVec2, ring: &[DVec2]) -> i32 {
    let mut w = 0;
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
        let side = (b - a).perp_dot(p - a);
        if a.y <= p.y {
            if b.y > p.y && side > 0.0 {
                w += 1;
            }
        } else if b.y <= p.y && side < 0.0 {
            w -= 1;
        }
    }
    w
}

/// A region of the arrangement as directed vertex pairs.
struct Region {
    outer: Vec<(usize, usize)>,
    holes: Vec<Vec<(usize, usize)>>,
}

/// A planar arrangement of segments: snapped vertices and split edges.
struct Arrangement {
    points: Vec<DVec2>,
    /// Undirected edges as vertex pairs (a < b).
    edges: Vec<(usize, usize)>,
}

impl Arrangement {
    fn build(raws: &[Vec<DVec2>], tol: &Tolerances) -> Self {
        let mut segments: Vec<(DVec2, DVec2)> = Vec::new();
        for r in raws {
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                if a.distance(b) > tol.length {
                    segments.push((a, b));
                }
            }
        }
        // Split parameters per segment.
        let mut splits: Vec<Vec<f64>> = vec![vec![0.0, 1.0]; segments.len()];
        for i in 0..segments.len() {
            for j in i + 1..segments.len() {
                let (a, b) = segments[i];
                let (c, e) = segments[j];
                for (ti, tj) in segment_contacts(a, b, c, e, tol) {
                    splits[i].push(ti);
                    splits[j].push(tj);
                }
            }
        }
        let mut arrangement = Arrangement {
            points: Vec::new(),
            edges: Vec::new(),
        };
        let mut seen = std::collections::BTreeSet::new();
        for (k, (a, b)) in segments.iter().enumerate() {
            let mut ts = splits[k].clone();
            ts.sort_by(f64::total_cmp);
            let ids: Vec<usize> = ts
                .iter()
                .map(|&t| arrangement.vertex(*a + (*b - *a) * t, tol))
                .collect();
            for w in ids.windows(2) {
                let (u, v) = (w[0].min(w[1]), w[0].max(w[1]));
                if u != v && seen.insert((u, v)) {
                    arrangement.edges.push((u, v));
                }
            }
        }
        arrangement
    }

    fn vertex(&mut self, p: DVec2, tol: &Tolerances) -> usize {
        if let Some(i) = self.points.iter().position(|q| q.distance(p) <= tol.length) {
            return i;
        }
        self.points.push(p);
        self.points.len() - 1
    }

    /// Bounded regions: a counter-clockwise outer cycle plus the clockwise
    /// cycles of the components it encloses, all with the region on their
    /// left.
    fn bounded_faces(&self, tol: &Tolerances) -> Vec<Region> {
        let mut outgoing: BTreeMap<usize, Vec<(f64, usize)>> = BTreeMap::new();
        for &(u, v) in &self.edges {
            let (pu, pv) = (self.points[u], self.points[v]);
            outgoing
                .entry(u)
                .or_default()
                .push((planar::sort_angle(pv - pu), v));
            outgoing
                .entry(v)
                .or_default()
                .push((planar::sort_angle(pu - pv), u));
        }
        for list in outgoing.values_mut() {
            list.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        }
        let next = |u: usize, v: usize| -> Option<usize> {
            let list = outgoing.get(&v)?;
            let i = list.iter().position(|&(_, w)| w == u)?;
            Some(list[(i + list.len() - 1) % list.len()].1)
        };
        let mut used = std::collections::BTreeSet::new();
        let mut faces = Vec::new();
        let directed: Vec<(usize, usize)> = self
            .edges
            .iter()
            .flat_map(|&(u, v)| [(u, v), (v, u)])
            .collect();
        for &(u0, v0) in &directed {
            if used.contains(&(u0, v0)) {
                continue;
            }
            let mut cycle = Vec::new();
            let (mut u, mut v) = (u0, v0);
            loop {
                if !used.insert((u, v)) || cycle.len() > directed.len() {
                    break;
                }
                cycle.push((u, v));
                let Some(w) = next(u, v) else {
                    break;
                };
                (u, v) = (v, w);
                if (u, v) == (u0, v0) {
                    break;
                }
            }
            faces.push(cycle);
        }
        let rings: Vec<Vec<DVec2>> = faces
            .iter()
            .map(|c| c.iter().map(|&(a, _)| self.points[a]).collect())
            .collect();
        let areas: Vec<f64> = rings.iter().map(|r| polygon::signed_area(r)).collect();
        let mut regions: Vec<Region> = Vec::new();
        let mut index_of = BTreeMap::new();
        for (k, cycle) in faces.iter().enumerate() {
            if areas[k] > tol.length_sq {
                index_of.insert(k, regions.len());
                regions.push(Region {
                    outer: cycle.clone(),
                    holes: Vec::new(),
                });
            }
        }
        for (n, cycle) in faces.iter().enumerate() {
            if areas[n] >= -tol.length_sq {
                continue;
            }
            let mut best: Option<usize> = None;
            for &k in index_of.keys() {
                if areas[k] < -areas[n] || best.is_some_and(|b| areas[k] >= areas[b]) {
                    continue;
                }
                let inside = rings[n]
                    .iter()
                    .copied()
                    .chain(
                        (0..rings[n].len())
                            .map(|i| (rings[n][i] + rings[n][(i + 1) % rings[n].len()]) * 0.5),
                    )
                    .find_map(|p| match polygon::point_in_ring(p, &rings[k], tol) {
                        PointInPolygon::Inside => Some(true),
                        PointInPolygon::Outside => Some(false),
                        PointInPolygon::Boundary => None,
                    })
                    .unwrap_or(false);
                if inside {
                    best = Some(k);
                }
            }
            if let Some(k) = best {
                regions[index_of[&k]].holes.push(cycle.clone());
            }
        }
        regions
    }

    /// A point strictly inside a region (centroid of its largest triangle).
    fn sample(&self, region: &Region) -> Option<DVec2> {
        let ring = |c: &[(usize, usize)]| -> Vec<DVec2> {
            c.iter().map(|&(a, _)| self.points[a]).collect()
        };
        let outer = ring(&region.outer);
        let holes: Vec<Vec<DVec2>> = region.holes.iter().map(|h| ring(h)).collect();
        let all: Vec<DVec2> = outer
            .iter()
            .chain(holes.iter().flatten())
            .copied()
            .collect();
        let tris = tessellate_polygon_2d(&outer, &holes).ok()?;
        tris.iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| all[i as usize]);
                (((b - a).perp_dot(c - a)).abs(), (a + b + c) / 3.0)
            })
            .max_by(|x, y| x.0.total_cmp(&y.0))
            .map(|(_, p)| p)
    }

    /// Boundary rings of the union of the kept faces.
    fn union_boundary(&self, faces: &[Region], kept: &[bool], tol: &Tolerances) -> Vec<Vec<DVec2>> {
        let mut kept_directed = std::collections::BTreeSet::new();
        for (face, &keep) in faces.iter().zip(kept) {
            if keep {
                kept_directed.extend(face.outer.iter().copied());
                kept_directed.extend(face.holes.iter().flatten().copied());
            }
        }
        let boundary: Vec<(usize, usize)> = kept_directed
            .iter()
            .copied()
            .filter(|&(u, v)| !kept_directed.contains(&(v, u)))
            .collect();
        let Some(cycles) = planar::boundary_cycles(&boundary, |i| self.points[i]) else {
            return Vec::new();
        };
        cycles
            .into_iter()
            .map(|c| clean_ring(&c.iter().map(|&i| self.points[i]).collect::<Vec<_>>(), tol))
            .filter(|r| r.len() >= 3 && polygon::area(r) > tol.length_sq)
            .collect()
    }
}

/// Contact parameters between segments `ab` and `ce` (crossings, touches and
/// collinear overlap ends).
fn segment_contacts(a: DVec2, b: DVec2, c: DVec2, e: DVec2, tol: &Tolerances) -> Vec<(f64, f64)> {
    let r = b - a;
    let s = e - c;
    let denom = r.perp_dot(s);
    let mut out = Vec::new();
    let param = |p: DVec2, o: DVec2, dir: DVec2| (p - o).dot(dir) / dir.length_squared();
    if denom.abs() > 1e-12 * r.length() * s.length() {
        let t = (c - a).perp_dot(s) / denom;
        let u = (c - a).perp_dot(r) / denom;
        let et = tol.length / r.length();
        let eu = tol.length / s.length();
        if t >= -et && t <= 1.0 + et && u >= -eu && u <= 1.0 + eu {
            out.push((t.clamp(0.0, 1.0), u.clamp(0.0, 1.0)));
        }
    } else {
        // Parallel: collinear overlaps contribute their end points.
        if polygon::distance_to_segment_2d(c, a, b).min(polygon::distance_to_segment_2d(a, c, e))
            > tol.length
            && polygon::distance_to_segment_2d(e, a, b) > tol.length
        {
            return out;
        }
        for p in [c, e] {
            if polygon::distance_to_segment_2d(p, a, b) <= tol.length {
                out.push((
                    param(p, a, r).clamp(0.0, 1.0),
                    param(p, c, s).clamp(0.0, 1.0),
                ));
            }
        }
        for p in [a, b] {
            if polygon::distance_to_segment_2d(p, c, e) <= tol.length {
                out.push((
                    param(p, a, r).clamp(0.0, 1.0),
                    param(p, c, s).clamp(0.0, 1.0),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::NoDeltas;
    use glam::DVec3;
    use proptest::prelude::*;

    fn tol() -> Tolerances {
        Tolerances::default()
    }

    fn v(x: f64, y: f64) -> DVec2 {
        DVec2::new(x, y)
    }

    fn square(s: f64) -> Vec<DVec2> {
        vec![v(0.0, 0.0), v(s, 0.0), v(s, s), v(0.0, s)]
    }

    #[test]
    fn square_offsets_in_and_out() {
        let inner = offset_polygon_2d(&square(1000.0), -100.0, &tol()).unwrap();
        assert!((polygon::area(&inner) - 800.0 * 800.0).abs() < 1e-6);
        let outer = offset_polygon_2d(&square(1000.0), 100.0, &tol()).unwrap();
        assert!((polygon::area(&outer) - 1200.0 * 1200.0).abs() < 1e-6);
        // Orientation does not matter for the pure polygon offset.
        let mut cw = square(1000.0);
        cw.reverse();
        let again = offset_polygon_2d(&cw, -100.0, &tol()).unwrap();
        assert!((polygon::area(&again) - 640_000.0).abs() < 1e-6);
        assert!(offset_polygon_2d(&square(100.0), -60.0, &tol()).is_err());
    }

    fn l_shape() -> Vec<DVec2> {
        // 1000 × 1000 with a 800 × 800 notch: arms 200 wide.
        vec![
            v(0.0, 0.0),
            v(1000.0, 0.0),
            v(1000.0, 200.0),
            v(200.0, 200.0),
            v(200.0, 1000.0),
            v(0.0, 1000.0),
        ]
    }

    #[test]
    fn l_shape_inward_trims_the_concave_corner() {
        let inner = offset_polygon_2d(&l_shape(), -50.0, &tol()).unwrap();
        // Arms 100 wide: (900 × 100) + (100 × 800).
        assert!((polygon::area(&inner) - (900.0 * 100.0 + 100.0 * 800.0)).abs() < 1e-6);
        assert_eq!(inner.len(), 6);
        assert!(!polygon::rings_cross(&inner, &inner, &tol()));
    }

    #[test]
    fn l_shape_past_its_narrow_width_collapses_cleanly() {
        let rings = offset_rings_2d(&[l_shape()], -120.0, &tol()).unwrap();
        assert!(rings.is_empty(), "{rings:?}");
        // A fat L with one thin arm: only the fat part survives.
        let fat = vec![
            v(0.0, 0.0),
            v(1000.0, 0.0),
            v(1000.0, 100.0),
            v(500.0, 100.0),
            v(500.0, 1000.0),
            v(0.0, 1000.0),
        ];
        let rings = offset_rings_2d(&[fat], -80.0, &tol()).unwrap();
        assert_eq!(rings.len(), 1);
        assert!(
            (polygon::area(&rings[0]) - 340.0 * 840.0).abs() < 1e-6,
            "{}",
            polygon::area(&rings[0])
        );
    }

    #[test]
    fn two_lobes_split_into_two_rings() {
        // A dumbbell: two 400 squares joined by a 100-wide bridge.
        let shape = vec![
            v(0.0, 0.0),
            v(400.0, 0.0),
            v(400.0, 150.0),
            v(600.0, 150.0),
            v(600.0, 0.0),
            v(1000.0, 0.0),
            v(1000.0, 400.0),
            v(600.0, 400.0),
            v(600.0, 250.0),
            v(400.0, 250.0),
            v(400.0, 400.0),
            v(0.0, 400.0),
        ];
        let rings = offset_rings_2d(&[shape], -60.0, &tol()).unwrap();
        assert_eq!(rings.len(), 2);
        for r in &rings {
            assert!((polygon::area(r) - 280.0 * 280.0).abs() < 1e-6);
        }
    }

    #[test]
    fn sharp_outward_corners_are_bevelled() {
        let spike = vec![v(0.0, 0.0), v(1000.0, 0.0), v(0.0, 50.0)];
        let out = offset_polygon_2d(&spike, 10.0, &tol()).unwrap();
        assert!(out.iter().all(|p| p.x < 1000.0 + 10.0 * MITER_LIMIT + 1e-9));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn convex_polygons_shrink_by_perimeter_times_distance(
            n in 3usize..10,
            r in 200.0f64..1000.0,
            d in 1.0f64..20.0,
        ) {
            let ring: Vec<DVec2> = (0..n)
                .map(|i| {
                    let a = std::f64::consts::TAU * i as f64 / n as f64;
                    v(r * libm::cos(a), r * libm::sin(a))
                })
                .collect();
            let inner = offset_polygon_2d(&ring, -d, &tol()).unwrap();
            // Regular polygon: A(d) = A − P·d + d²·n·tan(π/n).
            let expected = polygon::area(&ring) - polygon::perimeter(&ring) * d
                + d * d * n as f64 * libm::tan(std::f64::consts::PI / n as f64);
            prop_assert!((polygon::area(&inner) - expected).abs() < 1e-6 * expected);
            prop_assert!(polygon::is_convex(&inner, &tol()));
        }
    }

    fn rect_face(m: &mut HalfEdgeMesh, s: f64) -> FaceId {
        let pts = [
            DVec3::ZERO,
            DVec3::new(s, 0.0, 0.0),
            DVec3::new(s, s, 0.0),
            DVec3::new(0.0, s, 0.0),
        ];
        for i in 0..4 {
            m.add_edge(pts[i], pts[(i + 1) % 4], &tol(), &mut NoDeltas)
                .unwrap();
        }
        m.faces().next().unwrap().0
    }

    #[test]
    fn face_offset_inward_makes_ring_and_inner_face() {
        let mut m = HalfEdgeMesh::new();
        let f = rect_face(&mut m, 1000.0);
        let r = offset_face_boundary(&mut m, f, Length::mm(-100.0), &tol(), &mut NoDeltas).unwrap();
        assert_eq!(m.face_count(), 2);
        assert_eq!(r.edges.len(), 4);
        let inner = r.face.unwrap();
        assert!((m.face_area(inner).unwrap() - 640_000.0).abs() < 1e-6);
        assert!((m.face_area(f).unwrap() - 360_000.0).abs() < 1e-6);
        assert!(m.validate(&tol()).is_empty());
    }

    #[test]
    fn face_offset_outward_makes_a_surrounding_ring() {
        let mut m = HalfEdgeMesh::new();
        let f = rect_face(&mut m, 1000.0);
        let r = offset_face_boundary(&mut m, f, Length::mm(100.0), &tol(), &mut NoDeltas).unwrap();
        assert_eq!(m.face_count(), 2);
        let ring = r.face.unwrap();
        assert!((m.face_area(ring).unwrap() - (1200.0 * 1200.0 - 1e6)).abs() < 1e-6);
        assert!(m.validate(&tol()).is_empty());
    }

    #[test]
    fn face_with_a_hole_offsets_both_loops() {
        let mut m = HalfEdgeMesh::new();
        let f = rect_face(&mut m, 1000.0);
        let hole = [
            DVec3::new(400.0, 400.0, 0.0),
            DVec3::new(600.0, 400.0, 0.0),
            DVec3::new(600.0, 600.0, 0.0),
            DVec3::new(400.0, 600.0, 0.0),
        ];
        for i in 0..4 {
            m.add_edge(hole[i], hole[(i + 1) % 4], &tol(), &mut NoDeltas)
                .unwrap();
        }
        let inner = m.faces().map(|(id, _)| id).find(|id| *id != f).unwrap();
        m.erase_face(inner, false, &tol(), &mut NoDeltas).unwrap();
        let r = offset_face_boundary(&mut m, f, Length::mm(-50.0), &tol(), &mut NoDeltas).unwrap();
        assert_eq!(r.edges.len(), 8);
        let piece = r.face.unwrap();
        // 900² minus the grown hole 300².
        assert!((m.face_area(piece).unwrap() - (900.0 * 900.0 - 300.0 * 300.0)).abs() < 1e-6);
        assert!(m.validate(&tol()).is_empty());
    }
}
