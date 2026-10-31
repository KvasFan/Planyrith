# Changelog

All notable changes to Planyrith are recorded here. Versions follow
[Semantic Versioning](https://semver.org/); before 1.0, minor versions are milestones of
[`docs/13-roadmap.md`](docs/13-roadmap.md).

## 0.11.0-alpha.2 — Historical models, backups and native defaults (2026-10-31)

- Ship Windows per-user setup and portable ZIP, Linux AppImage, Debian package,
  Arch package and portable archive. Include GUI, rendering-enabled CLI, the
  separate optional format helper, notices, source bundles and checksums.
- Add verified release automation, installation documentation, an application
  icon and a refreshed project page with theme-aware artwork and screenshots.
- Open/import historical SKP models from 2013 onward, adding positive 2013–2016
  box fixtures and nested 2013/2014 regressions. Read older entity preambles,
  definition prefixes and instance tails; assign stable archive identities when
  the source has no persistent ids. Unchanged saves retain original bytes and
  versions; edited writers remain 2017 and 2026 with compatibility notices.
- Open/import/save/export `.skb` backups directly, including mixed-case file
  names, through the same desktop workflow as `.skp`.
- Default the first save of an externally opened model to a `.plyr` working
  copy. The GUI proposes a native filename in Save As; CLI/RPC choose the native
  sibling and refuse to overwrite an existing copy implicitly. Explicit foreign
  save choices remain active for that GUI/RPC session. Source files stay intact.

## 0.11.0-alpha.1 — Editable SKP exchange and annotation styling (2026-10-30)

- Fix classic SKP opening for packed color attributes, trailing component
  definitions and embedded BMP images. Convert legacy bitmaps losslessly to
  native PNG assets. Converter failures now report their cause without declaring
  the source container corrupt (2026-10-31).
- Retain warped source polygons as facets with unchanged vertex positions,
  holes and appearance. Avoid quadratic pid bookkeeping while assigning
  imported face materials and UVs.
- Add `label-style` to CLI, RPC and scripting, plus annotation appearance controls
  in Entity Info: font requests, screen/model height, arrow, text color, leaders
  and reset. Edits retain anchors/placement and undo in one step; dimension
  colors affect both lines and labels.
- Regenerate SKP text, associative linear/radial dimensions, analytic circular
  curves and welded chains, image entities/assets, styles, watermarks,
  geographic shadows and scene appearance. Native → SKP → native tests cover
  both 2017 and 2026, including actual edits after opening an SKP.
- Avoid accumulating private image definitions on repeated conversion. Keep
  temporary vertex/curve ids deterministic and separate from document ids.
- This is a prerelease: manual sun directions, some annotation presentation,
  custom strokes/masks and per-release certification remain incomplete.
- Keep `.plyr` as the principal lossless working format. Complete SKP
  interoperability for 2017 and later is the next acceptance target; it is
  not yet certified across that range.
- Preserve the entire SKP source through native working copies. Saving an
  unchanged model back to SKP restores its original bytes and version; edited
  models use a compatible 2017/2026 writer and retain conversion notices.
- Refuse decoder fallback with missing geometry instead of opening a
  misleading empty document. Intermediate-version coverage remains in progress.

- Recover geometry in SDK-saved 2019/2020 files with a calibrated Rust classic
  archive reader; add pinned geometry, nesting, texture and annotation fixtures.
- Import editable text and linear/radial dimensions with fonts, screen/world
  placement, leader settings and persistent nested/parametric anchors. Keep
  anchors valid when merging documents whose persistent-id ranges overlap.
- Import analytic circular curves, spatial polylines, placed images, styles,
  fog/backgrounds, watermarks, sun settings and scene appearance. Preserve
  classifications, custom stroke images and decoded settings in native data.
  Exact external font rendering and some appearance details remain limited;
  full edited-output and release-by-release certification remain open.

## 0.10.0 — SKP interchange and drawing tools (2026-10-30)

- **SKP on desktop**: Open, Save/Save As, CLI `-f`, import/export and RPC use
  the optional `planyrith-skp` converter, a separate GPL-3.0-only Rust executable.
  The applications keep their MIT/Apache-2.0 licensing and never link its decoder.
  Save writes real 2017 SKP files; export also offers the 2026 container.
- **Model transfer**: polygon faces and holes, wire edges/flags, nested/shared
  components and groups, transforms, two-sided materials, embedded textures/UVs,
  tags, cameras, scene visibility/sections, guides, axes, units and supported
  attribute values. Missing adapters and conversion failures are explicit errors;
  original-file fixtures and generated round-trips check scale and topology.
  Conflicting face sides are retained in private surface groups. Projective
  texture frames survive native saves; WebP textures export as lossless PNG.
- **Compatibility notices** are returned in CLI/RPC and visible in the GUI,
  including hoverable full messages. Keep a native copy for Sheets, annotations,
  placed images, rendering settings and history. Curves retain segmented geometry;
  intermediate SKP releases and browser conversion are not certified/supported.
- **Freehand**: planar press/drag/release drawing, two-pixel sampling,
  deterministic stroke simplification, curve grouping and a single undo step.
- **Rotated Rectangle**: first-edge orientation, drawing plane, signed second-side
  angle and exact width/length input, preview and one undo step. Both tools are
  registered commands with CLI goldens, RPC/Rhai bindings, property/undo/fuzz
  coverage and synthetic GUI input tests.

## 0.9.0 — Sheets (2026-10-30)

- **Paper-space documentation**: pages inside the native document, A0–A4/Letter/
  Tabloid/custom paper, rational model-view scales, scene/style links, convex crop
  boundaries, tag visibility and section overrides, print order and stable item ids.
- **Annotations**: live model or paper dimensions, rich text and leaders, literal
  and query-bound tables, images, polylines/rectangles/ellipses, reusable title
  blocks and attribute/scene labels. Freezing a view stores its raster and linked
  dimension measurements; Follow Model resumes live updates.
- **Export**: multi-page PDF with subset Unicode fonts, paper layers, page labels
  and exact mm-to-point scaling; standalone SVG pages with embedded assets; DXF
  pages with sheet/tag layers, fills, pen patterns and PNG side files. Raster and
  hybrid use a bounded deterministic software renderer; its scope is in docs/15.
- **CLI and automation**: the `sheet` verb, typed `sheet-*` registry commands,
  RPC and generated Rhai functions. Item, page, settings and layer changes undo
  without whole-document snapshots. New CLI goldens and randomized command coverage.
- **GUI**: Model/Sheets tabs, Send to Sheets, paper tools, properties, layers,
  snapping, zoom/pan, paper alignment and stacking, camera editing in the 3D view,
  freeze/unfreeze, and export/download on desktop and web.
- **Acceptance**: house plan/section/elevation fixtures and a reproducible Rust
  generator; actual PDF strokes measure a 1 m cube at 1:20 as 50 mm; live dimension
  re-measurement, native round-trips, byte-exact undo and frontend/browser tests.

## 0.8.0 — Automation and solids (2026-10-28)

- **Solids**: Union, Subtract, Intersect, Trim, Split and Outer Shell, with
  coplanar handling and a B-rep fallback. Sweep follows open or closed edge
  chains with minimum-rotation frames and mitered corners. Intersect Faces
  splits faces; Soften/Smooth edits edge flags from their dihedral angles.
  All operations use registry commands with CLI goldens and exact undo tests.
- **STEP**: import/export of solid bodies; planar boundaries and holes
  round-trip exactly within tolerance, including interior cavities. Fixtures
  are reproducible with the Rust generator in `tools/`.
- **Automation**: sandboxed Rhai, generated command bindings, units, queries,
  context, attributes, grouped undo, print/log/progress capture and limits.
  Entry points: `script`, batch `script:` lines, `tool script-eval`, RPC
  `script.eval` and the GUI Console. Failed scripts commit no document changes.
- **Extensions**: validated manifests, declarative forms, menus, optional toolbars
  and panels, Extension Manager, `ext install/check/list`, and three bundled
  examples: stairs, parametric cabinet and CSV guide points. CLI and GUI stairs
  produce identical canonical exports. Filesystem/network/time/random grants
  are refused; declarative forms are the supported UI capability.
- **GUI attach**: connect to `serve` over authenticated local TCP or a Unix socket;
  commands, scripts, undo/redo and saving run on the server. Native document
  snapshots keep geometry and persistent identities synchronized.

## 0.7.0 — Web build (2026-10-28)

- **Web**: the GUI runs in the browser (WebGPU, or WebGL2), built by `tools/build-web.sh` into
  static files (4.3 MB with brotli) with a service worker for offline use (network first, so a
  new build is used at once). Open…, Import…, Insert Image… and the panels' files use the
  computer's file picker; exports and Download This Document / Download a File… save to the
  computer. Documents also live in the browser's storage (IndexedDB, File › Open Stored in
  Browser…); dropping files on the page opens or imports them. The page asks before closing with
  unsaved changes and shows an error box if the application stops. Touch: pinch to zoom, two
  fingers to pan, twist to orbit.
