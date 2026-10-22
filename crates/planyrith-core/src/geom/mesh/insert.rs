//! Implements the direct-insertion half of docs/05-modeling-tools.md §5.3
//! `explode` ("interior geometry is inserted directly"): appending a piece of
//! geometry, given in indexed form, that touches nothing already in the mesh.
//! Pieces that touch existing geometry need `add_edge` semantics instead
//! (docs/05 Decisions, groups and components).

use glam::{DAffine3, DVec2, DVec3};

use super::{DirectedEdgeIndex, HalfEdgeMesh, IndexedIds, IndexedMesh, broken, indexed};
use crate::geom::{DeltaSink, GeometryError};
use crate::math::intersect::{segment_plane, segment_segment};
use crate::math::{Aabb, Plane, Segment, polygon};
use crate::tolerance::Tolerances;

impl HalfEdgeMesh {
    /// Appends `piece` unchanged (vertices, edges with their flags, curves,
    /// faces with holes, planes and sides) when it touches no vertex, edge
    /// or face of this mesh, and returns the new ids by local index.
    /// Returns `Ok(None)` and changes nothing when it touches existing
    /// geometry. Atomic.
    pub fn insert_disjoint(
        &mut self,
        piece: &IndexedMesh,
        tol: &Tolerances,
        sink: &mut dyn DeltaSink,
    ) -> Result<Option<IndexedIds>, GeometryError> {
        if self.touches(piece, tol)? {
            return Ok(None);
        }
        self.transact(tol, sink, |m, d| indexed::append_indexed(m, piece, tol, d))
            .map(Some)
    }

