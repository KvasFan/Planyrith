//! Implements docs/03-geometry-kernel.md §4.9 (extrude face, the Push/Pull
//! core): offset loops along the normal, side quads, cap, coplanar merge and
//! the push-through rule.
//!
//! Three cases (docs/03 Decisions):
//! - **Stretch**: every boundary edge of the face borders a face that contains
//!   the extrusion direction (the top of a box). The face's vertices move; its
//!   neighbors stretch. Works in both directions.
//! - **Generic**: vertical edges and cap edges are added with `add_edge`, so
//!   face finding builds the side faces and the cap with orientations that
//!   agree with the neighbors. A face whose boundary is free on the other side
//!   (a rectangle on its own) stays as the base, facing away from the volume;
//!   otherwise it is removed (it becomes interior). New side faces coplanar
//!   with the neighbor across their base edge merge into it (so pulling a
//!   rectangle drawn on a wall extends the wall).
//! - **Join / push-through**: when the cap outline lands inside a coplanar face
//!   of the same container, the outline is cut into that face first and the
//!   piece inside it is removed before the sides are added (an edge carries at
//!   most two faces), which opens a hole (pushing) or joins the volumes
//!   (pulling).
//!
//! Owner key: `later` (first consumer: the `push-pull` command, docs/05 §4.3).

#![warn(unused_variables, dead_code)]

use std::collections::BTreeSet;

use glam::{DAffine3, DVec3};

use crate::geom::mesh::TransformOptions;
use crate::geom::planar;
use crate::geom::{CurveDescriptor, DeltaSink, EdgeFlags, GeometryError, HalfEdgeMesh, MeshDelta};
use crate::ids::{EdgeId, FaceId, VertexId};
use crate::math::Plane;
use crate::math::polygon::{self, PointInPolygon};
use crate::tolerance::Tolerances;
use crate::units::Length;

/// Options for [`extrude_face`] (docs/03 §4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtrudeOptions {
    /// Keep the original face (Push/Pull modifier) instead of deleting it.
    pub create_new_start_face: bool,
    /// Merge new side faces with coplanar neighbors (step 3).
    pub merge_coplanar: bool,
    /// Cut a through-hole when the cap lands inside a coplanar face (step 4).
    pub push_through: bool,
}

impl Default for ExtrudeOptions {
    fn default() -> Self {
        ExtrudeOptions {
            create_new_start_face: false,
            merge_coplanar: true,
            push_through: true,
        }
    }
}

/// Result of [`extrude_face`]; mirrors the `push-pull` output of docs/08 §7.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtrudeResult {
    /// The new cap face (`None` when a through-hole consumed it).
    pub cap: Option<FaceId>,
    /// Side faces created (after merging).
    pub side_faces: Vec<FaceId>,
    /// Faces removed (the start face unless kept; the hole interior).
    pub removed_faces: Vec<FaceId>,
    /// Side faces merged into existing coplanar neighbors (`W2001`).
    pub merged_faces: Vec<FaceId>,
    /// `true` when the push-through rule created a hole.
    pub through_hole: bool,
}

/// Extrudes face `f` by `distance` along its normal (negative = push)
/// (docs/03 §4.9 steps 1–5). Atomic: on failure the mesh is unchanged.
pub fn extrude_face(
    mesh: &mut HalfEdgeMesh,
    f: FaceId,
    distance: Length,
    opts: ExtrudeOptions,
    tol: &Tolerances,
    sink: &mut dyn DeltaSink,
) -> Result<ExtrudeResult, GeometryError> {
    let d = distance.as_mm();
    if !d.is_finite() || d.abs() <= tol.length {
        return Err(GeometryError::Degenerate(
            "push/pull distance must be non-zero".into(),
        ));
    }
    mesh.face_ref(f)?;
    mesh.transact(tol, sink, |m, log| {
        if !opts.create_new_start_face && stretchable(m, f, tol)? {
            return stretch(m, f, d, tol, log);
        }
        extrude_generic(m, f, d, opts, tol, log)
    })
}