- **Rendering**: the viewport's frame is produced by a display pass that works on WebGL2 and GL
  drivers without texture view formats.
- **Autosave and recovery** on the desktop and the web: a journal of every command and periodic
  snapshots beside the document (never over it); after a session ends without saving, the GUI
  offers Recover or Discard. `planyrith recover <doc>` does it from the command line.
- **Examples**: File › Open Example ► Room, Bookcase.
- Dropping files on the desktop window opens or imports them.

## 0.6.0 — Sections, annotations, shadows (2026-10-26)

- **Annotations**: `dimension` (aligned, linear, radius, diameter) re-measures when its
  geometry moves, also after saving and inside components; `text` with default measurements.
- **Guides**: `tape-measure`, `protractor`, `guide-delete-all`, `resize-model`.
- **Sections**: `section-plane-add`/`-toggle`/`-reverse`/`-delete`/`-align-view`,
  `section-create-group-from-slice` (closed cuts become faces), `section-display-set`.
- **Shadows**: `shadows-set`; the sun position follows the NOAA algorithm (within 0.05° of the
  published reference).
- **Axes**: `axes-set`, `axes-reset`; inference uses the moved axes.
- **3D text**: `text-3d` with a bundled open font, flat, extruded or as outlines.
- **Rendering**: section cuts with filled caps wherever the cut is inside material (walls on a
  floor included) and cut lines; sun shadows on faces and
  the ground; guides, dimensions, texts and section plane widgets drawn in the viewport and in
  headless renders (with labels).
