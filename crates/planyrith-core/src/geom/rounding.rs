//! Implements docs/03-geometry-kernel.md §4.15 Edge rounding: fillets and
//! chamfers on the edges of a solid. Each edge becomes a prism whose cross
//! section is the material to take away (a convex edge) or to add (a
//! concave one) — the corner between the two faces and the chamfer line or
//! the arc tangent to both — run through the solid booleans of §4.13. The
//! prism reaches a little past both faces (and, when it cuts, past both
//! ends of the edge), so its sides never lie in a face of the solid.

use std::collections::BTreeSet;

use glam::DVec3;

use crate::geom::boolean::{BooleanOp, Piece, boolean, stitch};
use crate::geom::{EdgeFlags, FaceSides, GeometryError, HalfEdgeMesh, NoDeltas};
use crate::ids::EdgeId;
use crate::tolerance::Tolerances;

/// The shape a rounded edge gets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rounding {
    /// An arc of this radius (mm) tangent to both faces, in `segments`
    /// straight pieces.
    Fillet {
        /// Radius, mm.
        radius: f64,
        /// Straight pieces of the arc.
        segments: u32,
    },
    /// A flat cut this far (mm) along both faces from the edge.
    Chamfer {
        /// Distance along each face, mm.
        distance: f64,
    },
}

/// An edge as rounding needs it, read before the mesh changes.
#[derive(Debug, Clone, PartialEq)]
struct EdgeFrame {
    a: DVec3,
    b: DVec3,
    /// Outward normals of the two faces.
    na: DVec3,
    nb: DVec3,
    /// Directions in each face, square to the edge, away from it.
    ta: DVec3,
    tb: DVec3,
    /// The faces meet in a convex edge (material is taken away).
    convex: bool,
    /// Appearance of the first face (the new faces take it).
    sides: FaceSides,
}

/// What [`round_edges`] did.
#[derive(Debug, Clone)]
pub struct RoundingResult {
    /// The rounded solid.
    pub mesh: HalfEdgeMesh,
    /// Edges rounded.
    pub rounded: usize,
}

fn not_applicable(reason: impl Into<String>) -> GeometryError {
    GeometryError::NotApplicable(reason.into())
}

/// The frame of edge `e` of solid `mesh`.
fn frame(mesh: &HalfEdgeMesh, e: EdgeId, tol: &Tolerances) -> Result<EdgeFrame, GeometryError> {
    let [Some(fa), Some(fb)] = mesh.edge_faces(e)? else {
        return Err(not_applicable(format!("{e} does not join two faces")));
    };
    let seg = mesh.edge_segment(e)?;
    let dir = (seg.b - seg.a).normalize_or_zero();
    if dir == DVec3::ZERO {
        return Err(GeometryError::Degenerate(format!("{e} has no length")));
    }
    let (na, nb) = (mesh.face_normal(fa)?, mesh.face_normal(fb)?);
    if tol.is_parallel(na, nb) {
        return Err(not_applicable(format!(
            "the faces at {e} are flat to each other; there is no corner to round"
        )));
    }
    // In-face directions square to the edge, turned toward each face.
    let toward = |f, n: DVec3| -> Result<DVec3, GeometryError> {
        let t = n.cross(dir).normalize_or_zero();
        let center = mesh
            .face_vertices(f)?
            .iter()
            .map(|&v| mesh.position(v))
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .sum::<DVec3>()
            / mesh.face_vertices(f)?.len().max(1) as f64;
        Ok(if (center - seg.a).dot(t) < 0.0 { -t } else { t })
    };
    let (ta, tb) = (toward(fa, na)?, toward(fb, nb)?);
    let sides = mesh.face(fa).map(|f| f.sides.clone()).unwrap_or_default();
    Ok(EdgeFrame {
        a: seg.a,
        b: seg.b,
        na,
        nb,
        ta,
        tb,
        // Into face B goes below face A: the faces enclose the solid's
        // corner.
        convex: na.dot(tb) < 0.0,
        sides,
    })
}

