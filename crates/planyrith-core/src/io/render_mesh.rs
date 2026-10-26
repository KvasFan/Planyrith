//! Implements docs/06-rendering-and-viewport.md §2 (render data extraction)
//! and the incremental cache of docs/01 §5: GPU-agnostic `RenderMesh` buffers
//! per `Entities` container (plain `Vec<f32>`/`Vec<u32>`), coordinates relative
//! to the container origin, smooth normals across `SOFT` edges; and the flat
//! draw list of docs/06 §2 ("the renderer walks the hierarchy each frame"):
//! one item per container occurrence with its world transform, material
//! override, selection and edit-context state.
//!
//! Determinism: faces and edges are taken in container (pid) order, material
//! ranges in key order, draw items in depth-first container order, so the
//! same document gives the same buffers before and after a save/reload.

use std::collections::BTreeMap;
use std::ops::Range;

use glam::{DAffine3, DVec2, DVec3};

use crate::commands::transaction::Delta;
use crate::document::{
    ContainerRef, Document, EditContext, Entities, EntityKey, EntityView, FrontOrBack,
    InstancePath, Selection, UvMap,
};
use crate::geom::tessellate::{tessellate_face, tessellate_face_with_openings, vertex_normals};
use crate::geom::{EdgeFlags, GeometryError};
use crate::ids::{EdgeId, EntityId, FaceId, MaterialId};
use crate::math::Aabb;
use crate::spatial::InstanceProvider;

/// Bits of `EdgeBuffers::flags` (docs/06 §2).
pub mod edge_flag_bits {
    /// Silhouette/profile edge (computed per view by the renderer; reserved here).
    pub const PROFILE: u8 = 0b0000_0001;
    /// `EdgeFlags::SOFT`.
    pub const SOFT: u8 = 0b0000_0010;
    /// `EdgeFlags::SMOOTH`.
    pub const SMOOTH: u8 = 0b0000_0100;
    /// Hidden: `EdgeFlags::HIDDEN`, or the edge entity is hidden (itself or
    /// through its tag). Present only with `ExtractOptions::include_hidden`.
    pub const HIDDEN: u8 = 0b0000_1000;
    /// A segment of a curve descriptor (arc, circle).
    pub const CURVE_SEAM: u8 = 0b0001_0000;
}

/// "No face" in [`EdgeBuffers::faces`].
pub const NO_FACE: u32 = u32::MAX;

/// Material resolution per face side (docs/06 §2 `MaterialKey`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MaterialKey {
    /// `None` = the document default for `side` (or an instance override).
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
    /// The local indices of the (up to two) faces each edge borders,
    /// [`NO_FACE`] for an open side (profile edges, docs/06 §5).
    pub faces: Vec<[u32; 2]>,
}

/// Extracted buffers for one container (docs/06 §2 `RenderMesh`). Every face
/// is emitted twice: its front side (counter-clockwise, normals along the
/// face normal) and its back side (flipped winding and normals), each in the
/// index range of its own material key.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RenderMesh {
    /// Vertex positions (container-local, f32).
    pub positions: Vec<[f32; 3]>,
    /// Shading normals.
    pub normals: Vec<[f32; 3]>,
    /// Texture coordinates in tiles of the side's texture (0 without one).
    pub uvs: Vec<[f32; 2]>,
    /// Local face index per vertex (picking/highlight).
    pub face_ids: Vec<u32>,
    /// Triangle indices into the vertex arrays.
    pub indices: Vec<u32>,
    /// Index ranges grouped by material and side, in key order.
    pub indices_by_material: Vec<(MaterialKey, Range<u32>)>,
    /// Edge lines.
    pub edges: EdgeBuffers,
    /// Local bounds.
    pub bounds: Aabb,
    /// Local face index → `FaceId` lookup (parallel to `face_ids` values).
    pub face_lookup: Vec<FaceId>,
    /// Face normal per local face (profile edges, docs/06 §5).
    pub face_normals: Vec<[f32; 3]>,
    /// Per local face: hidden (present only with `include_hidden`).
    pub face_hidden: Vec<bool>,
    /// Local edge index → `EdgeId` lookup.
    pub edge_lookup: Vec<EdgeId>,
}