- **GUI**: Tape Measure (T), Protractor, Dimension, Text, 3D Text, Section Plane and Axes tools;
  View › Shadows, Shadow Settings, section display toggles and Viewports (one, two or four).
- **Queries**: a dimension's `length` is its measurement; picking ignores what a section cuts.

## 0.5.0 — Interchange (2026-10-26)

- **Import and export**: `import` and `export` commands and the `planyrith import` verb;
  `planyrith export` writes the mesh formats too. STL import (ASCII and binary, triangles merged
  into faces) and export (binary or ASCII, Y-up unless `--z-up`). OBJ import (objects, MTL
  materials and textures, concave polygons) and export (with an MTL and the textures beside
  it). glTF/GLB import (scenes, shared meshes as components, materials, textures, texture
  transforms) and export (shared meshes, embedded textures). Overlapping parts of one mesh import
  as separate groups; equal materials are reused; edges smooth in the source (shared vertex
  normals) import soft. A 100 MB glTF imports in about 11 s.
  `validate` is much faster on large meshes. Images import as image entities or textures, CSV
  points as guide points, `.plyr`/`.plyrc` documents merge as groups or components; `.plyr`
  copies and a CSV component report export. DXF import (layers as tags, blocks as components,
  arcs, splines, 3D faces) and 3D export. Drawings of a view (SVG, PDF, 2D DXF) with hidden
  lines removed, at a scale or fitted to the paper, from the current view or a scene.
  File › Import… and File › Export ► (3D Model, 2D Graphic, Component Report) in the GUI.
  The server's capabilities list
  the formats this build reads and writes.

## 0.4.0 — Organization and materials (2026-10-26)

- **Textures**: `material-create`/`material-edit` load a texture image (`--texture`,
  `--texture-width`, `--texture-height`, `--colorize`); `material-sample` reads a face's
  material; `material-library-save`/`material-library-load` write and read `.plyrm` libraries
  with their images. File commands go through the engine's storage (undoable image loads).
