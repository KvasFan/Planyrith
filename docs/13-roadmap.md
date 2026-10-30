# 13 — Roadmap and milestones

**Audience:** implementing agents and maintainers. Each milestone lists deliverables and
acceptance criteria. Milestones are sequential; within one, items are independent unless noted.

## 0.0.0 — Specification
- Complete `docs/`, empty workspace scaffold, licensing policy, CI skeleton definition.
- **Accept:** `cargo check` passes; docs reviewed; repository public.

## 0.1.0 — Core kernel and CLI foundation
- `ids`, `units`, `tolerance`, `math`, `geom::mesh`, `geom::planar` (face finding), `split`,
  `merge`, `erase`, `transform_vertices` with auto-fold, `tessellate`, `spatial` indexes.
- `document` with entities (edges/faces/groups/instances), tags, materials (color only), scenes
  (camera only), attributes, pids.
- Command system (`Command`, registry, transactions, undo), commands: `line`, `rectangle`,
  `circle`, `polygon`, `arc-*`, `erase`, `delete`, `select`, `move`, `rotate`, `scale`,
  `push-pull`, `offset`, `make-group`, `make-component`, `explode`, `tag-*`, `material-*` (color).
- Native `.plyr` read/write, `json` export, `validate`/`repair`.
- CLI: all session verbs except `render`; `batch`; `query`; JSON output; exit codes.
- Tests: property tests for kernel, CLI goldens, undo/redo byte-equality, fuzz targets.
- **Accept:** a batch script builds a furnished room (walls with openings, table, chairs as
  components in arrays) headless; file reopens identically; `validate` clean.

## 0.2.0 — Viewport
- `planyrith-render`: extraction, opaque/edge passes, camera, picking (CPU + ID buffer), styles
  (shaded, textured, hidden-line, wireframe, monochrome, X-ray), headless `render`.
- `planyrith-gui`: eframe shell, egui_tiles docks, viewport, orbit/pan/zoom, selection, Entity
  Info, Tags, Scenes (camera), Outliner/Hierarchy, undo/redo, open/save, status bar.
- **Accept:** open the 0.1 room, navigate at 60 FPS, select and delete entities, save; headless
  render snapshot tests pass on three OSes.

## 0.3.0 — Interactive modeling with inference
- Inference engine (all kinds, locks, references), value field, tool state machines for every
  0.1 command, previews, modifiers, shortcuts, context editing (enter/exit groups).
- Tool Help panel from metadata; `--steps` output.
- **Accept:** the "model a room in 10 steps" QA script completes with mouse+keyboard only; typed
  lengths are exact; inference golden tests pass.

## 0.4.0 — Organization and materials
- Components browser and library files (`.plyrc`), make unique, reload, glue/cut behavior,
  always-face-camera; scenes with full property capture and transitions; tag folders.
- Materials with textures, UV positioning tool, Paint tool fills, image entities, material libraries.
- Styles editor (edges, faces, background, watermarks), fog.
- **Accept:** cabinet fixture built from components with textures; scenes animate; materials panel
  round-trips `.plyrm`.

## 0.5.0 — Interchange
- Importers/exporters: OBJ, STL, glTF/GLB, DXF (3D), images, CSV, `.plyr` merge; options; fuzzing.
- 2D export of a view (SVG/PDF/DXF) with hidden-line removal.
- **Accept:** round-trip tests per format; external validators pass; 100 MB glTF imports < 20 s.

## 0.6.0 — Sections, annotations, shadows
- Section planes (cut, caps, slices to groups), dimensions (linear/aligned/radial, re-measuring),
  text (screen/leader), 3D text, guides (tape measure, protractor, axes), sun/shadow model,
  multiple viewports.
- **Accept:** section cut of the house renders with caps; dimensions update when geometry moves;
  shadow direction matches the solar algorithm within 0.5° on test dates.

## 0.7.0 — Web build
- wasm target for GUI, IndexedDB storage, file pickers, service worker, size budget, touch basics,
  autosave/recovery on both platforms.
- **Accept:** classroom scenario (open sample, model, save to browser, reload, recover) passes in
  headless Chrome CI; bundle ≤ 12 MB brotli.

## 0.8.0 — Automation and solids
- rhai scripting, Console, plugin manifests with declarative tools/menus/panels/forms, Extension
  Manager, three bundled example plugins; JSON-RPC `serve` and GUI attach mode.
- Solid tools (half-edge booleans) with `brep` fallback; STEP import/export via truck; sweep;
  intersect faces; soften/smooth panel.
- **Accept:** stairs plugin runs identically from CLI and GUI; boolean test suite passes;
  STEP round-trip of planar solids exact.

## 0.9.0 — Sheets (2D documentation)
- `docs/15`: sheets, model viewports at scale, dimensions/text/tables/title blocks, PDF/SVG/DXF
  export, "Send to Sheets".
- **Accept:** plan, section and elevation sheets of the house exported to a multi-page PDF with
  correct scale (measured).

## 0.10.0 — SKP interoperability and drawing completion
- Optional, separable Rust SKP converter; desktop open/save/import/export through
  shared commands and Storage, original-file fixtures and compatibility warnings.
- Complete the already specified Freehand and Rotated Rectangle drawing tools,
  CLI/RPC first, then GUI state machines.
- **Accept:** original 2017/2026 fixtures open at the measured scale; generated
  exports re-open with topology, hierarchy and texture coordinates intact;
  import undo is byte-exact; the two drawing tools have CLI/GUI/undo tests.

## 0.11.0 — Native-first SKP fidelity (in progress)
- `.plyr` remains the principal working format and the home of all application
  state. SKP open/save must support 2017 and later; expand certified coverage
  with actual files from each release, not altered version strings.
