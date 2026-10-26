# 07 — Interaction and user interface

**Audience:** implementing agents. Normative for `planyrith-gui`. The GUI never implements
document semantics; it collects parameters and dispatches commands (`docs/01 §3.3`).

## 1. Shell layout (egui + eframe + egui_tiles)

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ Menu bar: File Edit View Camera Draw Tools Window Extensions Help           │
├─────────────────────────────────────────────────────────────────────────────┤
│ Toolbars (dockable rows): Large tool set | Views | Styles | Sections | ...  │
├──────────┬──────────────────────────────────────────────────────┬───────────┤
│ Left     │                                                      │ Right     │
│ dock     │                 3D Viewport (wgpu)                   │ dock      │
│ (tiles)  │   ┌──────────────────────────────────────────────┐   │ (tiles):  │
│ Tool     │   │ scene tabs: [Scene 1][Scene 2][+]            │   │ Entity    │
│ options  │   └──────────────────────────────────────────────┘   │ Info      │
│          │                                                      │ Materials │
│          │                                                      │ Components│
│          │                                                      │ Styles    │
│          │                                                      │ Tags      │
│          │                                                      │ Scenes    │
│          │                                                      │ Shadows   │
│          │                                                      │ Outliner  │
│          │                                                      │ Fog       │
│          │                                                      │ Tool Help│
├──────────┴──────────────────────────────────────────────────────┴───────────┤
│ Status bar: [context breadcrumb] [hint for current step] [value field label][value field ▭] │
└─────────────────────────────────────────────────────────────────────────────┘
```

- Docks are `egui_tiles` trees; panels are tabs that can be split, stacked, dragged, floated
  (as separate egui windows) and hidden. Layouts are saved per user (not in the document) with
  a "Default", "Modeling", "Documentation" preset set.
- Multiple viewports (split view) are supported from milestone 0.6: each viewport is a tile with
  its own camera and style, sharing the document.
- Window title: `<file name>[*] — Planyrith`.

## 2. Input model

- Mouse: left = tool action; middle-drag = orbit, `Shift`+middle = pan, wheel = zoom to cursor,
  right = context menu (on release without drag). Trackpad: two-finger = orbit, `Shift` = pan,
  pinch = zoom. Pen/tablet: hover supported.
- Keyboard: single-letter tool shortcuts (`docs/05 §12`), modifiers (`Shift`/`Ctrl`/`Alt`,
  `Cmd` on macOS maps to `Ctrl`), arrow keys for axis locks, `Esc`, `Enter`, `Backspace`
  (remove last point in chains), `Delete`, `Tab` (cycle inference candidates when several are
  within tolerance), `Space` (select tool), number/unit keys go to the value field without focusing it.
- The **value field** (value control box) is always live: typing while a tool is in a step that accepts a
  value fills the value field; `Enter` applies; the value field stays editable *after* an action completes until
  the next pointer move with button press, so a user can type `2000` Enter after a Push/Pull to
  correct the distance (re-executes the last command via undo+redo with new params).
- Focus rules: the viewport keeps keyboard focus unless a text field is being edited; `Esc` from
  a text field returns focus to the viewport.
- Touch (web/tablets): one finger = tool action, two fingers = orbit/zoom, three = pan; long
  press = right-click. Not optimized for 1.0 but must work.

## 3. Tool lifecycle

```rust
pub trait InteractiveTool {
    fn activate(&mut self, cx: &ToolCx);                 // reset state, set status hint
    fn pointer(&mut self, ev: PointerEvent, cx: &mut ToolCx) -> ToolFlow;  // Continue | Done(ParamsJson) | Cancel
    fn key(&mut self, ev: KeyEvent, cx: &mut ToolCx) -> ToolFlow;
    fn value_field(&mut self, text: &str, cx: &mut ToolCx) -> ToolFlow;             // Enter in value field
    fn draw_overlay(&self, painter: &OverlayPainter, cx: &ToolCx);          // rubber-bands, marks
    fn preview(&self) -> Option<PreviewRequest>;                            // command + params for scratch apply
    fn deactivate(&mut self, cx: &mut ToolCx);
}
```
- `ToolCx` gives read access to the document, the inference engine, the camera, the selection,
  the status-bar hint setter and the value field. Tools cannot mutate the document except through
  `cx.execute(command, params)`.
- Sticky vs one-shot: drawing/editing tools stay active after completing (sticky); `Space`
  returns to Select. Organize commands (make group…) are one-shot.
- Preview: each frame the GUI applies `preview()` on a scratch fork and renders the fork's
  changed containers with a preview style (semi-transparent) — the real document is untouched.
  Preview is throttled to 60 Hz and skipped if the previous preview took > 16 ms (then only the
  rubber-band overlay is shown).

## 4. Status bar and hints

- Left: context breadcrumb (`Model › Kitchen › Cabinet (3 instances)`), clickable to exit levels.
- Middle: hint from `CommandMeta.steps[current].prompt`, e.g. "Select end point or enter length".
- Right: value field label from `steps[current].value_field` ("Length", "Distance", "Angle", "Dimensions",
  "Radius", "Sides") and the value field field.
- Tool Help panel (optional tab) shows an animated description of the active tool from the
  same metadata plus `modifiers`.

## 5. Inference UX

- Marks and labels per `docs/06 §10`. Labels appear after 300 ms hover and fade on motion.
- `Shift` locks the current inference (line/plane becomes bold); arrow keys lock axes
  (`docs/05 §2`); a lock glyph is drawn on the cursor; locked state persists until the step ends
  or the key is pressed again.
- Hover memory: hovering an endpoint/edge/face for 400 ms registers a reference (small pulsing
  ring); up to 3 references; "From Point" dotted lines appear when aligned with a reference.
- `Tab` cycles alternative candidates when more than one inference lies within the pixel
  tolerance (e.g. endpoint vs. intersection).
- Pixel tolerance is 8 px (HiDPI-aware); configurable in Preferences.

## 6. Panels (right dock)

| Panel | Content and behavior |
|---|---|
| **Entity Info** | For the selection: kind and count, tag combo, name, instance count ("3 in model"), lengths/areas/volume (solid), material swatches (front/back), hidden/locked toggles, shadow casting/receiving, dimension text, arc segments/radius editable. Edits dispatch commands. |
| **Materials** | Library tree (folders: in-model, bundled, user) with thumbnails; search; "In Model" shows usage counts; edit panel: color picker (RGB/HSL/hex), opacity slider, texture file/size (locked aspect), colorize, reset; sampler button; drag-drop onto faces/instances. |
| **Components** | Library browser (in-model, local folders, user library) with thumbnails; drag into viewport = `insert-component` with glue behavior; right-click: select instances, reload, save as, statistics. |
| **Styles** | Presets (bundled + user), edit tabs (Edges, Faces, Background, Watermark, Modeling), mix. |
| **Tags** | Tree with folders; visibility eye, color swatch, line style, current-tag radio (with warning about tagging raw geometry); context menu: add/delete/rename/purge unused/select entities. |
| **Scenes** | Thumbnails list; add/update/delete/reorder; properties checklist (camera, hidden, tags, sections, style, shadows); transition time; "include in animation"; play. |
| **Shadows** | Toggle, date/time sliders, time zone/UTC offset, light/dark, on faces/ground/from edges, geo-location fields (lat/lon/north). |
| **Fog** | Toggle, near/far distance sliders, color/use background. |
| **Outliner** | Hierarchy tree of groups/instances (named), search, drag to re-parent (`reparent` command), eye/lock toggles, double-click to enter context. |
| **Tool Help** | Tool help from metadata. |
| **Soften Edges** | Angle slider, smooth normals, soften coplanar (operates on selection). |

Left dock hosts **Tool Options** (dynamic fields for the active tool's params that are not
collected by clicks: e.g. segments for Circle, array count, copy toggle).

## 7. Menus (summary; items are commands)

- **File:** New, Open…, Open Recent, Save, Save As…, Save a Copy As…, Revert, Import…, Export ►
  (3D Model…, 2D Graphic…, Image…, Animation…, Section Slice…), Send to Sheets, Document
  Info…, Print…, Exit.
- **Edit:** Undo, Redo, Cut, Copy, Paste, Paste in Place, Delete, Delete Guides, Select All,
  Select None, Invert Selection, Hide, Unhide ►, Lock, Unlock, Make Component…, Make Group,
  Close Group/Component, Intersect Faces ►, Entity-specific submenu (Divide, Reverse Faces,
  Explode, Soften/Smooth, Texture ►).
- **View:** Toolbars ►, Scene Tabs, Hidden Geometry, Hidden Objects, Section Planes, Section
  Cuts, Section Fill, Axes, Guides, Shadows, Fog, Edge Style ►, Face Style ►, Component Edit ►
  (Hide rest of model, Hide similar components), Animation ►.
- **Camera:** Previous, Next, Standard Views ►, Parallel Projection, Perspective, Two-Point
  Perspective, Field of View, Zoom, Zoom Window, Zoom Extents, Position Camera, Walk, Look Around.
- **Draw:** Lines ► (Line, Freehand), Arcs ► (Arc, 2 Point Arc, 3 Point Arc, Pie), Shapes ►
  (Rectangle, Rotated Rectangle, Circle, Polygon), 3D Text.
- **Tools:** Select, Eraser, Paint, Move, Rotate, Scale, Push/Pull, Sweep, Offset,
  Outer Shell, Solid Tools ►, Tape Measure, Protractor, Axes, Dimensions, Text, Section Plane.
- **Window:** Panels ►, Layout Presets ►, Reset Layout, Model Info, Preferences, Extension Manager,
  Console (scripting REPL).
- **Extensions:** populated by plugin manifests (`docs/10`).
- **Help:** Documentation, Keyboard shortcuts, Check for updates (opt-in), About.

## 8. Dialogs

- **Model Info:** Units (format, precision, length snapping, angle units/precision/snapping),
  Geo-location, Dimensions (font, arrows, extension lines, text alignment), Text, Animation
  (scene transitions/delay), Components (fade similar/rest), Credits,
  File (size, version), Rendering (anti-aliasing), Statistics (entity counts, purge unused),
  Tolerances (advanced).
- **Preferences** (per user): Accessibility (inference colors, high-contrast marks), Applications
  (image editor), Compatibility (wheel direction), Drawing (click style: click-move-click vs
  click-drag; continue line drawing; display crosshairs), Files (default folders), General (auto-
  save interval, backup, check for problems/auto-fix, scene warnings), Graphics (MSAA, max
  textures, software/hardware), Shortcuts (remap), Template, Workspace (icon size, docks).
- **Make Component**, **Save As**, **Export options** per format, **Import options**,
  **Texture position** (contextual, in viewport), **Problem checker** results.

## 9. Undo/redo UX

- Edit menu shows the undo label from `CommandMeta.label` ("Undo Push/Pull").
- Chained drawing steps undo per segment while the tool is active; completed tools undo as one.
- Camera changes are not undo steps (Camera › Previous/Next instead).
- Undo stack limit: 500 steps or 512 MB of deltas (older steps dropped with a status message).

## 10. Edit context (groups/components)

- Double-click enters; `Esc` with nothing in progress (or clicking outside) exits one level;
  breadcrumb clicks exit to a level.
- Inside a component, the title shows the instance count; edits apply to all instances (live
  update in the viewport). "Make Unique" available in the context menu.
- Rest of model fades (style `inactive_fade`); "Hide rest of model" / "Hide similar components"
  toggles in View › Component Edit.
- Paste into a context pastes into that container; "Paste in Place" keeps world coordinates.

## 11. Clipboard

- Internal clipboard holds a serialized sub-document (`.plyr` bytes); cross-process copy/paste
  between Planyrith instances via the OS clipboard with a custom MIME type
  `application/x-planyrith` and a text fallback (JSON) for scripts.

## 12. Accessibility and i18n

- All colors in Preferences › Accessibility (inference marks, selection, axes) with high-
  contrast presets; marks have distinct shapes, not only colors.
- Full keyboard operation of panels (egui provides focus traversal); tooltips on every icon.
- Strings are externalized (Fluent `.ftl` files via the `fluent` crate, Apache-2.0/MIT);
  English is the source language; number/unit formatting respects document units (not locale) and
  decimal separator from user preference.
- Font scaling with OS HiDPI and a UI scale preference (75–200 %).

## 13. Web-specific UI

- File menu uses the browser: Open = file picker (File System Access API when available, else
  `<input type=file>`), Save = download or "Save to browser" (IndexedDB, `docs/11`), Recent =
  IndexedDB list with sizes and "Export all" (zip download).
- Keyboard shortcuts that the browser reserves (`Ctrl+N`, `Ctrl+W`, `Ctrl+T`) are remapped (`Alt+N`…),
  documented in Help.
- A persistent "unsaved changes" guard via `beforeunload`.

## 14. Performance rules for the GUI layer

- Never allocate per frame in hot paths (egui painter shapes are pooled).
- Document queries in panels are cached by `DocumentChanged` generation; panels redraw only on
  change (egui `request_repaint` on events, idle otherwise — important for laptops).
- The viewport requests continuous repaint only during camera motion, tool drag or scene
  animation.

## Decisions

- 2026-10-20 — egui_tiles docks; layouts stored per user, not in documents.
- 2026-10-20 — value field remains editable after an action to re-execute the last command with a new value.
- 2026-10-20 — Click-move-click is the default drawing style (drag style is a preference).
- 2026-10-20 — Fluent for localization; English source.
- 2026-10-25 — A bare signed or decimal number in the value field (`-45`, `22.5`) is a length in
  the document unit where the step wants a length and an angle in degrees where it wants an
  angle; bare unsigned integers stay counts where a count is expected.
- 2026-10-25 — **The 0.2 shell.** Docks: Tool Options on the left, the viewport in the middle,
  Entity Info and Tags, Outliner and Scenes on the right (tabs); the layout and the recent files
  are saved per user by the windowing framework's storage, never in documents. Keys: Ctrl+N/O/S,
  Ctrl+Shift+S (save as), Ctrl+Z, Ctrl+Shift+Z or Ctrl+Y, Ctrl+A (select all), Shift+Z (zoom
  extents), Delete (delete the selection), Esc (clear the selection, else leave the open group);
  they do not fire while a text field has focus. Backspace is left to the tools (§2).
- 2026-10-25 — **Viewport input (0.2).** Middle-drag orbits about the point under the cursor at
  the start of the drag (the CPU ray cast), else the model's center; Shift+middle-drag pans so
  that point follows the cursor; the wheel zooms toward the point under the cursor (factor
  e^(0.0025 · points)). During a drag the viewport sets the camera through the engine's view
  state each frame, as the `orbit`/`pan`/`zoom` commands do, instead of dispatching a command per
  mouse event. A click selects what `EntityPath::selectable_in` names (docs/06 Decisions): plain
  click replaces (a miss clears), Ctrl adds, Shift toggles, Ctrl+Shift removes; a double-click on
  a group or component opens it. While the pointer rests, the id buffer names the element under
  it in the status bar; clicks always use the CPU ray cast.
- 2026-10-25 — **Panels (0.2).** Entity Info shows the selection's kinds, measurements, face
  materials, and a tag combo and hidden/locked toggles that dispatch `tag-assign`, `hide`/`unhide`,
  `lock`/`unlock`. Tags: visibility, color, add, rename (double-click), delete, assign the
  selection. Scenes: click activates, add/update/rename (double-click)/reorder/delete. Outliner:
  groups and components as a tree; click selects, double-click opens, eye and lock toggles. View ›
  Face Style, Edges, Back Edges and Profiles dispatch `style-set`; View › Hidden Geometry is a
  viewport display option, not document state. New, Open and Quit ask to save unsaved changes.
- 2026-10-25 — The GUI journals the commands it runs like the CLI (with timestamps) and writes
  `planyrith-gui <version>` as the generator. The viewport shows the renderer's color texture
  through an `Rgba8Unorm` view, because egui composites in gamma space.
- 2026-10-25 — **Tools (0.3).** A tool is a state machine (`planyrith-gui::tools::Tool`) that
  receives the pointer already resolved by the inference engine, and answers each input with
  *continue*, *run these commands*, *select* or *open this group*; the shell runs the commands
  (each one undo step), so tools never touch the document. The §3 sketch maps onto it as:
  `activate` = a fresh tool instance, `pointer` = `hover`/`click`/`double_click`/
  `triple_click`/`drag`, `key` = `arrow`/`escape`, `value_field` = `value`, `draw_overlay` =
  `shapes`, plus `options` for Tool Options and `revalue` for the value-after-completion rule.
  Every tool's prompts and value field label come from its command's `CommandMeta.steps`; Tool
  Help (a right-dock tab) lists them with the current step highlighted. Undo or redo while a
  tool is mid-step restarts the tool, because its picked points may refer to what was undone.
- 2026-10-25 — **Value field keys.** With no text field focused, a digit or one of
  `- + . , [ < ' " * / ( ~` starts a value; once the value field is non-empty every typed
  character goes to it (units, `s` segments, `r` radius, `x` copies), so single-letter tool
  shortcuts act only while it is empty. `Backspace` edits it, `Enter` applies it, `Esc` clears it
  first, then cancels the tool's step, then clears the selection, then leaves the open group.
  `Backspace` with an empty value field removes the last point of a line chain and undoes its
  segment. `Enter` with an empty value field confirms the point under the cursor (docs/05 §1).
  A typed distance or angle takes the direction of the pointer's motion (or of the command it
  replaces); a negative value reverses it. The Select tool takes no value. A value replaces the last command (undo, then the same command with
  the new value, still one undo step) only when the tool is back at its first step and no
  pointer button was pressed since; in the middle of a line chain a typed length extends the
  chain instead.
