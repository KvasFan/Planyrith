# planyrith-core skeleton contract

> **Progress:** for which parts of this contract are now implemented and what to do next, see
> [`development-status.md`](development-status.md). As of 2026-10-20 the `foundations`,
> `spatial` and `io-native` owner keys are implemented; `mesh` is partial (tessellation done,
> topology operations still `todo!()`); `document` and `later` are not started.

**Audience:** the engineers implementing milestone 0.1 in parallel. This document freezes the
public types, traits and function signatures of `planyrith-core` as committed on 2026-10-20 so
that five owners can fill in module bodies against agreed interfaces. Every operation body is
`todo!("docs/0X §Y")` and cites the section it implements; trivial items (constructors,
`Display`, `From`, `Default`, simple accessors, the undo-stack bookkeeping, `MemoryStorage`,
the event bus and the registry) are implemented.

The crate compiles, `cargo test -p planyrith-core` passes (trivial tests), `cargo doc --no-deps`
is clean. `lib.rs` carries `#![allow(unused_variables, dead_code)]` with a TODO; owners remove
the allowance for their module once it is implemented.

## 1. Ownership

Owner keys: **foundations**, **mesh**, **spatial**, **document**, **io-native**, **later**.
"later" files belong to no 0.1 owner; they are implemented once their dependencies exist
(dependency notes in the table). Shared declaration files (`lib.rs`, `geom/mod.rs`,
`commands/mod.rs`) receive *additive* changes only (new re-exports, new enum variants); anything
else goes through a review by all affected owners.

