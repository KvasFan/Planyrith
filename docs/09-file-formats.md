# 09 — File formats

**Audience:** implementing agents. Normative. Lives in `planyrith-core::io`.

## 1. Native container: `.plyr`

A ZIP archive (`zip` crate, `Stored` or `Deflate` via `miniz_oxide`) with this layout:

```
model.plyr
├── mimetype                  "application/x-planyrith" (first entry, Stored, for sniffing)
├── manifest.json             { format_version, min_reader_version, generator, created, modified, title, parts: [...], checksums }
├── document.msgpack          Document (rmp-serde), excluding image bytes and history
├── images/<blake3-hex>.<ext> texture/image bytes (PNG/JPEG/WebP), deduplicated by content hash
├── thumbnail.png             256×256 preview rendered by the last saver (optional)
├── history.msgpack           optional: command journal since the last "compact" (see §3)
└── ext/<plugin-id>/...       optional plugin payloads (opaque; preserved on round-trip)
```

Rules:
- Readers **MUST** ignore unknown parts and unknown MessagePack map keys (forward compatibility).
- Writers **MUST** write parts in the order above and use deterministic ZIP metadata (fixed
  timestamps when `--deterministic`, no extra fields) so that identical documents produce
  identical bytes.
- `manifest.json.checksums` holds blake3 of each part; `planyrith validate` verifies them.
- Maximum single part size 4 GB (ZIP64 enabled).
- Extensions: `.plyr` document, `.plyrc` component (same container; `document.msgpack` holds one
  definition at the root plus referenced materials), `.plyrm` material library, `.plyrs` style
  library, `.plyrt` template (a `.plyr` flagged `is_template`).

## 2. `document.msgpack` schema

Serialized with `rmp-serde` in **named-field (map) mode** (not compact tuple mode) so fields can
be added and skipped. Top-level:

```
Document {
  format_version: u32,
  info: ModelInfo,
  model: EntitiesSer,
  definitions: [DefinitionSer],          // ordered; referenced by index within the file
  materials: [MaterialSer],
  tags: { folders: [...], tags: [...] },
  scenes: [SceneSer],
  styles: [StyleSer], active_style: u32,
  rendering: RenderingOptions,
  camera: Camera,
  attrs: AttributeDictionaries,
}
EntitiesSer {
  vertices: [[f64;3]],                    // index = local vertex index
  edges: [{ a: u32, b: u32, flags: u8, curve: Option<u32>, pid: u64 }],
  faces: [{ loops: [[u32 /*edge index, sign bit = direction*/]], plane: [f64;4], mat_front: Option<u32>, mat_back: Option<u32>, uv_front: Option<UvMapSer>, uv_back, pid }],
  curves: [CurveDescriptorSer],
  groups: [{ definition: u32, transform: [f64;12], name, pid, tag, hidden, locked, attrs }],
  instances: [{ definition: u32, transform, name, pid, tag, hidden, locked, material_override, glued_to, attrs }],
  guides, dimensions, texts, images, section_planes: [...],
  order: [u64 /*pid*/],                    // insertion order across kinds
}
```
- Vertex positions are exact `f64`; faces store their plane for validation (recomputed on load if
  missing).
- Face loops reference edges by index with a direction bit; the half-edge structure is rebuilt on
  load (`HalfEdgeMesh::from_indexed`) — this keeps the file compact and independent of internal
  slot layout.
- Pids are stored; `EntityId`s are not (they are allocated on load in `order`).
- Transforms are 3×4 column-major `f64`.

## 3. Edit history journal (optional)

`history.msgpack` is an append-only list of `{ command, params, group, timestamp? }` since the
last compaction. It enables "revert to" and crash recovery and makes files reviewable in version
control via `planyrith export --format json`. It is **not** required to load the document.
`planyrith tool compact-history` clears it. Journals are capped at 64 MB by default.

## 4. Versioning and migration

- `format_version` increments on any schema change. `min_reader_version` tells old readers
  whether they can load (ignoring unknown fields) or must refuse (E1401).
- Migrations live in `io::native::migrate::vN_to_vN+1` and are pure functions on the deserialized
  intermediate (`serde_value`-like tree) applied in sequence; every migration has a fixture pair.
