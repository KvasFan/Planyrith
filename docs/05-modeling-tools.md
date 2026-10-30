# 05 — Modeling tools (command catalogue)

**Audience:** implementing agents. Normative. Each tool is one `Command` in
`planyrith-core::commands` (kebab-case name), one subcommand in `planyrith tool <name>` and one
interactive `Tool` state machine in `planyrith-gui::tools`. Geometry algorithms are referenced
from `docs/03`; CLI syntax from `docs/08`; interaction details from `docs/07`.

## 1. Conventions used in this catalogue

Each entry has:

- **Name** — registry/CLI name. **Category** — `draw | edit | organize | annotate | guide | solid | view | file`.
- **Purpose** — one sentence.
- **Params** — the serde `Params` struct (field: type — meaning). Types: `Length`, `Angle`,
  `Point` (`DVec3`), `Vector`, `Plane`, `Sel` (selection query, `docs/08 §5`), `EntityRef`,
  `Count`, `bool`, enums. Optional fields have defaults in parentheses.
- **Interactive** — the GUI step sequence (clicks/keys), what the numeric entry field (value field,
  the measurement input, see docs/07 §2) accepts at each step, and which inference kinds are active.
- **Behavior** — rules beyond the kernel algorithm.
- **Output** — the `Output` struct.
- **Undo** — grouping/labels. **Tests** — minimum acceptance tests.

Common `Params` fields available on every tool: `context: Option<EntityPath>` (edit context,
default = current), `dry_run: bool` (validate + report only).

Common value field grammar (`units.rs`): `1200` (document units), `1.2m`, `47in`, `3' 6"`, `3'6`,
`120cm`, lengths with `,` as pair separator for 2D (`800,600`), `/` for divisions (`/5`),
`*` or `x` for multiples (`*3`, `3x`), angles `45` or `45deg` or `0.785rad`, slopes `1:12`,
counts `3`, segment or side counts `48s`, radii `500r` or `1.2m r`, and points: `x,y,z` or
`[x,y,z]` absolute, `<dx,dy,dz>` relative to the step's reference point (the last point). Negative values reverse direction. Pressing Enter without a value confirms the
current inferred value.

## 2. Inference and constraints (shared by all drawing/editing tools)

See `docs/03 §7` for the engine and `docs/07 §5` for visuals. Every tool declares which kinds it
listens to and which constraint plane/line it imposes per step. Shared keys: `Shift` = lock
current inference; arrow keys = axis lock (`→` X/red, `←` Y/green, `↑` Z/blue, `↓` parallel or
perpendicular to hovered edge); `Esc` = cancel current step (second `Esc` = cancel tool);
`Ctrl`/`Cmd` and `Alt`/`Option` are tool-specific modifiers (listed per tool); `Space` = Select.

## 3. Drawing tools (category `draw`)

### 3.1 `line`
- **Purpose:** draw a chain of straight edges; faces form automatically.
- **Params:** `points: Vec<Point>` (≥ 2) — vertices of the polyline; `close: bool` (false);
  `axis_lock: Option<Axis>`.
- **Interactive:** click start → click next point… ; value field accepts a length (extends from last point
  along current inferred direction), or a coordinate `[x,y,z]` absolute / `<x,y,z>` relative.
  Chain continues until the loop closes (auto-end when clicking the start vertex or when a face
  closes), `Esc`, or double-click. Inference: all kinds. Rubber-band preview shows the segment and
  highlights the face that would be created.
- **Behavior:** uses `add_edge` for each segment (splits and face finding happen automatically).
  If `close` or the last point equals the first, the loop closes.
- **Output:** `{ edges: Vec<Pid>, faces_created: Vec<Pid>, faces_split: Vec<Pid> }`.
- **Undo:** one step per segment while drawing (so Backspace/undo removes the last segment); the
  CLI call is one step.
- **Tests:** 4 points closing on the ground → 1 face, 4 edges; diagonal across a face → 2 faces.

### 3.2 `freehand`
- **Purpose:** draw a freehand polyline (curve descriptor `Freehand`).
- **Params:** `points: Vec<Point>` (≥ 2), `simplify_tolerance: Length` (0.5 mm).
- **Interactive:** press-drag-release on a face or ground plane; points sampled every 2 px,
  simplified with Douglas–Peucker. Inference: on-face only (keeps the stroke planar).
- **Output:** `{ edges, curve: Option<Pid>, faces_created }`; `curve` names a
  member edge, like the other curve tools.
- **Algorithm:** consecutive samples within geometric tolerance are removed;
  iterative Douglas–Peucker preserves endpoints with deterministic tie handling.
  Zero simplification keeps distinct samples. Closing on the first point forms
  a closed chain; the kernel creates planar faces and records the Freehand curve.
  GUI samples are projected onto the initial face/ground plane; direct CLI points
  may describe a spatial polyline. Limit: 65535 samples.
- **Edge cases:** reject negative/non-finite tolerance, collapsed strokes and a
  closed stroke simplified below three corners. Esc cancels without mutation.
- **Undo/tests:** one stroke is one transaction; undo/redo restores native bytes.
  Test simplification error bounds, closed-face creation, CLI golden output and
  actual press/drag/release input with planar samples.

### 3.3 `rectangle`
- **Purpose:** rectangle from two opposite corners on a plane.
- **Params:** `corner_a: Point`, `corner_b: Point`, `plane: Option<Plane>` (inferred from face
  under cursor or ground), `aspect_lock: Option<AspectLock>` (`Square | Golden | None`).
- **Interactive:** click corner → move (value field `w,h`, `w` alone keeps the other dimension) → click.
  Dotted diagonal shows "Square"/"Golden section" inference. Second corner is inferred *on the
  plane* of the first corner (face plane if started on a face; ground if in space; or the
  plane perpendicular to the camera-nearest axis when starting on an edge/vertex off any face).