| File (under `crates/planyrith-core/src/`) | Purpose | Spec | Owner |
|---|---|---|---|
| `lib.rs` | crate root, module tree, re-exports, crate-level allowances | docs/01 §2 | later (shared) |
| `ids.rs` | typed slot-map ids, `EntityId`, `EntityKind`, `Pid`, `PidAllocator`, id JSON schemas | docs/04 §2, docs/01 §2 | foundations |
| `units.rs` | `Length`, `Angle`, `LengthUnit`, `AngleUnit`, parse/format, value-field grammar | docs/03 §1, docs/08 §4, docs/05 §1 | foundations |
| `tolerance.rs` | `Tolerances` + comparison helpers | docs/03 §2 | foundations |
| `math/mod.rs` | glam re-exports, `Axis` | docs/03 §1 | foundations |
| `math/plane.rs` | `Plane`, `PlaneFrame`, fit, frame, transform | docs/03 §3–§4, §8 | foundations |
| `math/ray.rs` | `Ray`, ray–plane | docs/03 §6–§7 | foundations |
| `math/segment.rs` | `Segment`, closest points, on-segment test | docs/03 §4.2, §8 | foundations |
| `math/aabb.rs` | `Aabb` | docs/03 §6, docs/06 §2 | foundations |
| `math/transform.rs` | affine helpers, `TransformParts` (CLI `Transform` syntax) | docs/03 §1, docs/08 §2 | foundations |
| `geom/mod.rs` | `GeometryError`, `MeshDelta`, `DeltaSink`, `FaceSides`, `FaceSnapshot`, `NoDeltas` | docs/03 §4, docs/01 §3.1 | later (shared) |
| `geom/mesh.rs` | `HalfEdgeMesh` and element types, `EdgeFlags`, `CurveDescriptor`, fundamental ops §4.1–§4.8, `validate`, `is_solid`, `IndexedMesh` conversion, `from_triangles_merged` | docs/03 §3, §4.1–§4.8, §4.13 (`is_solid`), docs/09 §2, §6 | mesh |
| `geom/tessellate.rs` | ear clipping with holes, soft-edge normals, `TessellationCache` | docs/03 §5 | mesh |
| `geom/planar.rs` | face finding, minimal cycles, angular star sort, 2D helpers (`signed_area_2d`, `loop_orientation`, `default_face_normal` implemented) | docs/03 §4.3–§4.5 | later (needs mesh) |
| `geom/intersect.rs` | segment/plane/face intersections, `intersect_faces`, `ray_face` | docs/03 §4.12, docs/06 §8 | later (needs mesh, spatial) |
| `geom/boolean.rs` | `BooleanOp`, classification, `boolean`, `merge_coplanar_faces` | docs/03 §4.13 | later (0.8) |
| `geom/offset.rs` | `offset_face_boundary`, `offset_polygon_2d` | docs/03 §4.10 | later (0.1 `offset` tool) |
| `geom/sweep.rs` | `sweep_profile`, `path_frames` | docs/03 §4.11 | later (0.8) |
| `geom/extrude.rs` | `extrude_face` (Push/Pull core) | docs/03 §4.9 | later (0.1 `push-pull` tool) |
| `geom/brep.rs` | feature `brep` bridge contract | docs/03 §9 | later (0.8) |
| `spatial/mod.rs` | `ContainerIndex`, `DocumentIndex`, `Hit*`, queries | docs/03 §6, docs/06 §8 | spatial |
| `inference/mod.rs` | `InferenceKind`, `Constraint`, `InferenceEngine`, locks, references | docs/03 §7, docs/05 §2 | later (0.3) |
| `document/mod.rs` | `Document`, `ModelInfo`, `RenderingOptions`, `ContainerRef`, `Resolved`, `PathError`, `serde_seq` helper, `FORMAT_VERSION` | docs/04 §1, §2, §10, §12 | document |
| `document/entities.rs` | `Group`, `ComponentInstance`, `ComponentDefinition`, `ComponentBehavior`, `Guide`, `Dimension`, `Anchor`, `DimStyle`, `Text`, `ImageEntity`, `SectionPlane` | docs/04 §3.1–§3.8 | document |
| `document/hierarchy.rs` | `Entities` container, `EntityRecord`, `EntityView`, `EntityCounts`, CRUD, pid binding, side tables | docs/04 §3, §12 | document |
| `document/materials.rs` | `Rgba8`, `Material`, `Materials`, `Texture`, `UvMap`, `ImageRef`, `ImageStore` | docs/04 §4 | document |
| `document/tags.rs` | `Tag`, `TagFolder`, `Tags`, `LineStyle` | docs/04 §5 | document |
| `document/scenes.rs` | `Scene`, `Scenes` (implemented) | docs/04 §6 | document |
| `document/styles.rs` | `Style` tree, `Styles` (implemented) | docs/04 §7 | document |
| `document/attributes.rs` | `AttributeDictionaries`, `Value` (implemented) | docs/04 §9 | document |
| `document/camera.rs` | `Camera`, `Projection`, NDC ray | docs/04 §8 | document |
| `document/shadows.rs` | `ShadowSettings`, `SunModel`, `Date`, `TimeOfDay` | docs/04 §7 | document |
| `document/selection.rs` | `EntityKey`, `InstancePath`, `EntityPath`, `EntityRef`, `Selection`, `EditContext`, `Query`/`QueryExpr` parse+eval | docs/04 §11–§12, docs/08 §5 | document |
| `commands/mod.rs` | `Command`, `ErasedCommand` (+ blanket impl), `CommandMeta`, `CommonParams`, `CommandRegistry`, `Engine`, `EventBus`/`Event`, `CommandError` | docs/01 §3, §7, §8, docs/08 §9 | later (shared) |
| `commands/transaction.rs` | `Transaction`, `Delta`, `AttrTarget`, `CommittedTransaction`, `UndoStack`/`UndoEntry` | docs/01 §3.1–§3.2 | later (needs document + mesh) |
| `commands/{draw,edit,organize,annotate,view,file}/mod.rs` | one `Command` per tool, empty in the skeleton | docs/05 | later |
| `io/mod.rs` | `IoError`, `Progress`, `NoProgress`, `CancelToken` | docs/01 §6–§7, docs/08 §9 | io-native |
| `io/storage.rs` | `Storage` trait, `StorageKey/Entry/Caps`, `MemoryStorage` (implemented), `NativeFs`, `conformance_check` | docs/11 §1, §6–§7 | io-native |
| `io/native/mod.rs` | `.plyr` read/write/open/save, part names, `WriteOptions`, `HistoryEntry`, `ContainerKind` | docs/09 §1–§3, §7–§8 | io-native |
| `io/native/manifest.rs` | `Manifest` | docs/09 §1, §7 | io-native |
| `io/native/migrate.rs` | `migrate`, step functions | docs/09 §4 | io-native |
| `io/export/json.rs` | `JsonExporter`, `document_to_json`, `canonicalize` | docs/09 §5, docs/08 §10 | io-native |
| `io/export/mod.rs` | `Exporter`, `ExportFormat/Scope/Options/Report` | docs/09 §5–§6 | later |
| `io/import/mod.rs` | `Importer`, `ImportFormat/As/Options/Report`, `Confidence` | docs/09 §5–§6 | later (0.5) |
| `io/render_mesh.rs` | `RenderMesh`, `EdgeBuffers`, `MaterialKey`, `extract`, `RenderMeshCache` | docs/06 §2, docs/01 §5 | later (0.2) |
| `scripting/mod.rs` | feature `scripting` contract | docs/10 §2 | later (0.8) |
| `diagnostics.rs` | `Warning`/`WarningCode`, `Report`, `Problem`, `validate`, `repair`, `statistics` | docs/04 §13, docs/08 §2, §9 | later (needs mesh + document) |

