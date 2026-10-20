# Development status and handoff

**Audience:** the next implementing agent/session. This file records exactly where the draft
implementation stands, what is done, what remains, and how to resume. Update it at the end of
every working session. Ground truth for *what to build* stays in `docs/00`–`docs/15`; this file
is only *progress state*.

_Last updated: 2026-10-20 (mesh + face finding session)._

## 1. Where we are

Milestone 0.1 ("core kernel and CLI foundation", `docs/13-roadmap.md`) is **in progress**. The
workspace builds. `planyrith-core` compiles, is clippy-clean, and its implemented modules pass
their tests (227 tests); the rest are skeleton stubs (`todo!("docs/0X §Y")`) that compile but
panic if called. The half-edge mesh and face finding — the former critical path — are done.

Build/test commands used in this project (keep using these; do not build the GUI/render crates
while working on the core — they pull heavy GPU deps):

```sh
cargo check   -p planyrith-core
cargo test    -p planyrith-core
cargo clippy  -p planyrith-core --all-targets
cargo fmt --all
```

## 2. Module status (`planyrith-core`)

| Owner key | Files | Status | Notes |
|---|---|---|---|
| foundations | `ids.rs`, `units.rs`, `tolerance.rs`, `math/*` | **DONE** | Full value-grammar parsing/formatting, tolerances, planes/rays/segments/aabb/polygon/intersections, transforms. Property-tested. |
| spatial | `spatial/*` | **DONE** | parry3d-f64 BVHs over edges/faces, ray/segment/aabb queries, instance BVH, picking tie-breaks. Some tests build their own indexed fixtures because the mesh ops they would prefer are still stubs — revisit once mesh is done. |
| io-native | `io/storage.rs`, `io/native/*`, `io/export/json.rs`, `io/mod.rs` | **DONE** | `.plyr` zip container (deterministic), `NativeFs`/`MemoryStorage`, manifest + blake3 checksums, msgpack named-field, canonical JSON, migration scaffold (v1), `E1401/E1402` handling. |
| mesh | `geom/mesh.rs` (+ `geom/mesh/{edit,indexed,analysis,replay,tests}.rs`), `geom/tessellate.rs` | **DONE** | All accessors and operations of the contract: `add_vertex`/`add_edge` (splitting, overlaps, face finding), `erase_edge` (merge/delete rules), `erase_face`, `split_face_by_chord`, `merge_faces`, `transform_vertices` (merging, auto-fold, optional intersection check), curves, `validate`, `is_solid`, `volume`, `from_indexed`/`to_indexed`, `from_triangles_merged`. New: `MeshDelta::VerticesMoved`, `MeshDelta::inverse`, `HalfEdgeMesh::replay`/`replay_all` + `ReplayIds` (undo/redo by position). Public ops are atomic (snapshot rollback); test builds validate after every op. Decisions recorded in docs/03. |
| planar | `geom/planar.rs` | **DONE** | Face finding on the planar arrangement (seeded new faces, splits, holes, orientation conflict resolution), `candidate_planes`, `minimal_cycles`, `sort_star_by_angle`, 2D helpers. |
| document | `document/*` | **NOT STARTED** | Types are skeleton. Entities/hierarchy/materials/tags/scenes/styles/attributes/camera/shadows/selection + the selection Query parser/evaluator (`docs/08 §5`) all `todo!()`. Depends on the mesh API. |
| later | `geom/intersect.rs`, `geom/boolean.rs`, `geom/offset.rs`, `geom/sweep.rs`, `geom/extrude.rs`, `geom/brep.rs`, `inference/*`, `io/render_mesh.rs`, `io/import/*`, `io/export/*` (non-json), `commands/*`, `diagnostics.rs`, `scripting/*` | **NOT STARTED** | `extrude.rs` (push-pull) and `offset.rs` are 0.1 tools; build them on the mesh primitives (`pub(crate)` `new_edge`, `create_face`, `attach_face`/`detach_face`, `split_edge_at`, `transact`, `planar::arrangement_regions`). |

Frontends:

