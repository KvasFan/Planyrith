//! Implements docs/03-geometry-kernel.md §4.13 (solid classification and
//! booleans): union, intersect, subtract, trim, split, outer shell on closed
//! 2-manifold meshes, with the `brep` fallback hook.
//!
//! The algorithm follows §4.13 on world-space copies of both solids:
//! 1. intersection segments of every face pair (and, for coplanar pairs, the
//!    other face's boundary clipped to the face) are added to both copies
//!    with `add_edge`, which splits their faces along the intersection;
//! 2. every face piece is classified by a strictly interior sample point:
//!    on the other solid's surface (same or opposite normal), or inside or
//!    outside it by ray parity, retried along other directions when a ray
//!    grazes an edge;
//! 3. pieces are kept per operation (coplanar pieces once, from A);
//! 4. kept pieces are triangulated and stitched by
//!    [`HalfEdgeMesh::from_triangles_keyed`], which welds vertices and merges
//!    coplanar neighbors with the same appearance; the result must be a
//!    solid, else `NonRobust` with guidance.
//!
//! Owner key: `later` (milestone 0.8).

#![warn(unused_variables, dead_code)]

use std::collections::BTreeSet;

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::diagnostics::Warning;
use crate::geom::intersect::{add_segments, face_face};
use crate::geom::planar::face_polygon;
use crate::geom::tessellate::tessellate_face;
use crate::geom::{FaceSides, GeometryError, HalfEdgeMesh, NoDeltas};
use crate::ids::FaceId;
use crate::math::polygon::{PointInPolygon, point_in_polygon};
use crate::math::{Ray, Segment};
use crate::tolerance::Tolerances;

/// Boolean operation (docs/03 §4.13 step 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BooleanOp {
    /// Outside A ∪ outside B.
    Union,
    /// Inside ∩.
    Intersect,
    /// A − B.
    Subtract,
    /// Subtract but keep B.
    Trim,
    /// All three regions as separate solids.
    Split,
    /// Union of all, dropping internal voids.
    OuterShell,
}

/// Position of a point or face piece relative to a solid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// Strictly inside.
    Inside,
    /// Strictly outside.
    Outside,
    /// On the boundary (coplanar piece); `same_normal` tells the orientation.
    On {
        /// `true` when the piece's normal agrees with the solid's face.
        same_normal: bool,
    },
}

/// Result of [`boolean`].
#[derive(Debug, Clone)]
pub struct BooleanResult {
    /// True when the optional B-rep backend produced the answer.
    pub fallback_used: bool,
    /// Resulting solids: one for most operations; `[A − B, B]` for `Trim`;
    /// for `Split` the non-empty ones of `[A − B, A ∩ B, B − A]`.
    pub solids: Vec<HalfEdgeMesh>,
    /// Non-fatal notes (e.g. `W2001` coplanar merges).
    pub warnings: Vec<Warning>,
}

/// Fixed, deliberately irregular ray directions for parity tests: a ray
/// that grazes an edge or runs along a face is retried along the next one.
const RAY_DIRECTIONS: [[f64; 3]; 6] = [
    [0.314_159, 0.712_345, 0.632_456],
    [-0.618_034, 0.271_828, 0.738_005],
    [0.809_017, -0.309_017, 0.5],
    [-0.173_648, -0.984_808, 0.012_345],
    [0.123_457, 0.234_568, -0.964_197],
    [-0.957_427, 0.141_421, -0.251_189],
];

fn ambiguous() -> GeometryError {
    GeometryError::NonRobust("solid boundary is ambiguous within tolerance; separate coincident edges or adjust the model tolerance".into())
}

fn polygon_contains(
    mesh: &HalfEdgeMesh,
    f: FaceId,
    p: DVec3,
    tol: &Tolerances,
) -> Result<PointInPolygon, GeometryError> {
    let frame = mesh.face_ref(f)?.plane.frame();
    let (outer, holes) = face_polygon(mesh, f, &frame)?;
    let holes: Vec<_> = holes.iter().map(Vec::as_slice).collect();
    Ok(point_in_polygon(frame.to_2d(p), &outer, &holes, tol))
}

