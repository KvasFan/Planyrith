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
  state. SKP/SKB import targets 2013 and later (user expansion on 2026-10-31);
  edited writers currently target 2017/2026 and unchanged sources retain their
  original version. Expand certified coverage
  with actual files from each release, not altered version strings.
- Preserve complete sources through native working copies and unchanged
  round-trips; prevent failed decoders from opening empty replacement models.
- Close annotation, placed-image, curve, appearance and intermediate-version
  gaps before claiming complete interchange. Retained opaque source bytes do
  not count as editable support for a missing entity type.
- Import progress: SDK-saved 2019/2020 geometry, native annotations and persistent
  anchors, curves/polylines, image entities, styles/sun and scene appearance now
  have regressions. The `0.11.0-alpha.1` preview adds `label-style` with Entity
  Info controls and edited regeneration for supported annotations, curves,
  images and appearance. Remaining presentation/format limitations and complete
  release certification keep this milestone in progress.
- The `0.11.0-alpha.2` preview adds positive 2013–2016 import and nested
  2013/2014 fixtures, direct backup handling, native-default first saving and
  explicit foreign save choices through CLI/RPC/GUI. A historical header or
  unchanged source restoration does not certify edited writing for that year.
- After alpha.2: 680 public files from 2013–2026, including real 2018, 2021 and
  2022 saves, pass the exchange chain with both writers, including deletion and
  renames, one with refaceted self-touching facets. Surfaces and texture
  coordinates match exports of the original application, a field comparison
  covers the rest of the document, and CLI, RPC and GUI agree byte for byte
  (`docs/dev/skp-fidelity.md`, `tools/check-skp-corpus.py`). Opening regenerated
  files in the target application and 2023/2024 fixtures remain open: the
  acceptance's independent target-reader verification is not met.
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

## 1.1.0 — 3D printing, presentation and modeling additions
Added at the user's request on 2026-11-05 (see Decisions). In order:
1. **3D printing:** `solid-check` and `solid-repair` (docs/05 §8.1–§8.2); 3MF import and export
   (objects, units, base colors); export options for printing (one file per object, placing each
   object on the build plate).
2. **Presentation style:** a realistic style beside the technical ones — specular and physically
   based materials (read from glTF), ambient occlusion, sky lighting, soft shadows and tone
   mapping; later a progressive path-traced final render, also from `planyrith render`.
3. **Edge rounding and chamfers; thicken a surface into a shell.**
4. **Bézier curves and loft between profiles.**
5. **More exchange formats:** PLY, USDZ, then IFC.
- **Accept:** each item has its commands, CLI batches, golden files and GUI; the CI gates stay
  green.

## 1.2.0 — Exchange and parametric modeling
Continues 1.1 without a pause, at the user's request (2026-11-07, see Decisions). In order:
1. **COLLADA** import and export (geometry, components as shared geometry, materials and
   textures, units and up axis).
2. **IFC import** (tessellated and extruded geometry, element names, classes as tags, storeys
   as tags).
3. **Parametric components:** attributes with formulas that size and position a component's
   parts.
4. **Camera from a photograph:** match the view to a picture by its vanishing lines.
- **Accept:** each item has its commands, CLI batches, golden files, GUI and validation by an
  external tool where one exists; the CI gates stay green.

## 1.3.0 — Engines, plugins and live rendering
Continues 1.2 without a pause, at the user's request (2026-11-07). In order:
1. **FBX** import and export (binary 7.x; ASCII 7.x import): meshes, materials with colors and
   embedded or neighboring textures, the model hierarchy with shared geometry, units and axes.
2. **Native plugins** (docs/10 §7): WebAssembly modules hosted on the desktop with the same host
   API as scripts, sandboxed, declared by the manifest's `[entry] wasm` key.
3. **Progressive path-traced preview:** the path tracer of 1.1 refines an image in a GUI window
   while the model is open, restarting when the model or the camera changes.
- **Accept:** each item has its commands or API, tests and documentation; formats have golden
  round trips; the CI gates stay green.

## 1.4.0 — Graphics card tracing and touch
Continues 1.3 at the user's request ("riprendi", 2026-11-08). In order:
1. **Path tracing on the graphics card:** the tracer of 1.1 in a compute shader, for the preview
   and `render --path-traced --gpu`.
2. **Touch interface:** the GUI usable on tablets and phones (larger targets, a touch layout,
   long press for the context menu) in the web build and on touch screens.
- **Accept:** tests and documentation; the CI gates stay green.