impl RenderMesh {
    /// The local index of a face, if extracted.
    pub fn face_index(&self, f: FaceId) -> Option<u32> {
        self.face_lookup
            .iter()
            .position(|&x| x == f)
            .map(|i| i as u32)
    }

    /// The local index of an edge, if extracted.
    pub fn edge_index(&self, e: EdgeId) -> Option<u32> {
        self.edge_lookup
            .iter()
            .position(|&x| x == e)
            .map(|i| i as u32)
    }
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

/// `true` when an entity is hidden by its own flag or a hidden tag (docs/04 §5).
fn entity_hidden(doc: &Document, entities: &Entities, id: EntityId) -> bool {
    entities.is_hidden(id)
        || !doc
            .tags
            .is_visible(entities.tag_of(id).unwrap_or_else(|| doc.tags.untagged()))
}

/// Texture coordinates of `p` on one side of a face (docs/04 §4): the UV map
/// takes the point in the face frame (or the projection plane's frame) to
/// texture millimeters, which are divided by the texture's tile size; fixed
/// pins are already in tiles.
pub(crate) fn uv_of(
    map: Option<&UvMap>,
    frame_point: DVec2,
    vertex: crate::ids::VertexId,
    p: DVec3,
    tile: Option<DVec2>,
) -> [f32; 2] {
    let Some(tile) = tile else {
        return [0.0, 0.0];
    };
    let mm = match map {
        None => frame_point,
        Some(UvMap::Affine(a)) => a.transform_point2(frame_point),
        Some(UvMap::Projected { plane, affine }) => affine.transform_point2(plane.frame().to_2d(p)),
        Some(UvMap::Projective(m)) => {
            let h = *m * frame_point.extend(1.0);
            if h.z.abs() <= f64::MIN_POSITIVE {
                return [0.0, 0.0];
            }
            DVec2::new(h.x / h.z, h.y / h.z)
        }
        Some(UvMap::Fixed(pins)) => {
            let uv = pins
                .iter()
                .find(|(v, _)| *v == vertex)
                .map_or(frame_point / tile, |(_, uv)| *uv);
            return [uv.x as f32, uv.y as f32];
        }
    };
    let uv = mm / tile.max(DVec2::splat(f64::MIN_POSITIVE));
    [uv.x as f32, uv.y as f32]
}

fn f32x3(v: DVec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
}

/// The openings glued components that cut openings make in this
/// container's faces (docs/04 §3.3, docs/06 Decisions), by face, in
/// container coordinates.
fn openings(
    doc: &Document,
    entities: &crate::document::Entities,
) -> BTreeMap<FaceId, Vec<Vec<DVec3>>> {
    let mut out: BTreeMap<FaceId, Vec<Vec<DVec3>>> = BTreeMap::new();
    for (_, instance) in entities.instances() {
        let Some(path) = &instance.glued_to else {
            continue;
        };
        let Some(def) = doc.definition(instance.definition) else {
            continue;
        };
        if !def.behavior.cuts_opening {
            continue;
        }
        let Some(EntityId::Face(f)) = entities.key_to_id(path.target) else {
            continue;
        };
        let Some(ring) = cut_outline(def, doc.tolerances()) else {
            continue;
        };
        out.entry(f).or_default().push(
            ring.into_iter()
                .map(|p| instance.transform.transform_point3(p))
                .collect(),
        );
    }
    out
}

/// A component's opening outline (docs/06 Decisions): the outer loop of its
/// largest face lying on its red-green plane, else the rectangle of its
/// footprint on that plane.
pub fn cut_outline(
    def: &crate::document::ComponentDefinition,
    tol: &crate::tolerance::Tolerances,
) -> Option<Vec<DVec3>> {
    let mesh = def.entities.mesh();
    let mut best: Option<(f64, Vec<DVec3>)> = None;
    for (f, face) in mesh.faces() {
        let Ok(vertices) = mesh.loop_vertices(face.outer) else {
            continue;
        };
        let points: Vec<DVec3> = vertices
            .iter()
            .filter_map(|v| mesh.position(*v).ok())
            .collect();
        if points.len() < 3 || points.iter().any(|p| p.z.abs() > tol.length) {
            continue;
        }
        let area = mesh.face_area(f).unwrap_or(0.0);
        if best.as_ref().is_none_or(|(a, _)| area > *a) {
            best = Some((area, points));
        }
    }
    if let Some((_, points)) = best {
        return Some(points);
    }
    let b = mesh.aabb();
    (!b.is_empty() && b.max.x - b.min.x > tol.length && b.max.y - b.min.y > tol.length).then(|| {
        vec![
            DVec3::new(b.min.x, b.min.y, 0.0),
            DVec3::new(b.max.x, b.min.y, 0.0),
            DVec3::new(b.max.x, b.max.y, 0.0),
            DVec3::new(b.min.x, b.max.y, 0.0),
        ]
    })
}

/// Extracts the buffers of one container (docs/06 §2). Deterministic: faces
/// and edges in container (pid) order, material ranges sorted by key.
pub fn extract(
    doc: &Document,
    container: ContainerRef,
    opts: ExtractOptions,
) -> Result<RenderMesh, GeometryError> {
    let entities = doc
        .container(container)
        .ok_or_else(|| GeometryError::NotFound(format!("container {container:?}")))?;
    let mesh = entities.mesh();
    let tol = doc.tolerances();
    let mut out = RenderMesh::default();
    let mut buckets: BTreeMap<MaterialKey, Vec<u32>> = BTreeMap::new();
    let mut bounds = Aabb::EMPTY;

    let openings = openings(doc, entities);
    let mut face_index: BTreeMap<FaceId, u32> = BTreeMap::new();
    for &id in entities.order() {
        let EntityId::Face(f) = id else { continue };
        let Some(face) = mesh.face(f) else { continue };
        let hidden = entity_hidden(doc, entities, id);
        if hidden && !opts.include_hidden {
            continue;
        }
        // A face that cannot be triangulated is skipped, not fatal: the
        // rest of the model still renders (validate reports the defect).
        let cut = openings.get(&f);
        let tessellated = match cut {
            Some(rings) => tessellate_face_with_openings(mesh, f, tol, rings),
            None => tessellate_face(mesh, f, tol),
        };
        let Ok(tess) = tessellated else {
            continue;
        };
        let normal = mesh.face_normal(f)?;
        let normals = if opts.smooth_normals && cut.is_none() {
            vertex_normals(mesh, f, &tess)?
        } else {
            vec![normal; tess.vertices.len()]
        };
        let local = out.face_lookup.len() as u32;
        face_index.insert(f, local);
        out.face_lookup.push(f);
        out.face_normals.push(f32x3(normal));
        out.face_hidden.push(hidden);
        let frame = face.plane.frame();
        for side in [FrontOrBack::Front, FrontOrBack::Back] {
            let (material, map) = match side {
                FrontOrBack::Front => (face.sides.material_front, face.sides.uv_front.as_ref()),
                FrontOrBack::Back => (face.sides.material_back, face.sides.uv_back.as_ref()),
            };
            let tile = doc
                .materials
                .get(doc.materials.resolve(material, side))
                .and_then(|m| m.texture.as_ref())
                .map(|t| t.size_mm);
            let base = out.positions.len() as u32;
            let flip = if side == FrontOrBack::Front {
                1.0
            } else {
                -1.0
            };
            for (k, &p) in tess.positions.iter().enumerate() {
                bounds = bounds.including(p);
                out.positions.push(f32x3(p));
                out.normals.push(f32x3(normals[k] * flip));
                out.uvs
                    .push(uv_of(map, frame.to_2d(p), tess.vertices[k], p, tile));
                out.face_ids.push(local);
            }
            let bucket = buckets.entry(MaterialKey { material, side }).or_default();
            for t in &tess.triangles {
                if side == FrontOrBack::Front {
                    bucket.extend([base + t[0], base + t[1], base + t[2]]);
                } else {
                    bucket.extend([base + t[0], base + t[2], base + t[1]]);
                }
            }
        }
    }
    // Image entities: textured rectangles, the image filling them, seen
    // from both sides (docs/04 §3.7). They are not pickable by face.
    for &id in entities.order() {
        let EntityId::Image(i) = id else { continue };
        let Some(image) = entities.image(i) else {
            continue;
        };
        if entity_hidden(doc, entities, id) && !opts.include_hidden {
            continue;
        }
        let (w, h) = (image.width.as_mm(), image.height.as_mm());
        let corners = [
            (DVec3::ZERO, [0.0f32, 0.0]),
            (DVec3::new(w, 0.0, 0.0), [1.0, 0.0]),
            (DVec3::new(w, h, 0.0), [1.0, 1.0]),
            (DVec3::new(0.0, h, 0.0), [0.0, 1.0]),
        ];
        let normal = image
            .transform
            .matrix3
            .z_axis
            .try_normalize()
            .unwrap_or(DVec3::Z);
        for side in [FrontOrBack::Front, FrontOrBack::Back] {
            let base = out.positions.len() as u32;
            let flip = if side == FrontOrBack::Front {
                1.0
            } else {
                -1.0
            };
            for (c, uv) in corners {
                let p = image.transform.transform_point3(c);
                bounds = bounds.including(p);
                out.positions.push(f32x3(p));
                out.normals.push(f32x3(normal * flip));
                out.uvs.push(uv);
                out.face_ids.push(u32::MAX);
            }
            let bucket = buckets
                .entry(MaterialKey {
                    material: Some(image.material),
                    side,
                })
                .or_default();
            if side == FrontOrBack::Front {
                bucket.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
            } else {
                bucket.extend([base, base + 2, base + 1, base, base + 3, base + 2]);
            }
        }
    }
    for (key, list) in buckets {
        let start = out.indices.len() as u32;
        out.indices.extend(list);
        out.indices_by_material
            .push((key, start..out.indices.len() as u32));
    }

    for &id in entities.order() {
        let EntityId::Edge(e) = id else { continue };
        let Some(edge) = mesh.edge(e) else { continue };
        let hidden = edge.flags.contains(EdgeFlags::HIDDEN) || entity_hidden(doc, entities, id);
        if hidden && !opts.include_hidden {
            continue;
        }
        let seg = mesh.edge_segment(e)?;
        bounds = bounds.including(seg.a).including(seg.b);
        let mut bits = 0u8;
        if edge.flags.contains(EdgeFlags::SOFT) {
            bits |= edge_flag_bits::SOFT;
        }
        if edge.flags.contains(EdgeFlags::SMOOTH) {
            bits |= edge_flag_bits::SMOOTH;
        }
        if hidden {
            bits |= edge_flag_bits::HIDDEN;
        }
        if edge.curve.is_some() {
            bits |= edge_flag_bits::CURVE_SEAM;
        }
        let [a, b] = mesh.edge_faces(e)?;
        let local = |f: Option<FaceId>| {
            f.and_then(|f| face_index.get(&f).copied())
                .unwrap_or(NO_FACE)
        };
        out.edges.edge_ids.push(out.edge_lookup.len() as u32);
        out.edge_lookup.push(e);
        out.edges.positions.push(f32x3(seg.a));
        out.edges.positions.push(f32x3(seg.b));
        out.edges.flags.push(bits);
        out.edges.faces.push([local(a), local(b)]);
    }
    out.bounds = bounds;
    Ok(out)
}

/// Per-container cache invalidated by deltas (docs/01 §5).
#[derive(Debug, Default)]
pub struct RenderMeshCache {
    entries: BTreeMap<ContainerRef, (ExtractOptions, RenderMesh)>,
    generation: u64,
}

impl RenderMeshCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached mesh for a container, extracting on a miss (or
    /// when it was extracted with other options).
    pub fn get_or_extract(
        &mut self,
        doc: &Document,
        container: ContainerRef,
        opts: ExtractOptions,
    ) -> Result<&RenderMesh, GeometryError> {
        let fresh = self
            .entries
            .get(&container)
            .is_some_and(|(o, _)| *o == opts);
        if !fresh {
            let mesh = extract(doc, container, opts)?;
            self.entries.insert(container, (opts, mesh));
        }
        self.entries
            .get(&container)
            .map(|(_, m)| m)
            .ok_or_else(|| GeometryError::NotFound(format!("container {container:?}")))
    }

    /// Drops containers touched by `deltas`. Document-wide changes (tag
    /// visibility, materials, definitions) can change any container's
    /// buffers and clear the cache.
    pub fn apply_deltas(&mut self, deltas: &[Delta]) {
        for delta in deltas {
            match delta.container() {
                Some(c) => {
                    self.entries.remove(&c);
                }
                None => self.entries.clear(),
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

/// One container occurrence to draw (docs/06 §2 `DrawList`).
#[derive(Debug, Clone, PartialEq)]
pub struct DrawItem {
    /// The occurrence, every step keyed by pid.
    pub path: InstancePath,
    /// The container it opens.
    pub container: ContainerRef,
    /// Container-to-world transform.
    pub world: DAffine3,
    /// Material for the default-material faces: the innermost instance
    /// override on the path (docs/05 §5.8).
    pub material_override: Option<MaterialId>,
    /// Hidden itself or through a step or tag (listed with `include_hidden`).
    pub hidden: bool,
    /// The whole occurrence is selected (it or an enclosing group or
    /// instance is in the selection).
    pub selected: bool,
    /// Selected faces of this occurrence.
    pub selected_faces: Vec<FaceId>,
    /// Selected edges of this occurrence.
    pub selected_edges: Vec<EdgeId>,
    /// Outside the active edit context (drawn faded, docs/06 §9).
    pub faded: bool,
    /// The active section plane cutting this occurrence (the outermost one
    /// on its path, docs/04 §3.8), in container coordinates.
    pub section: Option<SectionCut>,
}

/// A section plane as it cuts one draw item (docs/06 §3.1 steps 2 and 4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SectionCut {
    /// Geometry on the `+normal` side is hidden (container coordinates).
    pub plane: crate::math::Plane,
    /// Fill the cut of solids.
    pub fill: bool,
}

/// The active, visible section plane of `entities` (in its coordinates).
fn active_section(doc: &Document, entities: &Entities) -> Option<SectionCut> {
    if !doc.rendering.section.cuts_visible {
        return None;
    }
    entities
        .order()
        .iter()
        .find_map(|&id| match entities.view(id) {
            Some(EntityView::SectionPlane(s)) if s.active && !entities.is_hidden(id) => {
                Some(SectionCut {
                    plane: s.plane,
                    fill: s.fill_cap && doc.rendering.section.fills_visible,
                })
            }
            _ => None,
        })
}

/// The cap of a section cut (docs/06 §3.1 step 4): the cut outlines of
/// `entities`' faces on `plane`, as filled triangles (both windings) and
/// as line segments, in container coordinates.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SectionCap {
    /// Triangle corners, three per triangle, front then back winding.
    pub triangles: Vec<[f32; 3]>,
    /// Outline segments, two points each.
    pub outline: Vec<[f32; 3]>,
}

/// The cap of `entities` cut by `plane`; filled only when its mesh is a
/// solid and `fill` (docs/06 §3.1: open cuts stay open).
pub fn section_cap(
    entities: &Entities,
    plane: &crate::math::Plane,
    fill: bool,
    tol: &crate::tolerance::Tolerances,
) -> SectionCap {
    let mesh = entities.mesh();
    let segments = crate::geom::section::slice(mesh, plane, tol);
    let mut cap = SectionCap::default();
    for (a, b) in &segments {
        cap.outline.push(a.as_vec3().to_array());
        cap.outline.push(b.as_vec3().to_array());
    }
    if !fill || segments.is_empty() || !mesh.is_solid() {
        return cap;
    }
    let mut outline = crate::geom::HalfEdgeMesh::new();
    for (a, b) in &segments {
        let _ = outline.add_edge(*a, *b, tol, &mut crate::geom::NoDeltas);
    }
    let faces: Vec<FaceId> = outline.faces().map(|(f, _)| f).collect();
    for f in faces {
        let Ok(t) = tessellate_face(&outline, f, tol) else {
            continue;
        };
        for tri in &t.triangles {
            let p = tri.map(|i| t.positions[i as usize].as_vec3().to_array());
            cap.triangles.extend([p[0], p[1], p[2]]);
            cap.triangles.extend([p[0], p[2], p[1]]);
        }
    }
    cap
}

/// The draw list of a document (docs/06 §2): every container occurrence in
/// depth-first container order, with selection and edit-context state.
/// Hidden occurrences are left out unless `include_hidden`.
pub fn draw_list(
    doc: &Document,
    selection: &Selection,
    context: &EditContext,
    include_hidden: bool,
) -> Vec<DrawItem> {
    let mut items: Vec<DrawItem> = Vec::new();
    for occ in InstanceProvider::occurrences(doc) {
        if occ.hidden && !include_hidden {
            continue;
        }
        // Walk the path once: canonical keys and the innermost override.
        let mut container = ContainerRef::Model;
        let mut steps = Vec::with_capacity(occ.path.depth());
        let mut material_override = None;
        // The outermost active section plane on the path, in world space.
        let mut level_world = DAffine3::IDENTITY;
        let mut section: Option<SectionCut> = doc
            .container(ContainerRef::Model)
            .and_then(|e| active_section(doc, e));
        for &step in &occ.path.steps {
            let Some(entities) = doc.container(container) else {
                break;
            };
            let Some(id) = entities.key_to_id(step) else {
                break;
            };
            level_world *= entities.transform_of(id).unwrap_or(DAffine3::IDENTITY);
            if let Some(EntityView::Instance(i)) = entities.view(id)
                && i.material_override.is_some()
            {
                material_override = i.material_override;
            }
            steps.push(
                entities
                    .pid_of(id)
                    .map_or(EntityKey::Id(id), EntityKey::Pid),
            );
            let Some(def) = entities.definition_of(id) else {
                break;
            };
            container = ContainerRef::Definition(def);
            if section.is_none()
                && let Some(cut) = doc
                    .container(container)
                    .and_then(|e| active_section(doc, e))
            {
                section = Some(SectionCut {
                    plane: cut.plane.transformed(&level_world),
                    ..cut
                });
            }
        }
        // World plane → this occurrence's coordinates.
        let section = section.and_then(|cut| {
            (occ.world.matrix3.determinant().abs() > 1e-12).then(|| SectionCut {
                plane: cut.plane.transformed(&occ.world.inverse()),
                ..cut
            })
        });
        items.push(DrawItem {
            path: InstancePath { steps },
            container: occ.container,
            world: occ.world,
            material_override,
            hidden: occ.hidden,
            selected: false,
            selected_faces: Vec::new(),
            selected_edges: Vec::new(),
            faded: false,
            section,
        });
    }

    let index: BTreeMap<InstancePath, usize> = items
        .iter()
        .enumerate()
        .map(|(i, item)| (item.path.clone(), i))
        .collect();
    let mut whole: Vec<InstancePath> = Vec::new();
    for path in selection.iter() {
        let path = path.canonical(doc);
        let Ok(resolved) = doc.resolve(&path) else {
            continue;
        };
        match resolved.entity {
            EntityId::Face(f) => {
                if let Some(&i) = index.get(&path.instances) {
                    items[i].selected_faces.push(f);
                }
            }
            EntityId::Edge(e) => {
                if let Some(&i) = index.get(&path.instances) {
                    items[i].selected_edges.push(e);
                }
            }
            EntityId::Group(_) | EntityId::Instance(_) => {
                whole.push(path.instances.child(path.target));
            }
            _ => {}
        }
    }
    let context = context.path.canonical(doc);
    for item in &mut items {
        item.selected = whole.iter().any(|w| w.is_prefix_of(&item.path));
        item.faded = !context.is_prefix_of(&item.path);
    }
    items
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::EntityPath;

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

    fn engine() -> Engine {
        Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
    }

    /// Area of the triangles of one face (front side) in a render mesh.
    fn face_triangle_area(mesh: &RenderMesh, local: u32) -> f64 {
        let (key, range) = &mesh.indices_by_material[0];
        assert_eq!(key.side, FrontOrBack::Front);
        mesh.indices[range.start as usize..range.end as usize]
            .chunks(3)
            .filter(|t| mesh.face_ids[t[0] as usize] == local)
            .map(|t| {
                let p = |i: u32| {
                    let v = mesh.positions[i as usize];
                    glam::DVec3::new(f64::from(v[0]), f64::from(v[1]), f64::from(v[2]))
                };
                (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0])).length() * 0.5
            })
            .sum()
    }

    #[test]
    fn glued_components_cut_openings_in_their_face() {
        let mut e = engine();
        // A 3 m × 2.5 m wall and a 900 × 1200 window component lying on
        // its own red-green plane, cutting an opening.
        let wall = run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [3000, 0, 2500], "plane": { "normal": [0, -1, 0], "d": 0 } }),
        );
        run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [0, 5000, 0], "corner_b": [900, 6200, 0] }),
        );
        run(
            &mut e,
            "make-component",
            json!({ "targets": "last", "name": "Window", "replace_selection": false,
                    "behavior": { "glue_to": "vertical", "cuts_opening": true } }),
        );
        let face = wall["face"].as_u64().unwrap();
        let mesh = |e: &Engine| {
            extract(e.document(), ContainerRef::Model, ExtractOptions::default()).unwrap()
        };
        let before = mesh(&e);
        let (_, id) = e.document().locate_pid(crate::ids::Pid(face)).unwrap();
        let EntityId::Face(f) = id else { panic!() };
        let local = before.face_index(f).unwrap();
        assert!((face_triangle_area(&before, local) - 7.5e6).abs() < 1.0);

        run(
            &mut e,
            "component-insert",
            json!({ "definition": "Window", "position": [1000, 0, 800], "on_face": { "Pid": face } }),
        );
        let after = mesh(&e);
        let local = after.face_index(f).unwrap();
        let area = face_triangle_area(&after, local);
        assert!((area - (7.5e6 - 900.0 * 1200.0)).abs() < 1.0, "{area}");
        // The opening's outline is the component's largest flat face.
        let def = e
            .document()
            .definition(e.document().definition_by_name("Window").unwrap())
            .unwrap();
        assert_eq!(
            cut_outline(def, e.document().tolerances()).unwrap().len(),
            4
        );
    }

    fn run(e: &mut Engine, name: &str, params: serde_json::Value) -> serde_json::Value {
        e.execute_erased(name, params)
            .unwrap_or_else(|err| panic!("{name}: {err}"))
    }

    fn cube(e: &mut Engine) {
        run(
            e,
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [1000, 1000, 0] }),
        );
        run(
            e,
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 1000.0 }),
        );
    }

    #[test]
    fn a_box_gives_two_sided_triangles_and_twelve_edges() {
        let mut e = engine();
        cube(&mut e);
        let m = extract(e.document(), ContainerRef::Model, ExtractOptions::default()).unwrap();
        assert_eq!(m.face_lookup.len(), 6);
        // 6 quads, 2 triangles each, front and back.
        assert_eq!(m.indices.len(), 6 * 2 * 2 * 3);
        assert_eq!(m.positions.len(), 6 * 4 * 2);
        assert_eq!(m.positions.len(), m.normals.len());
        assert_eq!(m.positions.len(), m.uvs.len());
        assert_eq!(m.positions.len(), m.face_ids.len());
        let sides: Vec<FrontOrBack> = m.indices_by_material.iter().map(|(k, _)| k.side).collect();
        assert_eq!(sides, vec![FrontOrBack::Front, FrontOrBack::Back]);
        assert_eq!(m.edge_lookup.len(), 12);
        assert_eq!(m.edges.positions.len(), 24);
        // Every box edge borders two faces, and each front triangle winds
        // with its face normal.
        assert!(
            m.edges
                .faces
                .iter()
                .all(|[a, b]| *a != NO_FACE && *b != NO_FACE)
        );
        let (_, front) = &m.indices_by_material[0];
        for t in m.indices[front.start as usize..front.end as usize].chunks(3) {
            let p = |i: u32| glam::Vec3::from(m.positions[i as usize]);
            let n = (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0]));
            let face_n = glam::Vec3::from(m.face_normals[m.face_ids[t[0] as usize] as usize]);
            assert!(n.dot(face_n) > 0.0);
        }
        assert_eq!(m.bounds.max, DVec3::splat(1000.0));
    }

    #[test]
    fn soft_edges_smooth_normals_and_hidden_entities_are_left_out() {
        let mut e = engine();
        cube(&mut e);
        run(
            &mut e,
            "erase",
            json!({ "targets": "edge", "mode": "soften" }),
        );
        run(
            &mut e,
            "hide",
            json!({ "targets": "face & normal:z & plane:xy@1000" }),
        );
        let doc = e.document();
        let m = extract(doc, ContainerRef::Model, ExtractOptions::default()).unwrap();
        assert_eq!(m.face_lookup.len(), 5);
        let soft = m
            .edges
            .flags
            .iter()
            .filter(|f| **f & edge_flag_bits::SOFT != 0)
            .count();
        assert_eq!(soft, 12);
        // Across soft edges neighboring faces share averaged normals.
        let bent = m
            .normals
            .iter()
            .filter(|n| n[0].abs() > 0.1 && n[1].abs() > 0.1)
            .count();
        assert!(bent > 0);
        let all = extract(
            doc,
            ContainerRef::Model,
            ExtractOptions {
                include_hidden: true,
                smooth_normals: false,
            },
        )
        .unwrap();
        assert_eq!(all.face_lookup.len(), 6);
        assert_eq!(all.face_hidden.iter().filter(|h| **h).count(), 1);
        assert!(
            all.normals
                .iter()
                .all(|n| n.iter().filter(|c| c.abs() > 0.5).count() == 1)
        );
    }

    #[test]
    fn draw_list_follows_instances_selection_and_context() {
        let mut e = engine();
        cube(&mut e);
        run(
            &mut e,
            "make-component",
            json!({ "targets": "all", "name": "Crate" }),
        );
        run(
            &mut e,
            "move",
            json!({ "targets": "instance", "delta": [2000, 0, 0], "copy": true }),
        );
        run(
            &mut e,
            "material-create",
            json!({ "name": "Oak", "color": "#A0522D" }),
        );
        run(
            &mut e,
            "material-apply",
            json!({ "targets": "instance", "material": "Oak" }),
        );
        let doc = e.document();
        let items = draw_list(doc, &Selection::new(), &EditContext::model(), false);
        assert_eq!(items.len(), 3);
        assert!(items[0].path.is_root());
        assert_eq!(items[1].container, items[2].container);
        assert_eq!(items[2].world.translation, DVec3::new(2000.0, 0.0, 0.0));
        assert!(items[1].material_override.is_some());
        assert!(items.iter().all(|i| !i.selected && !i.faded));

        // Select the second instance; enter the first.
        let second = items[2].path.clone();
        let (last, parent) = second.steps.split_last().unwrap();
        let selection = Selection::single(EntityPath::new(
            InstancePath {
                steps: parent.to_vec(),
            },
            *last,
        ));
        let context = EditContext {
            path: items[1].path.clone(),
        };
        let items = draw_list(doc, &selection, &context, false);
        assert!(!items[1].selected && items[2].selected);
        assert!(items[0].faded && !items[1].faded && items[2].faded);

        // A click on a face inside an instance selects the instance from
        // the model, the face from inside it.
        let hit = EntityPath::new(items[1].path.clone(), {
            let def = doc.container(items[1].container).unwrap();
            let f = def
                .order()
                .iter()
                .find(|id| matches!(id, EntityId::Face(_)))
                .unwrap();
            EntityKey::Pid(def.pid_of(*f).unwrap())
        });
        let from_model = hit.selectable_in(doc, &InstancePath::root()).unwrap();
        assert!(from_model.instances.is_root());
        assert_eq!(from_model.target, items[1].path.steps[0]);
        assert_eq!(hit.selectable_in(doc, &items[1].path), Some(hit.clone()));
        assert_eq!(hit.selectable_in(doc, &items[2].path), None);
    }

    #[test]
    fn the_cache_reextracts_touched_containers() {
        let mut e = engine();
        cube(&mut e);
        let mut cache = RenderMeshCache::new();
        let opts = ExtractOptions::default();
        assert_eq!(
            cache
                .get_or_extract(e.document(), ContainerRef::Model, opts)
                .unwrap()
                .face_lookup
                .len(),
            6
        );
        assert_eq!(cache.len(), 1);
        let before = e.history().applied();
        run(
            &mut e,
            "delete",
            json!({ "targets": "face & normal:z & plane:xy@1000" }),
        );
        assert!(e.history().applied() > before);
        let deltas: Vec<Delta> = e
            .history()
            .entries()
            .last()
            .unwrap()
            .transactions
            .iter()
            .flat_map(|t| t.deltas.clone())
            .collect();
        cache.apply_deltas(&deltas);
        assert!(cache.is_empty());
        assert_eq!(
            cache
                .get_or_extract(e.document(), ContainerRef::Model, opts)
                .unwrap()
                .face_lookup
                .len(),
            5
        );
    }
}
