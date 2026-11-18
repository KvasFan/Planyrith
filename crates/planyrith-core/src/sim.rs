//! Implements docs/03-geometry-kernel.md §12 (simulation, milestone 1.16):
//! a tetrahedral mesh of a solid, linear static stress, natural
//! frequencies and steady heat conduction by the finite element method.
//!
//! Units are consistent: millimeters, newtons, megapascals (N/mm²),
//! tonnes (N·s²/mm) and seconds; heat in watts and kelvin.
//!
//! The mesh fills the solid's bounding box with a grid of cells sized to
//! fit it exactly; a cell whose middle is inside the solid becomes six
//! tetrahedra sharing its main diagonal (a conforming split), each with
//! ten nodes (quadratic), which keeps bending accurate with few cells.
//! Faces square to the axes on grid lines are met exactly; other surfaces
//! are approximated in steps of the cell size. Every boundary triangle is
//! assigned to the nearest face of the solid, so fixtures and loads named
//! by faces reach the right nodes.
//!
//! Everything is sequential and in a fixed order, so results are
//! deterministic (docs/12 §2).

// Matrix code reads clearer with indices.
#![allow(clippy::needless_range_loop)]

use std::collections::BTreeMap;

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geom::boolean::{Classification, classify_point};
use crate::geom::{GeometryError, HalfEdgeMesh};
use crate::ids::FaceId;
use crate::tolerance::Tolerances;

/// Standard gravity, mm/s².
pub const GRAVITY_MM_S2: f64 = 9806.65;

/// A material's properties for simulation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SimMaterial {
    /// Its name.
    pub name: String,
    /// Young's modulus, MPa.
    pub youngs_mpa: f64,
    /// Poisson's ratio.
    pub poisson: f64,
    /// Density, kg/m³.
    pub density_kg_m3: f64,
    /// Thermal conductivity, W/(m·K).
    pub conductivity_w_mk: f64,
    /// Yield strength, MPa (none for brittle or anisotropic materials).
    #[serde(default)]
    pub yield_mpa: Option<f64>,
}

impl SimMaterial {
    /// Checks the values.
    pub fn check(&self) -> Result<(), String> {
        if !(self.youngs_mpa > 0.0 && self.youngs_mpa.is_finite()) {
            return Err("Young's modulus must be positive".into());
        }
        if !(self.poisson > -0.99 && self.poisson < 0.499) {
            return Err("Poisson's ratio must be between -0.99 and 0.499".into());
        }
        if !(self.density_kg_m3 >= 0.0 && self.density_kg_m3.is_finite()) {
            return Err("density must be zero or more".into());
        }
        if !(self.conductivity_w_mk > 0.0 && self.conductivity_w_mk.is_finite()) {
            return Err("conductivity must be positive".into());
        }
        Ok(())
    }

    /// Density in t/mm³.
    fn density(&self) -> f64 {
        self.density_kg_m3 * 1.0e-12
    }
}

/// The names of the built-in materials.
pub const MATERIALS: [&str; 13] = [
    "steel",
    "stainless-steel",
    "aluminum",
    "titanium",
    "copper",
    "brass",
    "cast-iron",
    "abs",
    "pla",
    "nylon",
    "oak",
    "concrete",
    "glass",
];

/// A built-in material by name (typical room-temperature values).
pub fn material(name: &str) -> Option<SimMaterial> {
    let (e, nu, rho, k, y) = match name.to_ascii_lowercase().as_str() {
        "steel" => (210_000.0, 0.30, 7850.0, 50.0, Some(235.0)),
        "stainless-steel" => (193_000.0, 0.29, 8000.0, 16.0, Some(215.0)),
        "aluminum" | "aluminium" => (69_000.0, 0.33, 2700.0, 167.0, Some(276.0)),
        "titanium" => (114_000.0, 0.34, 4430.0, 6.7, Some(880.0)),
        "copper" => (117_000.0, 0.34, 8960.0, 390.0, Some(70.0)),
        "brass" => (100_000.0, 0.34, 8500.0, 120.0, Some(200.0)),
        "cast-iron" => (120_000.0, 0.26, 7200.0, 52.0, None),
        "abs" => (2300.0, 0.35, 1050.0, 0.17, Some(40.0)),
        "pla" => (3500.0, 0.36, 1250.0, 0.13, Some(55.0)),
        "nylon" => (2700.0, 0.39, 1140.0, 0.25, Some(70.0)),
        "oak" => (12_000.0, 0.35, 750.0, 0.17, None),
        "concrete" => (30_000.0, 0.20, 2400.0, 1.7, None),
        "glass" => (70_000.0, 0.22, 2500.0, 1.0, None),
        _ => return None,
    };
    Some(SimMaterial {
        name: name.to_ascii_lowercase(),
        youngs_mpa: e,
        poisson: nu,
        density_kg_m3: rho,
        conductivity_w_mk: k,
        yield_mpa: y,
    })
}

// ----- the mesh -------------------------------------------------------------------------

/// A mesh of quadratic tetrahedra.
#[derive(Debug, Clone)]
pub struct TetMesh {
    /// Node positions.
    pub nodes: Vec<DVec3>,
    /// Tetrahedra: four corners (positive volume), then the middles of
    /// edges 01, 12, 02, 03, 13, 23.
    pub tets: Vec<[u32; 10]>,
    /// Boundary triangles: three corners counter-clockwise seen from
    /// outside, then the middles of edges 01, 12, 20.
    pub boundary: Vec<[u32; 6]>,
    /// The tetrahedron behind each boundary triangle.
    pub boundary_tet: Vec<u32>,
    /// The solid's face nearest each boundary triangle.
    pub boundary_face: Vec<Option<FaceId>>,
    /// The cell size along each axis, mm.
    pub cell: DVec3,
}

/// Distance from `p` to face `f` of `mesh`.
fn face_distance(mesh: &HalfEdgeMesh, f: FaceId, p: DVec3, tol: &Tolerances) -> Option<f64> {
    let face = mesh.face(f)?;
    let d = face.plane.signed_distance(p);
    let q = p - face.plane.normal * d;
    if crate::geom::boolean::polygon_contains(mesh, f, q, tol)
        .ok()?
        .is_inside_or_on()
    {
        return Some(d.abs());
    }
    let mut best = f64::MAX;
    let mut loops = vec![face.outer];
    loops.extend(face.holes.iter().copied());
    for l in loops {
        let pts = mesh.loop_positions(l).ok()?;
        for (a, b) in pts.iter().zip(pts.iter().cycle().skip(1)) {
            let ab = *b - *a;
            let t = ((p - *a).dot(ab) / ab.length_squared().max(1e-30)).clamp(0.0, 1.0);
            best = best.min(p.distance(*a + ab * t));
        }
    }
    Some(best)
}

