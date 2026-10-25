# Development status and handoff

**Audience:** the next implementing agent/session. This file records exactly where the draft
implementation stands, what is done, what remains, and how to resume. Update it at the end of
every working session. Ground truth for *what to build* stays in `docs/00`–`docs/15`; this file
is only *progress state*.

_Last updated: 2026-10-25 (release 0.3.0: inference engine, `infer`, interactive GUI tools)._

## 1. Where we are

Milestone 0.1 ("core kernel and CLI foundation", `docs/13-roadmap.md`) is **released as
0.1.0** (see `CHANGELOG.md`): every 0.1 command exists, and the acceptance test of docs/13 passes
(`crates/planyrith-cli/tests/cli.rs::furnished_room_acceptance`: a batch builds walls with a door
and a window, a table group and four chairs as instances of one component; the file validates
clean and re-saves byte-identically). Undo/redo is byte-exact for every 0.1 command
(`commands::undo_tests`). A randomized kernel test (`geom/mesh/tests.rs`,
`random_operation_sequences_keep_the_mesh_valid`: random edges, rectangles, erasures, vertex
moves, push/pull and offsets) stands in for cargo-fuzz; 100 000 cases ran clean on 2026-10-21.
Every registered command has a CLI golden test (`tests/commands/<name>.plyrb` →
`tests/golden/commands/<name>.json`). Transcendental math goes through `libm` so output is
bit-identical across OSes (docs/02 Decisions). An engine-level fuzz test
(`commands/fuzz_tests.rs`, `random_command_sequences`) runs random sequences of all 0.1 command
families and checks validation after every command, unchanged bytes after a failed one, replay,
save/reopen equivalence and undo/redo; 10 000 sequences ran clean on 2026-10-25. Every command is
also reachable through `planyrith serve` (JSON-RPC over stdio).

Build/test commands used in this project (do not build the GUI/render crates while working on
the core — they pull heavy GPU deps; the CLI does not depend on render unless built with
`--features render`):

```sh
cargo test    -p planyrith-core
cargo test    -p planyrith-cli          # integration tests in crates/planyrith-cli/tests
cargo clippy  -p planyrith-core -p planyrith-cli --all-targets
cargo fmt --all
PLANYRITH_UPDATE_GOLDEN=1 cargo test -p planyrith-cli    # accept a changed golden export
PLANYRITH_FUZZ_CASES=20000 cargo test -p planyrith-core --release random_operation   # long kernel fuzz
PLANYRITH_FUZZ_CASES=10000 cargo test -p planyrith-core --release random_command_sequences   # engine fuzz
```

## 2. Module status (`planyrith-core`)

| Area | Files | Status | Notes |
|---|---|---|---|
| foundations | `ids.rs`, `units.rs`, `tolerance.rs`, `math/*` | **DONE** | Property-tested. |
| spatial | `spatial/*` | **DONE** | parry3d-f64 BVHs; handles `VerticesMoved`. |
| io-native | `io/storage.rs`, `io/native/*`, `io/export/json.rs` | **DONE** | Containers are written as `EntitiesSer` (indexed mesh in canonical order, mesh entities by index); `StableMap` writes live pairs only; journal rows carry `created` pids. |
| mesh / planar / tessellate | `geom/mesh.rs` + `geom/mesh/*`, `geom/planar.rs`, `geom/tessellate.rs` | **DONE** | Decisions in docs/03. |
| extrude / offset | `geom/extrude.rs`, `geom/offset.rs` | **DONE** | Push/pull cases and 2D offset with clipping (docs/03 Decisions). |
| document | `document/*` | **DONE** | `StableMap` stable ids; query language (docs/08 §5). |
| commands | `commands/*` | **DONE for 0.1** | Engine, transactions, CLI parameter types; `line`, `rectangle`, `circle`, `polygon`, `arc-2point`, `arc-3point`, `arc-bulge`, `pie`, `push-pull`, `offset`, `select`, `erase`, `delete`, `hide`, `unhide`, `lock`, `unlock`, `move`, `rotate`, `scale`, `make-group`, `make-component`, `explode`, `tag-*`, `material-*`. |
| diagnostics | `diagnostics.rs` | **DONE** | validate / repair / statistics. |
| later | `geom/{intersect,boolean,sweep,brep}.rs`, `inference/*`, `io/render_mesh.rs`, `io/import/*`, `io/export/*` (non-json), `scripting/*`, `commands/{annotate,view,file}` | **NOT STARTED** | Milestones 0.2+. |

Frontends:

