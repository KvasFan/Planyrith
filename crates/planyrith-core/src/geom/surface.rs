//! Implements docs/03-geometry-kernel.md §4.21 (surfaces, milestone 1.15):
//! patches that fill a closed boundary — a bilinearly blended (Coons)
//! patch over four sides, or a radial patch over any number — and a builder
//! from polygons to an indexed mesh.

use std::collections::BTreeMap;

use glam::DVec3;

use crate::geom::mesh::{DirectedEdgeIndex, IndexedEdge, IndexedFace, IndexedMesh};
use crate::geom::{EdgeFlags, FaceSides, GeometryError};
use crate::tolerance::Tolerances;

/// How a boundary is filled.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum FillMethod {
    /// Four sides when the boundary has four corners or more, else radial.
    #[default]
    Auto,
    /// A Coons patch between the four sharpest corners.
    Coons,
    /// Rings shrinking toward the boundary's center.
    Radial,
}

/// Points of a polyline at `count + 1` equal arc-length steps.
fn resample(points: &[DVec3], count: usize) -> Vec<DVec3> {
    let lengths: Vec<f64> = points.windows(2).map(|w| w[0].distance(w[1])).collect();
    let total: f64 = lengths.iter().sum::<f64>().max(f64::MIN_POSITIVE);
    (0..=count)
        .map(|k| {
            let mut s = total * k as f64 / count as f64;
            for (i, l) in lengths.iter().enumerate() {
                if s <= *l || i + 1 == lengths.len() {
                    let t = if *l > 0.0 {
                        (s / l).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    return points[i].lerp(points[i + 1], t);
                }
                s -= l;
            }
            points[points.len() - 1]
        })
        .collect()
}

/// The polygons (quads where flat, else triangle pairs) filling the closed
/// `boundary` (first point not repeated), `divisions` steps each way.
pub fn fill(
    boundary: &[DVec3],
    divisions: usize,
    method: FillMethod,
    tol: &Tolerances,
) -> Result<Vec<Vec<DVec3>>, GeometryError> {
    let n = boundary.len();
    if n < 3 {
        return Err(GeometryError::NotApplicable(
            "a boundary needs three corners".into(),
        ));
    }
    let divisions = divisions.clamp(1, 200);
    let use_coons = match method {
        FillMethod::Coons => {
            if n < 4 {
                return Err(GeometryError::NotApplicable(
                    "a four-sided patch needs four corners".into(),
                ));
            }
            true
        }
        FillMethod::Radial => false,
        FillMethod::Auto => n >= 4,
    };
    let mut quads: Vec<[DVec3; 4]> = Vec::new();
    if use_coons {
        // The four sharpest corners, in loop order.
        let turn = |i: usize| {
            let a = boundary[(i + n - 1) % n];
            let b = boundary[i];
            let c = boundary[(i + 1) % n];
            let (u, v) = ((b - a).normalize_or_zero(), (c - b).normalize_or_zero());
            libm::acos(u.dot(v).clamp(-1.0, 1.0))
        };
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| turn(b).total_cmp(&turn(a)).then(a.cmp(&b)));
        let mut corners: Vec<usize> = order[..4].to_vec();
        corners.sort_unstable();
        let side = |from: usize, to: usize| -> Vec<DVec3> {
            let mut pts = vec![boundary[from]];
            let mut i = from;
            while i != to {
                i = (i + 1) % n;
                pts.push(boundary[i]);
            }
            pts
        };
        let bottom = resample(&side(corners[0], corners[1]), divisions);
        let right = resample(&side(corners[1], corners[2]), divisions);
        let mut top = resample(&side(corners[2], corners[3]), divisions);
        top.reverse();
        let mut left = resample(&side(corners[3], corners[0]), divisions);
        left.reverse();
        let (p00, p10, p11, p01) = (bottom[0], bottom[divisions], top[divisions], top[0]);
        let at = |i: usize, j: usize| -> DVec3 {
            let (u, v) = (i as f64 / divisions as f64, j as f64 / divisions as f64);
            (1.0 - v) * bottom[i] + v * top[i] + (1.0 - u) * left[j] + u * right[j]
                - ((1.0 - u) * (1.0 - v) * p00
                    + u * (1.0 - v) * p10
                    + (1.0 - u) * v * p01
                    + u * v * p11)
        };
        for j in 0..divisions {
            for i in 0..divisions {
                quads.push([at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)]);
            }
        }
    } else {
        let center = boundary.iter().copied().sum::<DVec3>() / n as f64;
        let ring = |r: usize, k: usize| {
            center + (boundary[k % n] - center) * (r as f64 / divisions as f64)
        };
        for r in 0..divisions {
            for k in 0..n {
                quads.push([
                    ring(r + 1, k),
                    ring(r + 1, k + 1),
                    ring(r, k + 1),
                    ring(r, k),
                ]);
            }
        }
    }
    Ok(quads
        .into_iter()
        .flat_map(|q| pieces(q.to_vec(), tol))
        .collect())
}

