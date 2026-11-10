# Changelog

All notable changes to Planyrith are recorded here. Versions follow
[Semantic Versioning](https://semver.org/); before 1.0, minor versions are milestones of
[`docs/13-roadmap.md`](docs/13-roadmap.md).

## Unreleased

### Added
- Per-person undo when working together (docs/08 §3.1): `history.undo {mine: true}` and
  `history.redo {mine: true}` take back or re-apply one's own newest change, under newer
  changes of others when they touched nothing of it; `session.status` reports one's own
  labels. Joined desktop sessions undo their own changes. The core's `Engine::revert` takes
  back any applied step under the same rule, and undo steps have serial numbers.
- Milestone 1.8, measuring and making (docs/05 §4.21–§4.23, §8.3–§8.5): `mirror`, `revolve`
  and `pattern` (rows and grids, around an axis, along edges); `mass-properties` (materials
  have a density), `interference-check` and `cut-list`; Tools › Mirror…, Revolve…, Pattern…
  and Measure… in the GUI.
- Milestone 1.9, constrained sketches (docs/03 §11, docs/05 §5.15): geometric constraints and
  driving dimensions on edges and edge ends, kept by a solver that keeps faces flat and reports
  the degrees of freedom left; `constraint-add|edit|delete|list|solve` and Tools › Constraints….
- Milestone 1.10, feature history (docs/05 §5.16): parametric parts whose contents are
  regenerated from an ordered list of features (commands with parameters, formulas of the part's
  parameters), with suppression, reordering, rollback and configurations; Tools › Features…
  records what is drawn in an open part.
- Milestone 1.11, solid features (docs/05 §8.6–§8.11): `hole` (plain, clearance, tapped with a
  cosmetic thread, counterbore, countersink; metric M2–M24), `shell`, `draft`, `split-solid`,
  `dome` and `rib`, on loose solids and in parts; Tools › Solid Features….
- Milestone 1.12, assemblies (docs/05 §5.17): mates between groups and components (fixed,
  coincident, distance, parallel, perpendicular, angle, concentric) kept by a rigid-body
  solver with the freedom left shown; driving mates; exploded views; the bill of materials;
  Tools › Assembly….
- Milestone 1.15, surfaces and analysis (docs/05 §8.14–§8.15): `surface-fill` (blended
  four-sided and radial patches over a closed boundary), `surface-extend`, `surface-trim` by a
  plane and `surface-knit` into one shell or solid; curvature, zebra, draft and wall-thickness
  analyses shown as colored copies, curvature combs along edges, `analysis-clear`;
  Tools › Surfaces… and Tools › Analysis….
- Milestone 1.14, sheet metal and frames (docs/05 §8.12–§8.13): sheet-metal parts with edge
  flanges, gauge changes and flat patterns with bend lines; frame members along edges with a
  profile library (tubes, angles, channels, IPE beams, flats), mitered joints and their
  lengths in the cut list; Tools › Fabrication….
- Milestone 1.13, engineering drawings (docs/15 §9): projected (first or third angle),
  auxiliary, section (hatched, cutting only its view) and detail views that follow their base;
  broken views; tolerance frames, datums, surface texture and weld symbols; balloons numbered
  like the bill-of-materials table; ISO and ANSI sheet formats with border, zones, title block
  and projection symbol; matching tools in the Sheets editor.
- Joining from the browser (docs/08 §3.3): `planyrith serve --web <port|address>` takes
  browsers over an encrypted WebSocket and serves the web build from `--web-root`; the web
  build has File › Work Together…, opened filled in by `?join=<document>`.

### Fixed
- Native plugins faulted in Windows development and test builds: wasmi, optimized while
  the code calling it was not, mixed two calling conventions (docs/02 Decisions,
  2026-11-10). Release builds were not affected.

## 0.11.0-alpha.4 — 1.0 groundwork and the 1.1–1.6 previews (2026-11-09)

A preview of everything since alpha.3: the 1.0 groundwork (frozen file format
and API, installers, Italian interface, accessibility) and the features
planned for 1.1 to 1.6. The 0.11 and 1.0 acceptance criteria are not met yet,
so this stays a pre-release.

- Fixed: a push/pull could fail with an internal mesh error after the document
  had been saved and reopened, where it succeeded in the original session.
- Fixed: faces created by one operation could get their ids in a different
  order after saving and reopening; they now follow their positions, and
  faces lying in an axis plane store that plane exactly.
- Fixed: splitting a face into equal halves could give its identity to either
  half depending on whether the document had been reopened; exploding a group
  onto a face it cuts now warns (W2008) when one of its faces cannot be kept.
- Encrypted connections: a server on a network address
  (`planyrith serve --socket 0.0.0.0:7400 --token …`) encrypts every
  connection, with keys derived from the token; File › Work Together…
  connects encrypted to such addresses.
- Fixed: when working together, `last` in a query could name what another
  person had just created; it is now each person's own.
- Fixed: bounds of turned components nested in turned groups were too large,
  and Zoom Extents counted hidden geometry; zoom now fits what is shown.
- Fixed: imported meshes whose parts overlap in one plane (cut-out figures,
  decals, doubled surfaces) lost those parts entirely; they now arrive as
  separate groups. A real house model keeps 58 000 more faces.
- Fixed: path-traced images showed reversed faces in the default back color
  (blue floors in real models); an unpainted back now shows the front's
  material. Images now have bounds (zoom to selection, queries), queries name
  an image's material, and `render --gpu` reports the graphics card used.
- Work together: several people edit one document at once through
  `planyrith serve` (now also on a network address). Each has a selection of
  their own, sees who else is connected and their selections in their colors,
  and gets the others' changes within half a second (File › Work Together…).
- Touch layout for tablets and phones: larger controls, the viewport across
  the window with panels in a drawer, and Shift, Ctrl and Alt buttons that
  stay down for the tools. It turns on by itself on touch or in a narrow
  window (View › Touch Layout).
- Path tracing on the graphics card: the Path-Traced Preview uses it when the
  card supports compute shaders, and `planyrith render --path-traced --gpu`
  renders final images about ten times faster.
- Path-Traced Preview (View menu, desktop): a window in which the path
  tracer refines an image of the current view while you work, starting
  again when the model or the camera changes.
- Native plugins: an extension can be a WebAssembly module instead of a
  script (`[entry] wasm = "plugin.wasm"`), written in any language that
  compiles to it. It calls the same commands, is sandboxed and metered, and
  runs on the desktop and in the browser.
- FBX import and export, the format game engines and asset stores use:
  meshes, materials with colors and textures (embedded in the file when
  exporting), the model hierarchy and copies sharing one geometry, units
  and axes. Binary and text FBX 7 files are read; binary 7.4 is written.
- Match Photo (Camera › Match Photo…, `camera-match`): open a photograph over
  the view, lay two red lines and two green lines along edges of the room or
  building and mark the origin; the camera becomes the one that took the
  picture, ready to model over it.
- Parametric components: give a component parameters such as `width = 80cm`
  and its parts rules such as "shelf width = width - 2 * board"
  (`parameter-set`, `part-rule-set`), then `parametric-update` resizes and
  places the parts. A copy can have its own values (in Entity Info or with
  `instance-parameter-set`) and becomes a component of its own.
- IFC import (IFC2X3 and IFC4): walls, slabs, columns, doors and the rest
  arrive as named groups tagged with their class, on their storeys, with their
  colors; extruded, faceted and tessellated geometry is read. Exporting again
  keeps classes and storeys.
- COLLADA (`.dae`) import and export: geometry, components shared between
  their copies, materials with colors and textures, units and up axis.
- Path-traced final images: `planyrith render --path-traced` traces the sun,
  the sky and light bouncing between surfaces, with reflections after each
  material's roughness and metalness; it runs on the CPU and gives the same
  image every time. In the desktop app: File › Export › Rendered Image….
- IFC export (IFC4) for BIM tools: each top-level group or component becomes
  a building element with its geometry and colors, in one storey — a wall, slab,
  door, window and so on when its tag says so ("Walls", "Muri", "Finestre"…) or
  an `ifc`/`class` attribute names the class, on the storey an `ifc`/`storey`
  attribute names, with its volume and a property set (tag, material, further
  `ifc` attributes); checked with IfcOpenShell.
- USD export (`.usda` text and `.usdz` packages for augmented-reality viewers),
  with colors, textures, roughness and metalness; checked with the reference
  USD library's validators.
- PLY files (meshes from 3D scanners and many other tools), ASCII or binary,
  with vertex or face colors, both ways.
- Bézier curves (Draw › Bézier Curve, `bezier`): click the start, two handles
  and the end of each span; closed flat curves form faces.
- Loft (`loft`, the Loft tool or Tools › Loft Selected Faces): a solid through
  two or more profiles, such as a square base rising into a circle.
- Round or cut the edges of solids: `fillet` (an arc of a given radius,
  smooth-shaded) and `chamfer` (a flat cut), on convex edges and inside
  corners alike; the Fillet and Chamfer tools round the edges you click with a
  live preview, and Tools › Round Edges… works on the selected edges.
- Thicken a surface into a solid (`thicken`, the Thicken tool or Tools ›
  Thicken…): flat or bent sheets keep their thickness on every face,
  optionally half on each side.
- A realistic look for styles (Styles › Realistic lighting, or `style-edit
  --realistic`): light from the sky as well as the sun, highlights after each
  material's new roughness and metallic settings, ambient occlusion in creases
  and where objects meet, softer shadows and a filmic tone curve. Materials read
  and write roughness and metalness from glTF.
- 3MF files, the format 3D-printing slicers prefer to STL: export writes each
  object as a closed mesh with its colors and the chosen unit, and warns when
  an object is not a closed solid; import reads objects, components, colors
  and the build plate placements. File › Import and File › Export › 3D Model
  list it.
- Export for printing: `--on-plate` rests the objects on Z = 0 and
  `--per-object` writes each top-level group or component to its own file
  (STL, OBJ, glTF, 3MF); the Check for 3D Printing window has an
  **Export for Printing…** button that does both.
- Check for 3D Printing (Tools › Solids): lists every object with its size and
  volume and tells which are not closed solids — open edges and holes (marked in
  red in the view), stray edges, parts touching at a corner, parts facing inward.
  **Repair** closes holes, removes stray edges and turns inside-out parts right
  side out. The same from the command line: `planyrith tool solid-check` and
  `planyrith tool solid-repair`.
- Sharper shadows near you in large models: the sun's shadow map now has a second,
  finer layer for the part of the model you are looking at. A thin post on a large
  site now casts its shadow.
- Search the Materials, Components and Outliner panels by name; the Outliner
  opens the groups that hold a match.
- The component library shows a picture of each component.
- Drag a material onto a face, or a component or component file into the model,
  straight from its panel.
- Window › Layout Presets: Modeling, Organizing (outliner and tags beside the
  model) and Viewport Only.
- Window › Preferences… (Ctrl+,) gathers the interface settings in one place:
  language, theme, interface size, accessibility options, snap distance, wheel
  zoom direction, antialiasing and the recent list.
- A macOS disk image: one application for Apple silicon and Intel Macs, macOS 11
  or later, opening `.plyr` files from the Finder. `cargo xtask macos` builds it.
- A Flatpak bundle for Linux, sandboxed on the Freedesktop runtime, with the
  description software centers show. `cargo xtask flatpak` builds it.
- A documentation site: `cargo xtask site` turns the README, install notes,
  guide, changelog and specification into web pages, and the Pages workflow
  publishes them with the browser edition under `app/`.
- Commands, their parameters and outputs, and the server's methods are frozen at
  `schema_version` 1: scripts and remote clients written now keep working in
  every later 1.x, which may only add to them.
- The `.plyr` format is frozen at version 1: later versions may add to it but
  never drop or change what it holds, so files saved now open unchanged in every
  later 1.x. A corpus written by every command checks this on each build.
- A Windows Installer package (`.msi`) alongside the setup program, for scripted
  and managed installs: per-user, no administrator rights, the format module as
  a removable feature. `cargo xtask msi` builds it.
- Keep the window responsive while saving to SKP, with a progress dialog.
- Draw the style's edge effects: depth cue, extensions past the ends, endpoint
  emphasis and jitter, set in the Styles panel or with `style-edit`. SKP models
  that use them now look as styled.
- Cast shadows from edges that bound no face, such as wires and lines, when
  "From edges" is on in the Shadows window or `shadows-set --from-edges`.
- Show distant textures smoothly: textures now have mipmaps, so floors, fabrics
  and tiles seen from afar no longer turn into speckled noise.
- Place a texture with pins: `texture-position --pins` puts chosen points of
  the texture on chosen points of the face. One pin moves it, two also turn
  and scale it, three also skew it, four in perspective with `--projective`.
- View › Color-Blind Safe Axis Colors draws the axes in vermilion, bluish green
  and blue, which stay distinct for the common color-vision deficiencies.
- Snap marks differ by shape as well as color (the midpoint is now a triangle),
  and View › High-Contrast Snap Marks makes them larger and easier to see.
- Make the whole interface larger or smaller, from 75 to 200 %: View › Interface
  Size, or Ctrl+plus and Ctrl+minus. The size is remembered.
- A new look: flat controls on neutral grey surfaces, rounded corners and one
  blue accent, dark by default (View › Theme switches to light or the system's).
- The interface speaks Italian: View › Language › Italiano, or automatically on
  an Italian system. Menus, panels, windows, tools, dialogs, step prompts,
  inference labels, command names, sheet settings, status messages and the
  kind of each error are translated; an error's details stay in English.
- Show large models sooner after opening: preparing a million-triangle model
  for display went from about 5 s to about 1 s.
- Hover, snap and click much faster in large models: on a 160 MB house the
  typical cursor move went from 2.4 ms to 0.1 ms. Finding the active section
  plane walked every face and edge of each container on every move.
- Move a texture by its pins in the model: Position Texture › Pins… shows the
  corners of one repeat; click a pin, then click where it goes. Red moves the
  texture, green turns and scales it, blue and yellow skew it; free pins give
  a perspective placement.
- Project a texture from a plane with `texture-position --project-from`, and
  continue a face's texture onto other faces with `--copy-to`, without seams.
- Browse a folder of component files in the Components panel's Library and
  place one by clicking it, then clicking in the model (desktop).
- Two saves of the same file at once, from two windows or the command line
  and the GUI, no longer make one of them fail with "not found".
- Select images by clicking them, as other entities; geometry in front of an
  image still takes the click.
- glTF cameras import as scenes, and scenes export as glTF cameras, so views
  survive a round trip through glTF.
- `planyrith render --shadows` now draws the sun's shadows, from the document's
  sun settings, without changing the document. It used to only print a warning.

## 0.11.0-alpha.3 — Real models and verified exchange (2026-11-01, prepared but not published; included in alpha.4)

- Keep the window responsive while a document opens, with a progress dialog.
- Open large foreign models about three times faster: attributes are found by
  owner instead of a scan per entity.
- Save large native documents much faster: a 160 MB model went from about 18 s
  to 2 s for open, edit and save, for a 9% larger file.
- Open SKP models from their saved view, which was being lost for every model,
  and frame the model when there is none.
- Open large real models that failed: custom line styles on tags (now native tag
  styles) and textures embedded as Photoshop, TIFF, GIF or Targa images, converted
  losslessly. An unreadable image no longer stops a model from opening.
- Open all 680 surveyed public files from 2013–2026, fixing models that failed
  outright: text attached to a face, large archives, layers colored by a
  textured material, a custom tag ahead of the default layer, classic archives
  with a 2021 header, face rings that cross or repeat an edge, and slivers
  thinner than the length tolerance.
- Save every surveyed file with both writers. A face or image that cannot be
  represented no longer stops the save; texture tints now transfer.
- Keep custom stroke images referenced, so `repair` no longer deletes them and
  invalidates byte-exact restoration.
- Add `component-rename`, in the Components panel by double-clicking the name.
- Fix radial dimensions attached to curves away from the origin, whose attachment
  point was misplaced, and imported arcs with opposed axes.
- Stop applying misread display settings: models from 2013–2016, 2019 and 2020
  opened as X-ray on a transparent background. They now use native defaults with
  a notice; their saved styles are not imported yet.
- Keep scene styles from multiplying on every reopen. Keep scene transition times,
  slide settings, vertex-anchored dimensions and guide and section attributes
  through regeneration.
- Refuse releases before 2013 by name instead of reporting undecoded geometry,
  and report each identical import notice once.
- Add files saved by real 2018, 2021 and 2022 releases, and
  `tools/check-skp-corpus.py`, which runs the exchange chain over a local corpus
  and compares surfaces with exports of the original application.

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
