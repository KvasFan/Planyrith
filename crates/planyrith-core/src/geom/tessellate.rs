//! Implements docs/03-geometry-kernel.md §5 (tessellation): polygon-with-holes
//! → triangles via ear clipping (`earcutr`), per-vertex normals honoring `SOFT`
//! edges, and the per-face cache keyed by face id and vertex positions.
//!
//! Owner key: `mesh`.

use std::collections::BTreeMap;

use glam::{DVec2, DVec3};

use crate::geom::{GeometryError, HalfEdgeMesh};
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
    todo!("docs/03 §5 tessellate_face")
}

/// Ear clipping of a 2D polygon with holes; returned indices address the
/// concatenation `outer ++ holes[0] ++ holes[1] …` (docs/03 §5, `earcutr`).
pub fn tessellate_polygon_2d(
    outer: &[DVec2],
    holes: &[Vec<DVec2>],
) -> Result<Vec<[u32; 3]>, GeometryError> {
    todo!("docs/03 §5 ear clipping via earcutr")
}

/// Per-vertex shading normals for a tessellated face: the face normal, except
/// across `SOFT` edges where adjacent face normals are angle-weighted averaged
/// (docs/03 §5, docs/06 §2).
pub fn vertex_normals(
    mesh: &HalfEdgeMesh,
    f: FaceId,
    tess: &FaceTessellation,
) -> Result<Vec<DVec3>, GeometryError> {
    todo!("docs/03 §5 soft-edge vertex normals")
}

/// Deterministic hash of the face's loop positions (bit patterns, loop order),
/// identical on every platform.
pub fn positions_hash(mesh: &HalfEdgeMesh, f: FaceId) -> Result<u64, GeometryError> {
    todo!("docs/03 §5 positions hash")
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
        todo!("docs/03 §5 tessellation cache lookup")
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