## 1.5.0 — Working together
At the user's request ("continue with milestones", 2026-11-09):
1. **Several people on one document:** the server takes several clients at once, orders their
   commands and gives each a selection and edit context of their own; changes reach everyone.
2. **Presence:** names and colors; everyone sees who is connected and what they selected.
3. **Desktop joining:** File › Work Together…, network addresses, a local copy on leaving.
- **Accept:** a CLI test with two clients and a GUI test with two sessions; documentation.

## 1.6.0 — Secure working together
At the user's request ("continue with milestones", 2026-11-09):
1. **Encrypted connections** for servers on the network (docs/08 §3.2).
- **Accept:** a round trip and a refused wrong token in tests; documentation.

## 1.7.0 — Working together, continued
At the user's request ("procedi con altri features", 2026-11-09):
1. **Per-person undo:** each person takes back their own changes, under newer ones by others
   when nothing is shared (docs/08 §3.1).
2. **Joining from the browser:** `serve --web` with a WebSocket and the web build; the web
   build's Work Together (docs/08 §3.3).
- **Accept:** a fuzzed property for out-of-order undo, a CLI test with two clients and a GUI
  test with two sessions; a browser session tested against the real server; documentation.

## 1.8.0 — Measuring and making
At the user's request ("aggiungi tutti feature mancanti senza compromettere quelli già
esistenti o distruggere interfaccia grafica", 2026-11-09), the start of the engineering
milestones 1.8–1.18 (see Decisions). In order:
1. **Mirror** a selection across a plane, as a copy or in place.
2. **Revolve** a profile about an axis, full or partial.
3. **Patterns:** linear (one or two directions), circular, and along a path.
4. **Mass properties:** density on materials; mass, volume, area, center of mass and the
   inertia tensor of solids, per object and in total.
5. **Cut list:** boards and members with their lengths, widths, thicknesses and counts.
6. **Interference check:** which solids overlap, with the volume of each overlap.
- **Accept:** each item has its command, CLI batch and golden file, GUI and documentation;
  existing tools and the GUI keep working (all CI gates).
- **Done (2026-11-09):** all six items, each as a command with tests, a CLI batch and golden
  file, documentation and a Tools-menu window (the Measure window holds the three
  measurements). Interactive click-by-click tools for mirror, revolve and pattern can follow.

## 1.9.0 — Constrained sketches
Geometric constraints (coincident, horizontal, vertical, parallel, perpendicular, tangent,
equal, fixed, symmetric, on-curve) and driving dimensions (distance, angle, radius) on the
edges of a face's plane, kept by a solver; under-, fully and over-constrained states shown.
- **Done (2026-11-09)** for straight edges: coincident, fixed, along an axis, parallel,
  perpendicular, equal, collinear, midpoint, on a line, symmetric, and length, distance and angle
  dimensions (docs/03 §11). Tangency and radius on arcs and circles follow with the feature
  history (1.10), where curves are regenerated from their parameters.

## 1.10.0 — Feature history
Parametric parts: a group whose geometry is regenerated from an ordered list of features —
each one a command with its parameters (sketches, extrusions, revolutions, fillets, patterns,
mirrors, Booleans). A history panel to edit, reorder, suppress and roll back features;
references by selection queries re-resolved at each regeneration. Configurations as named
parameter sets; equations between parameters.
- **Done (2026-11-09):** `part-create`, `feature-add|edit|delete|move|list`, `part-regenerate`
  (rollback and forward) and `part-configuration`; equations are the parameters' formulas; the
  GUI records features while a part is open. References use `last` and queries; matching
  regenerated faces to earlier ones by geometry is left for later.

## 1.11.0 — Solid features
Draft, shell, rib, standard holes (clearance, counterbore, countersink, tapped, with a size
table) and cosmetic threads, dome, split by a plane or a surface, and scale about a point.
- **Done (2026-11-09):** `hole`, `shell`, `draft`, `split-solid` (by a plane), `dome` and `rib`
  (docs/05 §8.6–§8.11); scaling about a point was already `scale --pivot`. Splitting by a curved
  surface waits for the surfaces of 1.15.

## 1.12.0 — Assemblies
Mates between components (coincident, concentric, distance, angle, parallel, perpendicular,
tangent, fixed) kept by a solver with degrees of freedom shown; exploded views stored in
scenes; a bill of materials; collision detection while dragging; driving a mate to animate.
- **Done (2026-11-09):** mates (all but tangent) with the solver and freedom shown, driving by
  editing a mate's value, exploded views kept in the container (collapsed again on demand rather
  than per scene), the bill of materials (docs/05 §5.17). Tangency and collision checks while
  dragging in the viewport remain; interference is checked on demand (§8.4).

