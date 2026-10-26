# 06 — Rendering and viewport

**Audience:** implementing agents. Normative. Lives in `planyrith-render` (wgpu, no UI) and in
`planyrith-core::io::render_mesh` (GPU-agnostic extraction). The GUI embeds the renderer through
an `egui-wgpu` paint callback; the CLI uses it headless (`planyrith render`).

## 1. Goals

- 60+ FPS interaction at 1080p with 1 M triangles / 200 k edges on integrated GPUs; graceful
  degradation (LOD of edges, skip shadows while orbiting) beyond.
- Visual language of a modeling tool, not a game: crisp edges, flat or softly shaded faces,
  readable inference marks, exact section cuts.
- Identical output on desktop and WebGPU; WebGL2 fallback may lose MSAA > 4 and compute passes.
- Headless rendering produces deterministic images for tests (within a per-pixel tolerance).

## 2. Render data extraction (`planyrith-core::io::render_mesh`)

Per `Entities` container (cached, invalidated by deltas):

```rust
pub struct RenderMesh {
    pub positions: Vec<[f32;3]>, pub normals: Vec<[f32;3]>, pub uvs: Vec<[f32;2]>,
    pub face_ids: Vec<u32>,                 // per vertex: local FaceId index (for picking/highlight)
    pub indices_by_material: Vec<(MaterialKey, Range<u32>)>,  // front faces; back faces as separate ranges with flipped winding
    pub edges: EdgeBuffers { positions: Vec<[f32;3]>, edge_ids: Vec<u32>, flags: Vec<u8> /*profile|soft|smooth|hidden|curve-seam*/ },
    pub bounds: Aabb,
}
```
- Coordinates are **relative to the container's local origin** in `f32`; the world transform is
  applied in the shader (`DAffine3` → relative-to-camera `f32` matrix each frame, see §3.2).
- Materials are resolved per face into `MaterialKey = (MaterialId or Default, side)`; instance
  material overrides are applied at draw time via per-instance uniforms, not by re-extraction.
- Smooth normals across `SOFT` edges (angle-weighted); hard elsewhere (duplicated vertices).
- Textures: atlas-free; one bind group per material; images uploaded once (content hash).

Instances: the renderer walks the hierarchy each frame into a flat `DrawList` of `(RenderMesh
handle, world transform, material override, highlight state, visibility)`; identical definitions
are drawn with GPU instancing.

## 3. Pipeline

### 3.1 Passes (per frame)

1. **Shadow pass** (optional): depth from the sun (orthographic, fitted to the view frustum ∩
   model bounds; 2 cascades; 2048² each; PCF 3×3). Skipped while the camera moves if frame
   budget is exceeded; re-enabled on idle.
2. **Section pre-pass**: for each active section plane, write the clip plane to a uniform;
   fragments behind the plane are discarded in all passes (`discard` on signed distance).
3. **Opaque faces**: forward shading (§4). Writes depth and an **ID buffer** (`R32Uint`:
   draw-item index + face id) used for hover picking (§8).
4. **Section caps**: stencil-based capping — render back faces of solids with stencil increment,
   front with decrement; where stencil ≠ 0 draw the cap fill color on the clip plane (only for
   containers reported `is_solid`; non-solids show open cuts, as users expect).
5. **Edges**: line rendering (§5) with depth test against the face depth with a small bias
   (polygon offset) so edges sit on faces. Profiles computed per frame on the GPU? **No** — on the
   CPU per draw item (silhouette edges depend on view) with caching per (item, camera quantized).
   Back edges (hidden-line style "back edges") rendered dashed with depth test inverted.
6. **Transparent faces**: sorted back-to-front per draw item (not per triangle); X-ray mode uses a
   global alpha; option: weighted blended OIT when the device supports it (desktop only).