- **Behavior:** 4 edges via `add_edge`; one face (unless intersecting existing geometry).
- **Output:** `{ edges, face: Option<Pid> }`.
- **Tests:** on an existing face → splits it into 2 (if inside) and the rectangle face.

### 3.4 `rotated-rectangle`
- **Params:** `origin: Point`, `edge_end: Point`, `width: Length`, `angle: Angle` (90°) — the
  second side direction measured from the first edge in the plane.
- **Interactive:** click origin → click/type first edge (length) → move for width and angle
  (value field `width,angle`) → click.
- **Purpose:** orient the first edge freely on a drawing plane and enter the
  second side exactly. `normal: Option<Vector>` sets the plane; default Z,
  or X when the first edge is vertical. A tilted first edge needs its plane normal.
- **Algorithm:** normalize the first edge `u`, validate perpendicular unit normal
  `n`, and construct the second side as `width * (u*cos(angle) + (n×u)*sin(angle))`.
  At 90° it is a rectangle; other non-collinear angles make an oblique quadrilateral.
  Signed angles select the other side. Four edges go through normal face finding.
- **Edge cases:** width and first edge must exceed geometric tolerance; reject
  collinear angles, non-finite/out-of-range corners and an incompatible normal.
- **Output/undo/tests:** same result as `rectangle`; one transaction, byte-exact
  undo/redo, side-length properties, vertical-plane CLI golden and GUI numeric input.

### 3.5 `circle`
- **Params:** `center: Point`, `radius: Length`, `normal: Vector` (plane normal; inferred),
  `segments: Count` (24, min 3, max 999).
- **Interactive:** hover to pick plane (face/ground/axis-aligned via arrow keys) → click center →
  move (value field `radius` or `Ns` for segments before clicking, e.g. `48s`) → click. Inference:
  center snaps to all kinds; radius point to endpoints/edges (sets radius).
- **Behavior:** polyline with `Circle` descriptor; member edges `SOFT|SMOOTH`; one face.
- **Output:** `{ edges, face, curve }`.

### 3.6 `polygon`
- **Params:** `center`, `radius`, `normal`, `sides: Count` (6, 3..=999), `inscribed: bool` (true).
- **Interactive:** as circle; value field `Ns` sets sides.

### 3.7 `arc-2point` (center-start-end), `arc-3point`, `arc-bulge` (start-end-bulge), `pie`
- **Params:** `arc-2point { center, start: Point, end_angle: Angle | end: Point, normal, segments: Count(12) }`;
  `arc-3point { a, b, c: Point, segments }`; `arc-bulge { start, end, bulge: Length | through: Point, segments }`;
  `pie` = `arc-2point` plus the two radii edges and a face.
- **Interactive (`arc-bulge`, default Arc tool):** click start → click end → move for bulge
  (value field `bulge` length or `rR` radius, `Ns` segments) → click. Inference: "Half circle"
  (magenta) when bulge = chord/2; "Tangent at vertex" when starting at an endpoint of an existing
  arc/edge — magenta arc preview; double-click accepts the tangent arc.
- **Behavior:** `Arc` descriptor; `SOFT|SMOOTH` member edges. Arcs on a face split it.
- **Tests:** arc + chord → face with N+1 edges; tangent continuation produces G1 continuity
  within `angle` tolerance.

### 3.8 `text-3d`
- **Purpose:** generate faces (optionally extruded) from text glyph outlines.
- **Params:** `text: String`, `font: String` (bundled OFL font by default), `height: Length`,
  `extrude: Length` (0 = flat faces), `filled: bool` (true), `align: Left|Center|Right`,
  `placement: Transform` (where to put the resulting group).
- **Behavior:** glyph outlines via `ttf-parser`, quadratic/cubic curves flattened to the curve
  tolerance, holes handled by orientation; result is a **group** (`Text3D` is not an entity).
- **Output:** `{ group: Pid }`.

## 4. Editing tools (category `edit`)

### 4.1 `select`
- **Purpose:** set the selection (not a document mutation; still a command for scripts).
- **Params:** `query: Sel`, `mode: Replace|Add|Remove|Toggle` (Replace).
- **Interactive:** click (replace), `Shift`+click toggle, `Ctrl`+click add, `Shift+Ctrl` remove;
  drag left→right = window (fully inside), right→left = crossing; double-click a face selects
  face + bounding edges; triple-click selects all connected geometry; double-click a group/
  instance enters it (edit context). Right-click context menu lists `select` sub-queries:
  bounding edges, connected faces, all connected, all on same tag, all with same material,
  all instances of this component.
- **Output:** `{ count: usize, kinds: Map<Kind, usize> }`.

### 4.2 `erase`
- **Params:** `targets: Sel`, `mode: Erase|Soften|Smooth|Hide|Unsoften|Unsmooth|Unhide` (Erase),
  `erase_orphans: bool` (false).
- **Interactive:** click or drag-paint over edges/guides/groups; `Ctrl` = soften/smooth,
  `Shift` = hide, `Ctrl+Shift` = unsoften. Faces are not erased by the Eraser (select + Delete does).
- **Behavior:** `erase_edge` rules in `docs/03 §4.6`.
- **Output:** `{ erased: usize, faces_removed: usize, faces_merged: usize }`.

### 4.3 `push-pull`
- **Purpose:** extrude a face along its normal.
- **Params:** `face: EntityRef`, `distance: Length` (or `to: Point` — distance measured to a
  point/face), `keep_start_face: bool` (false; `Ctrl` modifier), `repeat_last: bool`
  (double-click repeats the last distance).