/// The prism to subtract (convex) or add (concave) for one edge.
fn cutter(f: &EdgeFrame, shape: Rounding, tol: &Tolerances) -> Result<HalfEdgeMesh, GeometryError> {
    let dir = (f.b - f.a).normalize();
    let cos = f.ta.dot(f.tb).clamp(-1.0, 1.0);
    let angle = libm::acos(cos);
    if angle < 1e-3 || angle > std::f64::consts::PI - 1e-3 {
        return Err(not_applicable(
            "the faces meet too flat or too sharp to round",
        ));
    }
    let half = angle / 2.0;
    let size = match shape {
        Rounding::Fillet { radius, .. } => radius,
        Rounding::Chamfer { distance } => distance,
    };
    if size.partial_cmp(&tol.length) != Some(std::cmp::Ordering::Greater) {
        return Err(not_applicable(
            "the size must be larger than the model tolerance",
        ));
    }
    // The section in the plane square to the edge at `a`, from face A to
    // face B through the corner's replacement.
    let mut section = Vec::new();
    let reach = match shape {
        Rounding::Fillet { radius, segments } => {
            let back = radius / libm::tan(half);
            let (p, q) = (f.a + f.ta * back, f.a + f.tb * back);
            let center = f.a + (f.ta + f.tb).normalize() * (radius / libm::sin(half));
            let (u, w) = ((p - center) / radius, (q - center) / radius);
            let sweep = libm::acos(u.dot(w).clamp(-1.0, 1.0));
            let n = segments.clamp(1, 64);
            section.push(p);
            for k in 1..n {
                let t = f64::from(k) / f64::from(n);
                let s = libm::sin((1.0 - t) * sweep) / libm::sin(sweep);
                let r = libm::sin(t * sweep) / libm::sin(sweep);
                section.push(center + (u * s + w * r) * radius);
            }
            section.push(q);
            back
        }
        Rounding::Chamfer { distance } => {
            section.push(f.a + f.ta * distance);
            section.push(f.a + f.tb * distance);
            distance
        }
    };
    // Past the faces: outward of the solid for a convex edge, into it for
    // a concave one.
    let side = if f.convex { 1.0 } else { -1.0 };
    let margin = reach * 0.5 + tol.length * 100.0;
    let q = *section.last().unwrap_or(&f.a);
    let p = section[0];
    section.push(q + f.nb * side * margin);
    section.push(f.a + (f.na + f.nb).normalize() * side * margin * 1.5);
    section.push(p + f.na * side * margin);
    // Counter-clockwise seen from +dir.
    let mut newell = DVec3::ZERO;
    for (i, a) in section.iter().enumerate() {
        newell += a.cross(section[(i + 1) % section.len()]);
    }
    if newell.dot(dir) < 0.0 {
        section.reverse();
    }
    let length = (f.b - f.a).length();
    // A cut may run on past the ends (into the air there); added material
    // must stop at them.
    let extra = if f.convex { reach + margin } else { 0.0 };
    let start: Vec<DVec3> = section.iter().map(|&p| p - dir * extra).collect();
    let end: Vec<DVec3> = section
        .iter()
        .map(|&p| p + dir * (length + extra))
        .collect();
    // Subtracted pieces turn inside out, so their back carries the look.
    let sides = if f.convex {
        FaceSides {
            material_front: f.sides.material_back,
            material_back: f.sides.material_front,
            uv_front: None,
            uv_back: None,
        }
    } else {
        FaceSides {
            uv_front: None,
            uv_back: None,
            ..f.sides.clone()
        }
    };
    let mut pieces = vec![
        Piece {
            loops: vec![end.clone()],
            sides: sides.clone(),
        },
        Piece {
            loops: vec![start.iter().rev().copied().collect()],
            sides: sides.clone(),
        },
    ];
    for i in 0..section.len() {
        let j = (i + 1) % section.len();
        pieces.push(Piece {
            loops: vec![vec![start[i], start[j], end[j], end[i]]],
            sides: sides.clone(),
        });
    }
    stitch(&pieces, tol)
}

