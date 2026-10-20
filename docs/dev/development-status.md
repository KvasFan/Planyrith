# Development status and handoff

**Audience:** the next implementing agent/session. This file records exactly where the draft
implementation stands, what is done, what remains, and how to resume. Update it at the end of
every working session. Ground truth for *what to build* stays in `docs/00`–`docs/15`; this file
is only *progress state*.

_Last updated: 2026-10-20._

## 1. Where we are

Milestone 0.1 ("core kernel and CLI foundation", `docs/13-roadmap.md`) is **in progress**. The
workspace builds. `planyrith-core` compiles and its implemented modules pass their tests; the
rest are skeleton stubs (`todo!("docs/0X §Y")`) that compile but panic if called.

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
| mesh | `geom/mesh.rs`, `geom/tessellate.rs` | **PARTIAL** | `tessellate.rs` DONE (ear clipping + cache + positions hash; soft-edge normals deferred to the renderer, see its doc note). `mesh.rs`: all **types** are final (the contract); all `impl HalfEdgeMesh` **operation bodies are still `todo!()`** — accessors, loop walks, `add_vertex`/`add_edge`/`erase_*`/`transform_vertices`/`split_face_by_chord`/`merge_faces`/`add_face_from_loops`/`add_curve`/`validate`/`is_solid`/`volume`/`from_indexed`/`to_indexed`/`from_triangles_merged`. **This is the critical path.** |
| document | `document/*` | **NOT STARTED** | Types are skeleton. Entities/hierarchy/materials/tags/scenes/styles/attributes/camera/shadows/selection + the selection Query parser/evaluator (`docs/08 §5`) all `todo!()`. Depends on the mesh API. |
| later | `geom/planar.rs`, `geom/intersect.rs`, `geom/boolean.rs`, `geom/offset.rs`, `geom/sweep.rs`, `geom/extrude.rs`, `geom/brep.rs`, `inference/*`, `io/render_mesh.rs`, `io/import/*`, `io/export/*` (non-json), `commands/*`, `diagnostics.rs`, `scripting/*` | **NOT STARTED** | `planar.rs` has the pure 2D helpers (`signed_area_2d`, `loop_orientation`, `default_face_normal`) done; `find_faces`/`minimal_cycles`/`candidate_planes`/`sort_star_by_angle`/`point_in_polygon_2d`/`polygon_contains_polygon_2d` are `todo!()`. |

Frontends:

| Crate | Status |
|---|---|
| `planyrith-render` | scaffold only (`lib.rs` doc comment). Not started. |
| `planyrith-cli` | scaffold `main.rs` (prints version). The `tool`/`query`/`import`/`export`/session verbs (`docs/08`) are not started; they depend on the command registry (`commands/*`). |
| `planyrith-gui` | scaffold `main.rs`. Not started. |

## 3. Critical path to a first runnable draft

Recommended order (each step compiles + tests before the next):

1. **`geom/mesh.rs` topology** — implement in this order:
   a. Accessors/walks: `edge_faces`, `loop_half_edges`, `loop_vertices`, `loop_edges`,
      `face_vertices`, `face_edges`, `face_neighbors`, `face_area`, `face_snapshot`,
      `curve_edges`, `curve_is_intact`.
   b. Private primitives (add a `pub(crate)` impl block): make-edge (edge + twin half-edges +
      star update), find-half-edge-from-to, raw remove edge/face, build-loop (set `next`/`prev`
      and `face` around a cycle), split-edge-at.
   c. `add_vertex`, `add_face_from_loops`, `set_edge_flags`, `set_face_sides`, `add_curve`,
      `drop_curve`.
   d. Bulk: `from_indexed`/`to_indexed` (round-trips the file format — unblocks io-native's
      real round-trip tests), `from_triangles_merged` (unblocks importers).
   e. Analysis: `validate`, `is_solid`, `volume`.
   f. Interactive ops: `add_edge` (split against existing geometry, then call face finding),
      `erase_edge` (merge/delete rules), `erase_face`, `transform_vertices` (auto-fold),
      `split_face_by_chord`, `merge_faces`.
2. **`geom/planar.rs`** — `point_in_polygon_2d`/`polygon_contains_polygon_2d` (delegate to
   `math::polygon::{point_in_ring, ring_contains_ring}`), `sort_star_by_angle`,
   `candidate_planes`, `minimal_cycles`, `find_faces`. `add_edge` needs `find_faces`.
3. **`document/*`** — entities + hierarchy first, then the Query parser/evaluator, then
   materials/tags/scenes/styles. Unblocks a real `Document` for io-native round-trip tests.
4. **`commands/*`** — `Command`/`ErasedCommand`/`Transaction`/`UndoStack`/`Engine`/registry,
   then the draw/edit/organize commands of `docs/05` (each wraps mesh/document ops and records
   deltas via the `DeltaSink`). The `MeshDelta` + `DeltaSink` plumbing in `geom/mod.rs` is the
   bridge; `Transaction` implements `DeltaSink` to wrap mesh deltas into document deltas.
5. **`planyrith-cli`** — generate subcommands from the registry (`docs/08 §2`), `batch`,
   `query`, native open/save. First CLI golden test: build a shape headless, reopen, compare.

### Design notes that save time

- **Deltas are position-based, not id-based** (`geom/mod.rs` top comment): replay restores the
  mesh because invariant 3 (no two vertices within `length`) makes positions a stable identity.
  Honor this when implementing ops — record `VertexAdded { position }` etc.
- **Loop connectivity is set at face creation.** `next`/`prev` are authoritative only inside a
  live face loop; half-edges with `face = None` should not be walked via `next`. Recompute loop
  wiring from geometry whenever you create/split/merge a face rather than trying to maintain a
  global rotation system incrementally — simpler and robust for the draft (record this as a
  decision in `docs/03` when you implement it).
- **Face finding** (`planar::find_faces`) only needs to accept cycles that involve a
  newly-touched edge, so it never recreates or scans unrelated existing faces. Handle the three
  cases of `docs/03` §4.3–§4.5 explicitly: new empty-space loop → new face; chord between two
  boundary vertices of a face → split; closed loop strictly inside a face → hole + inner face.
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
