//! Implements docs/03-geometry-kernel.md §5 (tessellation): polygon-with-holes
//! → triangles via ear clipping (`earcutr`), per-vertex normals honoring `SOFT`
//! edges, and the per-face cache keyed by face id and vertex positions.
//!
//! Owner key: `mesh`.

#![warn(unused_variables, dead_code)]

use std::collections::BTreeMap;

use glam::{DVec2, DVec3};

use crate::geom::{EdgeFlags, GeometryError, HalfEdgeMesh};
use crate::ids::{FaceId, VertexId};
use crate::tolerance::Tolerances;

/// Triangulation of one face. `triangles` index into `vertices`/`positions`,
/// which list the outer loop first and then each hole loop in order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FaceTessellation {
    /// Loop vertices (outer, then holes).
    pub vertices: Vec<VertexId>,
    /// Positions matching `vertices`.
    pub positions: Vec<DVec3>,
    /// Triangles as indices into `vertices`, front-facing counter-clockwise.
    pub triangles: Vec<[u32; 3]>,
}

/// Cache key: the face id plus a hash of its loop vertex positions
/// (docs/03 §5 "keyed by (face_gen, vertex positions hash)").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TessellationKey {
    /// Face.
    pub face: FaceId,
    /// Deterministic hash of the loop positions' bit patterns.
    pub positions_hash: u64,
}

/// Per-container tessellation cache (docs/03 §5).
#[derive(Debug, Default)]
pub struct TessellationCache {
    entries: BTreeMap<FaceId, (u64, FaceTessellation)>,
}

/// Triangulates a face: project loops to the plane frame, ear-clip with holes,
/// map indices back (docs/03 §5).
pub fn tessellate_face(
    mesh: &HalfEdgeMesh,
    f: FaceId,
    tol: &Tolerances,
) -> Result<FaceTessellation, GeometryError> {
    let face = mesh
        .face(f)
        .ok_or_else(|| GeometryError::NotFound(f.to_string()))?;
    let frame = face.plane.frame();

    let outer_vertices = mesh.loop_vertices(face.outer)?;
    let mut vertices: Vec<VertexId> = Vec::new();
    let mut positions: Vec<DVec3> = Vec::new();
    let mut outer2d: Vec<DVec2> = Vec::with_capacity(outer_vertices.len());
    for v in &outer_vertices {
        let p = mesh.position(*v)?;
        vertices.push(*v);
        positions.push(p);
        outer2d.push(frame.to_2d(p));
    }

    let mut holes2d: Vec<Vec<DVec2>> = Vec::with_capacity(face.holes.len());
    for hole in &face.holes {
        let hole_vertices = mesh.loop_vertices(*hole)?;
        let mut ring = Vec::with_capacity(hole_vertices.len());
        for v in &hole_vertices {
            let p = mesh.position(*v)?;
            vertices.push(*v);
            positions.push(p);
            ring.push(frame.to_2d(p));
        }
        holes2d.push(ring);
    }

    // Front-facing counter-clockwise in the plane frame (whose normal is the
    // face normal); zero-area slivers from collinear boundary vertices are
    // dropped (docs/03 §2 degeneracy threshold).
    let all2d: Vec<DVec2> = outer2d
        .iter()
        .chain(holes2d.iter().flatten())
        .copied()
        .collect();
    let triangles = tessellate_polygon_2d(&outer2d, &holes2d)?
        .into_iter()
        .filter_map(|mut t| {
            let [a, b, c] = t.map(|i| all2d[i as usize]);
            let twice_area = (b - a).perp_dot(c - a);
            if twice_area.abs() * 0.5 <= tol.length_sq {
                return None;
            }
            if twice_area < 0.0 {
                t.swap(1, 2);
            }
            Some(t)
        })
        .collect();
    Ok(FaceTessellation {
        vertices,
        positions,
        triangles,
    })
}

/// Ear clipping of a 2D polygon with holes; returned indices address the
/// concatenation `outer ++ holes[0] ++ holes[1] …` (docs/03 §5, `earcutr`).
pub fn tessellate_polygon_2d(
    outer: &[DVec2],
    holes: &[Vec<DVec2>],
) -> Result<Vec<[u32; 3]>, GeometryError> {
    if outer.len() < 3 {
        return Err(GeometryError::Degenerate(
            "face outer loop has fewer than 3 vertices".into(),
        ));
    }
    let mut data: Vec<f64> =
        Vec::with_capacity((outer.len() + holes.iter().map(Vec::len).sum::<usize>()) * 2);
    for p in outer {
        data.push(p.x);
        data.push(p.y);
    }
    let mut hole_indices: Vec<usize> = Vec::with_capacity(holes.len());
    let mut cursor = outer.len();
    for hole in holes {
        hole_indices.push(cursor);
        for p in hole {
            data.push(p.x);
            data.push(p.y);
        }
        cursor += hole.len();
    }
    let flat = earcutr::earcut(&data, &hole_indices, 2)
        .map_err(|e| GeometryError::Degenerate(format!("ear clipping failed: {e:?}")))?;
    Ok(flat
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| [c[0] as u32, c[1] as u32, c[2] as u32])
        .collect())
}

/// Per-vertex shading normals for a tessellated face: the face normal, except
/// across `SOFT` edges where adjacent face normals are angle-weighted averaged
/// (docs/03 §5, docs/06 §2). At each vertex the faces reachable from `f`
/// through soft edges incident to the vertex form the smoothing group; each
/// contributes its normal weighted by its corner angle at the vertex.
pub fn vertex_normals(
    mesh: &HalfEdgeMesh,
    f: FaceId,
    tess: &FaceTessellation,
) -> Result<Vec<DVec3>, GeometryError> {
    let n = mesh.face_normal(f)?;
    tess.vertices
        .iter()
        .map(|&v| smooth_normal(mesh, f, v, n))
        .collect()
}