/// Classifies a point by ray parity, retrying grazes in a fixed order (§4.13).
pub fn classify_point(
    solid: &HalfEdgeMesh,
    p: DVec3,
    tol: &Tolerances,
) -> Result<Classification, GeometryError> {
    if !tol.in_range(p) {
        return Err(GeometryError::OutOfRange);
    }
    for (f, face) in solid.faces() {
        if tol.point_on_plane(&face.plane, p)
            && polygon_contains(solid, f, p, tol)?.is_inside_or_on()
        {
            return Ok(Classification::On { same_normal: true });
        }
    }
    'directions: for dir in RAY_DIRECTIONS {
        let ray = Ray::new(p, DVec3::from_array(dir));
        let mut hits = 0;
        for (f, face) in solid.faces() {
            if ray.dir.dot(face.plane.normal).abs() <= tol.angle {
                if tol.point_on_plane(&face.plane, p) {
                    continue 'directions;
                }
                continue;
            }
            if let Some(t) = ray.intersect_plane(&face.plane, tol) {
                match polygon_contains(solid, f, ray.at(t), tol)? {
                    PointInPolygon::Boundary => continue 'directions,
                    PointInPolygon::Inside => hits += 1,
                    PointInPolygon::Outside => {}
                }
            }
        }
        return Ok(if hits % 2 == 0 {
            Classification::Outside
        } else {
            Classification::Inside
        });
    }
    Err(ambiguous())
}

fn interior_point(
    mesh: &HalfEdgeMesh,
    f: FaceId,
    tol: &Tolerances,
) -> Result<DVec3, GeometryError> {
    let tess = tessellate_face(mesh, f, tol)?;
    let mut candidates = tess
        .triangles
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| tess.positions[i as usize]);
            ((b - a).cross(c - a).length_squared(), (a + b + c) / 3.0)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    candidates
        .into_iter()
        .map(|(_, p)| p)
        .find(|&p| {
            matches!(
                polygon_contains(mesh, f, p, tol),
                Ok(PointInPolygon::Inside)
            )
        })
        .ok_or_else(ambiguous)
}

/// Classifies a split face using a strictly interior sample and its normal.
pub fn classify_face(
    pieces: &HalfEdgeMesh,
    f: FaceId,
    other: &HalfEdgeMesh,
    tol: &Tolerances,
) -> Result<Classification, GeometryError> {
    let p = interior_point(pieces, f, tol)?;
    let n = pieces.face_normal(f)?;
    for (g, face) in other.faces() {
        if tol.point_on_plane(&face.plane, p)
            && polygon_contains(other, g, p, tol)?.is_inside_or_on()
        {
            if !tol.is_parallel(n, face.plane.normal) {
                return Err(ambiguous());
            }
            return Ok(Classification::On {
                same_normal: n.dot(face.plane.normal) > 0.0,
            });
        }
    }
    classify_point(other, p, tol)
}

// Clip each coplanar boundary to the other polygon, including its holes.
fn coplanar_segments(
    a: &HalfEdgeMesh,
    fa: FaceId,
    b: &HalfEdgeMesh,
    fb: FaceId,
    tol: &Tolerances,
) -> Result<Vec<Segment>, GeometryError> {
    let mut out = Vec::new();
    for e in b.face_edges(fb)? {
        let s = b.edge_segment(e)?;
        let mut ts = vec![0.0, 1.0];
        for ea in a.face_edges(fa)? {
            match crate::math::intersect::segment_segment(&s, &a.edge_segment(ea)?, tol) {
                crate::math::SegmentContact::Point { t_a, .. } => ts.push(t_a),
                crate::math::SegmentContact::Overlap { t0, t1 } => ts.extend([t0, t1]),
                crate::math::SegmentContact::None => {}
            }
        }
        ts.sort_by(f64::total_cmp);
        ts.dedup_by(|x, y| (*x - *y).abs() <= tol.parametric);
        for w in ts.windows(2) {
            let at = |t| s.a + (s.b - s.a) * t;
            if polygon_contains(a, fa, at((w[0] + w[1]) * 0.5), tol)?.is_inside_or_on()
                && at(w[0]).distance(at(w[1])) > tol.length
            {
                out.push(Segment::new(at(w[0]), at(w[1])));
            }
        }
    }
    Ok(out)
}

/// A planar face ready to stitch; loop order and appearance are retained.
pub(crate) struct Piece {
    pub(crate) loops: Vec<Vec<DVec3>>,
    pub(crate) sides: FaceSides,
}