- Policy: readers support all versions since 0.1; writers always write the latest.

## 5. Interchange formats — scope

| Format | Import | Export | Notes |
|---|---|---|---|
| OBJ (+MTL) | ✔ | ✔ | groups → groups or components (`o`/`g`), materials, UVs, normals (as soft edges). |
| STL (ASCII/binary) | ✔ | ✔ | triangles merged into coplanar faces on import (`--merge-coplanar`, default on); binary default on export. |
| glTF 2.0 / GLB | ✔ | ✔ | nodes → groups/instances (meshes shared → components), PBR base color + texture, transforms, cameras → scenes. KHR_texture_transform supported. |
| DXF (R12–2018 ASCII) | ✔ | ✔ (3D and 2D) | LINE, LWPOLYLINE/POLYLINE, ARC, CIRCLE, ELLIPSE (segmented), SPLINE (flattened), 3DFACE, POLYFACE mesh, INSERT/BLOCK → components, LAYER → tags, TEXT/MTEXT, DIMENSION (as text+lines on export). |
| STEP AP203/214 | ✔ (feature `brep`) | ✔ (feature `brep`) | via `truck-stepio`; solids/shells; curved faces tessellated with curve descriptors. |
| Images (PNG/JPEG/WebP) | ✔ | ✔ (render) | import as image entity or as a material texture. |
| CSV | ✔ (points) | ✔ (component report) | `x,y,z[,name]` points → guide points or vertices; report: definition, count, bbox, volume, attrs. |
| JSON | — | ✔ | full document dump (`--canonical` for diffing). |
| SVG / PDF | — | ✔ | 2D export of a view (`--2d`) and of sheets (`docs/15`). Hidden-line removal in software; vector output. |
| `.plyr/.plyrc` | ✔ | ✔ | merge another document as a group/component. |

Out of scope for 1.0 (documented as future work): IFC, FBX, 3DS, DWG, USD, COLLADA.
Import of any unsupported entity yields `W2004` with counts, never a failure.

## 6. Importer/exporter contracts

```rust
pub trait Importer { fn probe(bytes: &[u8], name: &str) -> Confidence; fn import(&self, src: &mut dyn Read, opts: &ImportOptions, tx: &mut Transaction, target: &Target, progress: &dyn Progress) -> Result<ImportReport, IoError>; }
pub trait Exporter { fn export(&self, doc: &Document, scope: &ExportScope, opts: &ExportOptions, dst: &mut dyn Write, progress: &dyn Progress) -> Result<ExportReport, IoError>; }
```
Common options: `units` (source unit for unitless formats), `scale`, `swap_yz` (Y-up formats are
auto-detected for glTF; OBJ/STL are assumed Y-up unless `--z-up`), `merge_coplanar`,
`soften_angle`, `as` (group/component/image), `at` (placement), `include_hidden`,
`selection_only`, `per_tag`, `triangulate`, `copy_textures`.

Rules:
- Importers build geometry through the kernel (`add_edge`/face creation) only for *boundary*
  edges; interior triangles of large meshes are inserted via a fast path
  `HalfEdgeMesh::from_triangles_merged` which merges coplanar adjacent triangles into faces,
  marks edges between faces below `soften_angle` as `SOFT|SMOOTH`, and reports non-manifold input.
- Exporters are deterministic (stable ordering by pid) and stream (never hold the whole output in
  memory for > 100 MB files).
- Texture references are exported relative to the output file unless `copy_textures`.

## 7. Thumbnails and metadata

The GUI and CLI write `thumbnail.png` (256² isometric render of the model, deterministic style) on
save unless `--no-thumbnail`. `manifest.json` carries `title`, `author`, `description`,
`generator` (`planyrith <version>`), `created`, `modified` (RFC 3339; fixed under
`--deterministic`).

## 8. Robustness

- Corrupt or truncated containers → `E1402` with the failing part; `planyrith repair --from-parts`
  can rebuild from `document.msgpack` alone.
- Fuzz targets: the ZIP walker, the MessagePack decoder path (via `arbitrary` structs), every
  importer. No panics allowed; memory bounded (`zip` entries are size-checked against the
  manifest before extraction, max 4 GB; images capped at 16384² px).