/// Every boundary edge borders another face that contains the face normal.
fn stretchable(m: &HalfEdgeMesh, f: FaceId, tol: &Tolerances) -> Result<bool, GeometryError> {
    let n = m.face_normal(f)?;
    for cycle in m.face_cycles(f)? {
        for h in cycle {
            let Some(g) = m.he(m.twin_of(h)?)?.face else {
                return Ok(false);
            };
            if g == f || !tol.is_perpendicular(m.face_normal(g)?, n) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// The stretch case: move the face's vertices along its normal.
fn stretch(
    m: &mut HalfEdgeMesh,
    f: FaceId,
    d: f64,
    tol: &Tolerances,
    log: &mut Vec<MeshDelta>,
) -> Result<ExtrudeResult, GeometryError> {
    let n = m.face_normal(f)?;
    let mut vertices = BTreeSet::new();
    for cycle in m.face_cycles(f)? {
        for h in cycle {
            vertices.insert(m.he(h)?.origin);
        }
    }
    let set: Vec<VertexId> = vertices.into_iter().collect();
    m.transform_vertices_inner(
        &set,
        &DAffine3::from_translation(n * d),
        TransformOptions {
            auto_fold: true,
            check_intersections: false,
        },
        tol,
        log,
    )?;
    Ok(ExtrudeResult {
        cap: m.face(f).is_some().then_some(f),
        ..ExtrudeResult::default()
    })
}

/// A curve on the face boundary: its descriptor and its member edges'
/// endpoint positions.
struct BoundaryCurve {
    descriptor: CurveDescriptor,
    members: Vec<(DVec3, DVec3)>,
}

fn extrude_generic(
    m: &mut HalfEdgeMesh,
    f: FaceId,
    d: f64,
    opts: ExtrudeOptions,
    tol: &Tolerances,
    log: &mut Vec<MeshDelta>,
) -> Result<ExtrudeResult, GeometryError> {
    let plane = m.face_ref(f)?.plane;
    let n = plane.normal;
    let w = n * d;
    let cycles = m.face_cycles(f)?;
    let rings: Vec<Vec<DVec3>> = cycles
        .iter()
        .map(|c| m.cycle_positions(c))
        .collect::<Result<_, _>>()?;
    let mut free = true;
    for &h in cycles.iter().flatten() {
        if m.he(m.twin_of(h)?)?.face.is_some() {
            free = false;
            break;
        }
    }
    if opts.create_new_start_face && !free {
        return Err(GeometryError::NotApplicable(
            "keeping the start face would put three faces on one edge".into(),
        ));
    }
    let curves = boundary_curves(m, &cycles)?;
    let base_edges: BTreeSet<EdgeId> = cycles
        .iter()
        .flatten()
        .map(|&h| m.he(h).map(|x| x.edge))
        .collect::<Result<_, _>>()?;

    // Join target: a coplanar face containing the whole cap outline.
    let cap_plane = Plane::from_point_normal(rings[0][0] + w, n);
    let join = if opts.push_through {
        landing_face(m, f, &cap_plane, &rings[0], w, tol)?
    } else {
        None
    };

    let mut result = ExtrudeResult::default();
    let reverse_base = free && plane.normal.dot(n * -d.signum()) < 0.0;
    // The half-edge of each base edge a side face will use: the one the face
    // used, unless the face stays as the base without being reversed.
    let mut side_half_edges = Vec::new();
    for &h in cycles.iter().flatten() {
        let needed = if free && !reverse_base {
            m.twin_of(h)?
        } else {
            h
        };
        side_half_edges.push((m.he(h)?.edge, needed));
    }
    if free {
        // The face stays as the base and must face away from the volume.
        if reverse_base {
            planar::reverse_faces(m, &BTreeSet::from([f]), tol, log)?;
        }
    } else {
        m.remove_face(f, log)?;
        result.removed_faces.push(f);
    }

    let pre_existing: BTreeSet<FaceId> = m.faces().map(|(id, _)| id).collect();
    let first_new = log.len();
    let mut swept: Vec<EdgeId> = Vec::new();
    let add_ring = |m: &mut HalfEdgeMesh,
                    ring: &[DVec3],
                    log: &mut Vec<MeshDelta>,
                    swept: &mut Vec<EdgeId>|
     -> Result<(), GeometryError> {
        for i in 0..ring.len() {
            let r = m.add_edge_inner(ring[i] + w, ring[(i + 1) % ring.len()] + w, tol, log)?;
            swept.extend(r.edges);
        }
        Ok(())
    };
    if join.is_some() {
        for ring in &rings {
            add_ring(m, ring, log, &mut swept)?;
        }
        // The piece of the landing face inside the outline is interior now.
        for g in faces_bounded_by(m, &rings[0], w, tol)? {
            m.remove_face(g, log)?;
            result.removed_faces.push(g);
        }
        result.through_hole = d < 0.0;
    }
    for ring in &rings {
        for &p in ring {
            let r = m.add_edge_inner(p, p + w, tol, log)?;
            swept.extend(r.edges);
        }
    }
    if join.is_none() {
        // Hole rings first. Face finding closes each one at the top; that
        // fill is removed at once (the hole stays open), so the outer ring,
        // added last, yields the cap with its holes and no orientation clash.
        for ring in rings.iter().skip(1) {
            let mark = log.len();
            add_ring(m, ring, log, &mut swept)?;
            let fresh: BTreeSet<FaceId> = log[mark..]
                .iter()
                .filter_map(|delta| match delta {
                    MeshDelta::FaceAdded { id, .. } if !pre_existing.contains(id) => Some(*id),
                    _ => None,
                })
                .collect();
            for g in faces_bounded_by(m, ring, w, tol)? {
                if fresh.contains(&g) {
                    m.remove_face(g, log)?;
                }
            }
        }
        add_ring(m, &rings[0], log, &mut swept)?;
    }
    clear_coincident_sides(m, &side_half_edges, &swept, w, tol, log)?;

    // Classify the faces this operation created.
    let mut created: Vec<FaceId> = Vec::new();
    for delta in &log[first_new..] {
        if let MeshDelta::FaceAdded { id, .. } = delta
            && !created.contains(id)
            && !pre_existing.contains(id)
        {
            created.push(*id);
        }
    }
    created.retain(|g| m.face(*g).is_some());
    let cap = if join.is_some() {
        None
    } else {
        faces_bounded_by(m, &rings[0], w, tol)?
            .into_iter()
            .find(|g| created.contains(g))
    };
    result.cap = cap;
    let mut sides: Vec<FaceId> = created.into_iter().filter(|g| Some(*g) != cap).collect();

    // Step 3: merge side faces into coplanar neighbors across their base edge.
    if opts.merge_coplanar {
        let mut kept_sides = Vec::new();
        for s in sides {
            let mut merged = false;
            for e in m.face_edges(s)? {
                if !base_edges.contains(&e) {
                    continue;
                }
                let [a, b] = m.edge_faces(e)?;
                let other = if a == Some(s) { b } else { a };
                let Some(nb) = other else {
                    continue;
                };
                if nb != s
                    && tol.is_coplanar_same_side(&m.face_ref(nb)?.plane, &m.face_ref(s)?.plane)
                {
                    m.merge_faces_inner(nb, s, tol, log)?;
                    result.merged_faces.push(s);
                    merged = true;
                    break;
                }
            }
            if !merged {
                kept_sides.push(s);
            }
        }
        sides = kept_sides;
    }
    result.side_faces = sides;

    // Curves: soften the side edges between curve segments, copy the curve
    // to the cap (docs/05 §4.3 "sides get SOFT|SMOOTH where the curve was").
    for curve in curves {
        soften_and_copy(m, &curve, w, tol, log)?;
    }
    Ok(result)
}

/// Removes faces that occupy a side strip from the wrong side (an opening that
/// reaches another face, e.g. a door down to the floor: the wall's underside
/// under the door), drops the edges that became wires, and lets face finding
/// build the side faces those faces were blocking.
fn clear_coincident_sides(
    m: &mut HalfEdgeMesh,
    side_half_edges: &[(EdgeId, crate::ids::HalfEdgeId)],
    swept: &[EdgeId],
    w: DVec3,
    tol: &Tolerances,
    log: &mut Vec<MeshDelta>,
) -> Result<(), GeometryError> {
    let mut doomed = BTreeSet::new();
    for &(e, needed) in side_half_edges {
        if m.edge(e).is_none() || m.half_edge(needed).is_none() {
            continue;
        }
        let other = m.twin_of(needed)?;
        let Some(x) = m.he(other)?.face else {
            continue;
        };
        let seg = m.edge_segment(e)?;
        let Some(side_normal) = (seg.b - seg.a).cross(w).try_normalize() else {
            continue;
        };
        let side = Plane::from_point_normal(seg.a, side_normal);
        if !tol.is_coplanar(&m.face_ref(x)?.plane, &side) {
            continue;
        }
        let frame = side.frame_at(seg.a);
        let strip: Vec<glam::DVec2> = [seg.a, seg.b, seg.b + w, seg.a + w]
            .iter()
            .map(|&p| frame.to_2d(p))
            .collect();
        let inside = m
            .face_vertices(x)?
            .into_iter()
            .map(|v| m.position(v))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .all(|p| {
                polygon::point_in_ring(frame.to_2d(p), &strip, tol) != PointInPolygon::Outside
            });
        if inside {
            doomed.insert(x);
        }
    }
    if doomed.is_empty() {
        return Ok(());
    }
    for x in doomed {
        m.remove_face(x, log)?;
    }
    let mut touched = BTreeSet::new();
    let mut seeds = Vec::new();
    let candidates: BTreeSet<EdgeId> = side_half_edges
        .iter()
        .map(|(e, _)| *e)
        .chain(swept.iter().copied())
        .collect();
    for e in candidates {
        if m.edge(e).is_none() {
            continue;
        }
        let (a, b) = m.edge_endpoints(e)?;
        if m.edge_faces(e)? == [None, None] {
            m.delete_edge(e, log)?;
            m.delete_if_isolated(a, log)?;
            m.delete_if_isolated(b, log)?;
        } else {
            touched.extend([a, b]);
            seeds.push(e);
        }
    }
    let touched: Vec<VertexId> = touched
        .into_iter()
        .filter(|v| m.vertex(*v).is_some())
        .collect();
    m.find_faces_seeded(&touched, &seeds, tol, log)?;
    Ok(())
}

/// Curves all of whose members lie on the face boundary.
fn boundary_curves(
    m: &HalfEdgeMesh,
    cycles: &[Vec<crate::ids::HalfEdgeId>],
) -> Result<Vec<BoundaryCurve>, GeometryError> {
    let edges: BTreeSet<EdgeId> = cycles
        .iter()
        .flatten()
        .map(|&h| m.he(h).map(|x| x.edge))
        .collect::<Result<_, _>>()?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for &e in &edges {
        let Some(c) = m.edge(e).and_then(|x| x.curve) else {
            continue;
        };
        if !seen.insert(c) {
            continue;
        }
        let members = m.curve_edges(c)?;
        if !members.iter().all(|x| edges.contains(x)) {
            continue;
        }
        let descriptor = m
            .curve(c)
            .cloned()
            .ok_or_else(|| GeometryError::NotFound(c.to_string()))?;
        let members = members
            .iter()
            .map(|&x| m.edge_segment(x).map(|s| (s.a, s.b)))
            .collect::<Result<_, _>>()?;
        out.push(BoundaryCurve {
            descriptor,
            members,
        });
    }
    Ok(out)
}

/// Marks the side edges at inner curve vertices `SOFT | SMOOTH` and attaches
/// a translated copy of the descriptor to the cap edges.
fn soften_and_copy(
    m: &mut HalfEdgeMesh,
    curve: &BoundaryCurve,
    w: DVec3,
    tol: &Tolerances,
    log: &mut Vec<MeshDelta>,
) -> Result<(), GeometryError> {
    let n = curve.members.len();
    for i in 0..n {
        if i + 1 == n && n < 3 {
            break;
        }
        let (a0, b0) = curve.members[i];
        let (a1, b1) = curve.members[(i + 1) % n];
        let junction = [a0, b0]
            .into_iter()
            .find(|p| tol.same_point(*p, a1) || tol.same_point(*p, b1));
        let Some(p) = junction else {
            continue;
        };
        let (Some(v), Some(top)) = (m.find_vertex_near(p, tol), m.find_vertex_near(p + w, tol))
        else {
            continue;
        };
        if let Some(e) = m.edge_between(v, top) {
            let flags = m.edge(e).map(|x| x.flags).unwrap_or_default();
            m.set_edge_flags(e, flags | EdgeFlags::SOFT | EdgeFlags::SMOOTH, log)?;
        }
    }
    let mut cap_members = Vec::with_capacity(n);
    for &(a, b) in &curve.members {
        let (Some(va), Some(vb)) = (
            m.find_vertex_near(a + w, tol),
            m.find_vertex_near(b + w, tol),
        ) else {
            return Ok(());
        };
        let Some(e) = m.edge_between(va, vb) else {
            return Ok(());
        };
        if m.edge(e).is_some_and(|x| x.curve.is_some()) {
            return Ok(());
        }
        cap_members.push(e);
    }
    let descriptor = translated(&curve.descriptor, w);
    m.add_curve_inner(descriptor, &cap_members, false, log)?;
    Ok(())
}

/// A curve descriptor moved by `w`.
fn translated(c: &CurveDescriptor, w: DVec3) -> CurveDescriptor {
    let mut c = c.clone();
    match &mut c {
        CurveDescriptor::Arc { center, .. }
        | CurveDescriptor::Circle { center, .. }
        | CurveDescriptor::Polygon { center, .. } => *center += w,
        CurveDescriptor::Freehand { points } => {
            for p in points {
                *p += w;
            }
        }
    }
    c
}

/// A face other than `f`, coplanar with `cap_plane`, whose polygon contains
/// every cap outline point (`outer + w`).
fn landing_face(
    m: &HalfEdgeMesh,
    f: FaceId,
    cap_plane: &Plane,
    outer: &[DVec3],
    w: DVec3,
    tol: &Tolerances,
) -> Result<Option<FaceId>, GeometryError> {
    let points: Vec<DVec3> = outer.iter().map(|&p| p + w).collect();
    let bbox = crate::math::Aabb::from_points(points.iter().copied()).expanded(tol.length);
    for g in m.coplanar_faces_near(cap_plane, &bbox, tol)? {
        if g == f {
            continue;
        }
        let frame = m.face_ref(g)?.plane.frame();
        let (ring, holes) = planar::face_polygon(m, g, &frame)?;
        let hole_refs: Vec<&[glam::DVec2]> = holes.iter().map(Vec::as_slice).collect();
        let inside = points.iter().all(|&p| {
            polygon::point_in_polygon(frame.to_2d(p), &ring, &hole_refs, tol)
                != PointInPolygon::Outside
        });
        if inside {
            return Ok(Some(g));
        }
    }
    Ok(None)
}

/// Faces whose outer loop runs through exactly the points `outer + w`.
fn faces_bounded_by(
    m: &HalfEdgeMesh,
    outer: &[DVec3],
    w: DVec3,
    tol: &Tolerances,
) -> Result<Vec<FaceId>, GeometryError> {
    let mut wanted: BTreeSet<VertexId> = BTreeSet::new();
    for &p in outer {
        match m.find_vertex_near(p + w, tol) {
            Some(v) => {
                wanted.insert(v);
            }
            None => return Ok(Vec::new()),
        }
    }
    let Some(&first) = wanted.iter().next() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for &h in m.vertex_star(first) {
        for side in [h, m.twin_of(h)?] {
            let Some(g) = m.he(side)?.face else {
                continue;
            };
            if out.contains(&g) {
                continue;
            }
            let loop_vertices: BTreeSet<VertexId> = m.face_vertices(g)?.into_iter().collect();
            if wanted.is_subset(&loop_vertices)
                && loop_vertices.iter().all(|v| {
                    wanted.contains(v)
                        || m.position(*v).is_ok_and(|p| {
                            // Extra vertices only where existing edges split the outline.
                            outer.iter().any(|&q| (p - (q + w)).length() <= tol.length)
                                || on_outline(p, outer, w, tol)
                        })
                })
            {
                out.push(g);
            }
        }
    }
    Ok(out)
}

/// `p` lies on the translated outline polygon.
fn on_outline(p: DVec3, outer: &[DVec3], w: DVec3, tol: &Tolerances) -> bool {
    (0..outer.len()).any(|i| {
        let s = crate::math::Segment::new(outer[i] + w, outer[(i + 1) % outer.len()] + w);
        tol.point_on_segment(p, &s)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::NoDeltas;

    fn tol() -> Tolerances {
        Tolerances::default()
    }

    fn p(x: f64, y: f64, z: f64) -> DVec3 {
        DVec3::new(x, y, z)
    }

    fn rect(m: &mut HalfEdgeMesh, pts: [DVec3; 4]) {
        for i in 0..4 {
            m.add_edge(pts[i], pts[(i + 1) % 4], &tol(), &mut NoDeltas)
                .unwrap();
        }
    }

    fn face_at(m: &HalfEdgeMesh, normal: DVec3, point: DVec3) -> FaceId {
        m.faces()
            .find(|(_, f)| {
                tol().is_parallel(f.plane.normal, normal) && tol().point_on_plane(&f.plane, point)
            })
            .map(|(id, _)| id)
            .unwrap()
    }

    fn assert_valid(m: &HalfEdgeMesh) {
        let v = m.validate(&tol());
        assert!(v.is_empty(), "{v:#?}");
    }

    #[test]
    fn pulling_a_lone_rectangle_makes_a_closed_box() {
        let mut m = HalfEdgeMesh::new();
        rect(
            &mut m,
            [
                p(0.0, 0.0, 0.0),
                p(100.0, 0.0, 0.0),
                p(100.0, 60.0, 0.0),
                p(0.0, 60.0, 0.0),
            ],
        );
        let f = m.faces().next().unwrap().0;
        let r = extrude_face(
            &mut m,
            f,
            Length::mm(30.0),
            ExtrudeOptions::default(),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
        assert_eq!(m.face_count(), 6);
        assert_eq!(r.side_faces.len(), 4);
        assert!(r.cap.is_some() && r.removed_faces.is_empty());
        assert!(m.is_solid());
        assert!(
            (m.volume().unwrap() - 180_000.0).abs() < 1e-6,
            "outward normals"
        );
        assert_valid(&m);
    }

    #[test]
    fn pushing_a_lone_rectangle_down_also_faces_outward() {
        let mut m = HalfEdgeMesh::new();
        rect(
            &mut m,
            [
                p(0.0, 0.0, 0.0),
                p(10.0, 0.0, 0.0),
                p(10.0, 10.0, 0.0),
                p(0.0, 10.0, 0.0),
            ],
        );
        let f = m.faces().next().unwrap().0;
        extrude_face(
            &mut m,
            f,
            Length::mm(-5.0),
            ExtrudeOptions::default(),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
        assert!(m.is_solid());
        assert!((m.volume().unwrap() - 500.0).abs() < 1e-9);
    }

    fn unit_box(m: &mut HalfEdgeMesh, size: f64) {
        rect(
            m,
            [
                p(0.0, 0.0, 0.0),
                p(size, 0.0, 0.0),
                p(size, size, 0.0),
                p(0.0, size, 0.0),
            ],
        );
        let f = m.faces().next().unwrap().0;
        extrude_face(
            m,
            f,
            Length::mm(size),
            ExtrudeOptions::default(),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
    }

    #[test]
    fn box_top_stretches_both_ways() {
        let mut m = HalfEdgeMesh::new();
        unit_box(&mut m, 10.0);
        let top = face_at(&m, DVec3::Z, p(0.0, 0.0, 10.0));
        let r = extrude_face(
            &mut m,
            top,
            Length::mm(5.0),
            ExtrudeOptions::default(),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
        assert_eq!(r.cap, Some(top));
        assert_eq!(m.face_count(), 6);
        assert!((m.volume().unwrap() - 1500.0).abs() < 1e-6);
        extrude_face(
            &mut m,
            top,
            Length::mm(-12.0),
            ExtrudeOptions::default(),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
        assert!((m.volume().unwrap() - 300.0).abs() < 1e-6);
        assert_valid(&m);
    }

    #[test]
    fn rectangle_on_a_box_top_pulls_into_a_second_box() {
        let mut m = HalfEdgeMesh::new();
        unit_box(&mut m, 100.0);
        rect(
            &mut m,
            [
                p(20.0, 20.0, 100.0),
                p(40.0, 20.0, 100.0),
                p(40.0, 40.0, 100.0),
                p(20.0, 40.0, 100.0),
            ],
        );
        let inner = m
            .faces()
            .find(|(id, _)| (m.face_area(*id).unwrap() - 400.0).abs() < 1e-9)
            .map(|(id, _)| id)
            .unwrap();
        let r = extrude_face(
            &mut m,
            inner,
            Length::mm(50.0),
            ExtrudeOptions::default(),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
        assert_eq!(r.side_faces.len(), 4);
        assert_eq!(r.removed_faces, vec![inner]);
        assert!(m.is_solid());
        assert!((m.volume().unwrap() - (1e6 + 400.0 * 50.0)).abs() < 1e-3);
        assert_valid(&m);
    }

    #[test]
    fn pushing_an_opening_through_a_wall_makes_a_hole() {
        let mut m = HalfEdgeMesh::new();
        // Wall: 1000 long (x), 200 thick (y), 500 high (z).
        rect(
            &mut m,
            [
                p(0.0, 0.0, 0.0),
                p(1000.0, 0.0, 0.0),
                p(1000.0, 200.0, 0.0),
                p(0.0, 200.0, 0.0),
            ],
        );
        let base = m.faces().next().unwrap().0;
        extrude_face(
            &mut m,
            base,
            Length::mm(500.0),
            ExtrudeOptions::default(),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
        let front = face_at(&m, DVec3::Y, p(0.0, 0.0, 0.0));
        let outward = m.face_normal(front).unwrap();
        assert!(outward.y < 0.0);
        rect(
            &mut m,
            [
                p(300.0, 0.0, 100.0),
                p(500.0, 0.0, 100.0),
                p(500.0, 0.0, 400.0),
                p(300.0, 0.0, 400.0),
            ],
        );
        let opening = m
            .faces()
            .find(|(id, f)| {
                tol().is_parallel(f.plane.normal, DVec3::Y)
                    && (m.face_area(*id).unwrap() - 60_000.0).abs() < 1e-6
            })
            .map(|(id, _)| id)
            .unwrap();
        let r = extrude_face(
            &mut m,
            opening,
            Length::mm(-200.0),
            ExtrudeOptions::default(),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
        assert!(r.through_hole);
        assert!(r.cap.is_none());
        assert_eq!(r.side_faces.len(), 4);
        assert!(m.is_solid());
        let wall = 1000.0 * 200.0 * 500.0;
        let hole = 200.0 * 300.0 * 200.0;
        assert!((m.volume().unwrap() - (wall - hole)).abs() < 1e-3);
        assert_valid(&m);
    }

    #[test]
    fn pulling_a_circle_gives_a_cylinder_with_smooth_sides() {
        let mut m = HalfEdgeMesh::new();
        let n = 16;
        let pts: Vec<DVec3> = (0..n)
            .map(|i| {
                let a = std::f64::consts::TAU * f64::from(i) / f64::from(n);
                p(50.0 * libm::cos(a), 50.0 * libm::sin(a), 0.0)
            })
            .collect();
        let mut members = Vec::new();
        for i in 0..pts.len() {
            let r = m
                .add_edge(pts[i], pts[(i + 1) % pts.len()], &tol(), &mut NoDeltas)
                .unwrap();
            members.extend(r.edges);
        }
        m.add_curve(
            CurveDescriptor::Circle {
                center: DVec3::ZERO,
                normal: DVec3::Z,
                radius: 50.0,
                segments: n,
            },
            &members,
            &mut NoDeltas,
        )
        .unwrap();
        let f = m.faces().next().unwrap().0;
        extrude_face(
            &mut m,
            f,
            Length::mm(100.0),
            ExtrudeOptions::default(),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
        assert!(m.is_solid());
        let smooth = m
            .edges()
            .filter(|(_, e)| e.flags.contains(EdgeFlags::SOFT | EdgeFlags::SMOOTH))
            .count();
        assert_eq!(smooth, n as usize);
        assert_eq!(m.curve_count(), 2, "the cap carries a copy of the circle");
        assert_valid(&m);
    }

    #[test]
    fn a_face_with_a_hole_gets_side_faces_for_the_hole() {
        let mut m = HalfEdgeMesh::new();
        rect(
            &mut m,
            [
                p(0.0, 0.0, 0.0),
                p(100.0, 0.0, 0.0),
                p(100.0, 100.0, 0.0),
                p(0.0, 100.0, 0.0),
            ],
        );
        rect(
            &mut m,
            [
                p(40.0, 40.0, 0.0),
                p(60.0, 40.0, 0.0),
                p(60.0, 60.0, 0.0),
                p(40.0, 60.0, 0.0),
            ],
        );
        let inner = m
            .faces()
            .find(|(id, _)| (m.face_area(*id).unwrap() - 400.0).abs() < 1e-9)
            .map(|(id, _)| id)
            .unwrap();
        m.erase_face(inner, false, &tol(), &mut NoDeltas).unwrap();
        let ring = m.faces().next().unwrap().0;
        let r = extrude_face(
            &mut m,
            ring,
            Length::mm(10.0),
            ExtrudeOptions::default(),
            &tol(),
            &mut NoDeltas,
        )
        .unwrap();
        assert_eq!(r.side_faces.len(), 8);
        assert!(m.is_solid());
        assert!((m.volume().unwrap() - (10_000.0 - 400.0) * 10.0).abs() < 1e-6);
        assert_valid(&m);
    }

    #[test]
    fn zero_distance_is_refused() {
        let mut m = HalfEdgeMesh::new();
        rect(
            &mut m,
            [
                p(0.0, 0.0, 0.0),
                p(1.0, 0.0, 0.0),
                p(1.0, 1.0, 0.0),
                p(0.0, 1.0, 0.0),
            ],
        );
        let f = m.faces().next().unwrap().0;
        assert!(
            extrude_face(
                &mut m,
                f,
                Length::mm(0.0),
                ExtrudeOptions::default(),
                &tol(),
                &mut NoDeltas
            )
            .is_err()
        );
    }
}
