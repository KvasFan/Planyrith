//! Implements docs/06-rendering-and-viewport.md §2 (render data extraction)
//! and the incremental cache of docs/01 §5: GPU-agnostic `RenderMesh` buffers
//! per `Entities` container (plain `Vec<f32>`/`Vec<u32>`), coordinates relative
//! to the container origin, smooth normals across `SOFT` edges.
//!
//! Owner key: `later` (milestone 0.2; depends on `geom::tessellate`).

use std::collections::BTreeMap;
use std::ops::Range;

use crate::commands::transaction::Delta;
use crate::document::{ContainerRef, Document, Entities, FrontOrBack, Materials};
use crate::geom::GeometryError;
use crate::ids::MaterialId;
use crate::math::Aabb;
use crate::tolerance::Tolerances;

/// Bits of `EdgeBuffers::flags` (docs/06 §2).
pub mod edge_flag_bits {
    /// Silhouette/profile edge (computed per view by the renderer; reserved here).
    pub const PROFILE: u8 = 0b0000_0001;
    /// `EdgeFlags::SOFT`.
    pub const SOFT: u8 = 0b0000_0010;
    /// `EdgeFlags::SMOOTH`.
    pub const SMOOTH: u8 = 0b0000_0100;
    /// `EdgeFlags::HIDDEN`.
    pub const HIDDEN: u8 = 0b0000_1000;
    /// Edge between two segments of one curve descriptor.
    pub const CURVE_SEAM: u8 = 0b0001_0000;
}

/// Material resolution per face side (docs/06 §2 `MaterialKey`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MaterialKey {
    /// `None` = the document default for `side`.
    pub material: Option<MaterialId>,
    /// Which side of the face.
    pub side: FrontOrBack,
}

/// Line buffers (docs/06 §2 `EdgeBuffers`): two positions per edge.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EdgeBuffers {
    /// Endpoint positions, two per edge.
    pub positions: Vec<[f32; 3]>,
    /// Local edge index per *edge* (for picking/highlight).
    pub edge_ids: Vec<u32>,
    /// Flag bits per edge ([`edge_flag_bits`]).
    pub flags: Vec<u8>,
}

/// Extracted buffers for one container (docs/06 §2 `RenderMesh`). Back faces
/// are emitted as separate index ranges with flipped winding.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RenderMesh {
    /// Vertex positions (container-local, f32).
    pub positions: Vec<[f32; 3]>,
    /// Shading normals.
    pub normals: Vec<[f32; 3]>,
    /// Texture coordinates.
    pub uvs: Vec<[f32; 2]>,
    /// Local face index per vertex (picking/highlight).
    pub face_ids: Vec<u32>,
    /// Triangle indices into the vertex arrays.
    pub indices: Vec<u32>,
    /// Index ranges grouped by material and side.
    pub indices_by_material: Vec<(MaterialKey, Range<u32>)>,
    /// Edge lines.
    pub edges: EdgeBuffers,
    /// Local bounds.
    pub bounds: Aabb,
    /// Local face index → `FaceId` lookup (parallel to `face_ids` values).
    pub face_lookup: Vec<crate::ids::FaceId>,
    /// Local edge index → `EdgeId` lookup.
    pub edge_lookup: Vec<crate::ids::EdgeId>,
}

/// Extraction options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractOptions {
    /// Include hidden edges/faces (drawn dashed by "show hidden geometry").
    pub include_hidden: bool,
    /// Average normals across `SOFT` edges (docs/03 §5).
    pub smooth_normals: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            include_hidden: false,
            smooth_normals: true,
        }
    }
}

/// Extracts the buffers of one container (docs/06 §2). Deterministic: faces
/// and edges in slot order, material ranges sorted by key.
pub fn extract(
    entities: &Entities,
    materials: &Materials,
    opts: ExtractOptions,
    tol: &Tolerances,
) -> Result<RenderMesh, GeometryError> {
    todo!("docs/06 §2 render mesh extraction")
}

/// Per-container cache invalidated by deltas (docs/01 §5).
#[derive(Debug, Default)]
pub struct RenderMeshCache {
    entries: BTreeMap<ContainerRef, (u64, RenderMesh)>,
    generation: u64,
}

impl RenderMeshCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached mesh for a container, extracting on a miss.
    pub fn get_or_extract(
        &mut self,
        doc: &Document,
        container: ContainerRef,
        opts: ExtractOptions,
    ) -> Result<&RenderMesh, GeometryError> {
        todo!("docs/01 §5 render mesh cache lookup")
    }

    /// Drops containers touched by `deltas`.
    pub fn apply_deltas(&mut self, deltas: &[Delta]) {
        for delta in deltas {
            if let Some(c) = delta.container() {
                self.entries.remove(&c);
            }
        }
        self.generation += 1;
    }

    /// Drops one container.
    pub fn invalidate(&mut self, container: ContainerRef) {
        self.entries.remove(&container);
    }

    /// Drops everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Number of cached containers.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_keys_order_front_before_back() {
        let a = MaterialKey {
            material: None,
            side: FrontOrBack::Front,
        };
        let b = MaterialKey {
            material: None,
            side: FrontOrBack::Back,
        };
        assert!(a < b);
        assert!(RenderMesh::default().bounds.is_empty());
    }
}