Exact owner file lists:

- **foundations:** `ids.rs`, `units.rs`, `tolerance.rs`, `math/mod.rs`, `math/aabb.rs`, `math/plane.rs`, `math/ray.rs`, `math/segment.rs`, `math/transform.rs`
- **mesh:** `geom/mesh.rs`, `geom/tessellate.rs`
- **spatial:** `spatial/mod.rs`
- **document:** `document/mod.rs`, `document/attributes.rs`, `document/camera.rs`, `document/entities.rs`, `document/hierarchy.rs`, `document/materials.rs`, `document/scenes.rs`, `document/selection.rs`, `document/shadows.rs`, `document/styles.rs`, `document/tags.rs`
- **io-native:** `io/mod.rs`, `io/storage.rs`, `io/native/mod.rs`, `io/native/manifest.rs`, `io/native/migrate.rs`, `io/export/json.rs`
- **later:** `lib.rs`, `geom/mod.rs`, `geom/planar.rs`, `geom/intersect.rs`, `geom/boolean.rs`, `geom/offset.rs`, `geom/sweep.rs`, `geom/extrude.rs`, `geom/brep.rs`, `inference/mod.rs`, `commands/mod.rs`, `commands/transaction.rs`, `commands/draw/mod.rs`, `commands/edit/mod.rs`, `commands/organize/mod.rs`, `commands/annotate/mod.rs`, `commands/view/mod.rs`, `commands/file/mod.rs`, `io/export/mod.rs`, `io/import/mod.rs`, `io/render_mesh.rs`, `scripting/mod.rs`, `diagnostics.rs`

## 2. Invariants every implementer must respect

### Ids and identity (docs/04 §2)
- Typed ids (`FaceId`, `EdgeId`, …) are slot-map keys, unique within one container or mesh.
  Never expose raw integers; `Display` is for diagnostics only.
- `EntityId` is the sum of the typed ids of *entities*; vertices are **not** entities and have
  no pid. Groups and instances are the only container steps and the only lockable entities.
- `Pid` is the document-wide, persistent identity. `PidAllocator` is monotonic and is never
  rewound by undo; `Pid::NONE` (0) is never allocated.
- `EntityId` values are **not** stable across undo/redo (a slot-map key cannot be re-created at a
  chosen index). Deltas therefore carry pids and order indices, and mesh deltas identify vertices
  by **position** (valid because no two vertices are within `tolerances.length`). Frontends and
  scripts address entities by `Pid` or `EntityPath`.
- `Entities` keeps `pids`/`by_pid` in sync: binding a pid appends to `order`; unbinding removes
  from `order` and returns the former index so undo can re-insert at the same place.