- Preserve complete sources through native working copies and unchanged
  round-trips; prevent failed decoders from opening empty replacement models.
- Close annotation, placed-image, curve, appearance and intermediate-version
  gaps before claiming complete interchange. Retained opaque source bytes do
  not count as editable support for a missing entity type.
- **Accept:** verified open → native save/reopen → edit → SKP save/reopen
  across the supported releases; geometry, hierarchy, materials/UVs, annotations,
  images, scenes and appearance retained; unchanged source byte identity;
  CLI/RPC/GUI equivalence and independent target-reader verification.

## 1.0.0 — Stabilization
- Performance targets met (`docs/00 §6`); accessibility pass; localization infrastructure with at
  least English and one additional language; documentation site; installers (MSI, DMG,
  AppImage/Flatpak) and web deployment; file format frozen at `format_version` 1; API stability
  for commands and scripting.
- **Accept:** zero S1/S2 bugs open; all CI gates green on all platforms for two consecutive weeks.

## Post-1.0 candidates
IFC import/export, FBX/COLLADA, camera calibration from photographs, WASM native plugins, collaborative editing,
parametric/dynamic components, GPU path-traced preview, mobile UI.

## Decisions

- 2026-10-29 — **0.9 as built.** Persistent paper-space pages, linked scaled
  model views, live/frozen dimensions, rich text, bound tables/labels, images,
  shapes, title blocks, layers/pen tables and print order have typed commands and
  CLI goldens. PDF is multi-page with subset Unicode fonts; SVG/DXF are per page.
  Model/Sheets GUI tabs and Send to Sheets dispatch the shared commands. Acceptance
  combines the house plan/section/elevation fixture, independently measured PDF
  scale, re-measure/save/undo tests and CLI/RPC/Rhai/GUI tests. Paper raster modes
  use the deterministic software renderer and limits documented in docs/15.

- 2026-10-28 — **0.8 as built.** Solid booleans with B-rep retry, Sweep,
  Intersect Faces, Soften/Smooth, planar STEP, Rhai, Console, plugin manifests,
  Extension Manager and local native GUI attach are implemented. Acceptance:
  core boolean/cylinder/ring/property and undo tests, STEP box/through-hole/
  cavity fixtures, CLI tests for all three bundled plugins and scripting/RPC/
  installation, and GUI `tests/automation.rs` (canonical stairs parity and a
  real authenticated server with script/history/save). Additional host
  capabilities, event subscriptions inside Rhai, startup scripts, dynamic UI
  prompts and scripted format registration remain reserved (docs/10 Decisions).
  STEP assembly placements are refused; the exchange unit is a placed solid.

- 2026-10-20 — Viewport (0.2) precedes interactive tools (0.3) so inference can be developed against
  real picking.
- 2026-10-20 — Web build is a milestone (0.7), not a continuous constraint before it; but all
  crates must keep compiling for wasm from 0.1 (CI job).
- 2026-10-25 — **0.1 CLI scope.** "All session verbs except `render`" means the verbs whose back
  ends exist in 0.1: `new`, `info`, `query`, `tool`, `export` (JSON), `validate`, `repair`,
  `batch`. `import` arrives with the first importers (0.5) and `script` with scripting (0.6).
  `serve` over standard input/output ships in 0.1 so every command is reachable through JSON-RPC
  (AGENTS.md §2); its socket transport and `repl` follow in 0.2.
- 2026-10-26 — **0.4 scope.** Delivered: textured materials, `material-sample`, `.plyrm`,
  `.plyrc` component files (save, insert, reload, replace), make unique, gluing with follow,
  cut openings (display), billboards, full scene capture with animated transitions and the
  scene animation, tag folders, the style commands and editor with watermarks and fog, `.plyrs`,
  image entities and `texture-position` (offset, rotation, scale), and the Materials,
  Components and Styles panels. The acceptance is `textured_cabinet_acceptance` (CLI),
  `scenes_animate_their_transitions_and_play_in_turn` (GUI) and the `.plyrm` round-trip tests.
  Deferred: texture pins (fixed/free), `project_from` and `copy_to`; browsing component folders
  and a user library (the Components panel shows the model's components and inserts files);
  picking image entities by clicking; edge depth cue, extensions, endpoints and jitter in the
  renderer (the style stores them).
- 2026-10-26 — **0.6 as built.** Acceptance: `house_section_acceptance` (CLI: dimensions
  re-measure after the model is resized, the sun's position, validation), the renderer snapshot
  `sections_shadows_and_overlays` (caps in the section fill, shadows, overlays) and the sun's test
  against the published SPA example (within 0.05°). Section caps are CPU-built (docs/06
  Decisions); one section plane cuts each occurrence (the outermost active one on its path), for
  drawing and picking alike. Not in 0.6: leader styles for texts (`leader: View|Pushpin`),
  `label-style` editing, two shadow cascades, edges casting shadows (`from_edges` is stored, not
  drawn) and scenes from glTF cameras.
- 2026-10-26 — **0.7 as built.** The GUI builds for `wasm32-unknown-unknown` from the same code
  (`planyrith-gui::platform` holds what differs: storage, file dialogs, clock, autosave place);
  browser storage, uploads and downloads, example documents, autosave and recovery on both
  platforms (`planyrith recover` in the CLI), a service worker, WebGPU with WebGL2 fallback,
  touch basics. Acceptance: `crates/planyrith-gui/tests/web.rs` (classroom scenario: open an
  example, model, save to the browser, reload, recover) in headless Chrome in CI, and
  `tools/build-web.sh`'s size check (4.5 MB brotli). Deferred items are listed in docs/11 and
  docs/07 Decisions.
