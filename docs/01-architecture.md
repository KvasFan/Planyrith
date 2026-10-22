# 01 — Architecture

**Audience:** implementing agents. Normative.

## 1. The three tiers

```
┌──────────────────────────────┐   ┌──────────────────────────────┐
│        planyrith-gui         │   │        planyrith-cli         │
│  egui/eframe shell, wgpu     │   │  clap subcommands, scripts,  │
│  viewport, interactive tools │   │  JSON output, JSON-RPC serve │
└──────────────┬───────────────┘   └──────────────┬───────────────┘
               │  in-process call  OR  JSON-RPC    │
               ▼                                   ▼
┌─────────────────────────────────────────────────────────────────┐
│                        planyrith-core                           │
│  Engine ─ CommandRegistry ─ Document ─ Geometry ─ Inference ─ IO │
│  (no GUI, no GPU, no direct filesystem; storage via trait)       │
└─────────────────────────────────────────────────────────────────┘
```

- **`planyrith-core`** owns all state and all behavior that changes state. It compiles for
  desktop and `wasm32-unknown-unknown` with no feature differences in semantics.
- **`planyrith-cli`** is the *reference frontend*. It maps every registered command to a
  subcommand and can run as a server (`planyrith serve`) speaking JSON-RPC 2.0.
- **`planyrith-gui`** is an *interactive frontend*. It links the core in-process (default) and
  can alternatively attach to a `planyrith serve` process (remote/headless-host mode). It
  contributes nothing to document semantics; it only collects parameters and dispatches
  commands.

A fourth crate, **`planyrith-render`** (wgpu renderer without UI), exists as a scaffold from
the start and receives its first renderer at milestone 0.2, so that both the GUI viewport and
the CLI's headless `render` subcommand share one renderer. It depends on `planyrith-core`,
`wgpu` and GPU-data helpers only.

### 1.1 Dependency rules (enforced by CI)

| Crate | May depend on | Must not depend on |
|---|---|---|
| `planyrith-core` | `glam`, `libm`, `parry3d-f64`, `earcutr`, `serde`, `serde_json`, `rmp-serde`, `zip` + `flate2` (pure-Rust deflate), `schemars`, `thiserror`, `slotmap`, `indexmap`, `bytemuck`, `blake3`, `uuid`, `image` (decoders only), `tracing`, importers, `truck-*` (feature), `rhai` (feature), `rayon` (feature) | `egui`, `wgpu`, `winit`, `eframe`, `web-sys`, `std::fs` outside `io::storage::native` |
| `planyrith-render` | `planyrith-core`, `wgpu`, `glam`, `bytemuck`, `image` | `egui`, `winit` |
| `planyrith-cli` | `planyrith-core`, `planyrith-render`, `clap`, `serde_json`, `tracing` | `egui`, `wgpu` directly |
| `planyrith-gui` | everything above, `egui`, `eframe`, `egui-wgpu`, `egui_tiles`, `web-sys` (wasm) | — |

## 2. Core module map