/// Meshes a closed solid with cells of about `size` mm; refused when it
/// would need more than `max_nodes` nodes.
pub fn tet_mesh(
    solid: &HalfEdgeMesh,
    size: f64,
    max_nodes: usize,
    tol: &Tolerances,
) -> Result<TetMesh, GeometryError> {
    let (mut lo, mut hi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
    for (_, v) in solid.vertices() {
        lo = lo.min(v.position);
        hi = hi.max(v.position);
    }
    if lo.x > hi.x {
        return Err(GeometryError::NotApplicable("the solid is empty".into()));
    }
    if !(size.is_finite() && size > 0.0) {
        return Err(GeometryError::NotApplicable(
            "the mesh size must be a positive length".into(),
        ));
    }
    let ext = hi - lo;
    let count = |e: f64| ((e / size).round() as usize).max(1);
    let n = [count(ext.x), count(ext.y), count(ext.z)];
    let estimate = n
        .iter()
        .fold(8usize, |acc, &k| acc.saturating_mul(k.saturating_add(1)));
    let cells = n.iter().fold(1usize, |acc, &k| acc.saturating_mul(k));
    if estimate > max_nodes.saturating_mul(4) || cells > max_nodes {
        return Err(GeometryError::NotApplicable(format!(
            "a mesh of {size} mm would be too fine for this solid; give a larger size"
        )));
    }
    let cell = DVec3::new(
        ext.x / n[0] as f64,
        ext.y / n[1] as f64,
        ext.z / n[2] as f64,
    );
    let corner = |i: usize, j: usize, k: usize| {
        lo + DVec3::new(i as f64 * cell.x, j as f64 * cell.y, k as f64 * cell.z)
    };
    let mut nodes: Vec<DVec3> = Vec::new();
    let mut corner_id: BTreeMap<[usize; 3], u32> = BTreeMap::new();
    let mut mid_id: BTreeMap<(u32, u32), u32> = BTreeMap::new();
    let mut tets: Vec<[u32; 10]> = Vec::new();
    // The six tetrahedra of a cube around its 0–7 diagonal.
    const PATHS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    for k in 0..n[2] {
        for j in 0..n[1] {
            for i in 0..n[0] {
                let mid = corner(i, j, k) + cell * 0.5;
                let inside = matches!(
                    classify_point(solid, mid, tol)?,
                    Classification::Inside | Classification::On { .. }
                );
                if !inside {
                    continue;
                }
                let mut c = [0u32; 8];
                for (b, slot) in c.iter_mut().enumerate() {
                    let key = [i + (b & 1), j + ((b >> 1) & 1), k + ((b >> 2) & 1)];
                    *slot = *corner_id.entry(key).or_insert_with(|| {
                        nodes.push(corner(key[0], key[1], key[2]));
                        (nodes.len() - 1) as u32
                    });
                }
                for path in PATHS {
                    let v1 = 1 << path[0];
                    let v2 = v1 | (1 << path[1]);
                    let mut t = [c[0], c[v1], c[v2], c[7]];
                    let [a, b, cc, d] = t.map(|x| nodes[x as usize]);
                    if (b - a).cross(cc - a).dot(d - a) < 0.0 {
                        t.swap(1, 2);
                    }
                    let mut e = [0u32; 10];
                    e[..4].copy_from_slice(&t);
                    for (slot, (p, q)) in [(0, 1), (1, 2), (0, 2), (0, 3), (1, 3), (2, 3)]
                        .into_iter()
                        .enumerate()
                    {
                        let (x, y) = (t[p].min(t[q]), t[p].max(t[q]));
                        e[4 + slot] = *mid_id.entry((x, y)).or_insert_with(|| {
                            nodes.push((nodes[x as usize] + nodes[y as usize]) * 0.5);
                            (nodes.len() - 1) as u32
                        });
                    }
                    tets.push(e);
                }
            }
        }
    }
    if tets.is_empty() {
        return Err(GeometryError::NotApplicable(
            "the solid is thinner than the mesh size; give a smaller size".into(),
        ));
    }
    if nodes.len() > max_nodes {
        return Err(GeometryError::NotApplicable(format!(
            "a mesh of {size} mm needs {} nodes (at most {max_nodes}); give a larger size",
            nodes.len()
        )));
    }
    // Boundary triangles: faces of one tetrahedron only.
    let mut faces: BTreeMap<[u32; 3], (u32, usize, u32)> = BTreeMap::new();
    for (ti, t) in tets.iter().enumerate() {
        for opposite in 0..4 {
            let mut f = [0u32; 3];
            let mut w = 0;
            for (i, v) in t[..4].iter().enumerate() {
                if i != opposite {
                    f[w] = *v;
                    w += 1;
                }
            }
            let mut key = f;
            key.sort_unstable();
            let e = faces.entry(key).or_insert((ti as u32, opposite, 0));
            e.2 += 1;
        }
    }
    let mid = |a: u32, b: u32| mid_id.get(&(a.min(b), a.max(b))).copied().unwrap_or(0);
    let mut boundary = Vec::new();
    let mut boundary_tet = Vec::new();
    for (key, (ti, opposite, uses)) in &faces {
        if *uses != 1 {
            continue;
        }
        let t = tets[*ti as usize];
        let [a, b, c] = *key;
        let (pa, pb, pc) = (nodes[a as usize], nodes[b as usize], nodes[c as usize]);
        let inner = nodes[t[*opposite] as usize];
        let (a, b, c) = if (pb - pa).cross(pc - pa).dot(pa - inner) >= 0.0 {
            (a, b, c)
        } else {
            (a, c, b)
        };
        boundary.push([a, b, c, mid(a, b), mid(b, c), mid(c, a)]);
        boundary_tet.push(*ti);
    }
    let mut mesh = TetMesh {
        nodes,
        tets,
        boundary,
        boundary_tet,
        boundary_face: Vec::new(),
        cell,
    };
    mesh.boundary_face = assign_faces(&mesh, solid, tol);
    Ok(mesh)
}

/// The solid's face nearest each boundary triangle that looks its way
/// (within ¾ of a cell).
fn assign_faces(mesh: &TetMesh, solid: &HalfEdgeMesh, tol: &Tolerances) -> Vec<Option<FaceId>> {
    let reach = mesh.cell.max_element() * 0.75 + tol.length;
    let solid_faces: Vec<(FaceId, DVec3, DVec3, DVec3)> = solid
        .faces()
        .filter_map(|(f, face)| {
            let pts = solid.loop_positions(face.outer).ok()?;
            let mut l = DVec3::splat(f64::MAX);
            let mut h = DVec3::splat(f64::MIN);
            for p in pts {
                l = l.min(p);
                h = h.max(p);
            }
            Some((f, face.plane.normal, l, h))
        })
        .collect();
    mesh.boundary
        .iter()
        .map(|tri| {
            let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| mesh.nodes[i as usize]);
            let centre = (a + b + c) / 3.0;
            let normal = (b - a).cross(c - a).normalize_or_zero();
            let mut best: Option<(f64, FaceId)> = None;
            for (f, fn_, l, h) in &solid_faces {
                if fn_.dot(normal) <= 1e-6 {
                    continue;
                }
                if (centre - centre.clamp(*l, *h)).length() > reach {
                    continue;
                }
                if let Some(d) = face_distance(solid, *f, centre, tol)
                    && d <= reach
                    && best.is_none_or(|(bd, _)| d < bd)
                {
                    best = Some((d, *f));
                }
            }
            best.map(|(_, f)| f)
        })
        .collect()
}

/// The point of triangle `t` nearest `p`.
fn nearest_on_triangle(p: DVec3, t: &[DVec3; 3]) -> DVec3 {
    let (a, b, c) = (t[0], t[1], t[2]);
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
}

/// How a mesh was fitted to its solid's faces.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FitReport {
    /// Boundary corners moved onto the faces.
    pub moved: usize,
    /// Boundary corners left where they were (the move would have folded
    /// an element).
    pub kept: usize,
    /// The largest move, mm.
    pub largest_mm: f64,
}

/// Moves the mesh's boundary onto the solid's faces (docs/03 §12,
/// milestone 1.41): each boundary corner goes to the nearest point of the
/// solid within ¾ of a cell — where the boundary turns by more than 60°
/// around it, to the nearest sharp edge of the solid when one is that
/// near — backing off (halving, at most six times, then
/// staying) wherever an element would fold or shrink below a tenth of its
/// volume; edge middles are recomputed, those on the boundary placed on the
/// faces when that bends the edge by less than a tenth of its length.
pub fn fit_boundary(
    mesh: &mut TetMesh,
    solid: &HalfEdgeMesh,
    tol: &Tolerances,
) -> Result<FitReport, GeometryError> {
    use crate::geom::tessellate::tessellate_face;
    let mut triangles: Vec<[DVec3; 3]> = Vec::new();
    for (f, _) in solid.faces() {
        let t = tessellate_face(solid, f, tol)?;
        for tri in &t.triangles {
            triangles.push(tri.map(|i| t.positions[i as usize]));
        }
    }
    let nearest = |p: DVec3| {
        let mut best = (f64::MAX, p);
        for t in &triangles {
            let q = nearest_on_triangle(p, t);
            let d = q.distance(p);
            if d < best.0 {
                best = (d, q);
            }
        }
        best
    };
    // The solid's sharp edges (faces turning by more than 30°): corners
    // where the mesh's boundary turns go onto them.
    let mut sharp: Vec<(DVec3, DVec3)> = Vec::new();
    for (e, _) in solid.edges() {
        let [Some(a), Some(b)] = solid.edge_faces(e)? else {
            continue;
        };
        let (Some(fa), Some(fb)) = (solid.face(a), solid.face(b)) else {
            continue;
        };
        if fa.plane.normal.dot(fb.plane.normal) < libm::cos(30f64.to_radians()) {
            let seg = solid.edge_segment(e)?;
            sharp.push((seg.a, seg.b));
        }
    }
    let nearest_edge = |p: DVec3| {
        let mut best = (f64::MAX, p);
        for (a, b) in &sharp {
            let ab = *b - *a;
            let t = ((p - *a).dot(ab) / ab.length_squared().max(1e-30)).clamp(0.0, 1.0);
            let q = *a + ab * t;
            let d = q.distance(p);
            if d < best.0 {
                best = (d, q);
            }
        }
        best
    };
    let reach = mesh.cell.max_element() * 0.75;
    // The boundary's normals around each corner.
    let mut normals: BTreeMap<u32, Vec<DVec3>> = BTreeMap::new();
    for b in &mesh.boundary {
        let [pa, pb, pc] = [b[0], b[1], b[2]].map(|i| mesh.nodes[i as usize]);
        let n = (pb - pa).cross(pc - pa).normalize_or_zero();
        for v in [b[0], b[1], b[2]] {
            normals.entry(v).or_default().push(n);
        }
    }
    let turning = |v: u32| {
        normals
            .get(&v)
            .is_some_and(|ns| ns.iter().any(|a| ns.iter().any(|b| a.dot(*b) < 0.5)))
    };
    let mut corners: Vec<u32> = mesh
        .boundary
        .iter()
        .flat_map(|b| [b[0], b[1], b[2]])
        .collect();
    corners.sort_unstable();
    corners.dedup();
    let start: Vec<DVec3> = mesh.nodes.clone();
    let volume = |nodes: &[DVec3], t: &[u32; 10]| {
        let [a, b, c, d] = [t[0], t[1], t[2], t[3]].map(|i| nodes[i as usize]);
        (b - a).cross(c - a).dot(d - a) / 6.0
    };
    let before: Vec<f64> = mesh.tets.iter().map(|t| volume(&start, t)).collect();
    let mut target: BTreeMap<u32, DVec3> = BTreeMap::new();
    let mut report = FitReport::default();
    for &v in &corners {
        let p = start[v as usize];
        let on_edge = if turning(v) {
            Some(nearest_edge(p)).filter(|(d, _)| *d <= reach)
        } else {
            None
        };
        let (d, q) = on_edge.unwrap_or_else(|| nearest(p));
        if d > tol.length && d <= reach {
            target.insert(v, q);
        }
    }
    let mut share: BTreeMap<u32, f64> = target.keys().map(|v| (*v, 1.0)).collect();
    for _ in 0..7 {
        for (v, q) in &target {
            let p = start[*v as usize];
            mesh.nodes[*v as usize] = p + (*q - p) * share[v];
        }
        let mut bad: Vec<u32> = Vec::new();
        for (k, t) in mesh.tets.iter().enumerate() {
            if volume(&mesh.nodes, t) < 0.1 * before[k] {
                bad.extend(t[..4].iter().filter(|v| target.contains_key(v)).copied());
            }
        }
        if bad.is_empty() {
            break;
        }
        bad.sort_unstable();
        bad.dedup();
        for v in bad {
            if let Some(s) = share.get_mut(&v) {
                *s = if *s > 0.02 { *s * 0.5 } else { 0.0 };
            }
        }
    }
    // Whatever still folds goes back.
    loop {
        let mut reverted = false;
        for (k, t) in mesh.tets.iter().enumerate() {
            if volume(&mesh.nodes, t) < 0.1 * before[k] {
                for v in &t[..4] {
                    if share.get(v).is_some_and(|s| *s > 0.0) {
                        share.insert(*v, 0.0);
                        reverted = true;
                    }
                }
            }
        }
        if !reverted {
            break;
        }
        for (v, s) in &share {
            if *s == 0.0 {
                mesh.nodes[*v as usize] = start[*v as usize];
            }
        }
    }
    for (v, s) in &share {
        let d = mesh.nodes[*v as usize].distance(start[*v as usize]);
        if *s > 0.0 && d > 0.0 {
            report.moved += 1;
            report.largest_mm = report.largest_mm.max(d);
        } else {
            report.kept += 1;
        }
    }
    // Edge middles: straight, then on the faces along the boundary.
    const EDGES: [(usize, usize); 6] = [(0, 1), (1, 2), (0, 2), (0, 3), (1, 3), (2, 3)];
    for t in &mesh.tets {
        for (slot, (a, b)) in EDGES.into_iter().enumerate() {
            let (pa, pb) = (mesh.nodes[t[a] as usize], mesh.nodes[t[b] as usize]);
            mesh.nodes[t[4 + slot] as usize] = (pa + pb) * 0.5;
        }
    }
    for tri in mesh.boundary.clone() {
        for (a, b, m) in [
            (tri[0], tri[1], tri[3]),
            (tri[1], tri[2], tri[4]),
            (tri[2], tri[0], tri[5]),
        ] {
            let (pa, pb) = (mesh.nodes[a as usize], mesh.nodes[b as usize]);
            let straight = (pa + pb) * 0.5;
            let (d, q) = nearest(straight);
            if d > tol.length && d < 0.1 * pa.distance(pb) {
                mesh.nodes[m as usize] = q;
            }
        }
    }
    mesh.boundary_face = assign_faces(mesh, solid, tol);
    Ok(report)
}