- 2026-10-25 — **Previews (0.3).** The shell applies the tool's preview command to a fork of the
  engine (a copy of the document with the same selection, edit context and `last`) and draws
  the copy instead of the document; only the containers the command changed are
  re-tessellated. The preview is recomputed when the request or the document changes; when one
  took more than 16 ms, the next is computed only after the request has been stable for 150 ms,
  and meanwhile only the rubber bands show. A preview that fails is not drawn and its error is
  shown in the status bar. In 0.3 the preview is drawn with the active style, not a
  semi-transparent preview style.
- 2026-10-25 — **Inference display (0.3).** Marks, directional guide lines (dashed, colored by
  axis, bold when locked) and the label are painted over the viewport image with the UI
  painter, not by the GPU renderer; the label shows next to the cursor once the same inference
  has held for 300 ms and disappears when it changes. `Tab` steps through the candidates within
  tolerance (one per distinct point, best first) until the pointer moves; it is taken from the
  input before the UI's focus traversal sees it, unless a text field has focus. Push/Pull,
  Offset and Paint act on the face under the cursor even when an edge is within the pick
  tolerance. They are not shown for Select and Eraser. `Shift` locks the current inference only once the tool is past its first step,
  so `Shift` stays a selection or fill modifier on first clicks; arrow keys go to the tool first
  (Rectangle, Circle, Polygon, the arcs and Rotate use them to force their plane before the
  first click) and otherwise lock an axis. A single row of tool buttons sits under the menu bar
  until icons exist; Draw and Tools menus list the same tools.