/// Welds and subdivides shared boundaries before assembling planar faces.
pub(crate) fn stitch(pieces: &[Piece], tol: &Tolerances) -> Result<HalfEdgeMesh, GeometryError> {
    use crate::geom::mesh::{DirectedEdgeIndex, IndexedEdge, IndexedFace, IndexedMesh};
    use std::collections::BTreeMap;
    let mut indexed = IndexedMesh::default();
    for piece in pieces {
        for &p in piece.loops.iter().flatten() {
            if !tol.in_range(p) {
                return Err(GeometryError::OutOfRange);
            }
            if !indexed.vertices.iter().any(|&q| tol.same_point(p, q)) {
                indexed.vertices.push(p);
            }
        }
    }
    let mut edges = BTreeMap::new();
    for piece in pieces {
        let mut loops = Vec::new();
        for ring in &piece.loops {
            let mut lp = Vec::new();
            for i in 0..ring.len() {
                let s = Segment::new(ring[i], ring[(i + 1) % ring.len()]);
                let dir = s.b - s.a;
                let mut on = indexed
                    .vertices
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| tol.point_on_segment(**p, &s))
                    .map(|(j, p)| (dir.dot(*p - s.a) / dir.length_squared(), j as u32))
                    .collect::<Vec<_>>();
                on.sort_by(|a, b| a.0.total_cmp(&b.0));
                for w in on.windows(2) {
                    let (a, b) = (w[0].1, w[1].1);
                    let key = (a.min(b), a.max(b));
                    let e = *edges.entry(key).or_insert_with(|| {
                        let e = indexed.edges.len() as u32;
                        indexed.edges.push(IndexedEdge {
                            a: key.0,
                            b: key.1,
                            flags: crate::geom::EdgeFlags::NONE,
                            curve: None,
                        });
                        e
                    });
                    lp.push(if a < b {
                        DirectedEdgeIndex::forward(e)
                    } else {
                        DirectedEdgeIndex::reversed(e)
                    });
                }
            }
            loops.push(lp);
        }
        indexed.faces.push(IndexedFace {
            loops,
            plane: None,
            sides: piece.sides.clone(),
        });
    }
    HalfEdgeMesh::from_indexed(&indexed, tol)
}

fn piece(mesh: &HalfEdgeMesh, f: FaceId, flip: bool) -> Result<Piece, GeometryError> {
    let mut loops = mesh
        .face_loops(f)?
        .into_iter()
        .map(|l| mesh.loop_positions(l))
        .collect::<Result<Vec<_>, _>>()?;
    let mut sides = mesh.face_ref(f)?.sides.clone();
    if flip {
        for lp in &mut loops {
            lp.reverse();
        }
        std::mem::swap(&mut sides.material_front, &mut sides.material_back);
        std::mem::swap(&mut sides.uv_front, &mut sides.uv_back);
    }
    Ok(Piece { loops, sides })
}

fn region(
    op: BooleanOp,
    a: &HalfEdgeMesh,
    b: &HalfEdgeMesh,
    original_a: &HalfEdgeMesh,
    original_b: &HalfEdgeMesh,
    tol: &Tolerances,
) -> Result<Option<HalfEdgeMesh>, GeometryError> {
    use Classification::{Inside, On, Outside};
    let mut pieces = Vec::new();
    for (from_a, mesh, other) in [(true, a, original_b), (false, b, original_a)] {
        for (f, _) in mesh.faces() {
            let c = classify_face(mesh, f, other, tol)?;
            let keep = match op {
                BooleanOp::Union | BooleanOp::OuterShell => {
                    c == Outside || (from_a && c == On { same_normal: true })
                }
                BooleanOp::Intersect => c == Inside || (from_a && c == On { same_normal: true }),
                BooleanOp::Subtract => {
                    if from_a {
                        c == Outside || c == On { same_normal: false }
                    } else {
                        c == Inside
                    }
                }
                _ => false,
            };
            if keep {
                pieces.push(piece(mesh, f, !from_a && op == BooleanOp::Subtract)?);
            }
        }
    }
    if pieces.is_empty() {
        return Ok(None);
    }
    let mut result = stitch(&pieces, tol)?;
    merge_coplanar_faces(&mut result, tol)?;
    let flags = result
        .edges()
        .map(|(e, _)| {
            let segment = result.edge_segment(e)?;
            let mut flags = crate::geom::EdgeFlags::NONE;
            for source in [original_a, original_b] {
                for (id, edge) in source.edges() {
                    let s = source.edge_segment(id)?;
                    if tol.point_on_segment(segment.a, &s) && tol.point_on_segment(segment.b, &s) {
                        flags |= edge.flags;
                    }
                }
            }
            Ok((e, flags))
        })
        .collect::<Result<Vec<_>, GeometryError>>()?;
    for (e, flags) in flags {
        result.set_edge_flags(e, flags, &mut NoDeltas)?;
    }
    if !result.is_solid() {
        return Err(ambiguous());
    }
    if op == BooleanOp::OuterShell {
        remove_voids(&mut result, tol)?;
    }
    Ok(Some(result))
}