| Crate | Status |
|---|---|
| `planyrith-render` | scaffold only (`lib.rs` doc comment). Not started. |
| `planyrith-cli` | scaffold `main.rs` (prints version). The `tool`/`query`/`import`/`export`/session verbs (`docs/08`) are not started; they depend on the command registry (`commands/*`). |
| `planyrith-gui` | scaffold `main.rs`. Not started. |

## 3. Critical path to a first runnable draft

Steps 1–2 (mesh topology, face finding) are **done**. Remaining order (each step compiles + tests
before the next):

3. **`document/*`** — entities + hierarchy first, then the Query parser/evaluator, then
   materials/tags/scenes/styles. Unblocks a real `Document` for io-native round-trip tests
   (`io::native` can now call `HalfEdgeMesh::to_indexed`/`from_indexed`).
4. **`commands/*`** — `Command`/`ErasedCommand`/`Transaction`/`UndoStack`/`Engine`/registry,
   then the draw/edit/organize commands of `docs/05` (each wraps mesh/document ops and records
   deltas via the `DeltaSink`). `Transaction` implements `DeltaSink` to wrap mesh deltas into
   document deltas. Pid rules that follow from the mesh decisions (docs/03 Decisions):
   `FaceRemoved { id }` followed by `FaceAdded { id }` with the same id keeps the pid (reshape);
   `FaceSplit { old, new: [old, other] }` — `other` gets a fresh pid; `FacesMerged { kept,
   removed }` — `removed`'s pid is unbound. Undo/redo apply `MeshDelta::inverse()` in reverse /
   the deltas in order through `HalfEdgeMesh::replay_all` with one `ReplayIds` kept per container
   (or seeded from pid bindings).
5. **`geom/extrude.rs`, `geom/offset.rs`** — push-pull and offset (0.1 tools).
6. **`planyrith-cli`** — generate subcommands from the registry (`docs/08 §2`), `batch`,
   `query`, native open/save. First CLI golden test: build a shape headless, reopen, compare.

### Design notes that save time

- **Deltas are position-based, not id-based** (`geom/mod.rs` top comment): replay restores the
  mesh because invariant 3 (no two vertices within `length`) makes positions a stable identity.
  New kernel code must record through the `pub(crate)` primitives of `geom/mesh.rs`
  (`new_vertex`, `new_edge`, `delete_edge`, `attach_face`, `detach_face`, …), which emit the
  right deltas; the undo test pattern in `geom/mesh/tests.rs`
  (`every_operation_replays_backwards_and_forwards`) checks a new op in a few lines.
- **Loop connectivity is set at face creation.** `next`/`prev` are authoritative only inside a
  live face loop; free half-edges point them at their twin.
- **Face finding** never re-creates an erased face unless the caller drew or re-drew one of its
  edges (seed rule); see the docs/03 decision for the full algorithm.
- Use the foundations helpers everywhere (`Tolerances::*`, `Plane::frame`/`to_2d`/`from_2d`,
  `math::intersect::segment_segment`, `math::polygon::*`). They are complete and tested.
- `cargo check -p planyrith-core` is fast; the GUI/render crates compile egui/wgpu and are slow —
  avoid them until working on those crates.

## 4. Repository state

- Branch `main`; local commits ahead of `origin/main` are **not pushed** (push only when the
  draft builds and tests green, per the user's standing rule).
- The multi-agent workflow run `wf_54e380cb-960` produced the foundations/spatial/io-native
  commits (merged) and left mesh/document unimplemented because it hit a weekly usage limit.
  Its temporary worktrees/branches were removed; the repo has only `main`.
- Commits from that workflow carry a `Co-Authored-By: Claude Fable 5.1` line; later commits use
  the model in use at the time. Attribution lines are harness-provided and not significant.

## 5. How to resume with multiple agents (optional)

The orchestration script is saved at
`.../workflows/scripts/planyrith-foundation-wf_54e380cb-960.js` (path in the run's tool result).
After the weekly limit resets, a fresh workflow can fan out the critical-path steps above — but
the mesh topology is a single tightly-coupled unit and is best implemented by one agent/session,
then document + commands can fan out. Keep the "owner key / disjoint files" discipline and the
integration step from the previous run.