// ----- element matrices -----------------------------------------------------------------

/// Gradients of the corner volume coordinates and the volume.
fn corner_gradients(p: [DVec3; 4]) -> ([DVec3; 4], f64) {
    let (a, b, c) = (p[1] - p[0], p[2] - p[0], p[3] - p[0]);
    let det = a.dot(b.cross(c));
    let g1 = b.cross(c) / det;
    let g2 = c.cross(a) / det;
    let g3 = a.cross(b) / det;
    ([-(g1 + g2 + g3), g1, g2, g3], det / 6.0)
}

/// Gradients of the ten shape functions at volume coordinates `l`.
fn shape_gradients(g: &[DVec3; 4], l: [f64; 4]) -> [DVec3; 10] {
    let mut out = [DVec3::ZERO; 10];
    for i in 0..4 {
        out[i] = g[i] * (4.0 * l[i] - 1.0);
    }
    for (slot, (p, q)) in [(0, 1), (1, 2), (0, 2), (0, 3), (1, 3), (2, 3)]
        .into_iter()
        .enumerate()
    {
        out[4 + slot] = (g[p] * l[q] + g[q] * l[p]) * 4.0;
    }
    out
}

/// The four-point rule, exact for quadratics: volume coordinates.
fn gauss_points() -> [[f64; 4]; 4] {
    let (a, b) = (0.585_410_196_624_968_5, 0.138_196_601_125_010_5);
    [[a, b, b, b], [b, a, b, b], [b, b, a, b], [b, b, b, a]]
}

fn corners(mesh: &TetMesh, t: &[u32; 10]) -> [DVec3; 4] {
    [0, 1, 2, 3].map(|i| mesh.nodes[t[i] as usize])
}

/// Strain (ε_xx, ε_yy, ε_zz, γ_xy, γ_yz, γ_zx) from nodal displacements.
fn strain(grads: &[DVec3; 10], u: impl Fn(usize) -> DVec3) -> [f64; 6] {
    let mut e = [0.0; 6];
    for (i, gr) in grads.iter().enumerate() {
        let d = u(i);
        e[0] += gr.x * d.x;
        e[1] += gr.y * d.y;
        e[2] += gr.z * d.z;
        e[3] += gr.y * d.x + gr.x * d.y;
        e[4] += gr.z * d.y + gr.y * d.z;
        e[5] += gr.x * d.z + gr.z * d.x;
    }
    e
}

/// Stress from strain for an isotropic material.
fn stress(m: &SimMaterial, e: [f64; 6]) -> [f64; 6] {
    let (ym, nu) = (m.youngs_mpa, m.poisson);
    let lambda = ym * nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
    let mu = ym / (2.0 * (1.0 + nu));
    let tr = e[0] + e[1] + e[2];
    [
        lambda * tr + 2.0 * mu * e[0],
        lambda * tr + 2.0 * mu * e[1],
        lambda * tr + 2.0 * mu * e[2],
        mu * e[3],
        mu * e[4],
        mu * e[5],
    ]
}

/// The von Mises equivalent stress.
pub fn von_mises(s: [f64; 6]) -> f64 {
    let a = (s[0] - s[1]).powi(2) + (s[1] - s[2]).powi(2) + (s[2] - s[0]).powi(2);
    let b = s[3] * s[3] + s[4] * s[4] + s[5] * s[5];
    (0.5 * a + 3.0 * b).sqrt()
}

// ----- sparse matrices ------------------------------------------------------------------

/// A symmetric sparse matrix in blocks of `b × b` per node pair.
struct Sparse {
    b: usize,
    start: Vec<usize>,
    cols: Vec<u32>,
    vals: Vec<f64>,
}

impl Sparse {
    fn new(mesh: &TetMesh, b: usize) -> Self {
        let mut rows: Vec<Vec<u32>> = vec![Vec::new(); mesh.nodes.len()];
        for t in &mesh.tets {
            for &i in t {
                rows[i as usize].extend_from_slice(t);
            }
        }
        let mut start = Vec::with_capacity(rows.len() + 1);
        let mut cols = Vec::new();
        start.push(0);
        for mut r in rows {
            r.sort_unstable();
            r.dedup();
            cols.extend(r);
            start.push(cols.len());
        }
        let n = cols.len();
        Sparse {
            b,
            start,
            cols,
            vals: vec![0.0; n * b * b],
        }
    }

    fn add(&mut self, i: u32, j: u32, r: usize, c: usize, v: f64) {
        let (s, e) = (self.start[i as usize], self.start[i as usize + 1]);
        if let Ok(k) = self.cols[s..e].binary_search(&j) {
            self.vals[(s + k) * self.b * self.b + r * self.b + c] += v;
        }
    }

    fn diagonal(&self) -> Vec<f64> {
        let b = self.b;
        let mut d = vec![0.0; (self.start.len() - 1) * b];
        for i in 0..self.start.len() - 1 {
            for k in self.start[i]..self.start[i + 1] {
                if self.cols[k] as usize == i {
                    for r in 0..b {
                        d[i * b + r] = self.vals[k * b * b + r * b + r];
                    }
                }
            }
        }
        d
    }

    fn mul(&self, x: &[f64], y: &mut [f64]) {
        let b = self.b;
        for i in 0..self.start.len() - 1 {
            for r in 0..b {
                let mut s = 0.0;
                for k in self.start[i]..self.start[i + 1] {
                    let j = self.cols[k] as usize;
                    let blk = &self.vals[k * b * b + r * b..k * b * b + r * b + b];
                    for c in 0..b {
                        s += blk[c] * x[j * b + c];
                    }
                }
                y[i * b + r] = s;
            }
        }
    }
}

/// Envelope entries the direct solver may hold (about 320 MB); larger
/// systems are solved by conjugate gradients.
const MAX_ENVELOPE: usize = 40_000_000;

/// A Cholesky factor `L` (`A = L Lᵀ`) of the free rows and columns of a
/// symmetric positive definite [`Sparse`] matrix, stored by rows in its
/// envelope after a reverse Cuthill–McKee ordering of the nodes keeps the
/// envelope narrow. Factored once, it solves each right-hand side in two
/// sweeps, which a modal analysis repeats hundreds of times.
struct Envelope {
    /// The unknown of each position in the ordering.
    order: Vec<usize>,
    /// First column stored for each row.
    first: Vec<usize>,
    /// Where each row starts in `vals`.
    start: Vec<usize>,
    /// Row `i` holds columns `first[i]..=i`.
    vals: Vec<f64>,
}

