# Development status and handoff

**Audience:** the next implementing agent/session. This file records exactly where the draft
implementation stands, what is done, what remains, and how to resume. Update it at the end of
every working session. Ground truth for *what to build* stays in `docs/00`–`docs/15`; this file
is only *progress state*.

_Last updated: 2026-10-20 (mesh → CLI draft session)._

## 1. Where we are

Milestone 0.1 ("core kernel and CLI foundation", `docs/13-roadmap.md`) is **in progress** and
there is a **runnable headless draft**: `planyrith new/info/query/tool/export/validate/repair/batch`
work on `.plyr` files. A batch builds a table (rectangles + push-pull) deterministically
(byte-identical files across runs) and `validate` reports it clean. `planyrith-core` and
`planyrith-cli` are clippy-clean; 286 core tests + 7 CLI tests pass.

Build/test commands used in this project (keep using these; do not build the GUI/render crates
while working on the core — they pull heavy GPU deps; the CLI no longer depends on render unless
built with `--features render`):

```sh
cargo check   -p planyrith-core
cargo test    -p planyrith-core
cargo test    -p planyrith-cli          # integration tests in crates/planyrith-cli/tests
cargo clippy  -p planyrith-core -p planyrith-cli --all-targets
cargo fmt --all
PLANYRITH_UPDATE_GOLDEN=1 cargo test -p planyrith-cli   # accept a changed golden export
```

## 2. Module status (`planyrith-core`)

| Owner key | Files | Status | Notes |
|---|---|---|---|
| foundations | `ids.rs`, `units.rs`, `tolerance.rs`, `math/*` | **DONE** | Property-tested. |
| spatial | `spatial/*` | **DONE** | parry3d-f64 BVHs; handles `MeshDelta::VerticesMoved`. Some tests still build indexed fixtures through serde (could now use `add_edge`). |
| io-native | `io/storage.rs`, `io/native/*`, `io/export/json.rs`, `io/mod.rs` | **DONE (draft form)** | `document.msgpack` is still the derived serde of `Document` (slot layout included). Journal rows now carry `created` pids. TODO: write the index-based `EntitiesSer` form via `HalfEdgeMesh::to_indexed` so files are independent of slot layout and byte-identical after undo/redo (docs/12 §2). |
| mesh | `geom/mesh.rs` + `geom/mesh/{edit,indexed,analysis,replay,tests}.rs`, `geom/tessellate.rs` | **DONE** | All contract ops; delta replay + `ReplayIds`; decisions in docs/03. |
| planar | `geom/planar.rs` | **DONE** | Arrangement-based face finding, orientation conflict resolution, `reverse_faces`. |
| extrude | `geom/extrude.rs` | **DONE** | Stretch / generic / join-push-through cases, coincident-strip cleanup, curve seams (docs/03 Decisions). |
| document | `document/*` | **DONE** | `StableMap` (stable ids across undo), hierarchy, path resolution, tags, materials, images, camera rays, sun position, selection query language + entity paths (docs/08 §5, implemented by a sub-agent; its decisions are in docs/08). |
| commands | `commands/mod.rs`, `transaction.rs`, `params.rs`, `draw/*`, `edit/*` | **PARTIAL** | Engine (execute/undo/redo/groups/context/events), transactions with pid bookkeeping, CLI parameter types. Commands: `line`, `rectangle`, `circle`, `polygon`, `arc-2point`, `arc-3point`, `arc-bulge`, `pie`, `push-pull`, `select`. |
| diagnostics | `diagnostics.rs` | **DONE** | validate / repair / statistics. |
| later | `geom/{intersect,boolean,offset,sweep,brep}.rs`, `inference/*`, `io/render_mesh.rs`, `io/import/*`, `io/export/*` (non-json), `scripting/*`, `commands/{organize,annotate,view,file}` | **NOT STARTED** | `offset` is a 0.1 tool. |

Frontends:

| Crate | Status |
|---|---|
| `planyrith-cli` | **DRAFT** — session verbs `new`, `info`, `query`, `tool`, `export` (json), `validate`, `repair`, `batch`; schema-driven tool options; JSON envelopes; exit codes. Not yet: `import`, `render` (0.2), `script` (0.6), `serve`, `repl`. |
| `planyrith-render` | scaffold only. Not started (0.2). |
| `planyrith-gui` | scaffold `main.rs`. Not started (0.2). |

## 3. Remaining work for milestone 0.1 (in order)

1. **Editing commands**: `erase` (+ soften/hide modes), `delete`, `move` / `rotate` / `scale`
   (vertices/edges/faces via `transform_vertices`; groups/instances via `set_transform`; `copy`,
   `copies`, linear arrays), `offset` (implement `geom/offset.rs` first).
2. **Organize commands**: `make-group`, `make-component` (move selected geometry into a new
   definition: copy edges/faces into the definition mesh, erase them from the source, insert the
   group/instance), `explode`, `tag-*` (add, rename, delete, assign, visibility), `material-*`
   (color: add, edit, apply to faces/instances, delete).
3. **io-native**: index-based `EntitiesSer` via `to_indexed`/`from_indexed`; test
   apply → undo → byte-identical `.plyr` (docs/12 §2).
4. **Acceptance batch**: the furnished room of docs/13 0.1 (walls with openings, a table, chairs as
   components in arrays) as a CLI integration test; reopen → identical; `validate` clean.
5. Fuzz targets for the kernel (cargo-fuzz needs nightly; at least a long proptest sequence of
   random add/erase/move ops with `validate` after each).

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