## 1.13.0 — Engineering drawings
Sheet views tied to the model and kept current: projected, section, detail, auxiliary and
broken views; geometric tolerances with datums, surface finish and weld symbols; balloons
and bill-of-materials tables; ISO and ANSI sheet formats with title blocks.
- **Done (2026-11-10):** all of it (docs/15 §9): derived views re-derived at every layout,
  sections cutting only their view with hatching, broken views, vector symbols, balloons
  matching the bill-of-materials table, ISO/ANSI formats; the Sheets editor has Section,
  Detail, Break and Symbol tools, projected-view buttons and Standard Format.

## 1.14.0 — Sheet metal and frames
Sheet metal parts (base and edge flanges, bends with a bend allowance, the flat pattern with
its DXF); frames of structural members along lines (a profile library, trimming at joints)
with their cut list.
- **Done (2026-11-10):** sheet-metal parts with base, edge flanges, gauge changes and the flat
  pattern (docs/05 §8.12); frame members with a profile library, mitered joints and cut-list
  lengths (§8.13). Flanges stand on the base outline only (not on other flanges), and corner
  reliefs are not cut.

## 1.15.0 — Surfaces and analysis
Boundary and filling surfaces, knitting, extending and trimming surfaces; curvature combs,
curvature and zebra displays, draft analysis and wall thickness analysis.
- **Done (2026-11-10):** `surface-fill` (four-sided blended and radial patches),
  `surface-extend`, `surface-trim` (by a plane) and `surface-knit` (docs/05 §8.14); curvature,
  zebra, draft and thickness analyses as colored copies and curvature combs (§8.15). Trimming
  by another surface and splitting a solid by a surface remain.

## 1.16.0 — Simulation
Linear static stress analysis of solids (tetrahedral meshing, materials with Young's modulus
and Poisson's ratio, fixtures and loads, displacement and von Mises results shown in color),
natural frequencies, steady heat conduction, and motion of assemblies over time.
- **Done (2026-11-10):** `sim-static`, `sim-frequency` and `sim-thermal` on a mesh of quadratic
  tetrahedra over a grid fitted to the body (docs/03 §12, docs/05 §8.16) with a library of
  materials, results shown as colored copies; `motion-study` drives a mate in steps with
  collision checks (§8.17). Curved surfaces are approximated in steps of the cell size (no
  boundary-fitted mesh yet), and conduction has no convection to air.

## 1.17.0 — Product data and exchange
Custom properties and part numbers; revisions recorded in the document; a library of standard
parts generated from size tables (screws, nuts, washers, bearings, pins); IGES import and
export; STEP with product structure and annotations.
- **Done (2026-11-10):** properties and part numbers, revisions shown on sheets, standard parts
  (docs/05 §5.18); STEP export and import with product structure, placements, part numbers and
  properties without the B-rep bridge, IGES 5.3 export and import of planar faces (docs/09 §5).
  STEP "annotations" are the products' properties and revision; graphical annotations (PMI),
  IGES assemblies and curved IGES surfaces are not exchanged yet.

## 1.18.0 — Molds and routing
Mold design (parting line, parting surface, core and cavity, shrink factor) and routing
(pipes and tubes along paths with standard sizes and fittings).

## Post-1.0 candidates
None open beyond 1.8–1.18.

## Decisions

- 2026-11-09 — **Milestones 1.8–1.18: engineering modeling.** Asked to add every feature a
  feature-based mechanical modeler has and Planyrith lacks, without harming what exists or the
  interface, the list becomes eleven milestones ordered by value and by what each builds on:
  measuring and making first (they need nothing new), then constraints, which the feature
  history and the assemblies need, then features, assemblies, drawings, the specialized
  part types, surfaces, simulation, data and the rest. Two former non-goals of docs/00 §3 are
  lifted for this (docs/00 Decisions): the feature history, built *on* direct modeling (a
  parametric part replays commands, so everything else keeps working unchanged), and
  simulation. The kernel stays the polygon mesh with exact curve descriptors (docs/03); exact
  freeform surfaces are approximated by fine tessellation rather than replacing the kernel,
  which would endanger every existing tool. Each item keeps the usual acceptance: command,
  CLI, tests, documentation, then GUI.

- 2026-11-09 — **Milestone 1.7.** Asked to go on with more features after the 0.11.0-alpha.4
  release, the two remaining candidates become a milestone. Per-person undo comes first: it
  needs no new transport and makes working together usable for real edits. Both are done.

- 2026-11-09 — **Milestone 1.5.** Collaborative editing, the last candidate, with the design
  in docs/08 Decisions ("Working together"): a server that orders commands, chosen because it
  needs no merge model and builds on the existing server and GUI attachment. Done in full.

- 2026-11-08 — **1.4 progress.** Path tracing on the graphics card (docs/06 §11.3) and the
  touch layout (docs/07 §13.1) are done, which completes the milestone.

- 2026-11-08 — **Milestone 1.4.** Asked to resume without a choice among the remaining
  candidates, the two that can be built and checked on this machine come first: graphics card
  path tracing (a GPU is present) and the touch interface. Collaborative editing needs a server
  and a merge model, decisions that are the user's; it stays a candidate.

- 2026-11-08 — **1.3 progress.** FBX import and export (docs/09 Decisions), native
  WebAssembly plugins (docs/10 §7) and the progressive path-traced preview (docs/06 §11.2) are
  done, which completes the milestone.

- 2026-11-08 — **Milestone 1.3.** With 1.2 implemented, the remaining candidates are ordered by
  use and by what can be verified here: FBX is the exchange format game engines and asset stores
  expect; native plugins were reserved in the manifest since 1.0 (docs/10 §7); a progressive
  preview reuses the deterministic CPU path tracer instead of a GPU tracer, which needs hardware
  ray queries not available on every adapter. Collaborative editing (a server and a merge model)
  and a mobile interface stay candidates.

- 2026-11-08 — **1.2 progress.** COLLADA import and export and IFC import are done (docs/09
  Decisions); parametric components (docs/04 §9.1, docs/05 §5.14) and the camera from a photograph
  (docs/05 §9.1) are done, which completes the milestone.

- 2026-11-08 — **Milestone 1.2.** The user asked to keep implementing and committing without
  stopping after each milestone (2026-11-07). With 1.1 implemented, the next items come from the
  post-1.0 candidates, ordered by use for the project's audience (architecture, interiors,
  woodworking, printing): COLLADA is the most common exchange format still missing, IFC import
  completes BIM exchange, parametric components serve furniture and joinery, and photo matching
  serves renovation work. Collaborative editing, native plugins and a mobile interface stay
  candidates.

