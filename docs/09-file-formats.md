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
- Frozen versions (1.0 and later): once released, a version's schema only grows. A
  writer of that version may add a field (absent means its default) but never
  removes, renames, retypes or reinterprets one, and never adds an enum variant an
  older reader would meet. Anything else needs the next `format_version`, a
  migration, and `min_reader_version` raised when older readers cannot ignore it.

## 5. Interchange formats — scope

| Format | Import | Export | Notes |
|---|---|---|---|
| OBJ (+MTL) | ✔ | ✔ | groups → groups or components (`o`/`g`), materials, UVs, normals (as soft edges). |
| 3MF | ✔ | ✔ | the 3D printing package: mesh objects with base materials or color groups, components and build items on import; one welded object per visible occurrence with faces, base-material colors and the file unit on export (`W2007` for objects that are not closed solids). |
| PLY (ASCII/binary) | ✔ | ✔ | polygon meshes, e.g. from 3D scanning: vertex and face colors become materials on import; export writes triangles with the front color on each vertex (binary little-endian unless `--ascii`). |
| USD (`.usda`, `.usdz`) | — | ✔ | a text stage, or packed (stored, 64-byte aligned) for augmented-reality viewers: one mesh per occurrence and material, Z up, `metersPerUnit` from `--file-units`, preview-surface materials with their textures. |
| IFC (IFC4) | — | ✔ | a building model for BIM tools: one storey holding an element per top-level group or component and the loose faces, triangulated face sets per material with surface colors, millimeters. |
| STL (ASCII/binary) | ✔ | ✔ | triangles merged into coplanar faces on import (`--merge-coplanar`, default on); binary default on export. |
| glTF 2.0 / GLB | ✔ | ✔ | nodes → groups/instances (meshes shared → components), PBR base color + texture, transforms, cameras → scenes. KHR_texture_transform supported. |
| DXF (R12–2018 ASCII) | ✔ | ✔ (3D and 2D) | LINE, LWPOLYLINE/POLYLINE, ARC, CIRCLE, ELLIPSE (segmented), SPLINE (flattened), 3DFACE, POLYFACE mesh, INSERT/BLOCK → components, LAYER → tags, TEXT/MTEXT, DIMENSION (as text+lines on export). |
| STEP AP203/214 | ✔ (feature `brep`) | ✔ (feature `brep`) | via `truck-stepio`; solids/shells; curved faces tessellated with curve descriptors. |
| SKP / SKB | ✔ (desktop format module, target 2013+) | ✔ (edited 2017 / 2026; unchanged source version retained) | `.skb` is a backup with the same model encoding; direct open/save/import/export, native working-copy default; release certification and fidelity limits in Decisions. |
| Images (PNG/JPEG/WebP) | ✔ | ✔ (render) | import as image entity or as a material texture. |
| CSV | ✔ (points) | ✔ (component report) | `x,y,z[,name]` points → guide points or vertices; report: definition, count, bbox, volume, attrs. |
| JSON | — | ✔ | full document dump (`--canonical` for diffing). |
| SVG / PDF | — | ✔ | 2D export of a view (`--2d`) and of sheets (`docs/15`). Hidden-line removal in software; vector output. |
| `.plyr/.plyrc` | ✔ | ✔ | merge another document as a group/component. |

Out of scope for 1.0 (documented as future work): FBX, 3DS, DWG, COLLADA; USD and IFC import.
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

- Golden `.plyr` fixtures for every frozen `format_version` under
  `crates/planyrith-core/tests/fixtures/native/v<N>/`: one per command, written by
  that command's CLI batch (`tools/native-fixtures.sh`, which only adds missing
  files). Each must load without warnings, and saving it again must keep every
  stored key and value of `document.msgpack` and `sheets.msgpack`; unknown fields
  must be ignored.
- Round-trip tests per interchange format: import → export → import ⇒ same topology counts and
  bounds within tolerance; glTF/OBJ validated with external validators in CI when available.

## Decisions

