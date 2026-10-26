# Changelog

All notable changes to Planyrith are recorded here. Versions follow
[Semantic Versioning](https://semver.org/); before 1.0, minor versions are milestones of
[`docs/13-roadmap.md`](docs/13-roadmap.md).

## Unreleased

- **Textures**: `material-create`/`material-edit` load a texture image (`--texture`,
  `--texture-width`, `--texture-height`, `--colorize`); `material-sample` reads a face's
  material; `material-library-save`/`material-library-load` write and read `.plyrm` libraries
  with their images. File commands go through the engine's storage (undoable image loads).
- **Components**: `make-unique`, `component-insert` (from the model or a `.plyrc` file, glued to
  a face), `component-save-as`, `component-reload`, `component-replace`. Definition UUIDs are
  now content hashes, so components from different documents no longer collide.

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