- 2026-11-08 — **1.1 as built so far.** 3D printing: `solid-check`, `solid-repair`, 3MF both
  ways, `per_object` and `on_plate` exports. Presentation: the realistic look (`realism` in a
  style: microfacet highlights after material roughness/metalness, sky and ground light, soft
  shadows, filmic tones, screen-space ambient occlusion). Modeling: `fillet`, `chamfer`,
  `thicken`, `bezier`, `loft`. Exchange: PLY both ways, USD (`usda`, `usdz`) and IFC4 export,
  checked by the reference libraries in `tools/validate-exports.sh`; USD carries textures.
  IFC elements take their class from an attribute or their tag; `render --path-traced` traces
  final images on the CPU (and from File › Export › Rendered Image…). IFC storeys come from
  an attribute and elements carry volumes and property sets. Fillet, Chamfer, Thicken
  and Loft are picking tools with a live preview. Every 1.1 item is implemented.

- 2026-11-05 — **Milestone 1.1.** The user asked what is missing for 3D printing, useful 3D tools,
  other exchange formats and a more attractive rendering, then to proceed. These are not part of
  1.0 stabilization, so they form a 1.1 milestone instead of changing 1.0's scope; new commands
  are added to the frozen API (additions only). Printing comes first: it builds on the existing
  solid classification and closes the gap between "looks closed" and "a slicer accepts it".

- 2026-11-02 — At the user's request ("procedi con prossima tappa"), 1.0 stabilization work
  starts while 0.11 waits only for the user's check of regenerated files in the target
  application. This replaces "no automatic 1.0 work" (2026-10-31). The first step measures the
  quality targets of docs/00 §6 (docs/12 Decisions, 2026-11-02).

- 2026-10-31 — At the user's request, ship installable Windows, AppImage,
  Debian and Arch preview packages before the 1.0 stabilization milestone.
  Publish the current 0.11.0-alpha.2 with the gates in docs/16. This does not
  change the remaining SKP fidelity acceptance work or authorize 1.0 work.
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