/// The shading normal of face `f` at vertex `v` (see [`vertex_normals`]).
fn smooth_normal(
    mesh: &HalfEdgeMesh,
    f: FaceId,
    v: VertexId,
    face_normal: DVec3,
) -> Result<DVec3, GeometryError> {
    let mut soft = Vec::new();
    for &h in mesh.vertex_star(v) {
        let e = mesh.he(h)?.edge;
        if mesh
            .edge(e)
            .is_some_and(|x| x.flags.contains(EdgeFlags::SOFT))
        {
            soft.push(mesh.edge_faces(e)?);
        }
    }
    if soft.is_empty() {
        return Ok(face_normal);
    }
    let mut group = vec![f];
    let mut grown = true;
    while grown {
        grown = false;
        for &[a, b] in &soft {
            if let (Some(a), Some(b)) = (a, b) {
                for (x, y) in [(a, b), (b, a)] {
                    if group.contains(&x) && !group.contains(&y) {
                        group.push(y);
                        grown = true;
                    }
                }
            }
        }
    }
    if group.len() == 1 {
        return Ok(face_normal);
    }
    let mut sum = DVec3::ZERO;
    for g in group {
        sum += mesh.face_normal(g)? * corner_angle(mesh, g, v)?;
    }
    Ok(sum.try_normalize().unwrap_or(face_normal))
}

/// The interior angle of face `f` at vertex `v` (0 when `v` is not on `f`).
fn corner_angle(mesh: &HalfEdgeMesh, f: FaceId, v: VertexId) -> Result<f64, GeometryError> {
    for cycle in mesh.face_cycles(f)? {
        for h in cycle {
            let he = mesh.he(h)?;
            if he.origin != v {
                continue;
            }
            let p = mesh.position(v)?;
            let a = mesh.position(mesh.he(he.prev)?.origin)? - p;
            let b = mesh.position(mesh.dest(h)?)? - p;
            return Ok(libm::atan2(a.cross(b).length(), a.dot(b)));
        }
    }
    Ok(0.0)
}

/// Deterministic hash of the face's loop positions (bit patterns, loop order),
/// identical on every platform.
pub fn positions_hash(mesh: &HalfEdgeMesh, f: FaceId) -> Result<u64, GeometryError> {
    let face = mesh
        .face(f)
        .ok_or_else(|| GeometryError::NotFound(f.to_string()))?;
    // FNV-1a over the little-endian bit patterns of every loop coordinate.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mix = |value: f64, h: &mut u64| {
        // Normalize -0.0 to 0.0 so equal geometry hashes equally.
        let bits = if value == 0.0 { 0u64 } else { value.to_bits() };
        for byte in bits.to_le_bytes() {
            *h ^= u64::from(byte);
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    let mut hash_loop = |l| -> Result<(), GeometryError> {
        for v in mesh.loop_vertices(l)? {
            let p = mesh.position(v)?;
            mix(p.x, &mut h);
            mix(p.y, &mut h);
            mix(p.z, &mut h);
        }
        // Loop separator.
        mix(f64::from_bits(0xdead_beef), &mut h);
        Ok(())
    };
    hash_loop(face.outer)?;
    for hole in &face.holes {
        hash_loop(*hole)?;
    }
    Ok(h)
}

impl TessellationCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached tessellation when the positions hash matches,
    /// otherwise tessellates and stores.
    pub fn get_or_tessellate(
        &mut self,
        mesh: &HalfEdgeMesh,
        f: FaceId,
        tol: &Tolerances,
    ) -> Result<&FaceTessellation, GeometryError> {
        let hash = positions_hash(mesh, f)?;
        let stale = self
            .entries
            .get(&f)
            .map(|(h, _)| *h != hash)
            .unwrap_or(true);
        if stale {
            let tess = tessellate_face(mesh, f, tol)?;
            self.entries.insert(f, (hash, tess));
        }
        self.entries
            .get(&f)
            .map(|(_, tess)| tess)
            .ok_or_else(|| GeometryError::Invariant(format!("{f} missing from the cache")))
    }

    /// Drops one face's entry.
    pub fn invalidate(&mut self, f: FaceId) {
        self.entries.remove(&f);
    }

    /// Drops everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Number of cached faces.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_tessellates_into_two_triangles() {
        let outer = vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(10.0, 0.0),
            DVec2::new(10.0, 10.0),
            DVec2::new(0.0, 10.0),
        ];
        let tris = tessellate_polygon_2d(&outer, &[]).unwrap();
        assert_eq!(tris.len(), 2);
    }

    #[test]
    fn square_with_square_hole_tessellates() {
        let outer = vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(10.0, 0.0),
            DVec2::new(10.0, 10.0),
            DVec2::new(0.0, 10.0),
        ];
        let hole = vec![
            DVec2::new(3.0, 3.0),
            DVec2::new(3.0, 7.0),
            DVec2::new(7.0, 7.0),
            DVec2::new(7.0, 3.0),
        ];
        let tris = tessellate_polygon_2d(&outer, &[hole]).unwrap();
        // 8 boundary vertices with one hole → 8 triangles.
        assert_eq!(tris.len(), 8);
    }

    #[test]
    fn degenerate_polygon_errors() {
        let outer = vec![DVec2::new(0.0, 0.0), DVec2::new(1.0, 0.0)];
        assert!(tessellate_polygon_2d(&outer, &[]).is_err());
    }
}