/// `poly` without repeated corners, as one flat piece, or as a fan of
/// triangles when it is not flat; nothing when it has no area.
pub fn pieces(poly: Vec<DVec3>, tol: &Tolerances) -> Vec<Vec<DVec3>> {
    let mut clean: Vec<DVec3> = Vec::with_capacity(poly.len());
    for p in poly {
        if clean.last().is_none_or(|l| !tol.same_point(*l, p)) {
            clean.push(p);
        }
    }
    while clean.len() > 1 && tol.same_point(clean[0], clean[clean.len() - 1]) {
        clean.pop();
    }
    if clean.len() < 3 {
        return Vec::new();
    }
    let mut area = DVec3::ZERO;
    for (a, b) in clean.iter().zip(clean.iter().cycle().skip(1)) {
        area += a.cross(*b);
    }
    if area.length() <= tol.length_sq {
        return Vec::new();
    }
    let nn = area.normalize();
    if clean.len() == 3
        || clean
            .iter()
            .all(|c| nn.dot(*c - clean[0]).abs() <= tol.planarity)
    {
        return vec![clean];
    }
    (1..clean.len() - 1)
        .map(|k| vec![clean[0], clean[k], clean[k + 1]])
        .filter(|t| (t[1] - t[0]).cross(t[2] - t[0]).length() > tol.length_sq)
        .collect()
}

/// An indexed mesh of `faces` (outer loop first, then holes; orientation
/// kept), vertices merged within the length tolerance.
pub fn indexed(faces: &[(Vec<Vec<DVec3>>, FaceSides)], tol: &Tolerances) -> IndexedMesh {
    let mut out = IndexedMesh::default();
    let mut ids: BTreeMap<[i64; 3], u32> = BTreeMap::new();
    let q = tol.length.max(1e-6);
    let mut vertex = |p: DVec3, out: &mut IndexedMesh| -> u32 {
        let key = [
            (p.x / q).round() as i64,
            (p.y / q).round() as i64,
            (p.z / q).round() as i64,
        ];
        *ids.entry(key).or_insert_with(|| {
            out.vertices.push(p);
            (out.vertices.len() - 1) as u32
        })
    };
    let mut edges: BTreeMap<(u32, u32), (u32, usize)> = BTreeMap::new();
    for (rings, sides) in faces {
        let mut loops = Vec::new();
        for poly in rings {
            let idx: Vec<u32> = poly.iter().map(|p| vertex(*p, &mut out)).collect();
            let mut lp = Vec::new();
            for k in 0..idx.len() {
                let (a, b) = (idx[k], idx[(k + 1) % idx.len()]);
                if a == b {
                    continue;
                }
                let key = (a.min(b), a.max(b));
                let entry = edges.entry(key).or_insert_with(|| {
                    out.edges.push(IndexedEdge {
                        a: key.0,
                        b: key.1,
                        flags: EdgeFlags::NONE,
                        curve: None,
                    });
                    ((out.edges.len() - 1) as u32, 0)
                });
                entry.1 += 1;
                lp.push(if a < b {
                    DirectedEdgeIndex::forward(entry.0)
                } else {
                    DirectedEdgeIndex::reversed(entry.0)
                });
            }
            if lp.len() >= 3 {
                loops.push(lp);
            } else if loops.is_empty() {
                break;
            }
        }
        if !loops.is_empty() {
            out.faces.push(IndexedFace {
                loops,
                plane: None,
                sides: sides.clone(),
            });
        }
    }
    out
}

