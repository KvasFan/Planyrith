# 03 — Geometry kernel

**Audience:** implementing agents. Normative. Lives in `planyrith-core::geom`, `::math`,
`::tolerance`, `::spatial`, `::inference` (the inference *engine* is here; its *UX* is in `docs/07`).

## 1. Coordinate system and units

- Right-handed, **Z up**. Drawing axes: X = red, Y = green, Z = blue. Ground plane is XY.
- Internal length unit: **millimeter**, stored as `f64`. All lengths in APIs use the newtype
  `Length(f64)`; angles use `Angle(f64)` in radians. Display units (mm, cm, m, in, ft-in,
  architectural fractions) are a *formatting* concern in `units.rs`.
- Model extent supported: ±1 × 10⁹ mm (1 000 km) with full tolerance behavior; beyond that,
  commands return `GeometryError::OutOfRange`.
- Transforms are `glam::DAffine3` (rotation+scale+shear 3×3 plus translation). Instances may be
  mirrored (negative determinant) and non-uniformly scaled; algorithms must not assume
  orthonormality unless they normalize first.

## 2. Tolerances

```rust
pub struct Tolerances {
    pub length: f64,        // 1e-3 mm  — two points closer than this are the same point
    pub length_sq: f64,     // length²
    pub angle: f64,         // 1e-6 rad — direction equality / coplanarity test
    pub planarity: f64,     // 1e-2 mm  — max distance of a loop's vertices from its fitted plane
    pub parametric: f64,    // 1e-9     — curve parameter equality
}
```

Rules:
- Never compare `f64` with `==` in geometry. Use `approx_eq_len`, `approx_eq_angle`,
  `is_coplanar`, `is_parallel`, `is_perpendicular` helpers from `tolerance.rs`.
- Tolerances are a property of the `Document` (`ModelInfo.tolerances`) with the defaults above.
  A document may raise them (e.g. imported sloppy meshes) but never below `1e-6 mm`.
- Edges shorter than `length` **MUST NOT** exist in a mesh; operations that would create one
  merge the endpoints instead and report a warning.
- Faces with area < `length²` are degenerate and are removed with a warning.

## 3. The half-edge mesh

Each `Entities` container (`docs/04 §3`) owns exactly one `HalfEdgeMesh` holding all of its
edges and faces. Groups and component definitions therefore have their own meshes; nothing in a
mesh references geometry in another container.

### 3.1 Data layout (struct-of-arrays, slot maps with generations)

```rust
pub struct HalfEdgeMesh {
    vertices:   SlotMap<VertexId, Vertex>,       // position: DVec3
    half_edges: SlotMap<HalfEdgeId, HalfEdge>,   // origin: VertexId, twin, next, prev, edge: EdgeId, face: Option<FaceId>
    edges:      SlotMap<EdgeId, Edge>,           // he: HalfEdgeId (one of the pair), flags: EdgeFlags, curve: Option<CurveRef>
    faces:      SlotMap<FaceId, Face>,           // outer: LoopId, holes: SmallVec<LoopId>, plane: Plane, material_front/back, uv_front/back
    loops:      SlotMap<LoopId, Loop>,           // start: HalfEdgeId, face: FaceId
    curves:     SlotMap<CurveId, CurveDescriptor>, // arcs/circles/polylines grouping consecutive edges
    vertex_edges: SecondaryMap<VertexId, SmallVec<[HalfEdgeId; 6]>>, // outgoing half-edges (star)
}
```

Invariants (checked by `Mesh::validate()` in debug builds and by `planyrith validate`):

1. Every edge has exactly two half-edges (twins). A half-edge's `face` is `None` when it borders
   no face on that side (open/wire edges are allowed — this is not a closed-manifold mesh).
2. A face has one outer loop (counter-clockwise when viewed from the front, i.e. along `+normal`)
   and zero or more hole loops (clockwise). All loop vertices lie within `planarity` of `face.plane`.
3. Two vertices are never closer than `tolerances.length`.
4. Two edges never intersect except at shared vertices (edges are split at intersections when
   added — see §4.2). Edges never pass through a face interior without splitting it.