pub(crate) fn remove_voids(mesh: &mut HalfEdgeMesh, tol: &Tolerances) -> Result<(), GeometryError> {
    let mut remaining: BTreeSet<_> = mesh.faces().map(|(f, _)| f).collect();
    let mut exterior = Vec::new();
    while let Some(&first) = remaining.first() {
        let mut stack = vec![first];
        let mut component = Vec::new();
        while let Some(f) = stack.pop() {
            if !remaining.remove(&f) {
                continue;
            }
            component.push(piece(mesh, f, false)?);
            stack.extend(mesh.face_neighbors(f)?);
        }
        if stitch(&component, tol)?.volume()? > tol.length * tol.length_sq {
            exterior.extend(component);
        }
    }
    *mesh = stitch(&exterior, tol)?;
    Ok(())
}

/// Boolean of two closed meshes in their supplied coordinate frames (§4.13).
fn boolean_mesh(
    op: BooleanOp,
    a: &HalfEdgeMesh,
    to_world_a: &DAffine3,
    b: &HalfEdgeMesh,
    to_world_b: &DAffine3,
    tol: &Tolerances,
) -> Result<BooleanResult, GeometryError> {
    if !a.is_solid() || !b.is_solid() {
        return Err(GeometryError::NotApplicable(
            "solid tools require closed, consistently oriented meshes without wire edges".into(),
        ));
    }
    crate::geom::intersect::invert(to_world_a)?;
    crate::geom::intersect::invert(to_world_b)?;
    let world = |mesh: &HalfEdgeMesh, t: &DAffine3| -> Result<HalfEdgeMesh, GeometryError> {
        let (indexed, _) = mesh.to_indexed();
        let indexed = crate::geom::mesh::transform_indexed(&indexed, t);
        let mut m = HalfEdgeMesh::from_indexed(&indexed, tol)?;
        if m.volume()? < 0.0 {
            let faces = m.faces().map(|(f, _)| f).collect();
            crate::geom::planar::reverse_faces(&mut m, &faces, tol, &mut NoDeltas)?;
        }
        Ok(m)
    };
    let (original_a, original_b) = (world(a, to_world_a)?, world(b, to_world_b)?);
    let (mut a, mut b) = (original_a.clone(), original_b.clone());
    let (mut sa, mut sb) = (Vec::new(), Vec::new());
    for (fa, pa) in original_a.faces() {
        for (fb, pb) in original_b.faces() {
            if !original_a
                .face_aabb(fa)?
                .expanded(tol.length)
                .intersects(&original_b.face_aabb(fb)?)
            {
                continue;
            }
            if tol.is_coplanar(&pa.plane, &pb.plane) {
                sa.extend(coplanar_segments(&original_a, fa, &original_b, fb, tol)?);
                sb.extend(coplanar_segments(&original_b, fb, &original_a, fa, tol)?);
            } else {
                let segments = face_face(
                    &original_a,
                    fa,
                    &DAffine3::IDENTITY,
                    &original_b,
                    fb,
                    &DAffine3::IDENTITY,
                    tol,
                )?;
                sa.extend_from_slice(&segments);
                sb.extend(segments);
            }
        }
    }
    add_segments(&mut a, &sa, tol, &mut NoDeltas)?;
    add_segments(&mut b, &sb, tol, &mut NoDeltas)?;
    let mut solids = Vec::new();
    match op {
        BooleanOp::Trim => {
            solids.extend(region(
                BooleanOp::Subtract,
                &a,
                &b,
                &original_a,
                &original_b,
                tol,
            )?);
            solids.push(original_b);
        }
        BooleanOp::Split => {
            solids.extend(region(
                BooleanOp::Subtract,
                &a,
                &b,
                &original_a,
                &original_b,
                tol,
            )?);
            solids.extend(region(
                BooleanOp::Intersect,
                &a,
                &b,
                &original_a,
                &original_b,
                tol,
            )?);
            solids.extend(region(
                BooleanOp::Subtract,
                &b,
                &a,
                &original_b,
                &original_a,
                tol,
            )?);
        }
        _ => solids.extend(region(op, &a, &b, &original_a, &original_b, tol)?),
    }
    Ok(BooleanResult {
        fallback_used: false,
        solids,
        warnings: Vec::new(),
    })
}

