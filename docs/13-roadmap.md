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
  than per scene), the bill of materials (docs/05 §5.17). Tangency is completed in 1.21;
  Move checks placement interference in 1.22, Rotate/Scale in 1.23, and scenes capture
  assembly placements in 1.24; Move stops at contact in 1.37. Interference is also checked on demand (§8.4).

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
- **Done (2026-11-12):** planar two-half molds with shrink compensation, a parting line and
  surface (`mold-split`, docs/05 §8.18); hollow tubes along open paths with diameter × wall
  presets/custom sizes and tangent elbows, plus sleeve and concentric-reducer fittings
  (`route`, `route-fitting`, §8.19). Tools › Molds… and Routing… use the same commands.
  Nonplanar divisions and automatic undercut resolution remain; branch fittings follow in 1.19.

## 1.19.0 — Branched routing
Complete the junction fittings reserved in 1.18: equal-section tees and crosses with
communicating bores, explicit branch lengths and orientation. Reuse `route-fitting` and its
GUI, with unchanged sleeve/reducer defaults. Acceptance: solid wall boundaries, open ports,
bore connectivity, rotated placements, rollback, native/undo and CLI/RPC/GUI tests.
- **Done (2026-11-12):** `route-fitting --kind tee|cross`, with branch length/direction,
  communicating bores, native undo/persistence and Routing controls (docs/05 §8.20).

## 1.20.0 — Thermal convection
Complete the convection boundary condition reserved in 1.16: film coefficient and ambient
temperature on selected faces, including problems anchored by convection alone. Use consistent
quadratic boundary integration and report the steady heat balance. Acceptance: analytic bar
temperature and convection-only energy balance, invalid-condition rejection, native/undo and
CLI/RPC/GUI tests; existing thermal results stay stable without convection.
- **Done (2026-11-12):** consistent quadratic convection boundaries, convection-only thermal
  references, heat-balance outputs, component anchoring and temperature-conflict checks,
  with Heat-page controls (docs/05 §8.16). Analytic, undo, native and frontend tests pass.

## 1.21.0 — Assembly tangency
Complete the tangent mate reserved in 1.12: circular axes tangent to planes or other circular
axes (external or internal contact), with exact radii from curve descriptors. Reject circular
references whose placement distorts their radii. Acceptance: analytic contact distances,
degrees of freedom, conflicts, transformed occurrences, native/undo and CLI/RPC/GUI tests.
Viewport Move interference checks follow in 1.22.
- **Done (2026-11-12):** tangent mates between circular cylinders and planes or circular
  cylinders, external/internal contact, similarity-transformed radii and deterministic
  initialization at symmetric starts. Analytic distances, freedom, conflicts, native
  persistence/undo, CLI/RPC and the actual Assembly button are covered (docs/05 §5.17).

## 1.22.0 — Interference during Move
Complete the assembly viewport gap with optional collision checks during Move: reuse the
core's volumetric interference measurement in the command's destination and scratch previews,
report only pairs involving changed or copied occurrences, draw intersection bounds and show
counts/undecided pairs. Preserve one-step undo and ordinary movement with the option off.
Acceptance: analytic overlap/contact, unrelated-pair exclusion, nested/shared/copied objects,
hidden/open objects, native undo and CLI/RPC/GUI parity. This is advisory placement checking;
continuous path collision and stopping at contact remain deferred.

- **Done (2026-11-13):** `move --check-collisions` reports affected pairs through CLI/RPC;
  Move's Tool Options enables scratch reports, red intersection bounds and localized status.
  Tests cover nested/shared placements, changed meshes, copies, hidden/open objects, native
  data and one-step undo; the existing command goldens and frozen API remain compatible.

## 1.23.0 — Transform interference

Extend the destination interference option of Move to Rotate and Scale, including copies
and mirrored/nonuniform placements. CLI/RPC first, matching GUI options, analytic tests,
native undo and unchanged defaults.

- **Done (2026-11-14):** Rotate/Scale share delta-based interference measurement, with
  CLI/RPC and Tool Options, reflected/nonuniform placements, copies and native undo tests.

## 1.24.0 — Assembly placements in scenes

Optionally capture occurrence placements in scenes and restore them atomically on
activation. Preserve legacy scenes, stable persistent paths, hidden/nested occurrences,
native round trips and undo.

- **Done (2026-11-14):** Scenes optionally capture persistent occurrence paths and local
  transforms. Atomic restore, shared/nested/deleted references, native undo,
  recovery/routing classification, read-only RPC and the Scenes-panel control are tested.

## 1.25.0 — Dimension extension lines