## 9. Compatibility tests

- Golden `.plyr` fixtures for every `format_version` since 0.1 under
  `crates/planyrith-core/tests/fixtures/native/`.
- Round-trip tests per interchange format: import → export → import ⇒ same topology counts and
  bounds within tolerance; glTF/OBJ validated with external validators in CI when available.

## Decisions

- 2026-10-20 — ZIP container with `mimetype` first entry; MessagePack named-field mode.
- 2026-10-20 — Faces serialized as edge-index loops; half-edge rebuilt on load.
- 2026-10-20 — History journal is optional and never required for loading.
- 2026-10-20 — IFC/FBX/DWG explicitly out of scope for 1.0.
- 2026-10-20 — Skeleton contract: the in-memory document types derive `serde` directly in
  named-field mode for the first draft; the index-based `EntitiesSer`/`DefinitionSer`/… forms of
  §2 are produced by `io::native` from `HalfEdgeMesh::to_indexed` (`IndexedMesh`,
  `DirectedEdgeIndex` with bit 31 = reversed) when the writer is implemented. `Document::images`
  is skipped by serde and written as `images/<hash>.<ext>` parts.
- 2026-10-20 — Migrations (§4) operate on a `serde_json::Value` intermediate tree
  (`io::native::migrate::migrate(tree, from)`), applied one version step at a time.
- 2026-10-20 — **Containers use the indexed `EntitiesSer` form.** `document.msgpack` stores each
  `Entities` as its mesh in indexed form (`HalfEdgeMesh::to_indexed_ordered`) with edges and faces
  in the container's order list and vertices by first use, plus the non-mesh entity maps, the
  order as `(key, pid)` pairs and the side tables, where mesh entities are referenced by index
  (`SerKey`). Collections with stable ids (`StableMap`) store only their live `(id, value)` pairs.
  The file therefore does not depend on slot layout: a document whose changes were undone saves to
  the same bytes, and a re-saved file is byte-identical. On load meshes are rebuilt leniently
  (structure and orientation checked, planarity left to `planyrith validate`, which knows the
  document's tolerances). Format version stays 1 (no file of the earlier draft form was released).
- 2026-10-23 — **Bounded loading.** A stable-id collection (definitions, materials, tags, groups,
  instances, …) may use key slots up to index 4 194 304 (`StableMap::MAX_SLOT_INDEX`); a larger
  index is a corrupt file (`E1402`), refused before anything is allocated. Hierarchy walks
  (bounds, nesting checks, picking occurrences) skip definitions already on the current chain and
  stop at the nesting limit, so a damaged file with cyclic nesting loads, validates (reporting the
  cycle) and reports its bounds instead of hanging.
- 2026-10-25 — **Journal cap and damaged parts.** Writing keeps the journal under 64 MB by
  dropping its oldest rows (a quarter at a time), so a long-lived document can always be saved.
  Reading never fails because of an optional part: a journal that is too large or undecodable, a
  preserved part whose name could not be written back, and a part the manifest lists but the
  container lacks are dropped with a `W2006` warning; the document itself still loads.
- 2026-10-26 — A `.plyrm` material library is the native container whose document holds the
  library's materials and their images and nothing else (an empty model, the two default
  materials ignored by readers). Library files are written with deterministic metadata (the core
  has no clock) and without a journal, so saving the same materials gives the same bytes.
- 2026-10-26 — A `.plyrc` component file is the native container whose model holds exactly one
  instance of the component at the origin (identity transform); its definitions are the
  component and those it nests, with the materials, images and tags they use. Copying a
  definition into another document keeps its UUID; a document that already has a definition with
  that UUID reuses it. Materials and tags are matched by name (a missing one is added; tags go
  to the top level); the default materials map onto the document's defaults; pids are assigned
  anew. Definition UUIDs are a hash (blake3, as a version-8 UUID) of the name, the contents and
  the document's pid counter: deterministic, yet distinct for components made in different
  documents. (Before 0.4 they were the pid counter alone, which collided across documents.)