impl Envelope {
    /// Factors `a` restricted to `free` unknowns; `None` when the envelope
    /// would exceed [`MAX_ENVELOPE`] or a pivot is not positive (the free
    /// part is not held).
    fn factor(a: &Sparse, free: &[bool]) -> Option<Envelope> {
        let b = a.b;
        let nodes = a.start.len() - 1;
        // Reverse Cuthill–McKee on the node graph, from a lowest-degree node
        // of each component; ties by index, so the order is deterministic.
        let degree = |i: usize| a.start[i + 1] - a.start[i];
        let mut by_degree: Vec<usize> = (0..nodes).collect();
        by_degree.sort_by_key(|&i| (degree(i), i));
        let mut seen = vec![false; nodes];
        let mut node_order = Vec::with_capacity(nodes);
        for &root in &by_degree {
            if seen[root] {
                continue;
            }
            seen[root] = true;
            let mut head = node_order.len();
            node_order.push(root);
            while head < node_order.len() {
                let v = node_order[head];
                head += 1;
                let mut next: Vec<usize> = a.cols[a.start[v]..a.start[v + 1]]
                    .iter()
                    .map(|&c| c as usize)
                    .filter(|&c| !seen[c])
                    .collect();
                next.sort_by_key(|&c| (degree(c), c));
                for c in next {
                    seen[c] = true;
                    node_order.push(c);
                }
            }
        }
        node_order.reverse();
        let order: Vec<usize> = node_order
            .iter()
            .flat_map(|&v| (0..b).map(move |r| v * b + r))
            .filter(|&u| free[u])
            .collect();
        let n = order.len();
        let mut position = vec![usize::MAX; nodes * b];
        for (i, &u) in order.iter().enumerate() {
            position[u] = i;
        }
        let mut first: Vec<usize> = (0..n).collect();
        for (i, &u) in order.iter().enumerate() {
            let v = u / b;
            for &c in &a.cols[a.start[v]..a.start[v + 1]] {
                for r in 0..b {
                    let j = position[c as usize * b + r];
                    if j < first[i] {
                        first[i] = j;
                    }
                }
            }
        }
        let mut start = Vec::with_capacity(n + 1);
        start.push(0usize);
        for i in 0..n {
            let size = start[i].checked_add(i - first[i] + 1)?;
            if size > MAX_ENVELOPE {
                return None;
            }
            start.push(size);
        }
        let mut vals = vec![0.0; start[n]];
        for (i, &u) in order.iter().enumerate() {
            let (v, r) = (u / b, u % b);
            for k in a.start[v]..a.start[v + 1] {
                let c = a.cols[k] as usize;
                for cc in 0..b {
                    let j = position[c * b + cc];
                    if j <= i {
                        vals[start[i] + j - first[i]] += a.vals[k * b * b + r * b + cc];
                    }
                }
            }
        }
        // Row-by-row Cholesky within the envelope.
        let mut largest = 0.0f64;
        for i in 0..n {
            largest = largest.max(vals[start[i] + i - first[i]].abs());
        }
        for i in 0..n {
            for j in first[i]..=i {
                let lo = first[i].max(first[j]);
                let mut sum = vals[start[i] + j - first[i]];
                let (ri, rj) = (start[i] - first[i], start[j] - first[j]);
                for k in lo..j {
                    sum -= vals[ri + k] * vals[rj + k];
                }
                if j < i {
                    vals[ri + j] = sum / vals[rj + j];
                } else {
                    if sum.is_nan() || sum <= 1e-12 * largest {
                        return None;
                    }
                    vals[ri + i] = sum.sqrt();
                }
            }
        }
        Some(Envelope {
            order,
            first,
            start,
            vals,
        })
    }