- 2026-11-03 — **`format_version` 1 frozen (1.0).** Files written since 0.1
  kept version 1 because every change was an added, defaulted field. The 1.0
  freeze makes that the rule (§4) and checks it on a corpus written by every
  command (§9). Earlier releases' files were not kept as a corpus, so it starts
  with what this revision writes; the older `.plyr` test files elsewhere under
  `tests/fixtures/` still load.
- 2026-10-31 — **Historical models and native-default saves (alpha.2).** The
  user expanded the minimum import target to 2013. Parse pre-2017 preambles
  without pid masks, use stable object-map identities for references, read the
  older definition prefix and pre-2014 instance tail, and account for the
  earlier layer-list prefix. Four externally saved 2013–2016 box fixtures and
  MIT nested 2013/2014 probes supplement the existing 2017/2019/2020/2026 tests;
  headers are never rewritten to claim coverage. Other per-release and
  entity-type certification remains in progress. Edited writers remain 2017
  and 2026; unchanged historical source bytes/versions restore exactly.
  `.skb` is an extension alias for the same importer/exporter and whole-model
  storage operations. The desktop GUI lists both extensions in Open/Import
  and Save As; no separate conversion action is required. The optional format
  process remains separate under the licensing decision in docs/02.
  A source opened from SKP/SKB initially proposes a native `.plyr` working copy.
  GUI Save opens Save As with the native filename; CLI/RPC implicit saving uses
  the sibling `.plyr` and refuses an existing destination. Explicit destinations
  retain their requested format; GUI/RPC sessions remember that choice until
  another file is opened. Backups, autosaves and snapshots remain native.
- 2026-10-31 — **Classic archive compatibility and converter diagnostics.**
  Read packed four-byte RGBA attributes as lossless signed integer values and
  consume additional component definitions after the declared definition list,
  including extended class tags, before reading root entities. Legacy BMP
  textures, thumbnails, watermarks and strokes are decoded by the separate Rust
  helper and stored as lossless PNG assets within the native image size limits;
  existing PNG/JPEG/WebP bytes stay unchanged. A nonzero converter exit reports
  `conversion failed` with its diagnostic (`E1402`); it does not establish that
  the source container is corrupt. Synthetic regressions cover color values,
  following-record alignment, trailing definitions and bitmap pixel/alpha
  preservation. `PLANYRITH_SKP_TEST_FILE` optionally exercises desktop open,
  native save/reopen and byte-exact unchanged SKP restoration on a private model
  without putting that model in the repository.
  Refit stale face planes from source vertices; polygons beyond native
  planarity tolerance become facets, retaining exact vertices, holes, side
  appearance and attributes with an explicit notice. New internal diagonals
  are soft/smooth. Appearance-only mesh deltas bypass pid rebinding work so
  assigning imported face materials/UVs does not rebuild the complete pid set
  for every face.
  Facets below the native area threshold retain their boundary wires with a
  notice. Interrupted or branching source curve memberships are split into
  deterministic editable chains; circular fragments retain their analytic
  center, normal, radius and actual angle range. The same normalization applies
  to fragments separated into private surface groups.
  Coincident source vertices are normalized with the core's vertex-only repair
  within document length tolerance, retaining pid/attribute bookkeeping and
  reporting merges. Other repairs are not enabled; remaining structural errors
  still fail conversion. The complete untouched input remains preserved.
  Before repair, separate overlapping edges and coincident vertices in different
  source faces/wires into private groups. A deterministic grid broad phase
  finds these conflicts; group assignment retains coordinates and both sides of
  each face, including loose edges and isolated vertices, without welding them
  across groups.