5. An edge may border more than two faces only through *distinct* half-edge pairs? **No:** an
   edge borders at most two faces (one per half-edge). Non-manifold junctions (three faces
   sharing a line) are represented by coincident but separate edges only if they belong to
   different containers; within one container the geometry is a 2-manifold with boundary plus
   wire edges. (Rationale: direct modeling of thin surfaces needs T-junctions along *edges* —
   that is handled by splitting faces, not by >2-valent edges.)

### 3.2 Edge flags

```rust
bitflags! { pub struct EdgeFlags: u8 {
    const SOFT    = 0b0001; // shading normals averaged across this edge (smooth shading)
    const SMOOTH  = 0b0010; // edge not drawn when both faces visible
    const HIDDEN  = 0b0100; // never drawn (unless "show hidden geometry")
    const CAST_SHADOW_PROFILE = 0b1000; // reserved
}}
```

### 3.3 Curves

Arcs, circles and polygons are polylines of ordinary edges plus a `CurveDescriptor`
(`Arc { center, normal, radius, start_angle, end_angle, segments }`, `Circle{…}`,
`Polygon{…}`, `Freehand{points}`) referenced by each member edge. Behavior:
- Selection, inference ("center", "tangent", "on arc") and Push/Pull treat the curve as one unit.
- Segment count is editable while the curve is still "intact" (all member edges present,
  no member edge used by a face other than the two the curve was created with).
- Any edit that breaks the invariant drops the descriptor (edges become plain).
- Faces whose boundary includes a curve mark the inter-segment edges `SOFT | SMOOTH`.

## 4. Fundamental operations

All operations are functions in `geom::*` taking `&mut HalfEdgeMesh` plus a `&mut DeltaSink`
that records inverse operations for undo (`docs/01 §3`). They return `Result<_, GeometryError>`
and never leave the mesh in an invalid state (apply-or-rollback within the function).

### 4.1 Add vertex / merge
`add_vertex(p) -> VertexId` returns an existing vertex if one is within `tolerances.length`.

### 4.2 Add edge (the workhorse)
`add_edge(a: DVec3, b: DVec3) -> AddEdgeResult { edges: Vec<EdgeId>, new_faces: Vec<FaceId>, split_faces: Vec<(FaceId, FaceId, FaceId)> }`