- **Components**: `make-unique`, `component-insert` (from the model or a `.plyrc` file, glued to
  a face), `component-save-as`, `component-reload`, `component-replace`. Definition UUIDs are
  now content hashes, so components from different documents no longer collide.
- **Bundled materials**: ten procedurally generated textured materials in the Materials panel;
  textures render in their own colors; Paint paints the side facing the eye.
- **Images and textures**: `image-insert` (image entities, drawn textured), `explode` of images
  into textured faces, `texture-position` (offset, rotation, scale) with a GUI window.
- **Tags**: `tag-move` and `tag-folder-set-visible`; the Tags panel shows folders.
- **Gluing**: glued components follow their face (move, rotate, scale, push/pull); components
  that cut openings show a hole in their face; billboard components face the camera.
- **GUI panels**: Materials, Components (with a placement tool) and Styles (with watermarks and
  fog), using the new commands and file dialogs.
- **Renderer**: distance fog and style watermarks (behind or over the model).
- **Styles**: `style-create`, `style-delete`, `style-edit`, `style-activate`,
  `style-watermark-add`/`-remove`, `fog-set`, and `.plyrs` style libraries.
- **Scenes**: capture hidden entities, section states and shadows too; scene descriptions,
  transition times and "include in animation"; the GUI animates transitions and plays the
  scene animation.

- **Look of the GUI**: light and dark themes (View › Theme), an icon toolbar (file, undo/redo,
  tools, standard views, face styles), tool cursors, a right-click context menu, scene tabs
  over the viewport, a clearer status bar with the value field always visible, and richer Tool
  Options (Copy for Move/Rotate, Keep the start face for Push/Pull).
- **Renderer**: the style's sky gradient (and ground color) behind the model; drawing axes
  (View › Axes), depth-tested and dashed on the negative side.
- **Tests**: whole-window GUI screenshots through wgpu (`PLANYRITH_SCREENSHOTS`).
- **Inference**: the center of a circle, arc or polygon snaps when the cursor is over its face,
  without hovering the curve first; the axes' origin snaps like an endpoint ("Origin"); points
  on the drawing axes and their crossings with edges and drawing directions snap; passing over a
  point offers its perpendicular projection onto the current direction (dotted guide).
- **Typed points**: `x,y,z`, `[x,y,z]` or `<dx,dy,dz>` in the value field place any point a tool
  asks for (first corner, center, next point, grab point...).

## 0.3.0 — Interactive modeling with inference (2026-10-25)

- **Inference engine** (`planyrith_core::inference`, docs/03 §7): endpoint, midpoint,
  intersection, center, on-edge, on-face, tangent, perpendicular, parallel, from-point, on-axis
  and on-plane inferences ranked by priority, with occlusion, hover references, axis and `Shift`
  locks and tool constraints. `planyrith infer` runs it for one ray (golden-tested).
- **Interactive tools** in the GUI for every 0.1 drawing and editing command: Select, Line,
  Rectangle, Circle, Polygon, Arc, 2 Point Arc, 3 Point Arc, Pie, Push/Pull, Offset, Move,
  Rotate, Scale, Eraser and Paint, with single-letter shortcuts, a toolbar and Draw/Tools menus.
- **Value field**: type without focusing; lengths, coordinates, `w,h`, angles, `Ns` segment
  counts, `500r` radii, `*N`/`/N` arrays; a value typed right after a tool completes replaces the
  last command.
- **Previews**: tools show the result on a copy of the document while the pointer moves.
- **Keys**: `Tab` cycles inference candidates, `Backspace` removes the last point of a line
  chain, `Enter` alone confirms the point under the cursor; typed distances follow the
  direction of the pointer's motion.
- **QA**: `docs/qa/model-a-room.md` ("model a room in 10 steps") with an automated twin that
  drives the whole window in `cargo test`.
- **Panels**: Tool Help shows the active tool's steps; Tool Options shows its settings (segments,
  sides, paint material).

## 0.2.0 — Viewport (2026-10-25)

- **Renderer** (`planyrith-render`): offscreen wgpu rendering of the draw list with 4× MSAA and
  reversed-Z depth; face styles (shaded, textured, monochrome, hidden-line, X-ray, wireframe);
  edges with GPU-computed profiles and dashed back edges; transparency; selection overlay and
  edit-context fading; an id buffer for hover picking. Golden-image snapshot tests run on Linux
  (lavapipe), Windows (WARP) and macOS (Metal).