- 2026-11-01 — **Real-model additions.** A private 160 MB 2019.3 model required the
  following. From 2019 a layer ends with a reference to a custom line style:
  a name, a dash pattern (`12.0, -6.0, 1.0, -6.0`, negative numbers are gaps) and
  three `f64`, and the "separator" null of earlier notes is that reference left
  empty. The pattern maps to the closest native tag style; tag styles are not
  written back, with a notice. Texture images in other encodings become
  lossless PNG assets: BMP, TIFF, GIF, Targa and 8-bit RGB or grayscale Photoshop
  files (merged image, raw or RLE), the last with a small decoder in
  `planyrith-skp`. An image that still cannot be read leaves its material's color
  with a notice, and drops a thumbnail or watermark: one asset never stops a
  model from opening. The saved camera sets the document's camera directly; a
  transaction only queues view requests for the interactive session, so import
  had been dropping it for every model. A model without a usable camera opens
  framed from the default direction. Native parts of 8 MiB or more use deflate level 1:
  they decide save time, level 6 took 16 s on a 160 MB document for a 9% smaller
  file, and any level is equally deterministic.
- 2026-10-31 — **Classic display settings.** The reader's rendering-options table
  uses the 2017 byte layout, and classic files of other releases shift it: a
  2013 box decoded as X-ray with a transparent background. Decoded options
  apply only when their colors are opaque or absent and their section width and
  ground transparency are in range; otherwise the import keeps native defaults
  with a notice, and the source is retained. No per-release layout is guessed,
  as a default template alone cannot calibrate one. By this test 2013–2016,
  2019, 2020 and the classic 2021 archive use defaults, and 2017, 2018 and the
  container releases keep theirs. In 2018 and later the locked color lies past
  the divergence, so it is applied only when opaque. All-zero colors were not
  stored and leave the native default. Working copies saved earlier keep the
  style they were imported with; reopening the original applies this.
- 2026-10-31 — **SKP regeneration fidelity.** An edge end is written as a vertex
  anchor (kind 2), as the format stores it, and an edge parameter of exactly 0
  or 1 reads as that end, so these anchors keep their native form. A scene's
  rendering reuses a style holding the same settings, preferring the one the
  scene names: creating one per scene added styles on every reopen. Slide delay
  and looping have no native field and travel as `org.planyrith.skp` document
  attributes. The format has one transition time, so the first nonzero scene
  value is written, with a notice if they differ. Guide and section attributes
  are written. Radial attachments use the native curve frame (x × y of the
  stored axes), which can oppose the stored normal.
- 2026-10-31 — **SKP corpus findings.** Survey of 680 public files (2013–2026),
  including real 2018/2021/2022 saves; evidence in `docs/dev/skp-fidelity.md`.
  - An anchor of kind 7 names a face and stores a pair normalized over the outer
    ring's extent in the face's plane. The pair is identical in 2017 and 2026
    saves whose rings start at different vertices, which supports that reading,
    but its axes are unverified: the leader end is on the correct face at an
    approximate position. Native text on a face is a fixed point (docs/04), so
    the import is a fixed point; one outside the polygon falls back to the
    ring's centroid. Unknown anchor kinds use their stored point with a notice
    rather than preventing the model from opening.
  - Conflicts between a face's own edges cannot be separated into groups. A face
    with crossing or touching rings, or a ring passing along one edge twice,
    becomes facets; a facet whose apex lies on
    its opposite side within length tolerance keeps its wires. Vertices are
    never moved. These facets are not reproduced as one face on regeneration:
    a later import may refacet them, adding a few faces.
  - The default layer is identified by its name, as a custom tag may be listed
    first. Classic archives of every release from 2013 use the calibrated
    reader when the general walk fails; a root list running into the document
    tail stops there with a notice, but an archive ending inside an item is an
    error. Releases before 2013 are refused by name.
  - Images retained for foreign data (custom stroke bitmaps) are referenced from
    the `org.planyrith.skp.strokes` document attributes; `validate` and `repair`
    honor those references, so repair cannot silently invalidate restoration.
  - Texture placement fitting uses well-spread samples. When no matrix reproduces
    a face's samples the closest is written and the total reported, as this
    is a limit of one face, not of the model. Tints transfer; a native hue shift
    is written as the format's only tint.