/// Rounds `edges` of the solid `mesh`, one after another; edges are read
/// before any changes, so their ids refer to `mesh` as given.
pub fn round_edges(
    mesh: &HalfEdgeMesh,
    edges: &[EdgeId],
    shape: Rounding,
    tol: &Tolerances,
) -> Result<RoundingResult, GeometryError> {
    if !mesh.is_solid() {
        return Err(not_applicable(
            "rounding needs a closed solid (check it with solid-check)",
        ));
    }
    let edges: BTreeSet<EdgeId> = edges.iter().copied().collect();
    let frames = edges
        .iter()
        .map(|&e| frame(mesh, e, tol))
        .collect::<Result<Vec<_>, _>>()?;
    let mut current = mesh.clone();
    let identity = glam::DAffine3::IDENTITY;
    for f in &frames {
        let cut = cutter(f, shape, tol)?;
        let op = if f.convex {
            BooleanOp::Subtract
        } else {
            BooleanOp::Union
        };
        let result = boolean(op, &current, &identity, &cut, &identity, tol)?;
        current =
            result.solids.into_iter().next().ok_or_else(|| {
                GeometryError::NonRobust("rounding removed the whole solid".into())
            })?;
    }
    if let Rounding::Fillet { segments, .. } = shape {
        soften_arcs(&mut current, &frames, shape, segments, tol)?;
    }
    Ok(RoundingResult {
        mesh: current,
        rounded: frames.len(),
    })
}