### Ordering and determinism (AGENTS.md §2, docs/04 §2)
- `Entities::order` is the one observable iteration order and MUST survive save/load and
  undo/redo. Side tables are `BTreeMap`/`BTreeSet` (ordered), never hash maps.
- `Materials`, `Tags`, `Scenes`, `Styles`, `Document::definition_order` keep insertion order
  in explicit `order` vectors; slot-map iteration order is only used where the result is not
  user-observable.
- Query evaluation returns matches in container order, then pid. Spatial queries return hits
  sorted by `t`, then kind priority (vertex > edge > face), then id. Inference breaks ties by
  priority, then smaller `t`, then lower id.
- No clocks, randomness or locale in the core. UUIDs for definitions are passed in by the
  caller (the engine or a command parameter), never generated inside `document`.

### Tolerance (docs/03 §2)
- Never compare `f64` with `==`; use `Tolerances::approx_eq_len`, `same_point`,
  `approx_eq_angle`, `is_parallel`, `is_perpendicular`, `is_coplanar`, `points_on_plane`.
- Edges shorter than `length` and faces with area below `length²` must not exist; operations
  merge/remove instead and emit `W2002`/`W2001` warnings through the transaction.
- Coordinates beyond `Tolerances::MAX_EXTENT` (±1e9 mm) are `GeometryError::OutOfRange`.
- Do not quantize coordinates; merge by tolerance only (docs/03 §8).

### Mutation path (docs/01 §3.1)
- Commands mutate only through `Transaction`. All `Document`/`Entities` mutators are
  `pub(crate)` and are called by `commands::transaction` only (plus `io::native` on load).
- Every kernel operation takes `&Tolerances` and a `&mut dyn DeltaSink`, applies-or-rolls-back
  internally, and records every elementary `MeshDelta` in application order. Composite markers
  (`FaceSplit`, `FacesMerged`) are emitted in addition to the elementary adds/removes.
- `Transaction::with_mesh` is the only place that assigns pids to edges/faces created by kernel
  operations (`EdgeAdded`/`FaceAdded` → `PidBound`), and the split face's pid goes to `new[0]`.
- `Delta::inverse` must be exact: apply → inverse → byte-identical `.plyr` (docs/12 §2).

### Errors and codes (docs/01 §7, docs/08 §9)
- Errors are values (`thiserror` enums). No `unwrap`/`expect`/`panic` in library code; fuzzers
  assert it. `GeometryError::code`, `IoError::code`, `CommandError::code` return the stable
  codes; new variants must map to an existing or newly documented code (docs/08 Decisions).
- Non-fatal conditions are `Warning`s on the transaction/output, never errors.

### Boundaries (docs/01 §1.1)
- `std::fs` appears only in `io::storage::NativeFs`. Commands that touch files receive
  `&dyn Storage` through `Transaction::with_storage`.
- No GUI, GPU or windowing crates. `RenderMesh` is plain `Vec<f32>`/`Vec<u32>`.
- Feature-gated modules (`brep`, `scripting`) compile to contracts only until their crates are
  pinned (docs/02); `parallel` must not change results (deterministic merges).

### Serialization (docs/09 §2)
- Named-field mode everywhere (`rmp_serde::to_vec_named`); readers ignore unknown fields.
- Maps keyed by slot-map keys or `EntityId` use `document::serde_seq` (pair sequences) so the
  same types also serialize to JSON. `Document::images` is skipped and stored as container
  parts. The spec's `*Ser` index-based forms are produced by `io::native` from
  `HalfEdgeMesh::to_indexed`.

## 3. Working agreement
- Replace a `todo!` only with a complete implementation plus tests (property tests for geometry).
- Keep signatures as committed; a signature change is a PR touching this document's table and
  the relevant spec "Decisions" list, reviewed by every owner whose module calls it.
- Remove `#![allow(unused_variables, dead_code)]` per module as it fills in; keep
  `clippy --all-targets` free of errors and minimize warnings.