- 2026-10-30 — **Edited SKP regeneration (0.11.0-alpha.1).** Native text,
  associative linear/radial dimensions, circular analytic curves and welded
  sampled chains, image entities/assets, styles/watermarks, geographic shadows
  and scene appearance now regenerate in both writers. Native annotation
  appearance edits survive conversion after an SKP open and native save/reopen.
  Image definitions stay internal to SKP and do not accumulate as unused native
  components. Temporary curve/vertex/image ids are allocated deterministically
  above document entity ids without advancing the document allocator.
  Native-only source catalogs are retained in `.plyr`, not copied as giant SKP
  attribute strings; classifications use their format records. Regenerated output
  explicitly reports possible omission of foreign/unrecognized records.
  Remaining limits include manual sun directions, some annotation placement and
  radial references, custom strokes/masks, polygon editing metadata, certain
  attribute/behavior types, Sheets/history and uncertified releases. Exact
  source restoration still applies only to unchanged documents. This prerelease
  does not satisfy the full 0.11 acceptance contract.

- 2026-10-30 — **Expanded editable SKP import.** A calibrated pure-Rust classic
  object-archive reader recovers geometry from the previously failing SDK-saved
  2019/2020 cases; small MIT-licensed feature fixtures are pinned and reproducible.
  Import also creates native text/linear and radial dimensions with font/layout
  data and associative nested/parametric anchors, analytic circular curves,
  spatial polylines, image entities, styles/fog/watermarks, geographic shadows and
  scene appearance. Full paths rebind during native merge, even when persistent-id
  ranges overlap. Font substitution and remaining display limits return notices;
  decoded settings, classifications and custom stroke images are native data.
  This expands import; edited SKP export retains the conversion limits below.
  Whole-model source preservation still applies. Release fixtures for 2018 and
  2021–2024 and independent edited-output validation remain required. See
  `docs/dev/skp-fidelity.md` for evidence and outstanding work.

- 2026-10-30 — **Native-first fidelity requirement.** The user confirmed that
  `.plyr` remains the principal, lossless working format; SKP is an interchange
  format with a target of complete open/save interoperability for 2017 and
  later. New documents, recovery, snapshots and full application state stay
  native. Complete SKP support is an acceptance target, not the current status:
  test real files per release, all transferred data and edited round-trips,
  rather than treating header recognition or an unchanged file as sufficient.
  Current gaps include edited export regeneration, exact appearance details and
  uncertified intermediate versions. See the 0.11 milestone in docs/13.

- 2026-10-30 — **Preserved SKP source in native working copies.** Successful
  whole-model conversion embeds the exact source in
  `ext/org.planyrith.skp/source.skp` and a versioned baseline digest in
  `ext/org.planyrith.skp/baseline-v1.blake3`. Normal native open/save and
  autosave carry these opaque parts without requiring a converter. The digest
  covers the editable document, images and Sheets, excluding source parts,
  journal, consumed PID allocator high-water mark, writer version and manifest
  timestamps. A fully undone edit can therefore restore the original.
  Whole-model SKP save restores
  the source byte for byte only when that baseline still matches; otherwise it
  generates current edited geometry and reports conversion losses. Regenerated
  saves use 2017 for sources before 2026 (and new models), 2026 for later
  sources; they never silently target a newer release than the source reader.
  Explicit exports still choose 2017 or 2026. A change of authoring container
  is reported. Merge import does not attach a whole source to the destination. Reader fallback with no decoded geometry
  fails explicitly instead of producing a successful empty document.