- **Interactive:** hover highlights the face; click → move (value field distance; negative pushes) →
  click. Inference during move: endpoints/faces anywhere in the model set the distance ("to
  this point"), e.g. pull to the height of another box. Preview shows the extruded volume.
- **Behavior:** `extrude_face` with merge and push-through rules (`docs/03 §4.9`). Works on
  curve-bounded faces (sides get `SOFT|SMOOTH` where the curve was smooth). Works inside the edit
  context only; cannot pull a face of a closed group from outside (enter the group).
- **Output:** `{ new_faces: Vec<Pid>, removed_faces: Vec<Pid>, through_hole: bool, solid_after: Option<bool> }`.
- **Tests:** `docs/03 §10` plus: pulling a face with a hole yields side faces for the hole;
  pulling a circle gives a cylinder with smooth sides; pull by typed `2m` exactly.

### 4.4 `move`
- **Purpose:** translate (and optionally copy) entities or vertices/edges/faces with auto-fold.
- **Params:** `targets: Sel`, `from: Point` (grab point), `to: Point` | `delta: Vector`,
  `copy: bool` (false; `Ctrl` toggles), `copies: Count` (1) with `array: Linear { count, spacing: Between|Total } | None`,
  `check_intersections: bool` (false: a move whose edges would cross other edges is refused;
  true: those edges are split, docs/03 §4.8), `auto_fold: bool` (true; `Alt` forces).
- **Interactive:** hover an entity (auto-select if nothing selected; with a selection, moving any
  selected entity moves all) → click grab point → move (inference everywhere; value field distance or
  coordinate) → click. After a copy-move, value field `*5` / `5x` makes 5 copies at that spacing,
  `/5` distributes 5 copies between. Hovering a component instance shows rotation grips
  (red crosses) on its bounding box faces: dragging one rotates about that face's axis
  (dispatches `rotate`).
- **Behavior:** vertices/edges/faces → `transform_vertices` (`docs/03 §4.8`); groups/instances →
  transform update (no geometry change). Moving an instance glued to a face slides it on the face.
  Copy of a group → new instance of the same hidden definition (copy-on-edit, `docs/04 §3.1`).
- **Output:** `{ moved: usize, copies: Vec<Pid>, folded_faces: usize, merged_vertices: usize }`.
- **Undo:** one step including all array copies.
- **Tests:** move a vertex of a cube top → auto-fold creates 1 hidden edge; move with `copy` ×3
  → 3 new instances; typed `1500` along locked red axis moves exactly 1500 mm in X.

### 4.5 `rotate`
- **Params:** `targets: Sel`, `center: Point`, `axis: Vector`, `angle: Angle` | `from_point,
  to_point: Point` (angle measured in the plane), `copy: bool`, `copies: Count`, `array: Radial { count, spacing: Between|Total }`.
- **Interactive:** protractor cursor orients to the hovered face/plane (arrow keys lock axis);
  click center → click first reference point → move (value field angle; snaps at 15° increments when
  angle snapping is on) → click. `Ctrl` copy; `*N`/`/N` radial arrays.
- **Behavior:** rotating vertices may auto-fold like Move; rotating a sub-selection of a face's
  edges folds the face.
- **Tests:** rotate copy `*11` by 30° → 12 instances evenly spaced; exact 90° preserves axis alignment within angle tolerance.

### 4.6 `scale`
- **Params:** `targets: Sel`, `handle: ScaleHandle` (`Corner|Edge|Face` with sign bits) or
  `factors: Vector`, `about: Center|Opposite` (Opposite; `Ctrl` = about center), `uniform: bool`
  (`Shift` toggles), `pivot: Point`.
- **Interactive:** selection shows a green-grip bounding box (in the edit context's axes, or the
  component's axes for a single instance); drag a grip; value field accepts a factor (`2`), two or three
  factors (`2,1.5`), or an absolute length with units (`3m` sets that dimension). Negative
  factor mirrors.
- **Behavior:** vertex scaling for raw geometry; transform scaling for groups/instances
  (instances store non-uniform scale in the transform — shared definition untouched). Mirroring
  (negative determinant) flips face orientation display, not stored normals.
- **Output:** `{ factors: Vector, bbox_after: Aabb }`.

### 4.7 `offset`
- **Params:** `face: EntityRef` | `edges: Sel` (coplanar connected chain), `distance: Length`
  (negative = inward), `repeat_last: bool`.
- **Interactive:** hover face or edge chain → click → move (value field distance) → click; double-click
  repeats the last offset on the hovered face.
- **Behavior:** `offset_face_boundary` (`docs/03 §4.10`). Result edges create a new face
  (ring) automatically.
- **Tests:** offset a square by −100 → inner square, ring face + inner face; offset an L-shape
  inward past its narrow width → clipped result without self-intersection.

### 4.8 `sweep`
- **Params:** `profile: EntityRef (face)`, `path: Sel (connected edges)` | `path_face: EntityRef`
  (use the face's boundary as path).
- **Interactive:** (a) preselect path edges, pick tool, click profile face; (b) click profile, then
  drag along edges (path highlighted red, must be contiguous); (c) `Alt`+click a face uses its
  boundary. Preview shows the swept solid.
- **Behavior:** `sweep_profile` (`docs/03 §4.11`). Profile must be perpendicular-ish to the first
  path edge (angle < 45°); otherwise error `NotApplicable` with the angle.
- **Tests:** `docs/03 §10`; sweep along an arc produces `SOFT|SMOOTH` seams.

### 4.9 `intersect-faces`
- **Params:** `targets: Sel`, `scope: model|selection|context` (required).
- **Behavior:** `docs/03 §4.12`.
- **Output:** `{ edges_added: usize }`.

### 4.10 `flip`
- **Params:** `targets: Sel`, `axis: X|Y|Z` (in context or component axes), `plane: Option<Plane>`.
- **Behavior:** mirror transform about the bounding-box center plane (or given plane); raw
  geometry gets vertex mirroring plus loop reversal to keep front faces front.

### 4.11 `reverse-faces` / `orient-faces`
- **Params:** `targets: Sel`. `orient-faces` propagates the orientation of one face across
  connected faces (consistent normals).

### 4.12 `soften-smooth`
- **Params:** `targets: Sel`, `angle: Angle` (20°) — edges between faces with dihedral angle
  below this get `SOFT` (and `SMOOTH` if `smooth: bool`), `soften_coplanar: bool` (false).
- **Interactive:** panel with a slider on the selection; live preview.

### 4.13 `hide` / `unhide` (`unhide --all`, `--last`)
- **Params:** `targets: Sel`. Hidden state is per entity; "show hidden geometry" renders them dashed.

### 4.14 `delete`
- **Params:** `targets: Sel`. Deletes any entity kind (faces, groups, guides…). Edges follow
  `erase_edge` rules; faces follow `erase_face` (edges kept).

### 4.15 `divide-edge`
- **Params:** `edge: EntityRef`, `segments: Count`. (Context menu "Divide" with value field.)

### 4.16 `explode-curve`, `set-curve-segments`
- `set-curve-segments { curve: EntityRef, segments: Count }` only while the curve is intact.

## 5. Organization tools (category `organize`)

### 5.1 `make-group`
- **Params:** `targets: Sel` (default: selection), `name: Option<String>`.
- **Behavior:** moves entities (and their connected edges? **No** — only the selected ones; edges
  shared with unselected faces are duplicated so both sides stay valid) into a new hidden
  definition; the group's origin is the selection's bounding-box min corner; axes aligned to context.
- **Output:** `{ group: Pid }`.

### 5.2 `make-component`
- **Params:** `targets: Sel`, `name: String`, `description: String`, `behavior: ComponentBehavior`
  (glue, cut opening, face camera, axes/insertion point), `replace_selection: bool` (true).
- **Interactive:** dialog with name, description, glue-to, cut-opening, always-face-camera,
  "Set component axes" (interactive 3-click axes placement), "Replace selection with component".
- **Output:** `{ definition: Uuid, instance: Pid }`.

### 5.3 `explode`
- **Params:** `targets: Sel` (groups/instances/images/curves/dimensions-groups).
- **Behavior:** moves the definition's entities into the parent context with the instance's
  transform applied; merges coincident vertices/edges with the parent (`add_edge` semantics for
  boundary edges only — interior geometry is inserted directly for speed); other instances of the
  definition are unaffected.

### 5.4 `make-unique`
- **Params:** `targets: Sel` (instances). Forks the definition for the selected instances only.

### 5.5 `component-reload`, `component-save-as`, `component-replace`
- `component-save-as { definition, path }` writes `.plyrc`; `component-replace { instances, with: Uuid|path }`.

### 5.6 `tag-create`, `tag-delete`, `tag-rename`, `tag-set-visible`, `tag-assign`, `tag-folder-*`
- `tag-assign { targets: Sel, tag: String }`. `tag-delete { tag, reassign_to: String("Untagged") }`.

### 5.7 `scene-add`, `scene-update`, `scene-delete`, `scene-reorder`, `scene-activate`, `scene-rename`
- `scene-add { name, capture: SceneCaptureFlags }` captures the current camera/tags/hidden/
  sections/style/shadows per flags. `scene-activate` is a view command (animates in GUI;
  `--no-transition`). `scene-update { scene, capture }`, `scene-delete { scene }`,
  `scene-rename { scene, new_name }`, `scene-reorder { scene, index }`.

### 5.8 `material-create`, `material-edit`, `material-delete`, `material-apply`, `material-sample`, `material-purge`
- `material-apply { targets: Sel, material: String, side: Front|Back|Both(Front), fill: Single|Connected|SameMaterial|Object|All }`
  — `Paint`: click = single face; `Ctrl` = adjacent coplanar/connected faces with same
  material; `Shift` = all faces with the same material in context; `Ctrl+Shift` = same material
  on connected object; `Alt` = sample material from face (`material-sample`). Applying to a
  group/instance sets `material_override` (affects default-material faces only).
- `material-edit { material, color?, opacity?, texture?: path|ImageRef, texture_size?: (w,h), colorize?: mode }`.
- `material-library-save { path, materials? }` writes a `.plyrm` library; `material-library-load
  { path, replace? }` adds its materials (see Decisions).

### 5.9 `texture-position`
- **Params:** `face: EntityRef`, `side`, `mapping: UvMap` or pins `{ fixed: [(uv, point);4] | free: [(uv, point);4] }`,
  `project_from: Option<Plane>` ("projected" mapping), `copy_to: Option<Sel>` (paint sampled
  texture coordinates onto other faces).
- **Interactive:** right-click face → Texture → Position: 4 pins (fixed mode: move/scale/
  rotate/skew pins; free mode: 4-point projective). `Esc`/Enter.

### 5.10 `axes-set`, `axes-reset`
- `axes-set { origin: Point, x: Vector, y: Vector }` relocates the drawing axes of the current
  context (document-level for model space; component axes inside a definition).

### 5.11 `section-plane-add`, `section-plane-toggle`, `section-plane-reverse`, `section-plane-align-view`, `section-plane-delete`, `section-create-group-from-slice`
- `section-plane-add { plane: Plane, name?, symbol?, activate: bool(true) }`; interactive: the
  plane follows hovered faces/axes (arrow keys lock), click to place. `section-create-group-
  from-slice { plane }` turns the cut lines into a group of edges (for 2D export).

### 5.12 `purge-unused`
- **Params:** `what: Set<Definitions|Materials|Tags|Styles|Images>` (all).

### 5.13 `lock` / `unlock`
- **Params:** `targets: Sel` (groups/instances). Locked entities cannot be selected for editing
  (they still infer).

## 6. Guide tools (category `guide`)

### 6.1 `tape-measure`
- **Params:** `from: Point`, `to: Point`, `create_guide: bool` (true; `Ctrl` toggles), `guide:
  Line|Point|Finite` (Line when starting on an edge: parallel guide line at distance; Point when
  starting on a vertex/endpoint: guide point at distance along direction).
- **Interactive:** click → move (value field distance) → click. Measures and shows the distance in the
  value field; typing a new value **after** a measurement between two points (and no guide) prompts
  `resize-model` (scale the entire model/context so that measurement becomes the typed length).
- **Output:** `{ distance: Length, guide: Option<Pid> }`.

### 6.2 `protractor`
- **Params:** `center`, `axis`, `from_point`, `angle: Angle` | `to_point`, `create_guide: bool (true)`.
- **Behavior:** measures angles; creates an angular guide line through center.

### 6.3 `guide-delete-all` (`erase-guides`)

### 6.4 `resize-model`
- **Params:** `measured: Length`, `target: Length`, `scope: Context|Model`. Scales uniformly
  about the context origin. Asks confirmation in GUI.

## 7. Annotation tools (category `annotate`)

### 7.1 `dimension`
- **Params:** `kind: Linear|Aligned|Radius|Diameter`, `start: Anchor`, `end: Anchor` (linear),
  `curve: EntityRef` (radial), `offset: Vector` (leader placement), `style: Option<DimStyle>`,
  `text_override: Option<String>`.
- **Interactive:** click first point (or an edge → both endpoints) → click second point → move
  to place the dimension line (axis-aligned inference for horizontal/vertical vs aligned) → click.
  Clicking an arc/circle creates a radial dimension (right-click toggles radius/diameter).
- **Behavior:** anchors re-measure (`docs/04 §3.5`). Dimension text uses document units and
  precision unless overridden with `<>` placeholder syntax (e.g. `<> (typ.)`).
- **Output:** `{ dimension: Pid, value: Length }`.

### 7.2 `text`
- **Params:** `text: String`, `anchor: Option<Anchor>`, `screen_offset: (f32,f32)`,
  `leader: None|View|Pushpin`, `font_size_px: f32`.
  The implemented typed command also accepts optional `layout` with normalized
  `screen_position`, `world_offset` in mm, leader visibility/arrow/color and
  font family/weight/point size or model height in mm (docs/04 Decisions).
  Invalid nonfinite placements or nonpositive sizes fail before mutation.
- **Interactive:** click an entity (default text = measured length for edges, area for faces,
  coordinates for vertices, name for components) → drag the label → click; type → Enter
  (`Shift+Enter` = newline). Click in empty space = screen text (no leader).

### 7.3 `label-style` (dimension/text style editing: font, arrow, extension lines, color)

## 8. Solid tools (category `solid`) — operate on solid groups/instances only

`solid-union`, `solid-subtract`, `solid-intersect`, `solid-trim`, `solid-split`, `outer-shell`.
- **Params:** `a: Sel`, `b: Sel` (ordered; subtract/trim = B cut by A); `outer-shell`/`union` take
  `targets: Sel`.
- **Interactive:** hover shows "Solid Group/Component (1)" / "(2)" badges; click first, click
  second. Non-solids are refused with a tooltip listing why (open edges count, interior faces).
- **Behavior:** `docs/03 §4.13`. Result is a **group** (loses component-ness) unless both inputs
  were instances of the same definition and `keep_component: bool`.
- **Output:** `{ result: Vec<Pid>, volume_mm3: f64, fallback_used: bool }`.
- **Tests:** `docs/03 §10`; subtract a cylinder from a slab → through-hole with smooth walls;
  trim keeps the cutter.

## 9. View tools (category `view`) — not undoable, do not dirty the document

- `orbit { yaw: Angle, pitch: Angle, pivot?: Point }` — middle-drag; `Shift` = pan; orbits about the
  point under the cursor at drag start (if geometry) else about the model center.
- `pan { delta: Vector }`, `zoom { factor, toward?: Point | window: Rect }`, `zoom-extents
  { targets?: Sel }` (with `targets`: zoom to selection), `zoom-previous`/`zoom-next`.
- `look-around { yaw, pitch }`, `walk { forward, strafe, eye_height: Length(1700) }` (with
  collision against faces optional), `position-camera { eye: Point, target: Point | look_dir }`.
- `camera-set { projection: Perspective|Parallel|TwoPoint, fov?: Angle }`, `camera-standard
  { view: Top|Bottom|Front|Back|Left|Right|Iso, extents: bool (false) }`, `camera-align-to-face { face }`.
- `field-of-view` (interactive drag changes FOV; value field degrees or `35mm` focal length).
- `style-set`, `shadows-set`, `fog-set`, `show-hidden-geometry`, `show-guides`, `show-section-planes`,
  `show-section-cuts`, `show-section-fill`, `xray`, `show-axes`.

## 10. File commands (category `file`) — see `docs/08 §6` and `docs/09`

`new`, `open`, `save`, `save-as`, `save-copy`, `import { path, format?, options }`, `export
{ path, format, options }`, `render { path, width, height, scene?, camera?, style?, transparent_bg }`,
`component-import-as-file`, `revert`, `purge-unused`.

- `import { path, format?, as?: group|component:<name>|image|texture, file_units?, scale (1), swap_yz?,
  merge_coplanar (true), soften (20°), at? }` reads `path` through the engine's storage into the
  edit context (docs/09 §5–§6, Decisions). Output: `{ format, created, faces, edges, skipped,
  warnings }`. Undo removes everything the import made (definitions, materials, images).
- `export { path, format?, targets?, include_hidden, file_units?, ascii, z_up, two_d, scene?,
  scale?, paper_width? }` writes the visible model, or the occurrences `targets` touches, in a
  mesh format, or — SVG, PDF, DXF with `two_d` — a drawing of the view (of `scene` when given)
  with hidden lines removed, at `scale` (`1:50`, parallel views) or fitted to `paper_width`
  (297 mm). It changes nothing and is not undoable. Output: `{ format, bytes_written, entities,
  warnings }` (`entities`: triangles, rows or drawn lines).

## 11. Tool metadata (for frontends)

```rust
pub struct CommandMeta {
    pub name: &'static str, pub category: Category, pub label: &'static str /* undo label */,
    pub icon: &'static str /* icon id */, pub shortcut: Option<&'static str>,
    pub steps: &'static [Step { prompt: &'static str, value_field: ValueFieldKind, inference: InferenceSet, constraint: ConstraintKind }],
    pub modifiers: &'static [(Modifier, &'static str)],
    pub undoable: bool, pub dirties: bool, pub needs_context: bool,
}
```
`planyrith tool <name> --steps` prints `steps`/`modifiers` as text and JSON so documentation and
UI hints are generated from one source.

## 12. Default shortcuts (GUI; user-remappable; CLI unaffected)

| Key | Tool | Key | Tool |
|---|---|---|---|
| Space | select | L | line |
| E | erase | R | rectangle |
| C | circle | A | arc-bulge |
| P | push-pull | M | move |
| Q | rotate | S | scale |
| F | offset | T | tape-measure |
| B | material-apply | O | orbit |
| H | pan | Z | zoom |
| Shift+Z | zoom-extents | G | make-component |
| X | solid tools menu | Ctrl/Cmd+Z/Y | undo/redo |

## 13. Acceptance checklist per tool (copy into PRs)

- [ ] Spec entry above complete (params, interactive, behavior, output, undo, tests).
- [ ] `Command` with `validate`/`apply`; `Params` has `JsonSchema` with units in docs.
- [ ] Registered; `planyrith tool <name> --help` and `--steps` render.
- [ ] CLI golden test(s); property tests where geometry is involved.
- [ ] GUI tool state machine; preview path; value field grammar; modifiers; inference set.
- [ ] Undo/redo returns byte-identical document (test).
- [ ] Documented in `docs/08` command table.

## Decisions

- 2026-10-29 — **Sheets (0.9, category `sheet`).** The complete command and
  parameter catalogue is docs/15 §5.1. Every paper item operation is a command,
  with a CLI golden and inverse-delta coverage. In the GUI choose its item tool,
  pick two paper corners or drag a frame, then edit typed properties. Dimensions
  snap to model vertices/edges through their viewport; ordinary paper points are
  also supported. Select moves items; Delete removes them; properties align or
  reorder them. Escape cancels placement. Page, item, layer and settings edits
  undo independently; freeze and its associated dimension snapshots undo together.
  Acceptance is docs/15 §8. `camera-set` also accepts eye/target/up, parallel
  height and aspect so Sheets camera editing uses the same view command.

- 2026-10-28 — **0.8 tools.** `sweep`, `intersect-faces`, `soften-smooth` and
  all six solid tools are registry commands, CLI subcommands and RPC commands.
  `sweep` requires exactly one of `path` and `path_face`; outputs `{faces, closed}`.
  The GUI supports preselected edges plus profile, or profile then path face.
  Closed path guide consumption is defined in docs/03 Decisions. Intersection
  writes only into the current edit context; model/context scope also considers
  transformed descendant occurrences. Coplanar face intersection adds no line.
  Softening sets or clears soft/smooth flags while preserving other edge flags;
  boundaries with one face are untouched. Its output is `{changed}`.
- 2026-10-28 — Solid commands accept `a`/`b` selecting exactly one distinct
  group or instance each; Subtract and Trim cut **B by A**, preserving A only
  for Trim. Union/Outer Shell alternatively accept `targets` with at least two
  solids. Nested instances and wire/open geometry are refused. Results are
  groups unless `keep_component` applies to instances sharing a definition.
  The whole operation is one undo step. CLI golden batches, byte-exact undo
  tests, cube inclusion–exclusion and cylindrical through-hole tests cover them.
- 2026-10-28 — `attribute-set` supports the scripting API (docs/10): parameters
  `targets: Sel`, `dictionary: String`, `key: String`, `value: JSON`; JSON null
  removes the attribute. Output is the affected entity count. It is a normal
  transactional registry command with CLI golden and undo tests.

- 2026-10-20 — Eraser never erases faces; `delete` does (§4.2/§4.14).
- 2026-10-20 — `make-group` duplicates shared boundary edges rather than stealing them (§5.1).
- 2026-10-20 — Solid tool results are groups unless `keep_component` (§8).
- 2026-10-20 — View commands are commands (scriptable) but not undoable (§9).

- 2026-10-20 — **Coordinates in params are world coordinates.** Commands convert points and
  directions into the edit context's container (the group or instance opened with `context` or the
  engine's edit context) with the inverse of its world transform, like clicks in the GUI.
- 2026-10-20 — **Curve member edges stay visible.** §3.5/§3.7 "member edges `SOFT|SMOOTH`" is read
  with docs/03 §3.3: it is the *inter-segment* edges of faces built along a curve (the sides of an
  extruded circle) that become `SOFT | SMOOTH`; the outline of a drawn circle or arc is plain.
- 2026-10-20 — Shape outputs report pids: `line` → `{edges, faces_created, faces_split}`;
  `rectangle` → `{edges, face, faces_split}`; `circle`, `polygon`, `arc-*`, `pie` →
  `{edges, face, faces_split, segments}` (curves have no pid, so `curve` is reported as the
  segment count). `faces_split` lists the *new* pieces of split faces (the original keeps its pid).
- 2026-10-20 — `rectangle` without `plane` uses the axis-aligned plane through both corners
  (ground-parallel when they share Z, else the vertical plane they share) and fails with `E1001`
  otherwise. `arc-bulge` measures the signed bulge to the left of the chord seen from `normal`
  (default +Z, or +Y for a vertical chord). `arc-2point` with `end` sweeps counter-clockwise
  about `normal` from the start to the end direction.
- 2026-10-20 — **Arrays.** `move`/`rotate` take `copy`, `copies` (count) and `spacing`
  (`between`: the offset/angle separates consecutive copies, the `*N` value field; `total`: it is
  the whole span, the `/N` value field) instead of a nested `array` object. Copies of raw geometry
  are redrawn with `add_edge` (faces form by face finding and take the originals' front side and
  materials, flipping the copied group as a whole when needed); copies of groups and instances are
  new occurrences of the same definition. `scale` takes `factor` (uniform) or `factors`
  (`[sx, sy, sz]`, CLI `--factors '2;1.5;1'`) and a pivot (default: the targets' bounding-box
  minimum corner, or its center with `about: center`). Locked groups/instances refuse
  move/rotate/scale/erase/delete (`E1201`).
- 2026-10-20 — **Groups and components.** `make-group`/`make-component` build the definition
  from the indexed mesh form of the selected faces and edges (faces bring their boundary edges),
  so orientation, materials, edge flags, tags and hidden state carry over exactly; the definition
  origin is the selection's bounding-box minimum (in the edit context) and the occurrence's
  transform is the translation to it. Selected faces, and selected edges no remaining face
  needs, leave the source. Nested groups, instances, guides, images, texts, dimensions and section
  planes move into the definition (they get new pids). Commands may not use randomness, so the
  definition UUID is derived from the document's pid counter. `make-component` takes `behavior`
  as a JSON object of `ComponentBehavior` fields and `replace_selection` (default true; false only
  creates the definition). `explode` redraws the definition's geometry in the parent with
  `add_edge` (merging with what is there), applies an instance's `material_override` to faces with
  the default material, re-creates nested occurrences and guides, and removes the occurrence (a
  group's private definition goes with it).
- 2026-10-20 — **Tags and materials (0.1).** Tags, folders and materials are named by their unique
  names in params; colors are `#RRGGBB[AA]` strings (default tag color mid gray). Renaming or
  hiding `Untagged`, deleting it or a default material, and deleting a non-empty tag folder are
  `E1201`. `material-apply` stores the two default materials as "no material" so faces keep
  following the document defaults; `fill` is `single | connected | same-material | all`
  (`connected` spreads across shared edges to faces with the same material on that side).
  Instances get `material_override`; groups have none, so painting a group paints the
  default-material faces of its private definition. Material color alpha mirrors `opacity`.
  Textures (`material-edit` texture fields, `texture-position`) arrive with milestone 0.4.
- 2026-10-20 — `offset` output is `{edges, face, faces_split}` (`face` = the face bounded by the
  offset outer loop: the inner face for an inward offset, the surrounding ring for an outward one).
  `edges` must be the complete outer boundary of one face in 0.1; open chains come later.
- 2026-10-22 — **Copies, groups and explode carry geometry exactly** (refines "Arrays" and
  "Groups and components" above). Copying raw geometry, building a definition and exploding an
  occurrence all move the selected faces and edges in indexed form: faces keep every loop (holes
  stay open), edges keep flags and the curves whose members all come along, faces keep sides and
  orientation, and each element keeps its tag, hidden state and attributes (groups, instances and
  other non-mesh entities too; a copy is never locked). When the moved geometry touches nothing at
  its destination it is inserted as is ("interior geometry is inserted directly", §5.3); when it
  touches existing geometry it is redrawn with `add_edge` so it merges with what is there, and only
  faces this redraw created take the copied sides or flip; existing faces are never re-oriented or
  repainted. A definition is complete when it is inserted (undo/redo restore that snapshot).
- 2026-10-22 — **Copy-on-edit of groups** (docs/04 §3.1). Copies of a group share its definition
  until one of them is edited. Before a command edits inside a group (its context path passes
  through it) or paints it, every *other* group that shares the definition moves to a copy of the
  definition with fresh pids; the edited group keeps its definition, ids and pids, so the edit
  context and the selection stay valid. Component instances are never forked (`make-unique`, 0.4).
- 2026-10-22 — **Edit context of a command.** The per-command `context` param is the domain of the
  command's queries (`targets`, `face`, `edges`, `query`) as well as the container it edits.
  Opening a locked group or instance as the context (`Engine::set_context`, CLI `--context`) or
  editing inside one is `E1201` (§5.13).
- 2026-10-22 — **The face of a drawn shape is the new face.** When a closed shape drawn on a face is
  the larger piece of the split, the pids of the shape's face and of the leftover piece are
  exchanged, so the shape's face is the new face (`face` in the output, in `last`) and the leftover
  keeps the original face's identity and attributes. `push-pull --face '{face & last}'` after a
  rectangle therefore always pulls the rectangle.
- 2026-10-22 — Removing a group also removes the private definitions of the groups nested in it
  that nothing else uses (recursively). Validation of lengths and angles uses the tolerances
  (`distance` within `length` of zero is `E1001`, as is a `from_point`/`to_point` within `length`
  of the rotation axis), so `--dry-run` agrees with the real run.
- 2026-10-25 — **View commands and scenes (0.2).** Camera view commands (`camera-set`,
  `camera-standard`, `position-camera`, `orbit`, `pan`, `zoom`, `zoom-extents`) hand the new camera
  to the engine, which stores it as the document camera and emits `CameraChanged`; they are not
  undo steps and do not start a new document generation (the camera is still saved with the
  file). `camera-set` keeps the target's apparent size when switching between perspective and
  parallel. `position-camera` takes `eye` and `target`; its `look_dir` form is not implemented yet.
  In 0.2 `SceneCaptureFlags` is `{ camera, tags, style }` (all on by default): scenes capture and
  `scene-activate` restores the camera, the visibility of every tag and the active style; hidden
  entities, section planes, shadows and axes arrive with those features. Activation is a view
  command, so restoring tag visibility is not an undo step either.
- 2026-10-26 — **Typed points.** Three comma-separated values in the value field are an absolute
  point, like `[x,y,z]`. In the GUI a typed point (absolute or relative) acts as a click at that
  point for every tool step that picks a point — the first corner of a rectangle, a circle's
  center, a line's next point, Move's grab point and destination, Rotate's center and reference
  points — so a drawing can start at exact coordinates. Select, Scale, Eraser and Paint do not
  pick points; Scale reads two or three values as factors (`2,1.5` or `2,1.5,1`).
- 2026-10-26 — **Textures and material libraries (0.4).** `material-create` and `material-edit`
  take `texture` (an image file path read through the engine's storage; `none` removes it in
  `material-edit`), `texture_width` and `texture_height` instead of the `texture_size` pair (one
  side alone keeps the tile's proportions; a new texture without a size is 1 m wide, its height
  following the image; a new image on a textured material keeps the tile width) and, in
  `material-edit`, `colorize: none|shift|tint`. `material-sample { face, side }` names the
  material on a face side (the default material when the side has none) and changes nothing.
  `material-library-save { path, materials }` writes the named materials (all but the defaults
  when empty) with their images; it is a view command (the document is unchanged).
  `material-library-load { path, replace }` adds a library's materials, skipping names the
  document already has unless `replace`; libraries never carry the two default materials.
- 2026-10-26 — **Components (0.4).** `make-unique { targets }` moves the selected instances of
  each definition to one new copy of it (named `Name #2`, `#3`…); instances that are the only
  users of their definition stay. `component-insert { definition | path, position, on_face,
  rotation }` places an instance of a model component or of a component file so that its
  insertion point lands on `position`; with `on_face`, a gluing component (its `glue_to`
  accepts the face's slope: horizontal, vertical, sloped, any) lies on the face — blue axis along
  the face normal, red axis horizontal (world red on horizontal faces), then `rotation` degrees
  about blue — and records `glued_to`. `component-save-as { definition, path }` writes a
  `.plyrc` (view command). `component-reload { definition, path }` replaces the component's
  contents and behavior with the file's, keeping its name, UUID and instances; definitions the
  file nests are matched by UUID, so nested components already in the model are not updated.
  `component-replace { instances, with }` takes a model component (name or UUID) or a component
  file. Moving glued instances with their face and cutting openings come later in 0.4.
- 2026-10-26 — **Scenes (0.4).** `SceneCaptureFlags` is `{ camera, tags, style, hidden,
  sections, shadows }`, all on by default: scenes also capture the hidden entities (by pid, every
  container), the active state of section planes and the shadow settings, and `scene-activate`
  restores them (still a view command: not an undo step, though hidden and section states are
  document state and mark it changed). `scene-add` and `scene-update` take `description`,
  `transition_s` (0–600), `include_in_animation` and `axes_visible` (the frontend's axes
  setting, which it applies itself on activation); `scene-update` with `properties_only` changes
  those without re-capturing the view.
- 2026-10-26 — **Styles (0.4).** The Styles panel's editor is a set of commands: `style-create
  { name, from?, activate (true) }` copies a style; `style-delete { style }` (not the active one;
  scenes that activated it stop changing the style); `style-edit { style?, name?, … }` sets any
  edge, face, background and modeling field (an undo step, unlike `style-set`, which stays the
  quick view switch of the active style); `style-watermark-add { style?, image, placement:
  stretched|tiled|centered|anchored, anchor_x, anchor_y, opacity, scale, behind_model }` and
  `style-watermark-remove { style?, index }`; `style-library-save { path, styles }` /
  `style-library-load { path, replace }` for `.plyrs` (the native container holding only styles
  and their watermark images). `style-activate { style }` and `fog-set { enabled?, start?, end?,
  color?: #RRGGBB|background }` are view commands (docs/05 §9): fog distances and color belong
  to the active style, fog on/off to the document's rendering options. An image that nothing
  uses any more after a texture, watermark, material or style is removed or replaced leaves the
  document in the same undo step.
- 2026-10-26 — **Tag folders (0.4).** `tag-move { tag, folder? }` puts a tag into a folder or
  back at the top level (not `Untagged`); `tag-folder-set-visible { name, visible }` hides a
  folder, which hides the entities of every tag inside it and its sub-folders. The Tags panel
  shows the folders as collapsible groups with their visibility, a folder menu on each tag and
  "Add folder"; an empty folder can be deleted.
- 2026-10-26 — **Images and texture positioning (0.4).** `image-insert { path, position?,
  width?, height?, normal (+Z) }` adds an image entity: a material named after the file (made
  unique) carries the image at the rectangle's size, red axis horizontal (world red on the
  ground), lower left corner at `position`; one side given keeps the image's proportions, none
  makes it 1 m wide. `explode` on an image entity draws its rectangle as a face painted with
  that material, mapped so the image fills it exactly. `texture-position { face, side (front,
  back, both), offset_u, offset_v, rotation (degrees), scale_u, scale_v, reset }` places a
  texture with an offset in the face frame, a rotation and a stretch (an affine map); the pin
  modes, `project_from` and `copy_to` of §5.9 come later. The GUI offers "Position Texture…" in
  the context menu of one selected face (a window applying the command) and File › Insert
  Image…. Image entities are drawn two-sided; they are not picked by clicking yet (select them
  with the `image` query).
- 2026-10-26 — **0.6 command details.** `dimension { kind: aligned|linear|radius|diameter,
  start?, end?, curve?, offset, direction?, text_override?, text_height? }`: points snap to the
  vertex or edge midpoint within 0.01 mm (else they stay fixed points); a linear dimension
  measures along `direction` or the model axis the points spread most along across the offset;
  `curve` may name a whole circle (its first edge is used). `text { text?, at?, screen_offset? }`
  defaults to coordinates (vertex), length (edge midpoint) or area (face). `tape-measure` leaves
  a guide point from a vertex, a guide line parallel to the edge it starts on, nothing from empty
  space (unless `guide` says). `resize-model` scales the edit context about its origin. Section
  planes add `section-display-set { planes?, cuts?, fills? }` (view) for docs/05 §9's
  show-section toggles; `section-plane-align-view` looks at the kept side in a parallel view.
  `shadows-set` sets docs/04 §7's shadow settings (`date` YYYY-MM-DD, `time` HH:MM, `latitude`,
  `longitude`, `utc_offset`, `north_angle`, or `azimuth`/`altitude`). `axes-set { origin, x, y }`
  and `axes-reset` move the drawing axes; inference uses the model's (origin snap, on-axis
  directions, axis locks). `text-3d` uses the bundled Inter (SIL OFL) or a font file read through
  the storage; `height` is the capital height; `placement` puts the baseline on local +X.