- 2026-10-26 — **Importers build into a target.** `Importer::import` also receives the command's
  resolved edit context (`Target`), so an import lands in the context it was run in. Every mesh
  importer fills one intermediate scene (meshes with per-triangle materials and texture
  coordinates, nodes with transforms, materials with optional texture bytes) and one shared
  builder turns it into geometry: units, `scale`, then Y-up → Z-up (`(x, y, z) → (x, −z, y)`)
  when `swap_yz` is set or, absent, when the format is Y-up (OBJ, STL; glTF always). Triangles
  merge into faces only with neighbours of the same material and the same texture mapping. A mesh
  used once becomes a group, a mesh used by several nodes a component; the import is one
  top-level group (named `Group N`) or, with `as: component:<name>`, one instance of a new
  component. A file holding a single mesh placed without a transform becomes that group or
  component directly, without a wrapper. Imported textures get a 1 m tile (texture coordinates
  in the file are in tiles).
- 2026-10-26 — **Export orientation and STL.** Exporters of Y-up formats (OBJ, STL) write Y-up
  (`(x, y, z) → (x, z, −y)`) unless `z_up`, mirroring the import rule, so an export imported back
  with default options lands where it was. STL is binary by default (`ascii` for text), with every
  visible face triangulated in world coordinates, in `file_units` (mm by default). The `export`
  command changes nothing in the document and is not undoable. The import/export unit parameter
  is `file_units` because `--units` is the CLI's global input unit; the `import`/`export` verbs
  accept `--units` for it as docs/08 §2 writes, and `--z-up` on `import` for `--no-swap-yz`.
- 2026-10-26 — **OBJ.** Import: objects and groups (`o`/`g`) become one group each (the parts of
  one object split by `usemtl` stay together); MTL files and `map_Kd` textures are read through
  the storage relative to the OBJ (texture options before the file name are ignored); a missing
  MTL or texture is a `W2004` warning and the default material is used; polygons are ear-clipped
  in their plane (concave ones included), points and lines are skipped and counted. `Kd` is the
  material color as given (0–1 per channel), `d` its opacity. Export: one object per occurrence
  (named after its definition, `_2`, `_3`… when repeated), faces as triangles with texture
  coordinates in texture repeats and one normal per face; the MTL is written next to the OBJ
  (`<name>.mtl`) and each texture under `<name>_textures/`; names have spaces turned into `_`.
  Exporters return side files to the `export` command, which writes them through the storage.
- 2026-10-26 — **glTF / GLB.** Import reads the default scene (else the first, else every
  node), flattening the node tree into world transforms; each glTF mesh is one mesh with its
  primitives' triangles (lists, strips and fans; points and lines skipped), a mesh several nodes
  use becomes a component. Materials: base color factor converted from linear to sRGB, opacity
  only when the alpha mode is blend, the base color texture (with `KHR_texture_transform`) as
  the material's texture; glTF's texture `v` runs down the image and is flipped. Cameras are
  skipped (`W2004`); scenes from cameras are future work. Buffers and images come from the GLB
  chunk, base64 data URIs, or files beside the glTF read through the storage. Units are meters
  unless `file_units`. Export writes one mesh per container (components share it) with one
  primitive per material, positions in meters (f32 keeps them precise), one node per occurrence
  with its world transform, double-sided materials with linear base color, blend when not
  opaque, and textures embedded in the buffer. `.gltf` writes `<name>.bin` beside it; `.glb` is
  one file. Output is deterministic.
- 2026-10-26 — **Imports always validate.** A mesh whose welded triangles cross each other
  (overlapping parts in one mesh) is split into its connected shells, each a group; a shell that
  crosses itself is left out with a `W2004` warning naming the count. A material named like the
  document's default material (as exporters write unpainted faces) keeps those faces unpainted,
  and an imported material equal to an existing one of the same name (or its `#n` variants) is
  reused instead of duplicated. The UUID of an imported definition hashes the source data instead
  of encoding the built definition (docs/09 Decisions of 0.4 otherwise unchanged).
- 2026-10-26 — **Import performance.** The acceptance case is a 100 MB GLB height field (3.6 M
  triangles with normals and texture coordinates), `import_benchmark` in the core's tests
  (ignored by default): about 11 s in a release build on the development machine while it was
  under heavy unrelated load, under the 20 s target. Saving such a document afterwards is not
  part of the measurement.