- 2026-10-30 — **SKP interoperability.** An explicit user request adds
  `.skp` opening, Save/Save As, import and export on desktop. The optional
  `planyrith-skp` executable is a separate GPL component (docs/02), discovered
  beside the application or via absolute `PLANYRITH_SKP_ADAPTER`. Storage
  invokes it with byte streams; core and frontends have no dependency on its
  decoder. Missing converters fail explicitly; no native container is written
  under a `.skp` extension. Browser conversion is unavailable.
  Save follows the native-first/source-preservation decisions above; export
  accepts `skp_version: 2017 | 2026`.
  Merge import follows native import semantics: geometry/assets enter a group
  or component; source cameras, scenes, units and drawing axes are retained by
  whole-model Open, rather than replacing the current document's settings.
  Both containers have original-file and generated round-trip tests; intervening
  releases are not certified. Source coordinates convert to millimeters without
  using display units as geometry units. Export uses fixed time/seed.
  Polygon boundaries and holes, wire edges/flags, nested and reused components,
  groups/transforms, two-sided materials, embedded textures/UVs, tags, cameras,
  scene visibility/sections, guides, drawing axes, display units and supported
  attribute values transfer. Circular curves retain analytic descriptors; other curves retain welded geometry. Painted
  groups use private component instances to retain inherited material. Conflicting
  face sides are partitioned into private surface groups without deleting faces;
  hidden-edge scene references follow all copies. Texture placements retain their
  projective frames through native saves. WebP textures export as PNG with the
  same decoded pixels.
  Edited export transfers supported annotations/images/appearance as described
  above. Unrepresentable data and native-only Sheets/history produce notices;
  retain `.plyr` for full fidelity. Unrepresentable UV placements and selected-only
  export fail explicitly. Conversion failure changes neither the document nor
  the destination. Autosave/snapshots always stay native. Inputs and converted
  files are capped at 512 MiB; protocol metadata at 1 MiB, surface partitions at
  4096 per source container. Fixture provenance,
  license and detailed scope are in `crates/planyrith-skp/README.md`.

- 2026-10-29 — **Sheets part (0.9).** `sheets.msgpack` follows the document
  body, is checksummed, and is omitted for an untouched empty set. It holds the
  typed data of docs/15, outside `document.msgpack`; older readers preserve it as
  an unknown part. Frozen images are content-addressed PNGs under `sheets/frozen/`.
  Live sheet images count as image references for validation, removal and undo.
  New readers load files without the part as empty Sheets. Format version stays 1.

- 2026-10-28 — **STEP in 0.8 (`brep`).** Export writes visible whole solid
  bodies in world-space millimeters with a fixed header timestamp. Import
  preserves planar line-edge loops exactly, including through-holes and inward
  cavity shells; curved shells use tolerance-based tessellation. Unsupported
  assembly placements are refused explicitly; provide placed solid bodies.
  Input is limited to 64 MiB. Explicit import units override automatic
  millimeter/meter detection. Golden fixtures for a decimal-coordinate box,
  through-hole and hollow solid live under `tests/fixtures/step`; regenerate
  with `cargo run -p planyrith-core --features brep --example generate-solids`.

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
- 2026-10-26 — **Images, CSV, native merge.** An image imports as an image entity lying on the
  ground at `at`, 1 m wide times `scale` (as `image-insert`), or with `as: texture` as a material
  named after the file with the image as its texture; it cannot import as a component. CSV rows
  `x,y,z[,name]` (`,`, `;` or tab; `z` optional; names ignored) become guide points in a new group
  or component; rows that do not start with numbers are skipped and counted. A `.plyr` or
  `.plyrc` merges as a group (or component) holding the other document's model, with the
  definitions, materials, images and tags it uses copied as `component-insert` copies them
  (definitions with a known UUID reused, materials and tags matched by name); its scenes, styles
  and camera are not merged. Export: `.plyr` writes a copy of the whole document without journal
  and with fixed timestamps; `.csv` writes the component report — one row per component shown,
  in definition order: name, occurrences, width/depth/height of its bounding box in its own axes
  and its volume when solid (in `file_units`), description.
- 2026-10-26 — **DXF.** Import (ASCII; binary refused): LINE, LWPOLYLINE (bulges as arcs),
  POLYLINE (2D, 3D, polyface and polygon meshes), ARC, CIRCLE, ELLIPSE and SPLINE (NURBS evaluated;
  fit points when there are no control points) become edges, segmented at 24 per full turn, in
  their object coordinate system; 3DFACE and mesh faces become faces. Edges go through the
  kernel's `add_edge` (as when drawn), so closed planar outlines become faces. A BLOCK becomes a
  component (built on first INSERT, base point at its origin) and INSERT an instance, rows and
  columns included; nesting deeper than 32 or recursive blocks are skipped. Model space content
  goes into one group per layer, tagged with a tag of the layer's name (made if missing, colored
  from the layer's first seven color indices); layer `0` is untagged, and a drawing with only
  layer `0` needs no layer groups. Entities inside blocks keep no layers. Text, dimensions,
  hatches and other entities are skipped and counted by type (`W2004`). Units from `$INSUNITS`
  (in, ft, mm, cm, m) unless `file_units`, else millimeters. Export (3D): R12 ASCII with each face
  as `3DFACE` triangles and each hard edge as a `LINE` in world coordinates and `file_units`
  (`$INSUNITS`), on the layer named after the occurrence's innermost tag (characters DXF forbids
  become `_`; untagged: `0`). 2D DXF of a view comes with the 2D exports.