- 2026-10-25 — **Look (after 0.3).** A light and a dark theme with one accent color (View ›
  Theme: System, Light, Dark; per-user setting). An icon toolbar under the menu bar: file,
  undo/redo, Select/Eraser/Paint, the drawing tools, the editing tools, standard views and zoom
  extents, face styles; icons are drawn as vector strokes in Planyrith's own style, with
  tooltips naming the shortcut. Over the viewport a drawing tool shows a crosshair cursor with
  its icon beside it; navigation shows a grab cursor. Right-click selects the entity under the
  cursor unless it is already selected and opens a context menu of commands on the selection
  and the view. Scene tabs sit above the viewport once the model has scenes (`+` adds one from
  the current view). The status bar shows the active tool's icon and prompt, and the value
  field at its right edge with its label; messages name files without their folders. Tool
  Options show the tool, its prompt and its settings (segments and sides, Copy for Move and
  Rotate, Keep the start face for Push/Pull).
- 2026-10-26 — Hover memory remembers a point (endpoint, midpoint, center, intersection) after
  150 ms, so passing over it is enough for its alignments and perpendicular projections; edges
  and faces still need 400 ms. A perpendicular projection is drawn as a dotted magenta line from
  the remembered point (marked with a ring) to the snapped point.
- 2026-10-26 — **Scene transitions and animation.** Activating a scene from the GUI (scene tabs,
  Scenes panel, animation) moves the document camera at once and shows cameras in between for
  the scene's `transition_s` seconds (eye and target move straight, eased in and out; the field
  of view blends; the projection kind switches halfway); navigating stops the transition. The
  Scenes panel edits the selected scene's transition, "Include in animation" and description,
  and "Play animation" activates the included scenes in turn — each transition, then a
  one-second pause — looping until Stop or `Esc`.