/// Boolean of two closed meshes, retrying ambiguous boundaries with the optional
/// B-rep backend. Inputs are never mutated (docs/03 §4.13).
pub fn boolean(
    op: BooleanOp,
    a: &HalfEdgeMesh,
    to_world_a: &DAffine3,
    b: &HalfEdgeMesh,
    to_world_b: &DAffine3,
    tol: &Tolerances,
) -> Result<BooleanResult, GeometryError> {
    let result = boolean_mesh(op, a, to_world_a, b, to_world_b, tol);
    #[cfg(feature = "brep")]
    if matches!(result, Err(GeometryError::NonRobust(_))) {
        return crate::geom::brep::boolean(op, a, to_world_a, b, to_world_b, tol);
    }
    result
}

/// Merges adjacent coplanar faces with matching appearance (§4.13 step 4).
pub fn merge_coplanar_faces(
    mesh: &mut HalfEdgeMesh,
    tol: &Tolerances,
) -> Result<usize, GeometryError> {
    let mut count = 0;
    loop {
        let mut pair = None;
        for (e, _) in mesh.edges() {
            if let [Some(a), Some(b)] = mesh.edge_faces(e)?
                && a != b
                && mesh.face_ref(a)?.sides == mesh.face_ref(b)?.sides
                && tol.is_coplanar_same_side(&mesh.face_ref(a)?.plane, &mesh.face_ref(b)?.plane)
            {
                pair = Some((a, b));
                break;
            }
        }
        let Some((a, b)) = pair else {
            break;
        };
        mesh.merge_faces(a, b, tol, &mut NoDeltas)?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    pub(crate) fn cube(origin: DVec3, size: DVec3) -> HalfEdgeMesh {
        let positions = (0..8u8)
            .map(|i| {
                origin
                    + DVec3::new(
                        if i & 1 != 0 { size.x } else { 0.0 },
                        if i & 2 != 0 { size.y } else { 0.0 },
                        if i & 4 != 0 { size.z } else { 0.0 },
                    )
            })
            .collect::<Vec<_>>();
        let quads = [
            [0, 2, 3, 1],
            [4, 5, 7, 6],
            [0, 1, 5, 4],
            [2, 6, 7, 3],
            [0, 4, 6, 2],
            [1, 3, 7, 5],
        ];
        let triangles = quads
            .iter()
            .flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]])
            .collect::<Vec<_>>();
        HalfEdgeMesh::from_triangles_merged(
            &positions,
            &triangles,
            Default::default(),
            &Tolerances::default(),
        )
        .unwrap()
        .mesh
    }

    fn run(op: BooleanOp, a: &HalfEdgeMesh, b: &HalfEdgeMesh) -> Vec<HalfEdgeMesh> {
        let r = boolean(
            op,
            a,
            &DAffine3::IDENTITY,
            b,
            &DAffine3::IDENTITY,
            &Tolerances::default(),
        )
        .unwrap();
        for s in &r.solids {
            assert!(s.is_solid());
            assert!(s.validate(&Tolerances::default()).is_empty());
        }
        r.solids
    }

    #[test]
    fn overlapping_cubes_have_twelve_faces_and_inclusion_exclusion_volume() {
        let a = cube(DVec3::ZERO, DVec3::splat(100.0));
        let b = cube(DVec3::splat(50.0), DVec3::splat(100.0));
        let union = run(BooleanOp::Union, &a, &b);
        let intersection = run(BooleanOp::Intersect, &a, &b);
        assert_eq!(union[0].face_count(), 12);
        assert!((union[0].volume().unwrap() - 1_875_000.0).abs() < 1e-6);
        assert!(
            (union[0].volume().unwrap() + intersection[0].volume().unwrap()
                - a.volume().unwrap()
                - b.volume().unwrap())
            .abs()
                < 1e-6
        );
        assert_eq!(run(BooleanOp::Split, &a, &b).len(), 3);
        assert_eq!(run(BooleanOp::Trim, &a, &b).len(), 2);
    }

    #[test]
    fn coplanar_identical_touching_contained_and_disjoint() {
        let a = cube(DVec3::ZERO, DVec3::splat(100.0));
        assert_eq!(run(BooleanOp::Union, &a, &a)[0].face_count(), 6);
        assert_eq!(run(BooleanOp::Intersect, &a, &a)[0].face_count(), 6);
        assert!(run(BooleanOp::Subtract, &a, &a).is_empty());
        for offset in [DVec3::X * 50.0, DVec3::X * 100.0, DVec3::X * 200.0] {
            let b = cube(offset, DVec3::splat(100.0));
            run(BooleanOp::Union, &a, &b);
            run(BooleanOp::Subtract, &a, &b);
            run(BooleanOp::Intersect, &a, &b);
        }
        let inner = cube(DVec3::splat(25.0), DVec3::splat(50.0));
        let hollow = run(BooleanOp::Subtract, &a, &inner).remove(0);
        assert!((hollow.volume().unwrap() - 875_000.0).abs() < 1e-6);
        let filled = run(BooleanOp::OuterShell, &hollow, &inner);
        assert!((filled[0].volume().unwrap() - 1_000_000.0).abs() < 1e-6);
    }

    #[test]
    fn cylinder_cut_through_slab_keeps_smooth_walls_and_holes() {
        let tol = Tolerances::default();
        let slab = cube(DVec3::ZERO, DVec3::splat(100.0));
        let mut cylinder = HalfEdgeMesh::new();
        let n = 24;
        let points = (0..n)
            .map(|i| {
                let angle = std::f64::consts::TAU * i as f64 / n as f64;
                DVec3::new(
                    50.0 + 20.0 * libm::cos(angle),
                    50.0 + 20.0 * libm::sin(angle),
                    -50.0,
                )
            })
            .collect::<Vec<_>>();
        let mut edges = Vec::new();
        for i in 0..n {
            edges.extend(
                cylinder
                    .add_edge(points[i], points[(i + 1) % n], &tol, &mut NoDeltas)
                    .unwrap()
                    .edges,
            );
        }
        cylinder
            .add_curve(
                crate::geom::CurveDescriptor::Circle {
                    center: DVec3::new(50.0, 50.0, -50.0),
                    normal: DVec3::Z,
                    radius: 20.0,
                    segments: n as u32,
                },
                &edges,
                &mut NoDeltas,
            )
            .unwrap();
        let face = cylinder.faces().next().unwrap().0;
        crate::geom::extrude::extrude_face(
            &mut cylinder,
            face,
            crate::units::Length::mm(200.0),
            Default::default(),
            &tol,
            &mut NoDeltas,
        )
        .unwrap();
        let result = run(BooleanOp::Subtract, &slab, &cylinder).remove(0);
        assert_eq!(
            result.faces().filter(|(_, f)| !f.holes.is_empty()).count(),
            2
        );
        assert_eq!(
            classify_point(&result, DVec3::splat(50.0), &tol).unwrap(),
            Classification::Outside
        );
        assert_eq!(
            classify_point(&result, DVec3::new(10.0, 10.0, 50.0), &tol).unwrap(),
            Classification::Inside
        );
        let removed = 0.5 * n as f64 * 400.0 * libm::sin(std::f64::consts::TAU / n as f64) * 100.0;
        assert!((result.volume().unwrap() - (1_000_000.0 - removed)).abs() < 1e-6);
        assert!(
            result
                .edges()
                .any(|(_, e)| e.flags.contains(crate::geom::EdgeFlags::SMOOTH))
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]
        #[test]
        fn translated_boxes_preserve_boolean_volumes(x in 10.0f64..90.0,y in 10.0f64..90.0,z in 10.0f64..90.0) {
            let a = cube(DVec3::ZERO,DVec3::splat(100.0));
            let b = cube(DVec3::new(x,y,z),DVec3::splat(100.0));
            let v = (100.0-x)*(100.0-y)*(100.0-z);
            let union = run(BooleanOp::Union,&a,&b);
            let cut = run(BooleanOp::Subtract,&a,&b);
            prop_assert!((union[0].volume().unwrap()-(2_000_000.0-v)).abs()<1e-6);
            prop_assert!((cut[0].volume().unwrap()-(1_000_000.0-v)).abs()<1e-6);
        }
    }
}