/// Softens the edges between the pieces of each arc and where the arcs
/// meet the faces (they are tangent there).
fn soften_arcs(
    mesh: &mut HalfEdgeMesh,
    frames: &[EdgeFrame],
    shape: Rounding,
    segments: u32,
    tol: &Tolerances,
) -> Result<(), GeometryError> {
    let Rounding::Fillet { radius, .. } = shape else {
        return Ok(());
    };
    let near = |p: DVec3| {
        frames.iter().any(|f| {
            let dir = (f.b - f.a).normalize_or_zero();
            let along = (p - f.a).dot(dir);
            let off = (p - f.a) - dir * along;
            off.length() <= radius * 2.0 + tol.length
        })
    };
    let steps = f64::from(segments.clamp(1, 64));
    let mut soft = Vec::new();
    for (e, _) in mesh.edges() {
        let [Some(fa), Some(fb)] = mesh.edge_faces(e)? else {
            continue;
        };
        let turn = libm::acos(
            mesh.face_normal(fa)?
                .dot(mesh.face_normal(fb)?)
                .clamp(-1.0, 1.0),
        );
        // One arc piece turns at most π / segments.
        if turn > std::f64::consts::PI / steps * 1.05 + tol.angle {
            continue;
        }
        let seg = mesh.edge_segment(e)?;
        if near(seg.a) && near(seg.b) {
            soft.push(e);
        }
    }
    for e in soft {
        mesh.set_edge_flags(e, EdgeFlags::SOFT | EdgeFlags::SMOOTH, &mut NoDeltas)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tol() -> Tolerances {
        Tolerances::default()
    }

    /// A box `x × y × z` at the origin facing outward.
    fn block(x: f64, y: f64, z: f64) -> HalfEdgeMesh {
        let corner = |i: u8| {
            DVec3::new(
                if i & 1 != 0 { x } else { 0.0 },
                if i & 2 != 0 { y } else { 0.0 },
                if i & 4 != 0 { z } else { 0.0 },
            )
        };
        let quad = |i: [u8; 4]| Piece {
            loops: vec![i.iter().map(|&k| corner(k)).collect()],
            sides: FaceSides::default(),
        };
        stitch(
            &[
                quad([0, 2, 3, 1]),
                quad([4, 5, 7, 6]),
                quad([0, 1, 5, 4]),
                quad([2, 6, 7, 3]),
                quad([0, 4, 6, 2]),
                quad([1, 3, 7, 5]),
            ],
            &tol(),
        )
        .unwrap()
    }

    /// The edge of `mesh` from `a` to `b` (either way).
    fn edge(mesh: &HalfEdgeMesh, a: DVec3, b: DVec3) -> EdgeId {
        mesh.edges()
            .find(|(e, _)| {
                let s = mesh.edge_segment(*e).unwrap();
                (s.a.distance(a) < 1e-6 && s.b.distance(b) < 1e-6)
                    || (s.a.distance(b) < 1e-6 && s.b.distance(a) < 1e-6)
            })
            .map(|(e, _)| e)
            .unwrap()
    }

    #[test]
    fn a_chamfer_cuts_a_triangular_prism_off_a_box() {
        let mesh = block(100.0, 50.0, 40.0);
        assert!(mesh.volume().unwrap() > 0.0);
        let e = edge(
            &mesh,
            DVec3::new(0.0, 0.0, 40.0),
            DVec3::new(100.0, 0.0, 40.0),
        );
        let out = round_edges(&mesh, &[e], Rounding::Chamfer { distance: 10.0 }, &tol()).unwrap();
        assert!(out.mesh.is_solid());
        assert_eq!(out.mesh.face_count(), 7);
        let volume = out.mesh.volume().unwrap();
        assert!(
            (volume - (200_000.0 - 50.0 * 100.0)).abs() < 1e-6,
            "{volume}"
        );
    }

    #[test]
    fn a_fillet_takes_away_the_corner_outside_the_arc() {
        let mesh = block(100.0, 50.0, 40.0);
        let e = edge(
            &mesh,
            DVec3::new(0.0, 0.0, 40.0),
            DVec3::new(100.0, 0.0, 40.0),
        );
        let shape = Rounding::Fillet {
            radius: 10.0,
            segments: 8,
        };
        let out = round_edges(&mesh, &[e], shape, &tol()).unwrap();
        assert!(out.mesh.is_solid());
        assert_eq!(out.mesh.face_count(), 6 + 8);
        // The corner square minus the quarter disc, polygonal: 8 chords.
        let polygon = 0.5 * 8.0 * 100.0 * libm::sin(std::f64::consts::FRAC_PI_2 / 8.0);
        let removed = 100.0 - polygon;
        let volume = out.mesh.volume().unwrap();
        assert!(
            (volume - (200_000.0 - removed * 100.0)).abs() < 1e-6,
            "{volume}"
        );
        let soft = out
            .mesh
            .edges()
            .filter(|(_, e)| e.flags.contains(EdgeFlags::SOFT))
            .count();
        assert_eq!(
            soft, 9,
            "seven between the pieces and two tangent to the faces"
        );
    }

    #[test]
    fn a_concave_edge_gets_material_added() {
        // An L: a block with a second block on its top front half.
        let mut pieces = Vec::new();
        let l = [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(0.0, 60.0, 0.0),
            DVec3::new(0.0, 60.0, 20.0),
            DVec3::new(0.0, 30.0, 20.0),
            DVec3::new(0.0, 30.0, 50.0),
            DVec3::new(0.0, 0.0, 50.0),
        ];
        let far: Vec<DVec3> = l.iter().map(|p| *p + DVec3::X * 80.0).collect();
        pieces.push(Piece {
            loops: vec![l.iter().rev().copied().collect()],
            sides: FaceSides::default(),
        });
        pieces.push(Piece {
            loops: vec![far.clone()],
            sides: FaceSides::default(),
        });
        for i in 0..l.len() {
            let j = (i + 1) % l.len();
            pieces.push(Piece {
                loops: vec![vec![l[i], l[j], far[j], far[i]]],
                sides: FaceSides::default(),
            });
        }
        let mut mesh = stitch(&pieces, &tol()).unwrap();
        if mesh.volume().unwrap() < 0.0 {
            let all = mesh.faces().map(|(f, _)| f).collect();
            crate::geom::planar::reverse_faces(&mut mesh, &all, &tol(), &mut NoDeltas).unwrap();
        }
        let before = mesh.volume().unwrap();
        let e = edge(
            &mesh,
            DVec3::new(0.0, 30.0, 20.0),
            DVec3::new(80.0, 30.0, 20.0),
        );
        let out = round_edges(&mesh, &[e], Rounding::Chamfer { distance: 10.0 }, &tol()).unwrap();
        assert!(out.mesh.is_solid());
        let added = out.mesh.volume().unwrap() - before;
        assert!((added - 50.0 * 80.0).abs() < 1e-6, "{added}");
    }

    #[test]
    fn open_shells_and_flat_edges_are_refused() {
        let mesh = block(10.0, 10.0, 10.0);
        let e = edge(&mesh, DVec3::ZERO, DVec3::new(10.0, 0.0, 0.0));
        let too_big = round_edges(&mesh, &[e], Rounding::Chamfer { distance: 0.0 }, &tol());
        assert!(too_big.is_err());
        let mut open = mesh.clone();
        let top = open
            .faces()
            .find(|(_, f)| f.plane.normal.z > 0.5)
            .map(|(f, _)| f)
            .unwrap();
        open.erase_face(top, false, &tol(), &mut NoDeltas).unwrap();
        assert!(round_edges(&open, &[e], Rounding::Chamfer { distance: 1.0 }, &tol()).is_err());
    }
}