- 2026-10-26 — **Materials, Components and Styles panels (0.4).** Tabs of the right dock next to
  Entity Info. Materials: the document's materials with swatches (a checker marks a texture),
  create, and for the selected one color (hex), opacity, texture load/replace/remove, tile size,
  colorize, "Paint with it" (double-click too: activates Paint with that material), delete,
  purge unused, and `.plyrm` load/save. Components: the model's components with their instance
  count; "Place" (or double-click) activates a placement tool — each click inserts an instance at
  the inferred point, and a gluing component over a face it accepts lies on it; save as, reload
  from a file, insert from a file (at the origin, selected), select instances, make unique.
  Styles: the list (click activates), create (a copy of the active one), `.plyrs` load/save, and
  the active style's editor — edges, faces, background with sky and ground, watermarks, fog,
  modeling colors — where colors are hex codes and numbers apply when a drag or an edit ends, so
  each change is one command. Files come from the file dialog with the matching filter.
- 2026-10-26 — **Bundled materials and Paint sides.** The Materials panel offers bundled
  textured materials (oak, walnut, brick, ceramic tile, concrete, marble, grass, stone paving,
  brushed steel, fabric) with thumbnails; their textures are generated procedurally by the
  application (no third-party images), written once to a cache folder and loaded with
  `material-create` like any file; clicking one creates it if needed and paints with it.
  Textured materials show their texture as the swatch. Paint paints (and Alt samples) the side
  of the face that faces the eye, not always the front.