- 2026-10-26 — **2D export of a view (SVG, PDF, DXF `two_d`).** The view is the document's
  camera, or the camera and visibility of `scene` applied to a copy as `scene-activate` would.
  Drawn lines are hard edges and profiles (soft edges between a face turned to the eye and one
  turned away); profiles and boundary edges are drawn 0.5 mm wide, other edges 0.25 mm. Hidden
  lines are removed in software: each line is split wherever another projected edge crosses it,
  and a piece is kept when a ray from its middle toward the eye meets no visible face (triangles
  found through a screen grid). Edges that pierce a face without crossing an edge are not split
  there, and in perspective, triangles reaching behind the near plane do not hide anything —
  both rare in modeled geometry. Lines are clipped to the camera's frame (its aspect and height
  or field of view). Paper: a parallel view at `scale` (`1:50`) is the frame at that scale;
  other views fit `paper_width` (297 mm). SVG and PDF are paper millimeters (y up in PDF, down
  in SVG); a 2D DXF has `LINE`s on layers `Edges` and `Profiles`, in model units of the view
  plane for parallel views (`file_units`) and paper millimeters for perspective views. Output is
  deterministic (PDF without metadata).
- 2026-10-26 — **Shading normals soften edges.** When a glTF or OBJ mesh carries vertex
  normals, an edge whose two sides have the same normal at both ends (within 1.8°) was drawn
  smooth by the source and becomes `SOFT | SMOOTH` whatever the angle between its faces; other
  edges follow `soften` (20°). Low-poly cylinders and spheres therefore import without hard
  lines between their facets, while models with flat normals keep their edges.
- 2026-11-02 — **glTF cameras and scenes** (§6), replacing "cameras are skipped". Each camera node
  imports as a scene named after the node (or its camera, or `Camera N`), made unique as
  `Name (2)`. Its view uses the node's world transform with the geometry's unit and Y-up
  conversion and the offset `at`, looking along −Z with +Y up. Perspective cameras keep `yfov`;
  orthographic cameras become parallel views as high as twice `ymag`. The look-at point is at the
  depth of the imported geometry's center, or 1 m ahead when that is behind the camera. A camera
  without a direction or with an unusable field of view is skipped with `W2004`. The report
  counts `scenes`, omitted at zero. Export writes each scene with a camera as a camera node:
  perspective (two-point too) with its `yfov`, parallel as orthographic with `ymag` half the
  height and `xmag` from the camera's aspect. `gltf_cameras_round_trip_as_scenes` exports two
  scenes to GLB and imports them back with the same eye, direction, up and projection.
- 2026-11-05 — **3MF** (milestone 1.1, docs/13). Export writes one object per visible occurrence
  with faces, in world coordinates (Z up, as the format requires), its triangles welded on exactly
  equal positions so a closed solid stays a closed mesh; components are not shared because
  slicers place each object on its own and mirrored instances would need flipped copies anyway.
  The face colors become one base-material group; the object's default is its first face's
  material and triangles only name another. Units: millimeter, centimeter, meter, inch and foot
  map directly, feet-and-inches write inches. Objects that are not printable
  (`docs/03 §4.14`) are still written, with `W2007`. Import reads the model part that the root
  relationship names (else `3D/3dmodel.model`), base materials and color groups as materials,
  components expanded depth-first into placements (32 levels at most), and the build items;
  micron files are scaled to millimeters. Parts in other model files, textures, composite and
  multi-property groups, beam lattices, slices and displacement meshes are skipped with
  `W2004`. The XML is read with `quick-xml` (MIT, docs/02); the writer is our own.