Add editable extension-line gaps, overshoot and visibility through label-style, Entity
Info and shared viewport/image overlays. Preserve anchors, measured values and historical
defaults.

- **Done (2026-11-14):** Label Style and Entity Info edit optional dimension extension
  gap, overshoot and visibility. Shared overlay geometry, unchanged measurement/anchors,
  native, undo and frontend tests pass.

## 1.26.0 — Text leader placement

Add View and Pushpin leader modes through text/label-style and Entity Info. View mode uses
screen offsets; Pushpin fixes the label position in model space. Share viewport and
rendered-image overlay behavior; model vector view exports retain their geometry-only
scope.

- **Done (2026-11-14):** View/Pushpin leaders use the existing layout offset, with
  explicit local-mm editing and validated mode changes. Anchor, native/undo, CLI/RPC and
  actual control tests pass.

## 1.27.0 — Lofts with cavities

Loft face profiles with corresponding holes, preserving through cavities and rejecting
mismatched or intersecting profiles atomically. Existing picking tool, native undo,
analytic volume and CLI/RPC coverage.

- **Done (2026-11-14):** Matching profile holes become subtractive through cavities with
  connectivity and volume validation. Analytic/property tests, invalid inputs, native
  undo, CLI goldens and the actual Loft Selected Faces menu pass.

## 1.28.0 — Camera direction and eye height

Complete position-camera with a look direction, optional target distance and eye height
above a picked base point. Validate degenerate directions and preserve target-based calls;
expose a camera placement form.

- **Done (2026-11-14):** position-camera accepts direction, target distance and eye
  height; Camera Placement dispatches it. Invalid directions, view-only history, native
  goldens, CLI/RPC and actual Apply-button tests pass.

## 1.29.0 — Autosave interval preference

Expose the snapshot interval in Preferences, persist it per user and apply it to
current/new/recovered sessions without discarding journals or pending work. Keep the
existing default and test recovery.

- **Done (2026-11-14):** Preferences persists 10–3600-second snapshot intervals (120 by
  default). Pending rows, recovery, invalid-policy checks and the actual reset control
  pass.

## 1.30.0 — Tool shortcut preferences

Allow per-user remapping of single-key tool shortcuts with collision validation, reset and
consistent tool-menu hints. Preserve value-field input, reserved keys and document
independence.

- **Done (2026-11-14):** Per-user letter overrides/unassigned tools support
  collision/reserved-key validation, reset and matching menu/toolbar hints. Persistence,
  keyboard/value input and the reset control are tested.

## 1.31.0 — Default file folders

Persist separate model/import/export folder preferences and use them in file dialogs while
keeping explicit document locations and browser storage semantics. Expose
editable/resettable preferences.

- **Done (2026-11-14):** Separate model/import/export folders initialize
  model/resource/render-image dialogs. Existing document locations win for Save As, and
  browser uploads retain their picker. Persistence, precedence and reset tests pass.

## 1.32.0 — Changed-geometry previews

Render only changed/new preview faces with transparency; unchanged model geometry stays
opaque. Compare stable mesh identities and placements, keep picking on the real document,
and cover cancellation and GPU rendering.

- **Done (2026-11-14):** Persistent face/path comparisons identify changed/new faces,
  skipping untouched containers. Those faces use 40% material opacity; CPU identity/native
  tests and GPU comparisons prove adjacent unchanged faces remain opaque and clearing
  restores normal rendering.

## 1.33.0 — Arc and circle constraints

Complete the sketch constraints reserved in 1.9: tangency and radius on arcs and circles, plus
equal radii, concentric curves and points on curves. Curves move by center and radius and keep
their descriptors. Acceptance: analytic tangency/radius, conflicts, undo, CLI/RPC and the
Constraints window.

- **Done (2026-11-16):** `radius`, `equal-radius`, `concentric`, `on-curve`, `tangent`
  (docs/03 §11.4, docs/05 §5.15), with Constraints-window buttons. Internal tangency between
  filled circles in one plane is solved but refused by the kernel (inscribed segments cross).

## 1.34.0 — Flanges on flanges and corner reliefs

Complete sheet metal from 1.14: edge flanges on the outer edges of earlier flanges, and corner
reliefs where two flanges meet at a base corner, in the 3D part and the flat pattern.

- **Done (2026-11-16):** stacked flanges with signed angles (`stack`), square and round corner
  reliefs (`relief_shape`, `relief_size`), stacked removal; folded part, blank and the
  Fabrication window agree (docs/03 §4.20, docs/05 §8.12). Flanges on a flange's side edges
  remain out of scope.

## 1.35.0 — Trimming and splitting by surfaces