    /// `true` when some element of `piece` meets an element of this mesh:
    /// coincident vertices, edges in contact, or an edge meeting a face.
    fn touches(&self, piece: &IndexedMesh, tol: &Tolerances) -> Result<bool, GeometryError> {
        if self.is_empty() || piece.vertices.is_empty() {
            return Ok(false);
        }
        let piece_box = Aabb::from_points(piece.vertices.iter().copied()).expanded(tol.length);
        if !self.aabb().expanded(tol.length).intersects(&piece_box) {
            return Ok(false);
        }
        if piece
            .vertices
            .iter()
            .any(|&p| self.find_vertex_near(p, tol).is_some())
        {
            return Ok(true);
        }

        // This mesh's faces near the piece, as planar polygons.
        let mut faces = Vec::new();
        for (f, _) in self.faces() {
            let bbox = self.face_aabb(f)?;
            if bbox.expanded(tol.length).intersects(&piece_box) {
                let rings = self
                    .face_loops(f)?
                    .into_iter()
                    .map(|l| self.loop_positions(l))
                    .collect::<Result<Vec<_>, _>>()?;
                faces.push(PolygonFace::new(self.face_ref(f)?.plane, rings, bbox));
            }
        }

        let point = |i: u32| {
            piece
                .vertices
                .get(i as usize)
                .copied()
                .ok_or_else(|| broken(format!("vertex index {i} out of range")))
        };
        for edge in &piece.edges {
            let s = Segment::new(point(edge.a)?, point(edge.b)?);
            let bbox = s.aabb().expanded(tol.length);
            for g in self.edges_near(&bbox)? {
                if !segment_segment(&s, &self.edge_segment(g)?, tol).is_none() {
                    return Ok(true);
                }
            }
            if faces
                .iter()
                .any(|face| face.bbox.intersects(&bbox) && face.meets(&s, tol))
            {
                return Ok(true);
            }
        }

        for face in &piece.faces {
            let mut rings = Vec::with_capacity(face.loops.len());
            for lp in &face.loops {
                let mut ring = Vec::with_capacity(lp.len());
                for de in lp {
                    let edge = piece
                        .edges
                        .get(de.index() as usize)
                        .ok_or_else(|| broken(format!("edge index {} out of range", de.index())))?;
                    ring.push(point(if de.is_reversed() { edge.b } else { edge.a })?);
                }
                rings.push(ring);
            }
            let Some(outer) = rings.first() else {
                continue;
            };
            let plane = match face.plane {
                Some(plane) => plane,
                None => Plane::from_polygon(outer, tol).ok_or_else(|| {
                    GeometryError::Degenerate("face outer loop has zero area".into())
                })?,
            };
            let bbox = Aabb::from_points(outer.iter().copied()).expanded(tol.length);
            let polygon = PolygonFace::new(plane, rings, bbox);
            for g in self.edges_near(&bbox)? {
                if polygon.meets(&self.edge_segment(g)?, tol) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

/// A planar face as world-space rings (outer first) with its bounds.
struct PolygonFace {
    plane: Plane,
    rings: Vec<Vec<DVec3>>,
    bbox: Aabb,
}

impl PolygonFace {
    fn new(plane: Plane, rings: Vec<Vec<DVec3>>, bbox: Aabb) -> Self {
        PolygonFace { plane, rings, bbox }
    }

    /// `p` (on the plane) lies inside the face or on its boundary.
    fn contains(&self, p: DVec3, tol: &Tolerances) -> bool {
        let frame = self.plane.frame();
        let rings: Vec<Vec<DVec2>> = self.rings.iter().map(|r| frame.project_all(r)).collect();
        let Some((outer, holes)) = rings.split_first() else {
            return false;
        };
        let holes: Vec<&[DVec2]> = holes.iter().map(Vec::as_slice).collect();
        polygon::point_in_polygon(frame.to_2d(p), outer, &holes, tol).is_inside_or_on()
    }

    /// The segment meets the face: crosses or touches it, or lies in its
    /// plane over (part of) it.
    fn meets(&self, s: &Segment, tol: &Tolerances) -> bool {
        let a_on = tol.point_on_plane(&self.plane, s.a);
        let b_on = tol.point_on_plane(&self.plane, s.b);
        if a_on && b_on {
            if self.contains(s.a, tol) || self.contains(s.b, tol) {
                return true;
            }
            // Both ends outside: it meets the face only by crossing a ring.
            return self.rings.iter().any(|ring| {
                (0..ring.len()).any(|i| {
                    let side = Segment::new(ring[i], ring[(i + 1) % ring.len()]);
                    !segment_segment(s, &side, tol).is_none()
                })
            });
        }
        match segment_plane(s, &self.plane, tol) {
            Some((_, p)) => self.contains(p, tol),
            None => false,
        }
    }
}

/// `piece` mapped through `t`: vertices, face planes and curve descriptors
/// move; under a mirroring transform every loop is reversed so each face
/// keeps its front side. Curves that `t` does not keep circular are dropped
/// (their edges become plain, docs/03 §3.3).
pub fn transform_indexed(piece: &IndexedMesh, t: &DAffine3) -> IndexedMesh {
    let mirrored = t.matrix3.determinant() < 0.0;
    let mut out = piece.clone();
    for p in &mut out.vertices {
        *p = t.transform_point3(*p);
    }
    for face in &mut out.faces {
        face.plane = face.plane.map(|plane| plane.transformed(t));
        if mirrored {
            for lp in &mut face.loops {
                lp.reverse();
                for de in lp.iter_mut() {
                    *de = if de.is_reversed() {
                        DirectedEdgeIndex::forward(de.index())
                    } else {
                        DirectedEdgeIndex::reversed(de.index())
                    };
                }
            }
        }
    }
    let kept: Vec<Option<super::CurveDescriptor>> =
        piece.curves.iter().map(|c| c.transformed(t)).collect();
    // Renumber the curves that survive.
    let mut index = Vec::with_capacity(kept.len());
    out.curves = Vec::new();
    for c in kept {
        index.push(c.map(|c| {
            out.curves.push(c);
            (out.curves.len() - 1) as u32
        }));
    }
    for edge in &mut out.edges {
        edge.curve = edge
            .curve
            .and_then(|c| index.get(c as usize).copied().flatten());
    }
    out
}