Algorithm:
1. Snap endpoints to existing vertices (§4.1). If `a≈b` → `Err(Degenerate)`.
2. **Split against existing geometry:** collect all edges whose segment intersects `[a,b]`
   (spatial index query on the segment's AABB expanded by tolerance), split both at the
   intersection points (creating vertices); collect vertices lying on `[a,b]` and split the new
   edge there. Result: a chain of edges `e1..en`.
3. For each edge in the chain whose interior lies on an existing face (point-in-polygon test in
   the face's plane, distance ≤ planarity): the edge becomes an *interior* edge of that face.
4. **Face finding (§4.3)** on every vertex touched: detect newly closed coplanar loops → create
   faces; detect faces split by a chord → split (§4.4).
5. Emit deltas.

### 4.3 Face finding
Given the set of touched vertices `V`:
1. For each pair of consecutive edges around each `v ∈ V` (angularly sorted star), consider the
   plane they span (skip if collinear). Collect candidate planes (deduplicated by
   `is_coplanar`).
2. For each candidate plane `P`: take the subgraph of edges lying in `P` (vertices within
   `planarity`), connected to `V`. Compute the planar embedding (edges sorted by angle around
   each vertex in `P`'s 2D frame). Walk each half-edge "turn left-most" to enumerate minimal
   cycles. Discard the unbounded outer cycle (the one with negative signed area in the frame
   chosen so interior faces are positive) — unless the subgraph has only one cycle, which means a
   single closed loop → one face.
3. For each minimal cycle not already a face loop: if it contains an existing face's loop as a
   sub-area (i.e. the cycle encloses existing geometry) → it's a hole situation handled in §4.5;
   otherwise create `Face { outer: cycle }` with plane `P`, orientation chosen so the normal
   points toward `+Z` if the plane is not vertical, else toward the viewer-agnostic rule
   `+Y` then `+X` (deterministic default; the user flips later). The caller (Push/Pull, etc.)
   may override orientation.
4. **Interior wire edges**: an edge chain inside a face whose both endpoints touch the face
   boundary (or each other forming a cycle) splits the face (§4.4) or creates a hole (§4.5).

Complexity target: O(k log n) per added edge for k local edges; never a global scan.

### 4.4 Split face by chord
Face `F` with outer loop `L`; chord `c` connects two boundary vertices through the interior
(possibly via a chain). Build the two new loops by walking `L` from `c.start` to `c.end` plus
`c`, and the complement. Assign existing holes to whichever new face contains them. Copy
materials/UVs. Delete `F`, create `F1`, `F2`. Deltas: `FaceSplit { old, new: [F1, F2] }` so
undo restores the single id.

### 4.5 Holes
A closed cycle strictly inside a face `F` (no shared vertices) becomes a **hole** of `F` and a
new **inner face** `G` with the same plane and opposite loop orientation (so the user can later
erase `G` to get a real hole, or push/pull it). Erasing `G` leaves the hole; erasing the cycle's
edges removes the hole and re-merges.

### 4.6 Erase edge
`erase_edge(e)`:
1. If `e` borders two faces `F1`, `F2` that are coplanar → **merge** them into one face (union
   of loops, holes recomputed; the new face keeps `F1`'s id and material).
2. If `e` borders exactly one face → delete that face (it is no longer closed).
3. If `e` borders two non-coplanar faces → delete both faces (the user asked for a hole in a
   box) — this matches direct-modeling expectations.
4. Delete `e`; delete vertices that became isolated.
Variant `erase_edge_keep_faces` is used by Soften/Hide tools (flags only, no topology change).

### 4.7 Erase face
Deletes the face only; edges stay (unless `erase_orphans=true`, used by Eraser with modifier).

### 4.8 Move vertices (translate/transform a set of vertices)
`transform_vertices(set, T)`:
1. Compute new positions; detect vertex merges (new positions within tolerance of each other
   or of untouched vertices) → merge, with edge collapse if the edge's endpoints merge.
2. For each affected face check planarity. If a face would become non-planar: **auto-fold** it —
   triangulate the face along the shortest diagonal that restores planar pieces (the new interior
   edges are flagged `SOFT|SMOOTH|HIDDEN` so the fold is invisible). This is the behavior users
   expect when dragging one vertex of a quad.
3. Intersection pass over the moved edges (and fold edges): with `check_intersections=true`
   edges they cross are split and the faces those pieces cross re-split; with the default
   `false` a move whose edges would cross or touch other edges is refused (`E1302`). Either way
   invariant 4 holds after a move. Moved faces passing through other faces' interiors are the job
   of Intersect Faces, which is explicit.

### 4.9 Extrude face (Push/Pull core)
`extrude_face(f, distance, options{ create_new_start_face: bool })`:
1. Offset all vertices of `f`'s loops along `f.normal * distance` into new vertices; create side
   faces (quads) for every boundary edge of every loop (holes included); create the new cap.
2. If `create_new_start_face` (modifier) keep the original face; otherwise delete it (the
   original is now interior to the volume).
3. **Merge rule:** after extrusion, for each new side face check coplanar adjacency with
   existing faces sharing an edge → merge (so pulling a rectangle on a wall extends the wall).
4. **Push-through rule:** if `distance` is negative and the cap lands coplanar (within
   `planarity`) on an existing face of the same container that fully contains the cap outline
   → split that face by the cap outline and delete the interior piece plus the cap
   (a through-hole is created). If the cap lands on the back of another face but is not
   contained → ordinary extrusion (user may run Intersect Faces).
5. Side faces inherit the material of the start face's *neighbors* when a neighbor face
   is coplanar and merged (step 3); otherwise default material.

### 4.10 Offset loop
`offset_face_boundary(f, d)`: offset each loop edge inward/outward by `d` in the face plane using
miter joins; resolve self-intersections by clipping (polygon offset with the straight skeleton
simplification: for convex corners use miter; for concave corners trim); result edges added via
§4.2 so faces form automatically.

### 4.11 Sweep
`sweep_profile(profile_face, path_edges)`: for each path vertex compute a frame (path tangent,
Frenet-free "minimum rotation" frame to avoid twisting); transform the profile to each frame;
connect consecutive profiles with quads; at path corners miter the profile (intersect adjacent
sweep planes). Closed path → closed ring. The profile face is deleted (it is interior).

### 4.12 Intersect faces
`intersect(set_a, set_b, scope)`: compute face–face intersection segments (plane–plane line
clipped to both polygons); add them as edges via §4.2 into the target container (`scope`
decides: with model / with selection / with context). Because §4.2 splits faces, the result is
ready for erasing pieces.

### 4.13 Solid classification and booleans
A container is a **solid** if its mesh is a closed 2-manifold (every edge borders exactly two
faces, no wire edges, no faces inside) and has consistent orientation; `is_solid()` is cached and
reported in the Entity Info panel and `planyrith query --solid`.

`boolean(op, A, B)` for solids (half-edge implementation, `geom::boolean`):
1. Intersect faces of A and B (§4.12) into copies.
2. Classify every face piece as inside/outside/on the other solid (ray parity + coplanar
   handling by normal comparison).
3. Keep pieces per op: Union = outside A ∪ outside B; Intersect = inside ∩; Subtract A−B =
   outside-B pieces of A ∪ inside-A pieces of B (flipped); Trim = Subtract but keep B;
   Split = all three regions as separate solids; Outer Shell = Union of all, dropping internal
   voids.
4. Stitch pieces into a new mesh, merge coplanar faces, validate solidity.
If step 2 reports ambiguity (coplanar overlaps with mismatched tolerances), fall back to the exact
kernel under feature `brep` (`geom::brep::boolean`) and otherwise return `GeometryError::
NonRobust` with guidance.

## 5. Tessellation (for rendering and export)

`tessellate_face(f) -> Vec<[u32; 3]>`: project loops to the face's 2D frame, run ear clipping
(`earcutr`) with holes; cache the result per face keyed by `(face_gen, vertex positions hash)`.
For faces with a texture and area > 10 m² use constrained Delaunay (`spade`) to avoid
sliver triangles that distort projected textures. Normals per vertex: face normal unless the edge
is `SOFT`, in which case average adjacent face normals (angle-weighted).

## 6. Spatial indexes (`spatial/`)

- Per container: a `Qbvh<EdgeId>` over edge AABBs and a `Qbvh<FaceId>` over face AABBs
  (`parry3d-f64`). Rebuilt incrementally from deltas (`refit` + periodic rebalance).
- Per document: a `Qbvh<InstancePath>` over instance world-space AABBs for broad-phase picking
  and inference across groups; narrow phase transforms the ray into the container's local frame.
- Queries: `ray_cast(ray, filter) -> Vec<Hit{ path, kind: Vertex|Edge|Face, t, point }>` sorted
  by `t`; `segment_intersections`, `aabb_query`, `nearest_edge_to_segment`.

## 7. Inference engine (`inference/`)

Input: a screen ray (camera + pixel), pixel tolerance (default 8 px), the current tool's
*constraint* (free / plane / axis / line), and an optional *reference point* (for "from point"
inferences).

Output: `Inference { point: DVec3, kind, on: Option<EntityPath>, locked: bool, label }`.

Kinds, in **priority order** (higher wins when several are within pixel tolerance):
1. `Endpoint` (vertex) — green
2. `Midpoint` — cyan
3. `Intersection` (edge×edge in screen space projected to 3D, or edge×face) — black/red
4. `Center` (arc/circle) — blue
5. `OnEdge` — red
6. `OnFace` — blue
7. `Tangent` / `Perpendicular` / `Parallel` to an edge (direction constraints) — magenta
8. `FromPoint` axis alignment with a remembered reference point — dotted colored line
9. `OnAxis` (red/green/blue) — the drawn direction is within tolerance of an axis
10. `OnPlane` (ground or current constraint plane) — none
11. `Free` (point on ground plane for first points; on the tool's constraint plane after)

Rules:
- **Hover memory:** hovering an entity for > 400 ms (GUI) or explicitly via CLI `--ref` adds it
  as a reference for `FromPoint`/`Parallel`/`Perpendicular` inferences (max 3 references).
- **Axis lock:** `Shift` locks the current inference's constraint; arrow keys lock to an axis
  (`→` red, `←` green, `↑` blue, `↓` parallel/perpendicular to hovered edge). While locked,
  the point is the projection of the ray onto the locked line/plane.
- **Depth resolution for free points:** ray–ground-plane; if the ray misses the ground (looking
  up), use a vertical plane through the last point facing the camera.
- All inference computations are in `f64` world space; pixel tolerance is converted to a world
  distance at the candidate's depth. Deterministic: ties broken by priority, then by smaller
  `t`, then by lower id.
- The CLI exposes the engine as `planyrith infer --from <x,y,z> --dir <dx,dy,dz>` for tests.

## 8. Numeric robustness guidelines

- Prefer formulations that avoid catastrophic cancellation (e.g. compute plane fit via centroid
  and covariance; compute circle centers via perpendicular bisectors in the plane frame).
- Clamp parameters to `[0,1]` for segment tests with `parametric` tolerance.
- When a result depends on a predicate (left/right of a line), evaluate the predicate once and
  reuse it; never recompute with a different formula in the same algorithm.
- Snap results to the vertex grid implied by `tolerances.length`? **No** — do not quantize
  coordinates; merge by tolerance only (quantization causes drift under repeated transforms).

## 9. B-rep bridge (feature `brep`)

`geom::brep`: `to_solid(&HalfEdgeMesh) -> truck::Solid` (planar faces → planar surfaces, loops →
wires of line/arc edges using curve descriptors) and `from_solid(&truck::Solid, tess_res) ->
HalfEdgeMesh` (curved faces tessellated with curve descriptors retained for cylinders/arcs where
possible). Used by STEP I/O and by boolean fallback. Round trip of a planar solid must be exact
within `tolerances.length`.

## 10. Acceptance tests (minimum)

- Property tests: random planar polygons → `add_edge` sequence → exactly one face; random
  chords → split count; erase chord → merge back to original face id-count.
- Push/Pull a rectangle on a cube face by +d: face count +4 (sides) (merged with coplanar
  neighbors when applicable); by −d through the cube: through-hole created, solid remains solid.
- Sweep a circle profile along a closed square path: closed solid ring with 4×segments faces.
- Boolean union of two overlapping cubes = solid with 12 faces (after coplanar merge); volume
  equals inclusion–exclusion within 1e-6 relative.
- Inference: endpoint beats on-edge at equal pixel distance; axis lock projects correctly;
  results identical across platforms (golden JSON).
- `validate()` passes after every fuzzed sequence of operations (cargo-fuzz target).

## Decisions

- 2026-10-20 — Z-up, millimeters, `f64`, tolerances in §2.
- 2026-10-20 — Within one container the mesh is a 2-manifold-with-boundary plus wire edges;
  non-manifold edges are not permitted (§3.1 inv. 5).
- 2026-10-20 — Curves are polylines with descriptors, not exact NURBS, in the interactive kernel.
- 2026-10-20 — Auto-fold on non-planar vertex moves creates hidden soft edges (§4.8).
- 2026-10-20 — Skeleton contract: `EdgeFlags` is a `u8` newtype with associated constants (the
  `bitflags` crate is not in the approved list); `SmallVec` fields are plain `Vec`s. The mesh is
  struct-of-slot-maps as in §3.1 with private fields and accessor methods.
- 2026-10-20 — Kernel operations take `&Tolerances` explicitly (the document's) and a
  `&mut dyn DeltaSink`. `MeshDelta` identifies vertices and edge endpoints by **position**, not by
  `VertexId`: invariant 3 (no two vertices within `length`) makes a position a stable identity,
  and slot-map keys cannot be re-created at a chosen index on undo. `FaceSplit { old, new }` and
  `FacesMerged { kept, removed }` are composite markers emitted alongside the elementary
  add/remove deltas so pid bookkeeping can keep the surviving identity.
- 2026-10-20 — `add_vertex` returns `Result` (it can fail with `OutOfRange`); `erase_edge_keep_faces`
  is exposed as `set_edge_flags`. The file form of a mesh is `IndexedMesh` with
  `DirectedEdgeIndex` (bit 31 = reversed), produced by `to_indexed` / consumed by `from_indexed`.
- 2026-10-20 — **Face finding is computed on the planar arrangement** (§4.3–§4.5, implemented in
  `geom::planar`). For each candidate plane (stored planes of faces holding a new edge first, then
  planes spanned by edge pairs at touched vertices): collect the in-plane edges connected to the
  touched vertices, the coplanar faces that use or contain them (with all their loops) and any
  in-plane geometry enclosed by a bounded region; trace every half-edge cycle with the "next edge
  clockwise from the twin" rule; drop edges whose two half-edges fall in the same cycle (antennae,
  bridges, wire edges lying on a face) until none remain; counter-clockwise cycles bound regions,
  each clockwise cycle is a hole of the smallest region strictly containing it. Every region is
  mapped to the existing face containing an interior sample point (centroid of its largest
  ear-clipping triangle). Unchanged faces are left alone; a face covering several regions is split;
  a region no face covers becomes a new face **only when its outer boundary uses an edge the caller
  just drew or re-drew** (a "seed"), so adding an unrelated edge never re-creates a face the user
  erased, while re-drawing an edge of a face-less loop does. Chords, holes, door openings that touch
  a face boundary, bridges and dangling edges all follow from this one rule.
- 2026-10-20 — **Face identity across edits.** A face whose region survives keeps its `FaceId`; the
  change is recorded as `FaceRemoved { id }` followed by `FaceAdded { id }` with the *same* id
  ("reshape"), which the transaction treats as "keep the pid". On a split the piece that keeps the
  whole old outer boundary (hole situation) or else the largest piece keeps the id, and
  `FaceSplit { old, new: [old, other] }` is emitted (this refines §4.4 "delete F, create F1, F2").
  On erase-edge merges the **larger** face survives (ties: the face left of `edge.he`), so healing an
  opening drawn on a wall keeps the wall's id and material (refines §4.6 "keeps F1's id").
- 2026-10-21 — **Edge identity across splits.** The kernel records an edge split as
  `EdgeRemoved` plus one `EdgeAdded` per piece; the transaction recognises the pieces (both
  endpoints on the removed segment) and gives the removed edge's pid, order slot and attributes to
  the **longest** piece (ties: the piece at the old start point). Every piece inherits the tag and
  hidden state; the other pieces get new pids. This mirrors the face rule above, so a pid named in
  a script keeps pointing at (most of) the same edge after unrelated drawing.
- 2026-10-20 — **Orientation of new faces.** A half-edge carries one face, so faces sharing an edge
  always have consistent orientation. A new face takes the default front of §4.3 step 3 when those
  half-edges are free, otherwise the opposite front; when both are taken (the last side of a box
  whose floor and top both face +Z) the smaller edge-connected group of conflicting faces is
  reversed first (front/back appearance swapped so each side keeps its material). Consequence for
  `reverse-faces` (0.4): reversing one face of a connected group is not representable under
  invariant 2; that tool must reverse whole groups, or invariant 2 must be relaxed to decouple the
  front from the loop direction. Open question, to be decided with that tool.
- 2026-10-20 — **Wire edges lying on a face** (dangling edges, bridges between a face boundary and
  a hole, the remaining pieces of a partly erased chord) are allowed; they are not part of any loop.
  `validate` checks the edge–edge half of invariant 4 only.
- 2026-10-20 — **Curves.** The mesh stores each curve's member edges in order (rebuilt by a chain
  walk on load). Splitting or deleting a member edge drops the descriptor. "Intact" is tested
  topologically: the member edges together border at most two distinct faces, which survives
  unrelated splits of a neighboring face and fails once the curve is extruded.
- 2026-10-20 — **Moves are batch deltas.** `transform_vertices` records one `MeshDelta::VerticesMoved`
  instead of per-vertex moves: a sequence of single moves can pass through states where two vertices
  coincide (rotating a square by 90°), which would make position-based replay ambiguous. Merging
  vertices lose their edges and are removed *before* the batch move. Replay locates vertices, edges
  and faces by position and resolves id-only deltas through `ReplayIds` (alias classes: recorded id
  → id that replay re-created), which must persist per container across undo/redo.
- 2026-10-20 — **Auto-fold** (§4.8): quads fold along the shorter valid diagonal; other faces are
  ear-clipped in their best-fit plane and the triangles regrown into maximal planar groups; the
  largest piece keeps the face id, fold edges are `SOFT | SMOOTH | HIDDEN`. Without auto-fold a move
  that would bend a face fails and rolls back.
- 2026-10-20 — **Fold triangles and intersection checks** (§4.8 steps 2–3, found by randomized
  operation sequences). A fold triangle whose side runs through another vertex of the face's loops
  (collinear boundary vertices) is subdivided at that vertex, and zero-area triangles are dropped,
  so fold edges never pass through vertices. With `check_intersections` the fold edges join the
  moved edges in the intersection pass: crossings split both edges at the crossing point, a vertex
  lying inside an edge splits that edge, and a moved edge that becomes **collinear and
  overlapping** with another edge fails the whole move (`E1302`, rolled back) — welding coincident
  edges is the job of an explicit intersect operation, not of a move.
- 2026-10-20 — **Draft performance trade-offs**, to revisit before importers (0.5): public mesh
  operations roll back by restoring a snapshot taken on entry (replaying inverse deltas is the
  planned optimization); proximity lookups (vertex at a position, edges near a segment, coplanar
  faces near a box) are linear scans with bounding-box rejection; face finding recomputes the
  arrangement of the whole coplanar region it touches. A 12×12 grid of squares drawn line by line
  takes well under a second in a debug build.
- 2026-10-20 — `from_triangles_merged` welds input positions by tolerance (grid hashing), flips
  triangles whose winding contradicts an already accepted neighbor, drops (and counts) triangles
  that would make an edge non-manifold, merges coplanar edge-connected triangles whose boundary is
  one planar region, and softens edges between faces meeting below `soften_angle`.
- 2026-10-20 — **Extrude (§4.9) cases.** (1) *Stretch*: when every boundary edge of the face borders
  a face containing the extrusion direction (the top of a box), the face's vertices move and the
  neighbors stretch, in both directions; the stretched edges get the §4.8 intersection pass
  (crossings split, overlaps refuse), like the generic case whose edges go through `add_edge`.
  (2) *Generic*: vertical and cap edges are added with
  `add_edge` so face finding builds sides and cap with orientations that agree with the neighbors;
  hole rings are capped before the outer ring and their top fills removed. The start face is
  removed (it becomes interior) unless nothing lies across its edges, in which case it stays as the
  base facing away from the volume (a lone rectangle becomes a closed box) — this refines step 2,
  since keeping a face whose edges already carry a second face would need three faces on an edge;
  `create_new_start_face` is refused in that case. (3) *Join / push-through*: when the cap outline
  lands inside a coplanar face, the outline is cut into it and the inside piece removed *before*
  the sides are added; this opens a hole when pushing and joins the volumes when pulling. Faces that
  occupy a side strip from the wrong side (the underside of a wall below a door that reaches the
  floor) are removed, the edges they leave as wires are deleted, and face finding builds the side
  faces they blocked. Sides of curve-bounded faces get `SOFT | SMOOTH` between segments and the cap
  receives a translated copy of the curve descriptor.
- 2026-10-20 — **Offset (§4.10).** Each ring is offset along the normal pointing out of the
  region with miter joins, bevelled where the miter would exceed 4 × |d|. Self-intersections are
  resolved on the planar arrangement of the raw offset curves: a region is kept when its winding
  number is at least one and, for inward offsets, it lies inside the original region at least |d|
  from its boundary; the result is the boundary of the kept regions. Holes offset in the opposite
  sense (the region's width stays constant); an inward offset past a narrow part yields several
  loops, past the whole width none (`E1301`). `offset_polygon_2d` returns the largest loop;
  `offset_rings_2d` returns all of them.
- 2026-10-22 — **Direct insertion and curve transforms.** `HalfEdgeMesh::insert_disjoint` appends
  a piece given in indexed form unchanged (faces with holes, flags, curves, sides) when no vertex,
  edge or face of it touches the mesh, and returns `None` otherwise (the caller then merges with
  `add_edge`); `transform_indexed` maps a piece through an affine transform, reversing loops under
  a mirror so faces keep their front side. `CurveDescriptor::transformed` keeps a descriptor only
  under similarity transforms (rotation, uniform scale, translation, mirror; a mirrored arc flips
  its normal so its sweep keeps its sign); under non-uniform scaling or shear the curve is dropped
  and its edges become plain (§3.3).
- 2026-10-23 — **No dependence on slot order.** Ties are broken by geometry, never by
  `EdgeId`/`HalfEdgeId`/`FaceId` order, which a save/reload renumbers: when a face splits into
  pieces of equal area (within what the length tolerance can change), the piece with the smaller
  position key (mean of its outer vertices, then its lowest vertex) keeps the id, and new pieces
  and new faces are created in position-key order. Copies, definitions and explode take edges
  and faces in container (pid) order. Merging two faces keeps every region whose sample point
  lies inside or on either old face (the sample of the merged region may lie on the removed
  edges).
- 2026-10-25 — **Extrude phases** (refines the 2026-10-20 extrude entry; found by the 0.1 review
  and randomized command sequences). The generic case runs in phases: (A) vertical and cap edges
  are inserted without face finding, splitting existing faces they cross; (B) on a join the cap
  outline is cut into the landing face and the inside piece removed, its identity passing to the
  face it merges with; (C) side strips occupied from the other side are cancelled (§4.9 entry);
  (D) face finding builds the side faces first, from the side planes only, then the cap, and a new
  face must be bounded by extrusion edges only (base, vertical and cap edges) and touch at least
  one swept edge; (E) hole tops and swept edges no face uses are removed, then each side merges
  into coplanar neighbors across its base edge (and its top edge when joining), repeatedly, as
  step 3 requires. Building the sides first gives the top of a hole ring to the hole's sides
  before the cap plane is searched, whatever the order of the vertices. A join needs a landing
  face that **covers** the cap outline: the outline lies inside or on the landing polygon, no
  landing vertex lies strictly inside the cap and the rings do not cross; otherwise the pull is an
  ordinary extrusion. Pushing a stretchable face (the top of a box) to or past the shallowest of
  its neighbors would turn the solid inside out and is refused (`E1301`); `through_hole` is
  reported only when a face of a solid is pushed through.
- 2026-10-25 — **Moves never leave crossings** (§4.8 step 3). Without `check_intersections` the
  intersection pass still runs, in detection mode: if it would split anything the move is refused
  (`E1302`) and rolled back, so a document never holds edges that cross away from a shared vertex
  (invariant 4, which `validate` checks). With it, the moved edges are not drawn: the pieces only
  re-split the faces they now cross, they never create a face of their own (seed rule), so a
  move cannot re-create an erased face or fill a hole. Curve descriptors follow a move when every
  member edge moves with them (§3.3, `CurveDescriptor::transformed`), otherwise they are dropped.
  A hole loop moved out of its face is dropped from it (the face closes over where it was);
  `validate` reports a hole loop outside its outer loop as an invariant-2 violation.
- 2026-10-25 — `add_edge` always splits an existing edge at an endpoint of the new segment that
  lies inside it, whatever the contact classification says (a long, nearly collinear edge can be
  reported as a single mid-stretch contact). Offset takes the ring of largest signed area as the
  outer ring of its result.
- 2026-10-25 — **Slot order, continued.** Besides the ties of the 2026-10-23 entry, the order of
  work itself is geometric where it decides ids or orientation: face finding visits touched
  vertices in position order and the edges around each by far-end position (the first plane
  found fixes the orientation of the faces after it); a loop starts at the half-edge whose
  (origin, next origin) positions are lowest; Newell's sums start at the lowest point, so every
  rotation of a loop fits the same plane to the bit; `add_edge` splits the edges it meets in
  order along the new segment, and the intersection pass splits at the vertex or crossing
  nearest the start of the edge; faces that face finding splits, and faces a move detaches and
  re-attaches (or folds), are handled in position-key order. A document saved and reopened between commands therefore gives
  the same geometry, orientations and pids as one session (checked by
  `commands::fuzz_tests`); only the keys of groups, instances and definitions may be numbered
  differently, because a session keeps the keys of removed ones reserved for undo.
- 2026-10-25 — **Numeric details.** Closest points between segments use the common normal
  `n = d1 × d2` (parallel when `|n|² ≤ 1e-24 · |d1|² · |d2|²`), and a collinear contact is tested
  in both directions. A transform is invertible when its linear part is well conditioned (the
  product of the Frobenius norms of the matrix and its inverse below `1 / parametric`), not when
  the raw determinant exceeds a constant: a uniform scale by 0.001 is invertible, a matrix that
  squashes one axis to nearly nothing is not.