Complete 1.11/1.15: trim a surface by another surface and split a solid by a surface (open
surface groups), keeping both sides as separate groups.

- **Done (2026-11-16):** `surface-trim --by [--flip]` and `split-solid --by` with the cutter's
  slab (docs/03 §4.21, docs/05 §8.9, §8.14); Surfaces and Solid Features windows pick the
  cutter from the selection. Cutters must face one way along their mean normal.

## 1.36.0 — Persistent references across regeneration

Complete 1.10: feature references to faces and edges survive regeneration by matching
regenerated geometry to earlier geometry (plane, position, area), with the match reported.

- **Done (2026-11-16):** recorded shapes (`refs`) matched before each feature replays,
  parameters rewritten to current pids, `references` in the output and a Features-window note
  (docs/05 §5.16, docs/04 §9.3). This also makes Push/Pull drags recorded in a part replay.

## 1.37.0 — Moving to contact

Complete the deferred continuous collision of 1.12/1.22: an optional Move mode that stops a
moving occurrence at first contact along its straight path, CLI/RPC first.

- **Done (2026-11-16):** `move --stop-at-contact` with exact vertex–face and edge–edge contact
  for translations, `contact` in the output and Move's Tool Options (docs/05 §4.4). Rotations
  keep the advisory destination check of 1.23.

## 1.38.0 — Curved and assembled IGES

Complete 1.17's IGES: curved surfaces on import (B-spline, surfaces of revolution, tabulated
cylinders, ruled surfaces), tessellated within tolerance; assemblies (subfigures 308/408)
in both directions as components.

- **Done (2026-11-16):** curved trimmed, bounded and freestanding surfaces as soft triangles
  (Delaunay in scaled parameters), subfigures both ways as components (docs/09 §5 and
  Decisions). The washer golden now holds a subfigure and its placement. IGES solids (186)
  remain out of scope.

## 1.39.0 — STEP annotations

Complete 1.17's STEP: dimensions and texts exported as graphical annotations (polylines and
text placements) and read back.

- **Done (2026-11-16):** draughting callouts per product, read back as dimensions and notes
  attached to the imported geometry (docs/09 Decisions, 2026-11-16).

## 1.40.0 — Stepped mold partings and undercuts

Complete 1.18: parting surfaces made of several planar steps, and draw-direction undercut
detection reported per face.

## 1.41.0 — Boundary-fitted simulation meshes

Complete 1.16: boundary nodes of the tetrahedral mesh moved onto the body's faces, so curved
bodies are no longer stepped at the cell size.

## 1.42.0 — Interactive mirror, revolve and pattern

Complete 1.8: viewport tools that collect the mirror plane, revolve axis and pattern direction
by clicking, dispatching the same commands.

## 1.43.0 — USD import

Complete docs/09's USD row: import of text USD (`.usda`) and USDZ packages holding text layers:
meshes, transforms, references as components, display colors and preview materials.

## Post-1.0 candidates
Other limits remain listed against their original milestones; no unrelated feature scope is
added by the continuation milestones 1.19–1.32.

## Decisions

- 2026-11-16 — **Completing reserved gaps.** Asked to resume and to implement and test every
  missing feature, the gaps recorded against earlier milestones become 1.33–1.43, in order of
  how often a modeling session meets them. Items needing the user (2023/2024 SKP fixtures,
  opening regenerated files in the target application, macOS packaging) and the formats docs/09
  keeps out of scope (DWG, 3DS) are not among them. Each milestone keeps defaults, the native
  format version and the frozen API (additions only).

- 2026-11-14 — **Ten uninterrupted milestones.** At the user’s request, 1.23–1.32 complete
  remaining modeling, scene, annotation, camera, preference and preview work. Existing UV
  projection/copy, component libraries, edge effects and imported cameras are already built
  and do not count as new milestones. Each stage keeps existing defaults and its own tests;
  the batch ends with the workspace quality gates. No release or file-format bump is implied.

- 2026-11-13 — **Continuation after 1.21.** At the user's request to continue development,
  1.22 addresses the explicit viewport interference gap of 1.12 using the existing Move and
  measurement commands. It adds no dependency, release or file-format version. Rotate/Scale
  checks and continuous collision detection are outside this milestone.

- 2026-11-12 — **Continuous development after 1.18.** At the user's request to proceed
  through the next milestones without pausing at each, 1.19–1.21 complete three explicit gaps
  in the existing engineering milestones: branch fittings, convection and tangent mates.
  Each keeps the core/CLI/RPC acceptance before GUI integration, with one final local CI gate
  and push for the batch. This does not authorize a release or a new file-format version.

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