```
planyrith-core/src/
├── lib.rs
├── ids.rs            // typed ids: EntityId newtypes, generational slot allocation
├── units.rs          // Length, Angle, unit parsing/formatting, precision settings
├── tolerance.rs      // Tolerances struct and comparison helpers (docs/03 §2)
├── math/             // re-exports glam f64 types, plane/line/ray utilities, transforms
├── geom/             // half-edge mesh, polygon triangulation, face finding, splitting, merging
│   ├── mesh.rs       // HalfEdgeMesh: vertices, half-edges, edges, faces, loops
│   ├── planar.rs     // planar graph face detection, loop orientation
│   ├── tessellate.rs // polygon-with-holes → triangles (ear clipping), normals
│   ├── intersect.rs  // edge/edge, edge/face, face/face intersection
│   ├── boolean.rs    // solid classification, union/subtract/intersect/trim/split
│   ├── offset.rs, sweep.rs, extrude.rs
│   └── brep.rs       // (feature "brep") bridge to the exact B-rep kernel
├── spatial/          // BVH / grid indexes over edges, faces, instances (picking, inference)
├── inference/        // snap candidates, priorities, axis locks (docs/05 §2)
├── document/         // Document, entities, Model, ComponentDefinition, Materials, Tags, Scenes
│   ├── entities.rs   // Edge, Face, Group, ComponentInstance, Guide, Dimension, Text, Image, SectionPlane
│   ├── hierarchy.rs  // Entities containers, paths, transforms, instance resolution
│   ├── materials.rs, tags.rs, scenes.rs, styles.rs, attributes.rs, camera.rs, shadows.rs
│   └── selection.rs  // Selection sets and addressing (docs/08 §5)
├── commands/         // Command trait, registry, transactions, undo stack
│   ├── mod.rs        // Command, CommandRegistry, Engine
│   ├── transaction.rs// Transaction, Delta, inverse deltas
│   ├── draw/ edit/ organize/ annotate/ view/ file/ ...  // one module per tool
├── io/
│   ├── storage.rs    // Storage trait (read/write/list/delete) + native + memory impls
│   ├── native/       // .plyr container, migrations (docs/09)
│   ├── import/ export/ // obj, stl, gltf, dxf, step (feature), image, csv, json
│   └── render_mesh.rs// GPU-agnostic RenderMesh extraction (positions, normals, uvs, ids, lines)
├── scripting/        // (feature "scripting") rhai bindings generated from the registry
└── diagnostics.rs    // Validate/repair model, statistics
```

## 3. The command system

Everything that mutates a `Document` is a **Command**. This is the mechanism that makes
"CLI first" true by construction.

### 3.1 Traits

```rust
/// A unit of document mutation. Implementations are stateless; all inputs are in `Params`.
pub trait Command: Send + Sync + 'static {
    /// Stable kebab-case name, e.g. "push-pull". Used by CLI, RPC, scripts, docs.
    const NAME: &'static str;
    /// Serde-typed parameters; every field documented; units explicit (Length/Angle newtypes).
    type Params: serde::Serialize + serde::de::DeserializeOwned + schemars::JsonSchema + Clone;
    /// Serde-typed result (new ids, measured values, warnings).
    type Output: serde::Serialize + schemars::JsonSchema;

    /// Validate params against the document without mutating (used for live preview and CLI --dry-run).
    fn validate(&self, doc: &Document, params: &Self::Params) -> Result<(), CommandError>;
    /// Apply inside a transaction. Must only mutate through `tx`, never through `doc` directly.
    fn apply(&self, tx: &mut Transaction<'_>, params: Self::Params) -> Result<Self::Output, CommandError>;
    /// Metadata for frontends: category, undo label, whether it is a view-only command, etc.
    fn meta(&self) -> CommandMeta;
}
```

Rules:

1. `apply` mutates only through `Transaction`, which records **deltas** (inverse operations) so
   undo is O(change). Direct access to `Document` mutators is private to `commands::transaction`.
2. Commands are **pure** with respect to the document and params: no clock, no randomness, no
   I/O (file commands receive a `&dyn Storage`).
3. Commands never open dialogs or ask questions. A command that needs a choice exposes it as a
   parameter with a documented default; the GUI tool asks the user *before* dispatching.
4. Interactive previews call `validate` plus a **preview** path: the GUI applies the command in
   a scratch transaction on a lightweight document clone (`Document::fork_for_preview()`, copy-on-
   write at entity-container granularity) and discards it.

### 3.2 Registry and Engine