| Crate | Status |
|---|---|
| `planyrith-cli` | **0.1.0** — `new`, `info`, `query`, `tool`, `export` (json), `validate`, `repair`, `batch`, `serve` (stdio); schema-driven tool options; JSON envelopes; exit codes. Later: `render` and the socket transport/`repl` (0.2), `import` (0.5), `script` (0.6). |
| `planyrith-render` | **0.2.0** — offscreen renderer (styles, edges with GPU profiles, transparency, selection, id buffer), headless `render_image`; snapshot tests on three OSes. Not yet: shadows, section caps, fog, sky, watermarks. |
| `planyrith-gui` | **0.3.0** — eframe shell, `egui_tiles` docks, viewport (navigation, selection, double-click to open), Entity Info, Tags, Scenes, Outliner, Tool Options, Tool Help, undo/redo, open/save. 0.3: `tools/` (one state machine per 0.1 command: Select, Line, Rectangle, Circle, Polygon, the four arcs, Push/Pull, Offset, Move, Rotate, Scale, Eraser, Paint), inference overlay, value field with replace-after-completion, previews on a forked engine (`Session::preview`). Tested with synthetic egui input, including a whole-window test (`app::tests`) that draws, pushes, corrects and moves a box. |

## 3. What next

1. **Milestone 0.3** (docs/13): done — the inference engine and `infer`, the value field, a tool
   per 0.1 command with previews, modifiers and shortcuts, Tool Help, `Tab` cycling, and the
   "model a room in 10 steps" QA script (`docs/qa/model-a-room.md`) with its automated twin
   (`app::tests::qa_model_a_room_in_ten_steps`); released as 0.3.0. User feedback on
   2026-10-25: the engine works very well, the GUI looks basic. Next: a GUI polish pass on what
   docs/07 already specifies (icon toolbars, viewport axes and background, cursor feedback,
   semi-transparent previews, context menu, theme), then milestone 0.4 (organization and
   materials).
2. **Journal and undo.** The journal (`history.msgpack`, docs/09 §3) is append-only and every
   frontend journals executed commands but not undo/redo, so after an undo it lists a command
   that is no longer in the document. Decide whether undo/redo are journaled (entries) or the
   journal follows the undo stack, for the GUI, `serve` and `repl` alike.
3. **Keep the fuzzers running** after kernel changes (both commands above). Work order in the
   kernel must stay geometric (docs/03 Decisions, slot order): the engine fuzz's save/reopen
   check catches any new dependence on slot layout.
4. Review the "unmaintained" advisories `cargo deny check` may report for GUI-side transitive
   dependencies with the 0.2 GUI work (replace, or add dated `deny.toml` ignores with a reason).
5. Open design question recorded in docs/03 Decisions: `reverse-faces` on part of a connected
   face group (needs either whole-group reversal or decoupling the front side from the loop
   direction) — decide before milestone 0.4.

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
- **Ids:** everything except mesh edges/faces keeps its id across undo/redo (`StableMap`);
  edge/face ids named by deltas resolve through the engine's session-wide `MeshAliases`.
  Commands address entities by pid or query; never store mesh ids in persistent data. A split
  face or edge keeps its pid on one piece (docs/03 Decisions); `last` skips edge fragments.
- **Math in the core:** call `libm::sin` etc., never `f64::sin` (`crates/planyrith-core/clippy.toml`
  rejects it) — cross-platform byte-identical output depends on it.
- **New commands:** follow `commands/edit/push_pull.rs` (EntityRef) or
  `commands/organize/tags.rs` (Sel): resolve with `commands::params::target_container` and
  `commands::common::targets_in`, mutate only through `Transaction`, run multi-step kernel work in
  one `tx.with_mesh(… mesh.transact(…))`, register in the category's `register`, add an
  engine-level test and a step in `commands/undo_tests.rs`.
- Use the foundations helpers everywhere (`Tolerances::*`, `Plane::frame`/`to_2d`/`from_2d`,
  `math::intersect::segment_segment`, `math::polygon::*`). They are complete and tested.
- `cargo check -p planyrith-core` is fast; the GUI/render crates compile egui/wgpu and are slow —
  avoid them until working on those crates.

## 4. Repository state

- Branch `main`; releases `v0.1.0`, `v0.2.0` and `v0.3.0` tagged and pushed (2026-10-25). The user wants finished work
  pushed, in batches: run the full local check once per batch, then push once (each push to
  `main` runs the CI matrix; tags do not).
- The 0.1 review fixes were made on a temporary branch and worktree, fast-forwarded into `main`
  and removed; the repo has only `main`.
- The multi-agent workflow run `wf_54e380cb-960` produced the foundations/spatial/io-native
  commits. Later sub-agents implemented the selection query language (merged) — two more were
  started for offset and tags/materials but stopped on a usage limit before writing anything;
  that work was done directly.
- Commits from that workflow carry a `Co-Authored-By: Claude Fable 5.1` line; later commits use
  the model in use at the time. Attribution lines are harness-provided and not significant.

## 5. How to resume with multiple agents (optional)

The orchestration script is saved at
`.../workflows/scripts/planyrith-foundation-wf_54e380cb-960.js` (path in the run's tool result).
After the weekly limit resets, a fresh workflow can fan out the critical-path steps above — but
the mesh topology is a single tightly-coupled unit and is best implemented by one agent/session,
then document + commands can fan out. Keep the "owner key / disjoint files" discipline and the
integration step from the previous run.