- **`planyrith render`**: headless PNG/JPEG/WebP of the document camera, a scene, a `--camera`
  description or every scene.
- **GUI** (`planyrith-gui`): docked shell with the 3D viewport, orbit/pan/zoom, click and
  double-click selection, Entity Info, Tags, Scenes and Outliner panels, undo/redo, open/save
  with an unsaved-changes guard.
- **Server and shell**: `serve` over a Unix socket or a localhost TCP port (with `--token` and
  `server.auth`), `document.render_mesh` and `command.preview` for remote viewers; `planyrith
  repl` runs batch lines interactively.
- **Commands**: `camera-set`, `camera-standard`, `position-camera`, `orbit`, `pan`, `zoom`,
  `zoom-extents`, `style-set`, `scene-add`, `scene-update`, `scene-delete`, `scene-rename`,
  `scene-reorder`, `scene-activate`.
- **Core**: camera-relative reversed-Z projections and navigation math, render-mesh extraction
  with smooth normals, the draw list, and the click-selection rule.

## 0.1.0 — Core kernel and CLI foundation (2026-10-25)

The first code release: a headless modeler driven entirely from the command line. There is no
viewport or GUI yet (milestone 0.2).

### Kernel and document

- Half-edge mesh with the invariants of docs/03 §3.1, automatic face finding on coplanar closed
  loops, edge splitting at intersections, face splitting and merging, erasing, and vertex moves
  with auto-fold of faces that would bend.
- Push/pull with the cases of docs/03 §4.9: extrusion of faces with holes, notches and grooves
  at a face boundary, stretching the top of a box, joining onto a coplanar face, cutting through
  holes, and refusal of pushes that would turn a solid inside out.
- 2D offset of face loops with clipping, tessellation and spatial indexes.
- Document model: edges and faces, groups, components (definitions and instances), tags and tag
  folders, color materials, scenes (camera), attributes, stable persistent ids (`@pid`), entity
  paths and the selection query language of docs/08 §5.
- Command system with typed, JSON-schema-described parameters, transactions and byte-exact
  undo/redo.

### Commands

`line`, `rectangle`, `circle`, `polygon`, `arc-2point`, `arc-3point`, `arc-bulge`, `pie`,
`push-pull`, `offset`, `move` and `rotate` (with copies and arrays), `scale`, `erase` (erase,
soften, smooth, hide and their inverses), `delete`, `select`, `hide`, `unhide`, `lock`,
`unlock`, `make-group`, `make-component`, `explode`, `tag-create`, `tag-rename`, `tag-delete`,
`tag-assign`, `tag-set-visible`, `tag-folder-create`, `tag-folder-delete`, `material-create`,
`material-edit`, `material-apply`, `material-delete`, `material-purge`.

### Files

- Native `.plyr` container (docs/09): versioned manifest, checksummed parts, optional edit
  journal, deterministic output; damaged optional parts are dropped with a warning.
- `json` export, with a canonical form for version control.
- `validate` and `repair` (merge near vertices, remove degenerate faces, fix dangling
  references).

### Command line

- Session verbs `new`, `info`, `query`, `tool`, `export`, `validate`, `repair` and `batch`;
  every command is `planyrith tool <name>` with options generated from its schema, units on
  every length, `--json`/`--jsonl` output and stable exit codes.
- `serve`: a JSON-RPC 2.0 server over standard input and output, through which every command,
  undo/redo, queries, selection and context are reachable.
- Same commands, same bytes: documents are bit-identical across runs and platforms, and a script
  run as separate invocations gives the same document as one batch.

### Quality

- Property tests for the math and kernel, randomized kernel operation sequences, an engine fuzz
  test over random command sequences (validation, undo/redo and save/reopen checks), undo/redo
  byte equality for every command, and a CLI golden test per command.
- Milestone acceptance: a batch script builds a furnished room (walls with a door and a window, a
  table, four chairs as instances of one component), the file validates clean and re-saves
  byte-identically.

### Not yet

The viewport, GUI and headless `render` (0.2), interactive tools and inference (0.3), importers
(0.5), scripting (0.6), the server's socket transport and `repl` (0.2).