```rust
pub struct CommandRegistry { /* name → Box<dyn ErasedCommand> + JSON schemas */ }
pub struct Engine {
    doc: Document,
    history: UndoStack,          // transactions with inverse deltas, grouped by user action
    registry: Arc<CommandRegistry>,
    events: EventBus,            // DocumentChanged{deltas}, SelectionChanged, SceneActivated, ...
}
impl Engine {
    pub fn execute<C: Command>(&mut self, params: C::Params) -> Result<C::Output, CommandError>;
    pub fn execute_erased(&mut self, name: &str, params_json: serde_json::Value) -> Result<serde_json::Value, CommandError>;
    pub fn undo(&mut self) -> Result<(), CommandError>;
    pub fn redo(&mut self) -> Result<(), CommandError>;
    pub fn begin_group(&mut self, label: &str) -> GroupToken; // several commands → one undo step
}
```

- The CLI builds its subcommand tree from the registry's JSON schemas at build time (via a
  `build.rs` that snapshots the schema) and at run time for `planyrith tool <name> --help`.
- The RPC server exposes `command.execute`, `command.list`, `command.schema`, `undo`, `redo`,
  `document.*` queries, `events.subscribe`.
- The scripting binding (`rhai`) registers one function per command from the same registry.

### 3.3 Interactive tools are state machines over commands

In the GUI, a **Tool** (e.g. `MoveTool`) is a state machine driven by pointer/keyboard events
and the inference engine. It accumulates a `Params` value and, at the final click or Enter,
calls `engine.execute::<MoveCommand>(params)`. During dragging it shows a preview via the
preview path. No tool contains geometry code; all of it lives in the core command.

Tools are described declaratively enough that `planyrith-cli` can print the interactive
step sequence for each tool (`planyrith tool move --steps`) from the same metadata.

## 4. Document and entities (summary; details in `docs/04`)

- `Document` = `Model` (root `Entities`) + `ComponentDefinitions` + `Materials` + `Tags` +
  `Scenes` + `Styles` + `ModelInfo` + `AttributeDictionaries`.
- `Entities` is an ordered container keyed by typed ids; iteration order is insertion order
  (stable). Internally a slot map with generation counters; ids are 64-bit `(index, gen)`.
- Hierarchy: `Group` and `ComponentInstance` reference an `Entities` container (groups own a
  private `ComponentDefinition`; instances share one). Transforms are `DAffine3`.
- Geometry inside one `Entities` is a single `HalfEdgeMesh` (`docs/03`) plus non-mesh entities
  (guides, dimensions, text, images, section planes).

## 5. Data flow

```
pointer/keys ──► Tool (gui) ──► Params ──► Engine.execute ──► Transaction ──► Deltas
                                                                    │
                      EventBus ◄── DocumentChanged{deltas} ◄────────┘
                         │
          ┌──────────────┼─────────────────┬────────────────────┐
          ▼              ▼                 ▼                    ▼
   SpatialIndex     RenderMesh cache   Panels (outliner,   Autosave journal
   incremental      incremental        materials, tags)
```

- **RenderMesh extraction** (`io::render_mesh`) is incremental: deltas invalidate per-
  `Entities`-container caches; unchanged containers are reused. Output buffers are plain
  `Vec<f32>`/`Vec<u32>` so the core stays GPU-agnostic.
- **Spatial indexes** update incrementally from deltas; a full rebuild is a fallback when more
  than 30 % of a container changed.

## 6. Concurrency model

- The `Document` is single-writer. The GUI owns the `Engine` on the main thread. Long operations
  (import, boolean on large solids, render export) run on a worker with a *forked* document and
  return a `Transaction` to be committed on the main thread; the UI shows progress and allows
  cancel via a `CancelToken`.
- Parallelism inside algorithms uses `rayon` behind the `parallel` feature; on wasm the facade
  falls back to sequential iterators. Results must be identical either way (no reduction-order
  dependence; use deterministic merges).
- The CLI is single-threaded per command except for `rayon`-internal parallelism.

## 7. Error model