- 2026-11-08 — **glTF surface factors** (docs/06 §4.1). Export writes a material's `metallic`
  and `roughness`, or 0 and 1 (the matte look exports had before) when unset. Import keeps a
  roughness below 1 and a metalness strictly between 0 and 1: the format's defaults (1 and 1)
  cannot be told from values a file left unset, and taking them would make every material a
  rough metal. A fully metallic material therefore imports as non-metallic.
- 2026-11-08 — **PLY** (milestone 1.1, docs/13). Own reader and writer (the format is a text
  header and a flat body). Import reads the three encodings, `vertex` (`x y z`, `red green blue`
  as integers 0–255 or floats 0–1) and `face` (the first list property, `vertex_indices` or
  `vertex_index`; optional colors); other elements are counted in `W2004`. Polygons are fanned
  into triangles; a face's color is its own or its first vertex's, and each distinct color
  becomes a material. Export writes `float x y z`, `uchar red green blue` and a `uchar`/`uint`
  index list per triangle, welding vertices of equal position and color; Z up and
  `--file-units` as for 3MF.
- 2026-11-08 — **USD export** (milestone 1.1, docs/13). Text (`usda`) rather than the binary
  crate format: it needs no library and every USD reader takes it, inside a package as well.
  The package holds `model.usda` first, stored uncompressed with each entry's data on a 64-byte
  boundary, as the package rules require. World transforms are baked into the points, as for
  3MF; each occurrence gives one `Mesh` per material (triangles, face-varying normals,
  double-sided) bound to a `UsdPreviewSurface` material with the linear base color, opacity,
  roughness and metalness. A textured material's image (PNG or JPEG; WebP converted to PNG)
  feeds the base color through a texture reader on the faces' `st` coordinates (texture
  repeats, face-varying); a colorizing material scales it by its color. Packed, the images go
  in `textures/`; beside a text stage, in `<stem>_textures/`, so exporting next to the model's
  own images never replaces them. `tools/validate-exports.sh` checks both files with the reference library's validators
  (`usd-core`, a local check). Import of USD is not planned for 1.1.
- 2026-11-08 — **IFC export** (milestone 1.1, docs/13). IFC4 in the exchange structure's text
  encoding, written by our own module: the project (named after the file), a site, a building
  and one storey "Level 0" aggregated in that order; one `IfcBuildingElementProxy` per top-level
  group or component instance (with everything inside it) and one for the model's loose faces,
  named like the Outliner, placed at the storey's origin; its body is one
  `IfcTriangulatedFaceSet` per material in world millimeters, styled with the material's color
  and transparency. GUIDs are the format's 22-character encoding of a hash of fixed labels and
  the element's pid, so exporting a model twice gives the same ids; the header's timestamp is
  fixed (docs/08 §10). Property sets and storeys other than "Level 0" are later work. `tools/validate-exports.sh` checks the file with IfcOpenShell's
  schema and rule validation and its geometry kernel (a local check; the tool is not a
  dependency).
- 2026-11-08 — **IFC element classes.** An element's class comes from an `ifc` / `class`
  attribute on the top-level group or instance naming one of `IfcWall`, `IfcSlab`, `IfcRoof`,
  `IfcColumn`, `IfcBeam`, `IfcStair`, `IfcRailing`, `IfcCovering`, `IfcPlate`, `IfcMember`,
  `IfcFurniture`, `IfcDoor`, `IfcWindow` (prefix and case optional), else from its tag's name:
  a word of it in English or Italian, singular or plural (walls/muri/pareti, floors/solai,
  roof/tetti, columns/pilastri, beams/travi, stairs/scale, railings/ringhiere,
  ceilings/controsoffitto, plates/lastre, members, furniture/arredi/mobili, doors/porte,
  windows/finestre), else `IfcBuildingElementProxy`. Doors and windows leave their sizes and
  operation unset. All fourteen classes pass IfcOpenShell's validation.
