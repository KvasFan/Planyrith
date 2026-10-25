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
counts `3`. Negative values reverse direction. Pressing Enter without a value confirms the
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
- **Output:** `{ edges, curve: Pid }`.

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
- **Params:** `targets: Sel`, `scope: WithModel|WithSelection|WithContext`.
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
  `--no-transition`).

### 5.8 `material-create`, `material-edit`, `material-delete`, `material-apply`, `material-sample`, `material-purge`
- `material-apply { targets: Sel, material: String, side: Front|Back|Both(Front), fill: Single|Connected|SameMaterial|Object|All }`
  — `Paint`: click = single face; `Ctrl` = adjacent coplanar/connected faces with same
  material; `Shift` = all faces with the same material in context; `Ctrl+Shift` = same material
  on connected object; `Alt` = sample material from face (`material-sample`). Applying to a
  group/instance sets `material_override` (affects default-material faces only).
- `material-edit { material, color?, opacity?, texture?: path|ImageRef, texture_size?: (w,h), colorize?: mode }`.

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

- `orbit { delta: (f32,f32) | to: Camera }` — middle-drag; `Shift` = pan; orbits about the
  point under the cursor at drag start (if geometry) else about the model center.
- `pan`, `zoom { factor | window: Rect | to: Sel }`, `zoom-extents`, `zoom-selection`,
  `zoom-previous`/`zoom-next`.
- `look-around { yaw, pitch }`, `walk { forward, strafe, eye_height: Length(1700) }` (with
  collision against faces optional), `position-camera { eye: Point, target: Point | look_dir }`.
- `camera-set { projection: Perspective|Parallel|TwoPoint, fov?: Angle }`, `camera-standard
  { view: Top|Bottom|Front|Back|Left|Right|Iso }`, `camera-align-to-face { face }`.
- `field-of-view` (interactive drag changes FOV; value field degrees or `35mm` focal length).
- `style-set`, `shadows-set`, `fog-set`, `show-hidden-geometry`, `show-guides`, `show-section-planes`,
  `show-section-cuts`, `show-section-fill`, `xray`, `show-axes`.

## 10. File commands (category `file`) — see `docs/08 §6` and `docs/09`

`new`, `open`, `save`, `save-as`, `save-copy`, `import { path, format?, options }`, `export
{ path, format, options }`, `render { path, width, height, scene?, camera?, style?, transparent_bg }`,
`component-import-as-file`, `revert`, `purge-unused`.

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