```rust
#[derive(thiserror::Error, Debug)]
pub enum CommandError {
    #[error("invalid parameter `{name}`: {reason}")] InvalidParam { name: String, reason: String },
    #[error("entity not found: {0}")] NotFound(EntityRef),
    #[error("geometry error: {0}")] Geometry(#[from] GeometryError),
    #[error("operation not applicable: {0}")] NotApplicable(String),
    #[error("cancelled")] Cancelled,
    #[error("io: {0}")] Io(#[from] IoError),
}
```

- Errors are values; the core never panics on bad input. Fuzzers assert this.
- Commands may return **warnings** inside `Output` (e.g. "3 faces could not be merged") without
  failing. The CLI prints them to stderr; the GUI shows a non-blocking toast.
- Every error has a stable machine-readable code (`E1001`…) listed in `docs/08 §9` for scripts.

## 8. Events

`EventBus` is synchronous and in-process. Event payloads carry deltas, never full snapshots.
Frontends and the RPC server fan events out to subscribers. Event kinds:
`DocumentChanged`, `SelectionChanged`, `ActiveContextChanged` (entered/exited a group),
`SceneActivated`, `CameraChanged` (GUI-local, not part of the document), `ToolChanged`,
`Progress`, `Warning`.

## 9. Versioning and compatibility

- Crates share one version (workspace). Public API stability starts at 1.0.
- The native file format has its own `format_version` with forward-compatible unknown-field
  skipping and backward migrations (`docs/09 §4`).
- The command registry has a `schema_version`; RPC clients negotiate it.

## 10. Build targets and features

| Target | Features | Notes |
|---|---|---|
| Linux/macOS/Windows desktop | `parallel`, `brep`, `scripting` | default for `planyrith` and `planyrith-gui` |
| `wasm32-unknown-unknown` | `scripting` | no `parallel`, `brep` optional (size budget) |
| `planyrith` server mode | same as desktop | `planyrith serve --stdio` |

## Decisions

- 2026-10-20 — Commands are the only mutation path; undo via inverse deltas (not snapshots).
- 2026-10-20 — A shared `planyrith-render` crate is added at 0.2 so the CLI can render headless
  without depending on the GUI crate. The user-proposed three-way split is preserved
  conceptually: `render` is part of the "GUI tier" but has no UI.
- 2026-10-20 — GUI links the core in-process by default; RPC attach is optional.
- 2026-10-20 — `planyrith-render` is scaffolded at 0.1 (empty library with its dependency
  table wired) rather than created at 0.2, so the CLI and GUI manifests can depend on it from
  the first commit and the boundary check in CI covers all four crates from the start.
- 2026-10-20 — Skeleton contract (`docs/dev/skeleton-contract.md`): the `Command` trait exposes
  `fn name(&self) -> &'static str` instead of an associated `const NAME`, so the object-safe
  `ErasedCommand` is a blanket impl over the same type and `CommandRegistry` stores one box per
  command. `Engine::execute::<C>` therefore requires `C: Default` (commands are stateless);
  `Engine::execute_with(&cmd, params)` exists for non-default instances.
- 2026-10-20 — Every tool's `Params` flattens `commands::CommonParams { context, dry_run }`
  (docs/05 §1). The CLI `--dry-run` calls `Engine::validate_erased` only.
- 2026-10-20 — `Delta` is a closed enum of elementary, invertible changes carrying before/after
  values, pids and order indices (never whole-document snapshots). `EntityId`s may change across
  undo/redo; `Pid`s do not (docs/04 Decisions).
- 2026-10-22 — **Undo/redo bookkeeping.** Undo and redo are all-or-nothing: an entry that fails
  part-way is reverted before the error is returned and the history cursor does not move. A failed
  command is rolled back through the session's alias table (rollback re-creates edges and faces
  under new ids that older history entries must find), and re-inserting a definition snapshot
  makes its mesh ids the current members of their alias classes. `DocumentChanged` events after
  undo/redo carry the deltas as applied, with the ids the elements have now, so incremental
  consumers (spatial index, render cache) can use them like the events of executed commands.