- 2026-10-26 — File › Import… opens every importable file (glTF/GLB, OBJ, STL, `.plyr`/`.plyrc`,
  images, CSV) and runs `import` in the active context; File › Export ► 3D Model… runs `export`
  with the format from the chosen extension (GLB when none), and Export ► Component Report…
  writes the CSV report (not in the §7 list, which predates the report: it belongs with the other
  exports). Further Export entries arrive with their exporters.
- 2026-10-26 — File › Export ► 2D Graphic… writes the current view as PDF, SVG or DXF (`export`
  with `two_d`), hidden lines removed (docs/09 Decisions).
- 2026-10-26 — **0.6 tools and views.** Tape Measure (T), Protractor, Dimension, Text, 3D Text,
  Section Plane and Axes are toolbar tools (a group after the editing tools) and in Tools. The
  Dimension tool takes two points then the line's place; an arc or circle edge gives a radius
  dimension (Ctrl: diameter). Text takes the point it points to and the label's place (empty
  space: screen text); its text and 3D Text's text, height and extrusion are in Tool Options.
  Typing a length right after a Tape Measure measurement resizes the model (one undoable
  command; the confirmation of docs/05 §6.4 is the undo). View has Shadows, Shadow Settings…
  (a window with date, time, place, north, light and dark), Section Planes / Cuts / Fill, and
  Viewports ► One / Two / Four: extra viewports show the top, front and right in parallel
  projection with cameras of their own (not stored in the document), fitted to the model when
  they open; tools work in any of them; the value field follows the main one. Labels of
  dimensions, texts and section planes are drawn over the model, not hidden by faces.
- 2026-10-26 — **Web UI and recovery (0.7).** File gains Open Example ► (Room, Bookcase: example
  documents built by commands, `planyrith-gui::samples`); in the browser also Upload from
  Computer…, Download This Document and Download a File… (docs/11 Decisions), and no Quit. Files
  dropped on the window open (`.plyr`) or import (every importable format). After a session ended
  without saving, a Recover unsaved work prompt offers Recover or Discard (docs/11 §2). The
  browser asks before closing a page with unsaved changes (`beforeunload`). Touch: two fingers
  pinch to zoom toward their center, move to pan and twist to orbit; one finger works the tools.
  Not in 0.7: remapping the shortcuts browsers reserve (`Ctrl+N`, `Ctrl+W`, `Ctrl+T`), and the
  overwrite confirmation of the browser's Save As (the dialog checks the computer's disk).