7. **Overlays**: guides (infinite lines clipped to the frustum), section plane widgets, axes,
   inference marks and rubber-bands, selection highlight (face tint + thick edge), dimension and
   text (via egui painter layer in the GUI; via the renderer's text pass headless).
8. **Post**: MSAA resolve (4× default; 8× desktop option), optional FXAA on WebGL2, sRGB.

### 3.2 Precision

- Camera-relative rendering: world transforms are composed on the CPU in `f64` as
  `view * world`, translated so the camera is at the origin, then converted to `f32`. This keeps
  models far from the origin jitter-free.
- Depth: reversed-Z with a `Depth32Float` buffer; near plane auto-fitted to the bounds
  (clamped to ≥ 1 mm), far to bounds radius × 4.

## 4. Shading model

- Fixed headlight + sun (if shadows enabled) + ambient; Lambert diffuse only, no specular
  (readability). Face color = material color × texture; back faces use `back_default` unless a
  back material is set; `Monochrome` style ignores materials; `HiddenLine` draws white faces
  and edges; `Wireframe` skips the face pass but keeps depth from an invisible face pass so
  hidden edges can be dashed; `XRay` sets global alpha 0.5 and disables depth writes for faces.
- Transparency ordering uses material opacity; faces of one container with mixed opacity are
  split into opaque/transparent ranges at extraction.
- Fog: linear by distance in view space using style settings.
- Watermarks: textured quads in screen space (background or overlay) with tiling/stretch modes.

## 5. Edge rendering

- Edges are drawn as screen-space quads (instanced line segments) with configurable width in
  pixels; endpoints extended by `extension` px; `endpoints` draws thicker short caps; `jitter`
  applies a deterministic hash-based wobble per edge (hand-drawn look); `depth_cue` thickens
  edges nearer the camera.
- Profiles (silhouette edges): an edge is a profile if its two faces face opposite ways
  relative to the camera, or it borders one face only. Computed on the CPU per visible item per
  frame (SIMD-friendly loop over `edge_faces: Vec<(u32,u32)>` with face normals), cached while
  the camera is still.
- Soft/smooth/hidden flags are honored per style; "show hidden geometry" draws hidden edges dashed.
- Edge color modes: all one color; by material (of the adjacent front face); by axis (red/green/
  blue when parallel to an axis, else the default color).
- LOD: when the edge count on screen exceeds `max_edges_interactive` (default 2 M) during
  camera motion, drop `SMOOTH` edges first, then edges shorter than 2 px.

## 6. Shadows

- Sun direction from `ShadowSettings` (`docs/04 §7`): geographic model computes solar azimuth/
  altitude from date, time, latitude, longitude, UTC offset and north angle (NOAA-style
  algorithm; accuracy ±0.5° is sufficient); manual mode uses azimuth/altitude directly.
- Shadows on faces (shadow map) and on the ground plane (a virtual plane at the model's lowest Z,
  or at Z = 0 when `ground_at_zero`); "from edges" casts shadows from wire edges (drawn as thin
  quads into the shadow map).
- `light`/`dark` sliders scale lit and shadowed intensities.

## 7. Camera and navigation

- Perspective (vertical FOV 35° default; `fov` tool accepts degrees or 35 mm-equivalent focal
  length), Parallel (scale by view height in mm — exact for 2D export), Two-point (verticals
  remain vertical: roll locked and vertical shear applied).
- Orbit about the picked point under the cursor (depth from ID buffer/CPU cast); when nothing is
  under the cursor orbit about the center of the visible bounds. Up vector stays world Z unless
  the user holds `Ctrl` while orbiting (free roll) — then `camera-reset-up` restores it.
- Zoom toward the cursor point; zoom-extents fits the visible bounds with 5 % margin; parallel
  projection zooms by changing view height.
- Walk/look-around: eye height default 1700 mm, collision with faces optional, speed scales with
  model size.
- Standard views set direction and keep the current projection; `Iso` is (-1,-1,1) direction from
  the +X+Y side.

## 8. Picking

- **Hover (GUI):** read back a 9×9 pixel window from the ID buffer around the cursor (async
  readback, 1–2 frames latency acceptable) → candidate (item, face) → confirm with a CPU ray cast
  against that face/its edges (exact). This gives pixel-perfect edge/vertex preference: a vertex
  within 8 px beats an edge within 8 px beats a face.
- **Click / inference / headless:** CPU ray casting via `parry3d-f64` BVHs (`docs/03 §6`).
  Picking respects: hidden entities, tag visibility, locked state (lockable but inferable), the
  edit context (entities outside the context are pickable for inference, selectable only if
  `select_outside_context` is on — default off; they render faded per style).
- Section planes cut picking too: hits behind an active plane are ignored for faces/edges in the
  cut containers.

## 9. Selection and highlight rendering

- Selected faces: tinted overlay (style `selection_color`, alpha 0.35) + bounding edges thick.
  Selected edges: thick in `selection_color`. Selected groups/instances: bounding box in
  `selection_color` plus contained edges highlighted. Locked: `locked_color`.
- Active edit context: everything outside the context is faded (`inactive_fade`) and the
  context's bounding box is drawn dashed.
- Hover feedback per tool (e.g. Push/Pull highlights the face with a hatched tint).

## 10. Inference marks (drawn by GUI overlay from `Inference` data, `docs/03 §7`)

| Kind | Mark |
|---|---|
| Endpoint | green filled circle 7 px |
| Midpoint | cyan filled circle |
| Intersection | red ×, label "Intersection" |
| Center | blue circle with dot |
| OnEdge | red square (hollow) |
| OnFace | blue diamond |
| OnAxis / FromPoint | dotted line in axis color from reference point; label "On Red Axis" etc. |
| Parallel/Perpendicular/Tangent | magenta line/arc with label |
| Locked | the constraint line/plane drawn bold; cursor gains a lock glyph |

Labels appear after 300 ms hover near the cursor (tooltip style), never obscuring the point.

## 11. Headless rendering (`planyrith render`)

- Uses `wgpu` with a headless adapter (prefers a real GPU; falls back to a software adapter
  if available — e.g. lavapipe on Linux CI); renders to an offscreen texture, reads back,
  encodes PNG/JPEG/WebP via `image`.
- Params: width, height, scene or camera, style, shadows on/off, transparent background, MSAA,
  `--deterministic` (disables time-based effects, fixes jitter seed, forces single cascade).
- Snapshot tests compare against golden images with per-pixel tolerance 2/255 and ≤ 0.1 %
  differing pixels, with `--update-goldens` in CI tooling.

## 12. Performance budget (1080p, integrated GPU, 1 M tris / 200 k edges)

| Pass | Budget |
|---|---|
| Draw-list build (CPU) | 1.0 ms |
| Profile edges (CPU, cached when idle) | 1.5 ms moving / 0 idle |
| Shadow pass | 2.0 ms (skipped when moving if over budget) |
| Opaque + ID | 2.0 ms |
| Edges | 1.5 ms |
| Caps, transparent, overlays, resolve | 1.0 ms |
| **Total** | **≤ 8 ms** |

Profiling: `tracing` spans per pass with `wgpu` timestamp queries where supported; the GUI
exposes a frame-time overlay (`F11` panel).

## 13. Resource management

- Buffers per `RenderMesh` live in a GPU cache keyed by `(container id, generation)`; evicted LRU
  when exceeding a budget (default 1 GB desktop, 256 MB web).
- Textures: mipmapped, BC/ASTC compression not required (pure-Rust encoders are limited); large
  textures are downscaled to ≤ 4096 px on web.
- All GPU work is on the main thread (wgpu is `Send`, but egui integration owns the frame).

## Decisions

- 2026-10-20 — Forward Lambert shading; no PBR in the viewport (export hook for renderers instead).
- 2026-10-20 — Reversed-Z, camera-relative `f64`→`f32` for precision.
- 2026-10-20 — ID-buffer hover + CPU-exact confirmation; CPU ray casting is authoritative.
- 2026-10-20 — Stencil capping only for containers that are solids.
- 2026-10-25 — **Projection matrices** (§3.2, `document::camera`). Perspective and two-point
  views use reversed Z with an **infinite** far plane (depth = near / distance), so nothing is
  ever clipped behind the model; the near plane is 90 % of the nearest bounds corner, at least
  1 mm. Parallel views map the bounds' depth span (plus 1 %) linearly to 1…0 and may start
  behind the eye. Two-point views project on a vertical image plane with the vertical shear that
  keeps the target centered. Every matrix is the exact inverse of `ray_through_ndc`, so picking
  rays and drawing agree; the clip-from-local matrix is composed in `f64` relative to the eye and
  only then narrowed to `f32`.
- 2026-10-25 — **Extraction** (§2, `io::render_mesh`). `extract(doc, container, options)` takes
  the document (tag visibility and materials decide what is hidden and the texture tile size).
  Faces and edges come in container (pid) order. Each face is emitted twice, front side
  (counter-clockwise, normals along the face normal) and back side (flipped winding and
  normals), each in the index range of its own `MaterialKey`, so back-face culling draws the
  right material on each side. Hidden faces and edges (own flag, `HIDDEN` edge flag or hidden
  tag) are left out unless `include_hidden`, which keeps them with `face_hidden`/`HIDDEN` bits.
  `CURVE_SEAM` marks edges that are segments of a curve. The buffers also carry per-face normals
  and the two faces of every edge for profile edges. UVs are in texture tiles: the face's UV map
  takes the point in the face frame (or the projection plane's frame) to texture millimeters,
  divided by the texture's tile size; fixed pins are already in tiles; without a texture UVs
  are 0. The cache drops a container on any delta that names it and everything on document-wide
  deltas (tags, materials, definitions).
- 2026-10-25 — **Draw list in the core.** `io::render_mesh::draw_list(doc, selection, context,
  include_hidden)` walks the occurrences (parents first, children in container order) into items
  with canonical (pid-keyed) paths, world transforms, the innermost instance material override,
  selection (whole occurrences for selected groups/instances and everything inside them;
  selected faces and edges per occurrence) and the faded state outside the edit context. Being
  GPU-free it is unit-tested in the core and shared by the GUI and headless rendering.
- 2026-10-25 — **What a click selects** (§8, docs/07 §10): `EntityPath::selectable_in(doc,
  context)` — the element itself when it lies in the context's container, else the outermost
  group or instance below the context that holds it; nothing outside the context.
- 2026-10-25 — **One render path** (0.2). The renderer always draws into its own offscreen targets
  (4× MSAA color resolved to `Rgba8UnormSrgb`, `Depth32Float` reversed Z); headless rendering
  reads the resolved color back, the GUI shows it as a texture in the viewport, so both draw the
  same pixels. Pass order: opaque faces (depth only in Wireframe), visible edges, dashed back
  edges (style `back_edges`, Wireframe, X-ray; depth test inverted), transparent faces (per draw
  item, back to front by the distance of its bounds' center), then the selection overlay.
- 2026-10-25 — **Lighting** (§4): Lambert from a headlight offset above and to the left of the eye
  (`−0.8·forward + 0.45·up − 0.4·right`, normalized) plus 35 % ambient, so the faces of a box
  seen corner-on still differ; no specular. HiddenLine draws faces unlit in the background color.
- 2026-10-25 — **Edges** (§5). Instanced screen-space quads, 1 px (× pixels per point) and the
  style's profile width; each end extends half a pixel so joints close; edges are pulled toward
  the eye by scaling clip z by 1.0004 instead of offsetting the faces. **Profiles are computed on
  the GPU** in the edge vertex shader from the two face normals and the eye in local coordinates
  (the sign of n·v is the same in local and world space for any affine transform), not on the
  CPU per item: no per-frame CPU work. Soft and smooth edges are drawn only as profiles unless
  hidden geometry is shown. Color modes: All = black; ByMaterial = the front material of the
  first adjacent face (instance overrides are not considered); ByAxis = red/green/blue for edges
  parallel to X/Y/Z, computed in the shader.
- 2026-10-25 — **Selection rendering** (§9): selected faces are drawn again from a per-frame index
  buffer with the selection color at 35 % (depth test ≥, no depth write) and their boundary edges
  3 px; selected edges 3 px; whole selected occurrences (and everything inside them) draw their
  edges in the selection color at 2 px. Occurrences outside the edit context are mixed toward the
  background by `inactive_fade`.
- 2026-10-25 — **Id buffer** (§8): a separate single-sample `Rg32Uint` pass writes (draw item + 1,
  face + 1) or (draw item + 1, edge + 1 with the top bit set) when a frame asks for ids; `pick(x, y,
  radius)` reads a window back synchronously and prefers edges, then the nearest pixel. The GUI
  confirms with the CPU ray cast for clicks (§8 "authoritative").
- 2026-10-25 — **Textures** (§2, §13): decoded to RGBA8 sRGB, downscaled to the device limit,
  no mipmaps yet; sampled with repeat. Not rendered yet in 0.2: shadows, section caps, fog, sky
  and ground, watermarks, depth cue, jitter, extensions and endpoints (later milestones).
- 2026-10-25 — **Sky and axes.** The style's sky color is drawn behind everything as a
  full-screen pass: above the horizon the background color turns into the sky color with the
  ray's height (fully sky about 27° up), below it the ground color covers the background with
  its opacity (none by default). Parallel views spread the gradient as a 35° perspective would.
  The drawing axes (View › Axes, on by default in the GUI, off headless) are edges from the
  origin colored by axis, solid on the positive side and dashed on the negative side, depth
  tested against faces like model edges; they reach four times past the eye and the model, and
  that extent is included when fitting the depth range.
- 2026-10-26 — **Fog and watermarks.** With the document's fog switch on and a fog in the active
  style, faces and edges blend linearly toward the fog color (the background's, or its own)
  between `start` and `end`, measured along the view (clip `w` in perspective views, the linear
  depth in parallel ones); the sky is not fogged. Watermarks are screen quads drawn after the sky
  (`behind_model`) or after everything, with the image's alpha times the watermark's opacity;
  sizes are `scale` × the target height (the width follows the image): stretched fills the
  target, tiled repeats tiles of that size from the top left, centered sits in the middle,
  anchored places it at `(anchor_x, anchor_y)` of the free space with a margin of 2 % of the
  height. Transparent-background renders draw neither.
- 2026-10-26 — **Openings and billboards.** A face that a glued, opening-cutting instance of the
  same container sits on is triangulated with that instance's opening as an extra hole: the
  outer loop of the component's largest face lying on its red-green plane (else its footprint
  rectangle there), placed by the instance's transform; an outline that is not entirely inside
  the face (and outside its holes) is ignored. Instances of components that always face the
  camera turn about their blue axis, through their origin, so that their front (the definition's
  −green direction) faces the eye; picking still uses the stored placement.