/// How many faces use each edge of `mesh`.
pub fn edge_uses(mesh: &IndexedMesh) -> Vec<usize> {
    let mut uses = vec![0; mesh.edges.len()];
    for f in &mesh.faces {
        for d in f.loops.iter().flatten() {
            if let Some(u) = uses.get_mut(d.index() as usize) {
                *u += 1;
            }
        }
    }
    uses
}

/// Marks soft and smooth every edge of `mesh` shared by two faces whose
/// normals differ by at most `max_angle` radians.
pub fn soften(mesh: &mut IndexedMesh, max_angle: f64) {
    let mut faces_of: Vec<Vec<usize>> = vec![Vec::new(); mesh.edges.len()];
    let mut normals = Vec::with_capacity(mesh.faces.len());
    for (i, f) in mesh.faces.iter().enumerate() {
        let mut n = DVec3::ZERO;
        if let Some(outer) = f.loops.first() {
            let pts: Vec<DVec3> = outer
                .iter()
                .map(|d| {
                    let e = &mesh.edges[d.index() as usize];
                    mesh.vertices[(if d.is_reversed() { e.b } else { e.a }) as usize]
                })
                .collect();
            for (a, b) in pts.iter().zip(pts.iter().cycle().skip(1)) {
                n += a.cross(*b);
            }
        }
        normals.push(n.normalize_or_zero());
        for d in f.loops.iter().flatten() {
            if let Some(v) = faces_of.get_mut(d.index() as usize) {
                v.push(i);
            }
        }
    }
    let limit = libm::cos(max_angle.clamp(0.0, std::f64::consts::PI));
    for (e, fs) in faces_of.iter().enumerate() {
        if let [a, b] = fs.as_slice()
            && normals[*a].dot(normals[*b]) >= limit - 1e-12
        {
            mesh.edges[e].flags = EdgeFlags::SOFT | EdgeFlags::SMOOTH;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_saddle_and_a_flat_disc() {
        let tol = Tolerances::default();
        // A saddle: corners alternately up and down.
        let boundary = vec![
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(100.0, 0.0, 50.0),
            DVec3::new(100.0, 100.0, 0.0),
            DVec3::new(0.0, 100.0, 50.0),
        ];
        let polys = fill(&boundary, 8, FillMethod::Auto, &tol).unwrap();
        // Non-flat quads became triangle pairs.
        assert!(polys.len() > 64);
        // The patch's middle is at the corners' average height.
        let mid = polys
            .iter()
            .flatten()
            .find(|p| (p.x - 50.0).abs() < 1e-9 && (p.y - 50.0).abs() < 1e-9)
            .unwrap();
        assert!((mid.z - 25.0).abs() < 1e-9, "{mid}");
        // A flat hexagon fills radially with flat pieces.
        let hex: Vec<DVec3> = (0..6)
            .map(|k| {
                let t = std::f64::consts::TAU * f64::from(k) / 6.0;
                DVec3::new(50.0 * libm::cos(t), 50.0 * libm::sin(t), 0.0)
            })
            .collect();
        let polys = fill(&hex, 3, FillMethod::Radial, &tol).unwrap();
        assert!(polys.iter().flatten().all(|p| p.z.abs() < 1e-12));
        let mut m = indexed(
            &polys
                .into_iter()
                .map(|p| (vec![p], FaceSides::default()))
                .collect::<Vec<_>>(),
            &tol,
        );
        assert_eq!(m.vertices.len(), 1 + 6 * 3);
        soften(&mut m, 0.1);
        let uses = edge_uses(&m);
        for (e, u) in m.edges.iter().zip(&uses) {
            assert_eq!(e.flags.contains(EdgeFlags::SOFT), *u == 2);
        }
        assert!(fill(&boundary[..2], 4, FillMethod::Auto, &tol).is_err());
    }
}