    /// `x` on the free unknowns from `A x = rhs`; the others are zero.
    fn solve(&self, rhs: &[f64], x: &mut [f64]) {
        let n = self.order.len();
        let mut y: Vec<f64> = self.order.iter().map(|&u| rhs[u]).collect();
        for i in 0..n {
            let row = self.start[i] - self.first[i];
            let mut sum = y[i];
            for k in self.first[i]..i {
                sum -= self.vals[row + k] * y[k];
            }
            y[i] = sum / self.vals[row + i];
        }
        for i in (0..n).rev() {
            let row = self.start[i] - self.first[i];
            y[i] /= self.vals[row + i];
            let yi = y[i];
            for k in self.first[i]..i {
                y[k] -= self.vals[row + k] * yi;
            }
        }
        x.fill(0.0);
        for (i, &u) in self.order.iter().enumerate() {
            x[u] = y[i];
        }
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Solves `A x = rhs` on the free unknowns (fixed ones keep `x`'s value,
/// whose effect must already be in `rhs`) by conjugate gradients with a
/// diagonal preconditioner. Returns the iterations, or `None` when it did
/// not converge.
fn solve_cg(a: &Sparse, rhs: &[f64], free: &[bool], x: &mut [f64], rel_tol: f64) -> Option<usize> {
    let n = rhs.len();
    let diag = a.diagonal();
    let inv: Vec<f64> = diag
        .iter()
        .zip(free)
        .map(|(d, f)| if *f && *d > 0.0 { 1.0 / d } else { 0.0 })
        .collect();
    let mut ax = vec![0.0; n];
    let mut fixed_x = x.to_vec();
    for (v, f) in fixed_x.iter_mut().zip(free) {
        if *f {
            *v = 0.0;
        }
    }
    // Residual of the free rows with the fixed values moved to the right.
    let mut free_x: Vec<f64> = x
        .iter()
        .zip(free)
        .map(|(v, f)| if *f { *v } else { 0.0 })
        .collect();
    a.mul(&free_x, &mut ax);
    let mut r: Vec<f64> = (0..n)
        .map(|i| if free[i] { rhs[i] - ax[i] } else { 0.0 })
        .collect();
    let norm_b = dot(rhs, rhs).sqrt().max(1e-300);
    let mut z: Vec<f64> = r.iter().zip(&inv).map(|(a, b)| a * b).collect();
    let mut p = z.clone();
    let mut rz = dot(&r, &z);
    let max = 20 * n + 100;
    let mut it = 0;
    while dot(&r, &r).sqrt() > rel_tol * norm_b {
        if it >= max {
            return None;
        }
        it += 1;
        a.mul(&p, &mut ax);
        for (v, f) in ax.iter_mut().zip(free) {
            if !*f {
                *v = 0.0;
            }
        }
        let pap = dot(&p, &ax);
        if pap <= 0.0 {
            return None;
        }
        let alpha = rz / pap;
        for i in 0..n {
            free_x[i] += alpha * p[i];
            r[i] -= alpha * ax[i];
        }
        for i in 0..n {
            z[i] = r[i] * inv[i];
        }
        let rz_new = dot(&r, &z);
        let beta = rz_new / rz;
        rz = rz_new;
        for i in 0..n {
            p[i] = z[i] + beta * p[i];
        }
    }
    for i in 0..n {
        x[i] = if free[i] { free_x[i] } else { fixed_x[i] };
    }
    Some(it)
}

// ----- boundary conditions --------------------------------------------------------------

/// The nodes of the boundary triangles on `faces`.
fn nodes_on(mesh: &TetMesh, faces: &[FaceId]) -> Vec<u32> {
    let mut out: Vec<u32> = mesh
        .boundary
        .iter()
        .zip(&mesh.boundary_face)
        .filter(|(_, f)| f.is_some_and(|f| faces.contains(&f)))
        .flat_map(|(t, _)| t.iter().copied())
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Spreads `per_area(normal)` (a value per mm²) over the boundary
/// triangles on `faces` as consistent nodal values (a quadratic triangle
/// takes a third of its area at each edge middle, none at the corners);
/// returns the area covered.
fn spread(mesh: &TetMesh, faces: &[FaceId], mut add: impl FnMut(u32, f64, DVec3)) -> f64 {
    let mut total = 0.0;
    for (t, f) in mesh.boundary.iter().zip(&mesh.boundary_face) {
        if !f.is_some_and(|f| faces.contains(&f)) {
            continue;
        }
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| mesh.nodes[i as usize]);
        let cross = (b - a).cross(c - a);
        let area = 0.5 * cross.length();
        let normal = cross.normalize_or_zero();
        total += area;
        for &m in &t[3..] {
            add(m, area / 3.0, normal);
        }
    }
    total
}

// ----- linear statics -------------------------------------------------------------------

/// A load on faces.
#[derive(Debug, Clone, PartialEq)]
pub enum Load {
    /// A total force, N, spread over the faces by area.
    Force {
        /// The faces.
        faces: Vec<FaceId>,
        /// The force.
        force: DVec3,
    },
    /// A pressure, MPa, pushing on the faces.
    Pressure {
        /// The faces.
        faces: Vec<FaceId>,
        /// The pressure.
        mpa: f64,
    },
}

/// What a static analysis found.
#[derive(Debug, Clone)]
pub struct StaticResult {
    /// Displacement of every node, mm.
    pub displacement: Vec<DVec3>,
    /// Von Mises stress at every node, from the stress averaged over the
    /// tetrahedra meeting there, MPa.
    pub node_stress: Vec<f64>,
    /// The largest von Mises stress at any integration point, MPa.
    pub max_von_mises: f64,
    /// The largest displacement, mm.
    pub max_displacement: f64,
    /// The total reaction at the fixtures, N.
    pub reaction: DVec3,
    /// Solver iterations.
    pub iterations: usize,
}

fn stiffness(mesh: &TetMesh, m: &SimMaterial) -> Sparse {
    let mut k = Sparse::new(mesh, 3);
    let gp = gauss_points();
    for t in &mesh.tets {
        let (g, vol) = corner_gradients(corners(mesh, t));
        let mut ke = [[0.0f64; 30]; 30];
        for l in gp {
            let grads = shape_gradients(&g, l);
            // Columns of B: the strain of a unit displacement of one dof.
            let mut db = [[0.0f64; 6]; 30];
            let mut bcols = [[0.0f64; 6]; 30];
            for i in 0..10 {
                for d in 0..3 {
                    let mut unit = [DVec3::ZERO; 10];
                    unit[i][d] = 1.0;
                    let e = strain(&grads, |n| unit[n]);
                    bcols[i * 3 + d] = e;
                    db[i * 3 + d] = stress(m, e);
                }
            }
            let w = vol / 4.0;
            for a in 0..30 {
                for b in a..30 {
                    let mut s = 0.0;
                    for c in 0..6 {
                        s += bcols[a][c] * db[b][c];
                    }
                    ke[a][b] += w * s;
                }
            }
        }
        for a in 0..30 {
            for b in 0..a {
                ke[a][b] = ke[b][a];
            }
        }
        for (ia, &na) in t.iter().enumerate() {
            for (ib, &nb) in t.iter().enumerate() {
                for r in 0..3 {
                    for c in 0..3 {
                        k.add(na, nb, r, c, ke[ia * 3 + r][ib * 3 + c]);
                    }
                }
            }
        }
    }
    k
}

/// The free unknowns with every node on `fixed` held.
fn free_dofs(
    mesh: &TetMesh,
    fixed: &[FaceId],
    per_node: usize,
) -> Result<Vec<bool>, GeometryError> {
    let held = nodes_on(mesh, fixed);
    if held.is_empty() {
        return Err(GeometryError::NotApplicable(
            "no part of the mesh lies on the fixed faces".into(),
        ));
    }
    let mut free = vec![true; mesh.nodes.len() * per_node];
    for n in held {
        for d in 0..per_node {
            free[n as usize * per_node + d] = false;
        }
    }
    Ok(free)
}

/// Linear static stress under `loads` (and the part's own weight along
/// −Z when `gravity`), with the nodes on `fixed` held.
pub fn solve_static(
    mesh: &TetMesh,
    m: &SimMaterial,
    fixed: &[FaceId],
    loads: &[Load],
    gravity: bool,
) -> Result<StaticResult, GeometryError> {
    let k = stiffness(mesh, m);
    let n = mesh.nodes.len() * 3;
    let mut f = vec![0.0; n];
    for load in loads {
        match load {
            Load::Force { faces, force } => {
                let area = spread(mesh, faces, |_, _, _| {});
                if area <= 0.0 {
                    return Err(GeometryError::NotApplicable(
                        "no part of the mesh lies on a loaded face".into(),
                    ));
                }
                spread(mesh, faces, |node, share, _| {
                    let v = *force * (share / area);
                    for d in 0..3 {
                        f[node as usize * 3 + d] += v[d];
                    }
                });
            }
            Load::Pressure { faces, mpa } => {
                let area = spread(mesh, faces, |node, share, normal| {
                    let v = -normal * (mpa * share);
                    for d in 0..3 {
                        f[node as usize * 3 + d] += v[d];
                    }
                });
                if area <= 0.0 {
                    return Err(GeometryError::NotApplicable(
                        "no part of the mesh lies on a loaded face".into(),
                    ));
                }
            }
        }
    }
    if gravity {
        let rho = m.density();
        for t in &mesh.tets {
            let (_, vol) = corner_gradients(corners(mesh, t));
            let w = rho * vol * GRAVITY_MM_S2;
            // Consistent loads of a uniform body force on a quadratic
            // tetrahedron: −1/20 at the corners, 1/5 at the middles.
            for (i, node) in t.iter().enumerate() {
                let share = if i < 4 { -0.05 } else { 0.2 };
                f[*node as usize * 3 + 2] -= w * share;
            }
        }
    }
    let free = free_dofs(mesh, fixed, 3)?;
    let mut u = vec![0.0; n];
    let iterations = solve_cg(&k, &f, &free, &mut u, 1.0e-9).ok_or_else(|| {
        GeometryError::NotApplicable(
            "the solver did not converge (is the part held against every motion?)".into(),
        )
    })?;
    let displacement: Vec<DVec3> = (0..mesh.nodes.len())
        .map(|i| DVec3::new(u[i * 3], u[i * 3 + 1], u[i * 3 + 2]))
        .collect();
    // Reactions: K u − f on the held rows.
    let mut ku = vec![0.0; n];
    k.mul(&u, &mut ku);
    let mut reaction = DVec3::ZERO;
    for i in 0..mesh.nodes.len() {
        for d in 0..3 {
            if !free[i * 3 + d] {
                reaction[d] += ku[i * 3 + d] - f[i * 3 + d];
            }
        }
    }
    let dsp = &displacement;
    let mut max_von_mises: f64 = 0.0;
    for t in &mesh.tets {
        let (g, _) = corner_gradients(corners(mesh, t));
        for l in gauss_points() {
            let s = stress(m, strain(&shape_gradients(&g, l), |i| dsp[t[i] as usize]));
            max_von_mises = max_von_mises.max(von_mises(s));
        }
    }
    // Stress averaged at every node over the tetrahedra meeting there.
    let mut sum = vec![[0.0f64; 6]; mesh.nodes.len()];
    let mut count = vec![0u32; mesh.nodes.len()];
    let at_nodes: [[f64; 4]; 10] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
        [0.5, 0.5, 0.0, 0.0],
        [0.0, 0.5, 0.5, 0.0],
        [0.5, 0.0, 0.5, 0.0],
        [0.5, 0.0, 0.0, 0.5],
        [0.0, 0.5, 0.0, 0.5],
        [0.0, 0.0, 0.5, 0.5],
    ];
    for t in &mesh.tets {
        let (g, _) = corner_gradients(corners(mesh, t));
        for (c, l) in at_nodes.iter().enumerate() {
            let st = stress(m, strain(&shape_gradients(&g, *l), |i| dsp[t[i] as usize]));
            let node = t[c] as usize;
            for k in 0..6 {
                sum[node][k] += st[k];
            }
            count[node] += 1;
        }
    }
    let node_stress = sum
        .iter()
        .zip(&count)
        .map(|(s, c)| von_mises(s.map(|v| v / f64::from((*c).max(1)))))
        .collect();
    let max_displacement = displacement.iter().map(|d| d.length()).fold(0.0, f64::max);
    Ok(StaticResult {
        displacement,
        node_stress,
        max_von_mises,
        max_displacement,
        reaction,
        iterations,
    })
}

// ----- natural frequencies --------------------------------------------------------------

/// Eigenvalues (ascending) and eigenvectors (columns) of a small symmetric
/// matrix by cyclic Jacobi rotations.
fn jacobi_eigen(mut a: Vec<Vec<f64>>) -> (Vec<f64>, Vec<Vec<f64>>) {
    let n = a.len();
    let mut v = vec![vec![0.0; n]; n];
    for (i, row) in v.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for _sweep in 0..100 {
        let off: f64 = (0..n)
            .flat_map(|i| (0..n).filter(move |j| *j != i).map(move |j| (i, j)))
            .map(|(i, j)| a[i][j] * a[i][j])
            .sum();
        let scale: f64 = (0..n).map(|i| a[i][i] * a[i][i]).sum::<f64>().max(1e-300);
        if off <= 1e-30 * scale {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                if a[p][q].abs() <= 1e-300 {
                    continue;
                }
                let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let (akp, akq) = (a[k][p], a[k][q]);
                    a[k][p] = c * akp - s * akq;
                    a[k][q] = s * akp + c * akq;
                }
                for k in 0..n {
                    let (apk, aqk) = (a[p][k], a[q][k]);
                    a[p][k] = c * apk - s * aqk;
                    a[q][k] = s * apk + c * aqk;
                }
                for row in v.iter_mut() {
                    let (vp, vq) = (row[p], row[q]);
                    row[p] = c * vp - s * vq;
                    row[q] = s * vp + c * vq;
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|x, y| a[*x][*x].total_cmp(&a[*y][*y]));
    let values = order.iter().map(|i| a[*i][*i]).collect();
    let vectors = (0..n)
        .map(|r| order.iter().map(|c| v[r][*c]).collect())
        .collect();
    (values, vectors)
}

/// What a frequency analysis found.
#[derive(Debug, Clone)]
pub struct ModalResult {
    /// Natural frequencies, Hz, ascending.
    pub frequencies_hz: Vec<f64>,
    /// Mode shapes (displacement per node, largest 1).
    pub shapes: Vec<Vec<DVec3>>,
}

/// The `count` lowest natural frequencies with the nodes on `fixed` held,
/// by subspace iteration (lumped masses).
pub fn solve_modes(
    mesh: &TetMesh,
    m: &SimMaterial,
    fixed: &[FaceId],
    count: usize,
) -> Result<ModalResult, GeometryError> {
    if m.density_kg_m3 <= 0.0 {
        return Err(GeometryError::NotApplicable(
            "the material needs a density".into(),
        ));
    }
    let k = stiffness(mesh, m);
    let nn = mesh.nodes.len();
    let n = nn * 3;
    let mut mass = vec![0.0; n];
    for t in &mesh.tets {
        let (_, vol) = corner_gradients(corners(mesh, t));
        let w = m.density() * vol;
        for (i, node) in t.iter().enumerate() {
            let share = if i < 4 { 1.0 / 36.0 } else { 4.0 / 27.0 };
            for d in 0..3 {
                mass[*node as usize * 3 + d] += w * share;
            }
        }
    }
    let free = free_dofs(mesh, fixed, 3)?;
    let free_count = free.iter().filter(|f| **f).count();
    let p = (2 * count).max(count + 8).min(free_count);
    if count == 0 || p < count {
        return Err(GeometryError::NotApplicable("too few free nodes".into()));
    }
    // Deterministic, well-spread start vectors (a fixed-seed hash).
    let noise = |i: usize, j: usize| {
        let mut h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (j as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        h ^= h >> 31;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 29;
        (h >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    let fresh = |j: usize| -> Vec<f64> {
        (0..n)
            .map(|i| if free[i] { noise(i, j) } else { 0.0 })
            .collect()
    };
    let mdot = |a: &[f64], b: &[f64]| -> f64 {
        a.iter()
            .zip(b)
            .zip(&mass)
            .map(|((u, v), w)| u * v * w)
            .sum()
    };
    // Makes the vectors M-orthonormal (twice, for accuracy), replacing any
    // that falls into the span of the others.
    let orthonormalize = |v: &mut Vec<Vec<f64>>, salt: usize| {
        for j in 0..v.len() {
            for attempt in 0..3 {
                let before = mdot(&v[j], &v[j]).sqrt();
                for _pass in 0..2 {
                    for i in 0..j {
                        let c = mdot(&v[j], &v[i]);
                        let (head, tail) = v.split_at_mut(j);
                        for (x, y) in tail[0].iter_mut().zip(&head[i]) {
                            *x -= c * y;
                        }
                    }
                }
                let norm = mdot(&v[j], &v[j]).sqrt();
                if norm > 1e-8 * before.max(1e-300) && norm > 0.0 {
                    for x in v[j].iter_mut() {
                        *x /= norm;
                    }
                    break;
                }
                v[j] = fresh(salt + 1000 * (attempt + 1) + j);
            }
        }
    };
    let mut x: Vec<Vec<f64>> = (0..p).map(fresh).collect();
    orthonormalize(&mut x, 0);
    // Factored once when it fits; every iteration then solves directly.
    let direct = Envelope::factor(&k, &free);
    let mut values = vec![0.0; p];
    let mut previous = vec![f64::MAX; count];
    for iteration in 0..80 {
        // Y = K⁻¹ M X, started from X / λ.
        let mut y = Vec::with_capacity(p);
        for (j, xj) in x.iter().enumerate() {
            let rhs: Vec<f64> = xj.iter().zip(&mass).map(|(a, b)| a * b).collect();
            let mut yj: Vec<f64> = if values[j] > 0.0 {
                xj.iter().map(|v| v / values[j]).collect()
            } else {
                vec![0.0; n]
            };
            match &direct {
                Some(l) => l.solve(&rhs, &mut yj),
                None => {
                    solve_cg(&k, &rhs, &free, &mut yj, 1.0e-10).ok_or_else(|| {
                        GeometryError::NotApplicable(
                            "the solver did not converge (is the part held?)".into(),
                        )
                    })?;
                }
            }
            y.push(yj);
        }
        orthonormalize(&mut y, 7919 * (iteration + 1));
        // Rayleigh–Ritz in the span of Y (M-orthonormal, so a standard
        // symmetric problem).
        let mut ky = Vec::with_capacity(p);
        for yj in &y {
            let mut out = vec![0.0; n];
            k.mul(yj, &mut out);
            for (o, f) in out.iter_mut().zip(&free) {
                if !*f {
                    *o = 0.0;
                }
            }
            ky.push(out);
        }
        let kr: Vec<Vec<f64>> = (0..p)
            .map(|a| {
                (0..p)
                    .map(|b| 0.5 * (dot(&y[a], &ky[b]) + dot(&y[b], &ky[a])))
                    .collect()
            })
            .collect();
        let (vals, q) = jacobi_eigen(kr);
        x = (0..p)
            .map(|col| {
                let mut v = vec![0.0; n];
                for (r, yr) in y.iter().enumerate() {
                    let f = q[r][col];
                    for (vi, yi) in v.iter_mut().zip(yr) {
                        *vi += f * yi;
                    }
                }
                v
            })
            .collect();
        values = vals;
        let done = (0..count).all(|i| (values[i] - previous[i]).abs() <= 1e-7 * values[i].abs());
        previous = values[..count].to_vec();
        if done {
            break;
        }
    }
    let frequencies_hz = values[..count]
        .iter()
        .map(|l| l.max(0.0).sqrt() / (2.0 * std::f64::consts::PI))
        .collect();
    let shapes = x[..count]
        .iter()
        .map(|v| {
            let s: Vec<DVec3> = (0..nn)
                .map(|i| DVec3::new(v[i * 3], v[i * 3 + 1], v[i * 3 + 2]))
                .collect();
            let top = s.iter().map(|d| d.length()).fold(0.0, f64::max).max(1e-300);
            s.into_iter().map(|d| d / top).collect()
        })
        .collect();
    Ok(ModalResult {
        frequencies_hz,
        shapes,
    })
}

// ----- steady heat conduction -----------------------------------------------------------

/// A thermal boundary condition on faces.
#[derive(Debug, Clone, PartialEq)]
pub enum HeatCondition {
    /// The faces are held at a temperature, °C.
    Temperature {
        /// The faces.
        faces: Vec<FaceId>,
        /// The temperature.
        celsius: f64,
    },
    /// Heat flows in through the faces, W in all.
    Power {
        /// The faces.
        faces: Vec<FaceId>,
        /// The power.
        watts: f64,
    },
    /// Exchange heat with air at a prescribed ambient temperature.
    Convection {
        /// Faces exposed to this environment.
        faces: Vec<FaceId>,
        /// Film coefficient, W/(m²·K), finite and positive.
        coefficient: f64,
        /// Ambient temperature, °C.
        ambient_c: f64,
    },
}

/// What a thermal analysis found.
#[derive(Debug, Clone)]
pub struct ThermalResult {
    /// Temperature of every node, °C.
    pub temperature: Vec<f64>,
    /// The lowest and highest temperatures.
    pub range: (f64, f64),
    /// Total prescribed power entering the body, W.
    pub power_in_w: f64,
    /// Integrated heat leaving through convection, W.
    pub convection_out_w: f64,
    /// Net heat supplied by held-temperature boundaries, W (negative cools).
    pub held_heat_in_w: f64,
    /// Applied power plus held heat minus convection loss, W.
    pub balance_error_w: f64,
}

/// Whether every tetrahedral node component has a thermal reference.
fn thermally_anchored(mesh: &TetMesh, anchored: &[bool]) -> bool {
    fn root(parents: &mut [usize], mut i: usize) -> usize {
        while parents[i] != i {
            parents[i] = parents[parents[i]];
            i = parents[i];
        }
        i
    }
    let mut parents: Vec<usize> = (0..mesh.nodes.len()).collect();
    for t in &mesh.tets {
        let a = root(&mut parents, t[0] as usize);
        for b in &t[1..] {
            let b = root(&mut parents, *b as usize);
            parents[b] = a;
        }
    }
    let mut held = vec![false; parents.len()];
    for (i, a) in anchored.iter().enumerate() {
        if *a {
            held[root(&mut parents, i)] = true;
        }
    }
    (0..parents.len()).all(|i| held[root(&mut parents, i)])
}

/// Steady temperatures under `conditions`; each connected component needs
/// a held temperature or convection. Uses consistent quadratic film terms
/// and reports the heat balance (docs/03 §12, milestone 1.20).
pub fn solve_thermal(
    mesh: &TetMesh,
    m: &SimMaterial,
    conditions: &[HeatCondition],
) -> Result<ThermalResult, GeometryError> {
    if mesh.nodes.is_empty() || mesh.tets.is_empty() {
        return Err(GeometryError::NotApplicable(
            "the thermal mesh is empty".into(),
        ));
    }
    if !m.conductivity_w_mk.is_finite() || m.conductivity_w_mk <= 0.0 {
        return Err(GeometryError::NotApplicable(
            "conductivity must be positive and finite".into(),
        ));
    }
    let k_mm = m.conductivity_w_mk * 1.0e-3;
    let mut k = Sparse::new(mesh, 1);
    for t in &mesh.tets {
        let (g, vol) = corner_gradients(corners(mesh, t));
        let mut ke = [[0.0; 10]; 10];
        for l in gauss_points() {
            let grads = shape_gradients(&g, l);
            for a in 0..10 {
                for b in 0..10 {
                    ke[a][b] += vol / 4.0 * k_mm * grads[a].dot(grads[b]);
                }
            }
        }
        for (ia, &na) in t.iter().enumerate() {
            for (ib, &nb) in t.iter().enumerate() {
                k.add(na, nb, 0, 0, ke[ia][ib]);
            }
        }
    }
    let n = mesh.nodes.len();
    let mut q = vec![0.0; n];
    let mut x = vec![0.0; n];
    let mut free = vec![true; n];
    let mut anchored = vec![false; n];
    let mut films = Vec::new();
    let mut power_in_w = 0.0;
    for c in conditions {
        match c {
            HeatCondition::Temperature { faces, celsius } => {
                if !celsius.is_finite() {
                    return Err(GeometryError::NotApplicable(
                        "temperature must be finite".into(),
                    ));
                }
                let held = nodes_on(mesh, faces);
                if held.is_empty() {
                    return Err(GeometryError::NotApplicable(
                        "no part of the mesh lies on a face held at a temperature".into(),
                    ));
                }
                for i in held {
                    if !free[i as usize]
                        && (x[i as usize] - celsius).abs() > 1e-9 * celsius.abs().max(1.0)
                    {
                        return Err(GeometryError::NotApplicable(
                            "conflicting temperatures at shared boundary nodes".into(),
                        ));
                    }
                    free[i as usize] = false;
                    anchored[i as usize] = true;
                    x[i as usize] = *celsius;
                }
            }
            HeatCondition::Power { faces, watts } => {
                if !watts.is_finite() {
                    return Err(GeometryError::NotApplicable("power must be finite".into()));
                }
                let area = spread(mesh, faces, |_, _, _| {});
                if area <= 0.0 {
                    return Err(GeometryError::NotApplicable(
                        "no part of the mesh lies on a heated face".into(),
                    ));
                }
                spread(mesh, faces, |node, share, _| {
                    q[node as usize] += watts * share / area;
                });
                power_in_w += watts;
            }
            HeatCondition::Convection {
                faces,
                coefficient,
                ambient_c,
            } => {
                if !coefficient.is_finite() || *coefficient <= 0.0 || !ambient_c.is_finite() {
                    return Err(GeometryError::NotApplicable("convection needs a positive finite coefficient and finite ambient temperature".into()));
                }
                // Exact integrals of the quadratic triangle's shape-function
                // products, in corner/edge-middle order (docs/03 §12).
                const FILM: [[f64; 6]; 6] = [
                    [6.0, -1.0, -1.0, 0.0, -4.0, 0.0],
                    [-1.0, 6.0, -1.0, 0.0, 0.0, -4.0],
                    [-1.0, -1.0, 6.0, -4.0, 0.0, 0.0],
                    [0.0, 0.0, -4.0, 32.0, 16.0, 16.0],
                    [-4.0, 0.0, 0.0, 16.0, 32.0, 16.0],
                    [0.0, -4.0, 0.0, 16.0, 16.0, 32.0],
                ];
                let h = coefficient * 1.0e-6;
                let mut area = 0.0;
                for (t, f) in mesh.boundary.iter().zip(&mesh.boundary_face) {
                    if !f.is_some_and(|f| faces.contains(&f)) {
                        continue;
                    }
                    let [a, b, c] = [t[0], t[1], t[2]].map(|i| mesh.nodes[i as usize]);
                    let triangle_area = (b - a).cross(c - a).length() * 0.5;
                    area += triangle_area;
                    for (ia, &na) in t.iter().enumerate() {
                        anchored[na as usize] = true;
                        for (ib, &nb) in t.iter().enumerate() {
                            k.add(na, nb, 0, 0, h * triangle_area / 180.0 * FILM[ia][ib]);
                        }
                    }
                    for &node in &t[3..] {
                        q[node as usize] += h * ambient_c * triangle_area / 3.0;
                    }
                    films.push((*t, h * triangle_area / 3.0, *ambient_c));
                }
                if area <= 0.0 {
                    return Err(GeometryError::NotApplicable(
                        "no part of the mesh lies on a convection face".into(),
                    ));
                }
            }
        }
    }
    if !thermally_anchored(mesh, &anchored) {
        return Err(GeometryError::NotApplicable(
            "each connected mesh component needs a held temperature or convection boundary".into(),
        ));
    }
    // Move the held temperatures to the right-hand side.
    let held: Vec<f64> = x
        .iter()
        .zip(&free)
        .map(|(v, f)| if *f { 0.0 } else { *v })
        .collect();
    let mut kx = vec![0.0; n];
    k.mul(&held, &mut kx);
    let rhs: Vec<f64> = (0..n).map(|i| q[i] - kx[i]).collect();
    solve_cg(&k, &rhs, &free, &mut x, 1.0e-11).ok_or_else(|| {
        GeometryError::NotApplicable("the thermal solver did not converge".into())
    })?;
    if x.iter().any(|t| !t.is_finite()) {
        return Err(GeometryError::NotApplicable(
            "the thermal solution is not finite".into(),
        ));
    }
    k.mul(&x, &mut kx);
    let held_heat_in_w: f64 = (0..n).filter(|i| !free[*i]).map(|i| kx[i] - q[i]).sum();
    let convection_out_w: f64 = films
        .iter()
        .map(|(t, share, ambient)| {
            share * t[3..].iter().map(|i| x[*i as usize] - ambient).sum::<f64>()
        })
        .sum();
    let lo = x.iter().copied().fold(f64::MAX, f64::min);
    let hi = x.iter().copied().fold(f64::MIN, f64::max);
    Ok(ThermalResult {
        temperature: x,
        range: (lo, hi),
        power_in_w,
        convection_out_w,
        held_heat_in_w,
        balance_error_w: power_in_w + held_heat_in_w - convection_out_w,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::geom::FaceSides;
    use crate::geom::loft::loft;

    /// A box from `lo` to `hi`.
    fn block(lo: DVec3, hi: DVec3) -> HalfEdgeMesh {
        let tol = Tolerances::default();
        let square = |z: f64| {
            vec![
                DVec3::new(lo.x, lo.y, z),
                DVec3::new(hi.x, lo.y, z),
                DVec3::new(hi.x, hi.y, z),
                DVec3::new(lo.x, hi.y, z),
            ]
        };
        loft(&[square(lo.z), square(hi.z)], FaceSides::default(), &tol).unwrap()
    }

    fn face_at(mesh: &HalfEdgeMesh, normal: DVec3, at: f64) -> FaceId {
        mesh.faces()
            .find(|(_, f)| {
                f.plane.normal.dot(normal) > 0.999
                    && (f.plane.signed_distance(normal * at)).abs() < 1e-6
            })
            .map(|(id, _)| id)
            .unwrap()
    }

    /// The direct envelope solver and conjugate gradients agree; a free
    /// (unheld) part has no factor.
    #[test]
    fn the_envelope_factor_solves_as_conjugate_gradients_do() {
        let tol = Tolerances::default();
        let beam = block(DVec3::ZERO, DVec3::new(200.0, 20.0, 20.0));
        let mesh = tet_mesh(&beam, 10.0, 100_000, &tol).unwrap();
        let root = face_at(&beam, -DVec3::X, 0.0);
        let k = stiffness(&mesh, &material("steel").unwrap());
        let free = free_dofs(&mesh, &[root], 3).unwrap();
        let rhs: Vec<f64> = (0..free.len())
            .map(|i| {
                if free[i] {
                    ((i * 37) % 11) as f64 - 5.0
                } else {
                    0.0
                }
            })
            .collect();
        let mut by_cg = vec![0.0; rhs.len()];
        solve_cg(&k, &rhs, &free, &mut by_cg, 1e-12).unwrap();
        let l = Envelope::factor(&k, &free).unwrap();
        let mut direct = vec![1.0; rhs.len()];
        l.solve(&rhs, &mut direct);
        let scale = by_cg.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        for (a, b) in by_cg.iter().zip(&direct) {
            assert!((a - b).abs() <= 1e-8 * scale, "{a} vs {b}");
        }
        assert!(Envelope::factor(&k, &vec![true; free.len()]).is_none());
    }

    #[test]
    fn a_cantilever_bends_as_beam_theory_says() {
        let tol = Tolerances::default();
        let beam = block(DVec3::ZERO, DVec3::new(200.0, 20.0, 20.0));
        let mesh = tet_mesh(&beam, 10.0, 100_000, &tol).unwrap();
        assert_eq!(mesh.tets.len(), 20 * 2 * 2 * 6);
        let root = face_at(&beam, -DVec3::X, 0.0);
        let tip = face_at(&beam, DVec3::X, 200.0);
        let steel = material("steel").unwrap();
        let r = solve_static(
            &mesh,
            &steel,
            &[root],
            &[Load::Force {
                faces: vec![tip],
                force: DVec3::new(0.0, 0.0, -1000.0),
            }],
            false,
        )
        .unwrap();
        // δ = F L³ / 3 E I = 0.952 mm (plus a little shear).
        let tip_drop = -r
            .displacement
            .iter()
            .zip(&mesh.nodes)
            .filter(|(_, p)| (p.x - 200.0).abs() < 1e-9)
            .map(|(d, _)| d.z)
            .sum::<f64>()
            / mesh
                .nodes
                .iter()
                .filter(|p| (p.x - 200.0).abs() < 1e-9)
                .count() as f64;
        assert!((tip_drop - 0.952).abs() < 0.05, "{tip_drop}");
        // The fixture holds the load.
        assert!((r.reaction.z - 1000.0).abs() < 1e-3, "{}", r.reaction);
        // Bending stress near the root is about M c / I = 150 MPa.
        assert!(
            r.max_von_mises > 120.0 && r.max_von_mises < 300.0,
            "{}",
            r.max_von_mises
        );
    }

    #[test]
    fn the_cantilevers_first_frequency() {
        let tol = Tolerances::default();
        let beam = block(DVec3::ZERO, DVec3::new(200.0, 20.0, 20.0));
        let mesh = tet_mesh(&beam, 20.0, 100_000, &tol).unwrap();
        let root = face_at(&beam, -DVec3::X, 0.0);
        let r = solve_modes(&mesh, &material("steel").unwrap(), &[root], 2).unwrap();
        // 1.875² / 2π · √(E I / ρ A L⁴) = 418 Hz, twice (square section).
        for f in &r.frequencies_hz {
            assert!((f - 418.0).abs() < 0.02 * 418.0, "{f}");
        }
    }

    #[test]
    fn heat_flows_along_a_bar() {
        let tol = Tolerances::default();
        let bar = block(DVec3::ZERO, DVec3::new(100.0, 10.0, 10.0));
        let mesh = tet_mesh(&bar, 5.0, 100_000, &tol).unwrap();
        let cold = face_at(&bar, -DVec3::X, 0.0);
        let hot = face_at(&bar, DVec3::X, 100.0);
        let steel = material("steel").unwrap();
        let r = solve_thermal(
            &mesh,
            &steel,
            &[
                HeatCondition::Temperature {
                    faces: vec![cold],
                    celsius: 0.0,
                },
                HeatCondition::Power {
                    faces: vec![hot],
                    watts: 10.0,
                },
            ],
        )
        .unwrap();
        // T = P L / k A = 10 · 100 / (0.05 · 100) = 200 °C.
        assert!((r.range.1 - 200.0).abs() < 1e-6, "{:?}", r.range);
        assert!(solve_thermal(&mesh, &steel, &[]).is_err());
    }

    #[test]
    fn convection_matches_series_resistances_and_conserves_heat() {
        let tol = Tolerances::default();
        let bar = block(DVec3::ZERO, DVec3::new(100.0, 10.0, 10.0));
        let mesh = tet_mesh(&bar, 5.0, 100_000, &tol).unwrap();
        let root = face_at(&bar, -DVec3::X, 0.0);
        let tip = face_at(&bar, DVec3::X, 100.0);
        let steel = material("steel").unwrap();
        let film = HeatCondition::Convection {
            faces: vec![tip],
            coefficient: 100.0,
            ambient_c: 20.0,
        };
        let r = solve_thermal(
            &mesh,
            &steel,
            &[
                HeatCondition::Temperature {
                    faces: vec![root],
                    celsius: 120.0,
                },
                film.clone(),
            ],
        )
        .unwrap();
        // Rcond = L/(kA) = 20 K/W; Rfilm = 1/(hA) = 100 K/W.
        let heat = 100.0 / 120.0;
        assert!(
            (r.range.0 - (20.0 + heat * 100.0)).abs() < 1e-5,
            "{:?}",
            r.range
        );
        assert!((r.range.1 - 120.0).abs() < 1e-6);
        assert!((r.convection_out_w - heat).abs() < 1e-7);
        assert!((r.held_heat_in_w - heat).abs() < 1e-7);
        assert!(r.balance_error_w.abs() < 1e-7);
        let r = solve_thermal(
            &mesh,
            &steel,
            &[
                HeatCondition::Power {
                    faces: vec![root],
                    watts: 10.0,
                },
                film.clone(),
            ],
        )
        .unwrap();
        assert!((r.range.0 - 1020.0).abs() < 1e-4, "{:?}", r.range);
        assert!((r.range.1 - 1220.0).abs() < 1e-4);
        assert!((r.convection_out_w - 10.0).abs() < 1e-6);
        assert!(r.held_heat_in_w.abs() < 1e-12);
        assert!(r.balance_error_w.abs() < 1e-6);
        let r = solve_thermal(&mesh, &steel, &[film]).unwrap();
        assert!(r.temperature.iter().all(|t| (t - 20.0).abs() < 1e-5));
    }

    #[test]
    fn thermal_references_and_conditions_are_checked() {
        let tol = Tolerances::default();
        let bar = block(DVec3::ZERO, DVec3::new(20.0, 10.0, 10.0));
        let mesh = tet_mesh(&bar, 10.0, 100_000, &tol).unwrap();
        let face = face_at(&bar, -DVec3::X, 0.0);
        let steel = material("steel").unwrap();
        assert!(
            solve_thermal(
                &mesh,
                &steel,
                &[
                    HeatCondition::Temperature {
                        faces: vec![face],
                        celsius: 0.0
                    },
                    HeatCondition::Temperature {
                        faces: vec![face],
                        celsius: 20.0
                    },
                ]
            )
            .is_err()
        );
        assert!(
            solve_thermal(
                &mesh,
                &steel,
                &[HeatCondition::Convection {
                    faces: vec![face],
                    coefficient: -1.0,
                    ambient_c: 20.0
                },]
            )
            .is_err()
        );
        // Two disconnected tetrahedral node components need independent anchors.
        let count = mesh.nodes.len() as u32;
        let mut disconnected = mesh.clone();
        disconnected
            .nodes
            .extend(mesh.nodes.iter().map(|p| *p + DVec3::X * 100.0));
        disconnected
            .tets
            .extend(mesh.tets.iter().map(|t| t.map(|i| i + count)));
        let mut anchored = vec![false; disconnected.nodes.len()];
        anchored[0] = true;
        assert!(!thermally_anchored(&disconnected, &anchored));
        anchored[count as usize] = true;
        assert!(thermally_anchored(&disconnected, &anchored));
    }

    #[test]
    fn eigenvalues_of_a_small_matrix() {
        let (v, _) = jacobi_eigen(vec![vec![2.0, 1.0], vec![1.0, 2.0]]);
        assert!((v[0] - 1.0).abs() < 1e-12 && (v[1] - 3.0).abs() < 1e-12);
    }

    /// A bar of 48-gon section, radius 10, 100 long along Z.
    fn round_bar() -> HalfEdgeMesh {
        use crate::geom::fabrication::{ProfileFrame, prism};
        let ring: Vec<glam::DVec2> = (0..48)
            .map(|k| {
                let a = std::f64::consts::TAU * f64::from(k) / 48.0;
                glam::DVec2::new(10.0 * libm::cos(a), 10.0 * libm::sin(a))
            })
            .collect();
        prism(
            ProfileFrame {
                origin: DVec3::ZERO,
                u: DVec3::X,
                v: DVec3::Y,
            },
            &[ring],
            DVec3::Z,
            (DVec3::ZERO, DVec3::Z),
            (DVec3::new(0.0, 0.0, 100.0), DVec3::Z),
            &Tolerances::default(),
        )
        .unwrap()
    }

    fn mesh_volume(mesh: &TetMesh) -> f64 {
        mesh.tets
            .iter()
            .map(|t| {
                let [a, b, c, d] = [t[0], t[1], t[2], t[3]].map(|i| mesh.nodes[i as usize]);
                (b - a).cross(c - a).dot(d - a) / 6.0
            })
            .sum()
    }

    #[test]
    fn fitted_meshes_follow_curved_faces() {
        let tol = Tolerances::default();
        let bar = round_bar();
        let exact = bar.volume().unwrap();
        let stepped = tet_mesh(&bar, 4.0, 100_000, &tol).unwrap();
        let mut fitted = stepped.clone();
        let report = fit_boundary(&mut fitted, &bar, &tol).unwrap();
        assert!(report.moved > 100, "{report:?}");
        assert!(report.largest_mm <= 0.75 * stepped.cell.max_element() + 1e-9);
        let (before, after) = (mesh_volume(&stepped), mesh_volume(&fitted));
        // Corners alone make a polygon of about twenty sides per section at
        // this size (1.6% short); the curved edge middles make up the rest.
        assert!((after - exact).abs() < 0.025 * exact, "{after} vs {exact}");
        assert!(
            (after - exact).abs() < (before - exact).abs() / 3.0,
            "{before} {after} {exact}"
        );
        // Every element keeps a positive volume.
        for t in &fitted.tets {
            let [a, b, c, d] = [t[0], t[1], t[2], t[3]].map(|i| fitted.nodes[i as usize]);
            assert!((b - a).cross(c - a).dot(d - a) > 0.0);
        }
        // Conduction along the bar: T = P L / k A.
        let steel = material("steel").unwrap();
        let cold = face_at(&bar, -DVec3::Z, 0.0);
        let hot = face_at(&bar, DVec3::Z, 100.0);
        let area = exact / 100.0;
        let want = 10.0 * 100.0 / (steel.conductivity_w_mk * 1e-3 * area);
        let heat = |m: &TetMesh| {
            solve_thermal(
                m,
                &steel,
                &[
                    HeatCondition::Temperature {
                        faces: vec![cold],
                        celsius: 0.0,
                    },
                    HeatCondition::Power {
                        faces: vec![hot],
                        watts: 10.0,
                    },
                ],
            )
            .unwrap()
            .range
            .1
        };
        let (rough, close) = (heat(&stepped), heat(&fitted));
        assert!((close - want).abs() < 0.02 * want, "{close} vs {want}");
        assert!(
            (close - want).abs() < (rough - want).abs(),
            "{rough} {close} {want}"
        );
        // A box on grid lines does not move at all.
        let beam = block(DVec3::ZERO, DVec3::new(200.0, 20.0, 20.0));
        let mut boxed = tet_mesh(&beam, 10.0, 100_000, &tol).unwrap();
        let nodes = boxed.nodes.clone();
        let report = fit_boundary(&mut boxed, &beam, &tol).unwrap();
        assert_eq!(report.moved, 0);
        assert_eq!(boxed.nodes, nodes);
    }
}
