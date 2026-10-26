# 04 — Document model

**Audience:** implementing agents. Normative. Lives in `planyrith-core::document`.

## 1. Overview

```
Document
├── ModelInfo            units, precision, tolerances, geo-location, author, description, created/modified (set by frontends, not core)
├── Model: Entities      the root container ("model space")
├── ComponentDefinitions name → Definition { entities: Entities, behavior, metadata, uuid }
├── Materials            name → Material
├── Tags                 TagFolder tree, Tag { name, visible, color, line_style }
├── Scenes               ordered list of Scene
├── Styles               named Style presets (+ the active one)
├── RenderingOptions     shadows, fog, background, section settings (active document state)
├── Camera               active camera (saved in file; not undoable)
└── Attributes           AttributeDictionaries on the document
```

Everything the user edits is reachable from here; nothing lives only in the GUI except
window layout, hover state and tool state.

## 2. Identity

- Every entity has an `EntityId` (64-bit: 32-bit slot index + 32-bit generation) **unique within
  its `Entities` container**, and a **persistent id** `Pid(u64)` unique within the document and
  stable across save/load (monotonic counter in `ModelInfo.next_pid`). Frontends and scripts
  address entities by `Pid` or by *path* (`docs/08 §5`); internal algorithms use `EntityId`.
- `ComponentDefinition` additionally has a `Uuid` (v4) so definitions can be recognized across
  files (library reuse, "reload component").
- Entity order inside a container is insertion order and **MUST** be preserved by save/load and
  undo/redo (determinism).

## 3. Entities container

```rust
pub struct Entities {
    mesh: HalfEdgeMesh,                        // all edges and faces (docs/03)
    groups: SlotMap<GroupId, Group>,
    instances: SlotMap<InstanceId, ComponentInstance>,
    guides: SlotMap<GuideId, Guide>,           // GuideLine (infinite/finite), GuidePoint
    dimensions: SlotMap<DimensionId, Dimension>,
    texts: SlotMap<TextId, Text>,
    images: SlotMap<ImageId, ImageEntity>,
    section_planes: SlotMap<SectionPlaneId, SectionPlane>,
    order: Vec<EntityId>,                      // insertion order across all kinds
    attrs: SecondaryMap<EntityId, AttributeDictionaries>,
    tag_of: SecondaryMap<EntityId, TagId>,     // default: Tag::UNTAGGED
    hidden: SecondaryMap<EntityId, bool>,
    locked: SecondaryMap<EntityId, bool>,      // groups/instances only
}
```

### 3.1 Group
```rust
pub struct Group { definition: DefinitionId /* private, 1 instance */, transform: DAffine3, name: String }
```
A group is a `ComponentInstance` whose definition has exactly one instance and is not listed in
the component browser. "Make Unique" on a copied group happens automatically when the second
copy is edited (copy-on-edit): copying a group creates a second instance of the same hidden
definition; the first edit to either forks the definition.

### 3.2 ComponentInstance
```rust
pub struct ComponentInstance {
    definition: DefinitionId, transform: DAffine3, name: Option<String>,
    material_override: Option<MaterialId>,   // applied to faces with the *default* material inside
    glued_to: Option<EntityPath>,            // for gluing behavior (face it is attached to)
    dynamic_attrs: AttributeDictionaries,    // per-instance attributes
}
```
### 3.3 ComponentDefinition
```rust
pub struct ComponentDefinition {
    uuid: Uuid, name: String, description: String,
    entities: Entities,
    behavior: ComponentBehavior {
        glue_to: GlueTo /* None | Horizontal | Vertical | Sloped | Any */,
        cuts_opening: bool,            // hides the face region it is glued on (windows/doors)
        always_face_camera: bool,      // 2D billboards (trees, people)
        shadows_face_sun: bool,
        snap_to: SnapTo /* None | Horizontal | Vertical | Sloped | Any */,
        insertion_point: DVec3,        // relative to definition origin
        axes: DAffine3,                // definition axes (can be relocated)
    },
    is_internal_group: bool,
    thumbnail: Option<ImageRef>,
    attrs: AttributeDictionaries,
    usage_count: u32 /* cached */,
}
```
Definitions with zero instances are kept ("In Model" list) until `purge-unused`.

### 3.4 Guide
`GuideLine { origin: DVec3, dir: DVec3, finite: Option<(f64,f64)> }`, `GuidePoint { pos }`.
Guides do not participate in face finding, do participate in inference, are never exported to
3D formats, are optionally exported to DXF.

### 3.5 Dimension
```rust
pub enum Dimension {
    Linear { start: Anchor, end: Anchor, offset: DVec3 /* leader direction/length */, style: DimStyle, text_override: Option<String> },
    Radial { curve: Anchor, kind: Radius | Diameter, leader: DVec3, style, text_override },
}
pub struct Anchor { path: EntityPath, kind: Vertex(VertexId) | EdgeMid(EdgeId) | Point(DVec3) }
```
Dimensions **re-measure** when anchors move (anchors are entity references, resolved on read).
If an anchor entity is deleted the dimension becomes "detached" (shown in red; CLI `validate`
lists it).

### 3.6 Text
`ScreenText { anchor: Option<Anchor>, text, screen_offset }` (screen-space size, leader to
anchor) and `Text3D` is **not** an entity: 3D Text generates ordinary faces in a group.

### 3.7 ImageEntity
A textured rectangle (`width`, `height`, `transform`, `material: MaterialId`) that can be
*exploded* into a face with a texture.

### 3.8 SectionPlane
```rust
pub struct SectionPlane { plane: Plane, name: String, symbol: String, active: bool /* at most one active per container level */, fill_cap: bool, cuts_hidden: bool }
```
Section planes inside groups cut only that group's contents.

## 4. Materials

```rust
pub struct Material {
    name: String, color: Rgba8, opacity: f32 /* 0..1, color.a mirrors it */,
    texture: Option<Texture { image: ImageRef, size_mm: DVec2, colorize: Option<ColorizeMode> }>,
    is_default_for: Option<FrontOrBack>, // the two built-in "Default" materials
    attrs: AttributeDictionaries,
}
```
- Faces have `material_front: Option<MaterialId>` and `material_back` (None = default material).
- Per-face UV mapping: `UvMap { kind: Affine(DAffine2 in face frame) | Projected{ plane, affine } | Fixed(Vec<(vertex, uv)>) }`. "Fixed pins" (4-point) are stored as an affine when possible,
  projective otherwise (`Projective(DMat3)`).
- Images are stored once by content hash (`blake3`) in the document's `ImageStore`; materials
  reference `ImageRef(hash)`. Formats: PNG, JPEG, WebP; max 16384 px per side.
- Materials are purgeable when unreferenced.

## 5. Tags

```rust
pub struct Tag { name: String, visible: bool, color: Rgba8, line_style: LineStyle /* for sheets */, folder: Option<TagFolderId> }
```
- `Tag::UNTAGGED` always exists, cannot be deleted or hidden.
- Tags control **visibility and style**, never ownership: entities belong to containers, not tags.
- Rule: raw edges/faces SHOULD stay untagged; tags are for groups/instances. The GUI warns when
  tagging raw geometry (configurable); the core allows it.
- Visibility of an entity = its tag visible AND not hidden AND all ancestors visible.

## 6. Scenes

```rust
pub struct Scene {
    name: String, description: String,
    camera: Option<Camera>,
    tag_visibility: Option<BTreeMap<TagId, bool>>,
    hidden_entities: Option<BTreeSet<Pid>>,
    section_planes: Option<BTreeMap<Pid, bool /*active*/>>,
    style: Option<StyleId>,
    shadows: Option<ShadowSettings>,
    axes_visible: Option<bool>,
    transition_s: f32, include_in_animation: bool,
}
```
Each property is `Option`: `None` means "do not change when activating this scene".
`activate_scene` is a **view command** (not undoable, does not dirty the document except for
section/hidden state which *are* document state and are grouped into one undo step).

## 7. Styles and rendering options

```rust
pub struct Style {
    name: String,
    edges: EdgeStyle { show: bool, profiles: Option<u8>, depth_cue: Option<u8>, extension: Option<u8>, endpoints: Option<u8>, jitter: bool, color: EdgeColorMode /*All|ByMaterial|ByAxis*/, back_edges: bool },
    faces: FaceStyle { mode: Wireframe|HiddenLine|Shaded|ShadedTextured|Monochrome|XRay, front_default: Rgba8, back_default: Rgba8 },
    background: Background { color, sky: Option<Rgba8>, ground: Option<(Rgba8, f32 /*transparency*/)>, show_ground_from_below: bool },
    watermarks: Vec<Watermark>,
    modeling: ModelingStyle { selection_color, locked_color, guide_color, inactive_fade: f32, section_fill, section_line_color, section_line_width_px },
    fog: Option<Fog { start, end, use_background_color, color }>,
}
pub struct ShadowSettings { enabled: bool, sun: SunModel /*Geo{date, time, lat, lon, utc_offset, north_angle} | Manual{azimuth, altitude}*/, light: u8, dark: u8, on_faces: bool, on_ground: bool, from_edges: bool }
```

## 8. Camera

```rust
pub struct Camera { eye: DVec3, target: DVec3, up: DVec3, projection: Perspective{ fov_y_deg: f64 } | Parallel{ height_mm: f64 } | TwoPoint{ fov_y_deg }, aspect: f64 /* frontend-provided */ }
```
Stored in the document (last view) and in scenes; camera changes are not undoable.

## 9. Attribute dictionaries

```rust
pub struct AttributeDictionaries(BTreeMap<String /*dict*/, BTreeMap<String /*key*/, Value>>);
pub enum Value { Bool, Int(i64), Float(f64), Length(f64), Str(String), Bytes, Vec3, Color, Array(Vec<Value>), Map(BTreeMap<String, Value>), EntityRef(Pid) }
```
Reserved dictionary prefixes: `plyr.*` (core), `sheets.*` (`docs/15`), `ext.<plugin-id>.*`.

## 10. ModelInfo

```rust
pub struct ModelInfo {
    display_units: LengthUnit /* mm|cm|m|in|ft|ft_in|arch_fraction */, precision: u8 /* decimals or fraction denominator power */,
    angle_precision: u8, length_snap: Option<Length>, angle_snap: Option<Angle>,
    tolerances: Tolerances, geo: Option<GeoLocation { lat, lon, north_angle }>,
    title: String, author: String, description: String, format_version: u32, next_pid: u64,
    attrs: AttributeDictionaries,
}
```

## 11. Selection and context (document-side, see `docs/08 §5` for syntax)

```rust
pub struct Selection { items: IndexSet<EntityPath> }   // ordered, deduplicated
pub struct EditContext { path: Vec<InstancePath> }     // the group/instance currently "open" for editing; empty = model
```
Selection and edit context are **Engine** state (not file content), but scripts and the CLI can
set them, and they are reported in events.

## 12. Entity paths

`EntityPath = (Vec<InstanceStep>, EntityId-or-Pid)` where each step is an instance id in the
parent container. Paths identify an entity *occurrence* (e.g. the third chair's leg). Operations
on a path inside a component definition affect **all** instances (that is what a component is);
"Make Unique" first if that is not wanted. The GUI shows the instance count when entering a
component context.

## 13. Validation (`diagnostics.rs`)

`validate(doc) -> Report` checks: mesh invariants per container, dangling anchors, unreferenced
images, cyclic component nesting (forbidden), duplicate tag names, scenes referencing deleted
tags, non-invertible transforms, `next_pid` monotonicity. `repair` fixes what it can
(merge near vertices, remove degenerate faces, delete dangling references) and reports the rest.

## 14. Size limits (soft, warnings beyond)

| Quantity | Comfortable | Hard limit |
|---|---|---|
| Entities per container | 2 M | 2³¹ |
| Nesting depth | 32 | 256 |
| Instances per definition | 100 k | 2³¹ |
| Images | 2 k, 2 GB total | — |
| Scenes | 500 | 65 535 |

## Decisions

- 2026-10-20 — Groups are single-instance components with copy-on-edit uniqueness (§3.1).
- 2026-10-20 — Dimensions re-measure via anchors; detached dimensions are allowed and flagged.
- 2026-10-20 — Tags are visibility/style categories only; untagged raw geometry is the norm.
- 2026-10-20 — Camera and selection are not undoable; section-plane and hidden state are.
- 2026-10-20 — Skeleton contract: `EntityId` is an enum over the typed ids (`Edge`, `Face`,
  `Group`, `Instance`, `Guide`, `Dimension`, `Text`, `Image`, `SectionPlane`). Vertices are not
  entities and carry no pid (`EntityKind::Vertex` exists for queries, hits and inference only);
  how the `Point` CLI form `@pid` maps onto vertices is settled with the CLI at 0.1.
- 2026-10-20 — The per-entity side tables of §3 (`attrs`, `tag_of`, `hidden`, `locked`, plus
  `pids`/`by_pid`) are `BTreeMap`/`BTreeSet` keyed by `EntityId` (a `SecondaryMap` needs a single
  key type). They iterate in id order and serialize as pair sequences (`document::serde_seq`).
- 2026-10-20 — `EntityId` values are not stable across undo/redo; `Pid` is the stable identity.
  Deltas record pids and order indices so undo re-creates entities with the same pid at the same
  position in `order`. `next_pid` is never rewound.
- 2026-10-20 — `Tag::UNTAGGED` and the two default materials are slot-map keys created with the
  document (`Tags::untagged()`, `Materials::default_for(side)`), not constants. `Materials`,
  `Tags`, `Scenes`, `Styles` and `Document::definition_order` keep explicit insertion-order lists.
- 2026-10-20 — Minimal definitions where the spec was silent: `DimStyle { text_height, arrow,
  units, precision }`; `Watermark { image, placement, opacity, scale, behind_model }`;
  `RenderingOptions { shadows, fog_enabled, section: SectionDisplay }`; `SunModel::Geo` uses
  dependency-free `Date { year, month, day }` / `TimeOfDay { hour, minute }`; `ModelInfo`
  defaults to millimeters with precision 1 and `format_version = 1`; `Guide` is an enum
  `Line | Point`; `Text` is the §3.6 `ScreenText`.
- 2026-10-20 — `EntityPath { instances: InstancePath, target: EntityKey }` with
  `EntityKey = Id(EntityId) | Pid(Pid)`; `EditContext { path: InstancePath }`; `Selection` is an
  `IndexSet<EntityPath>`. `ComponentDefinition::new(uuid, name)` takes the UUID from the caller so
  the document layer stays deterministic.
- 2026-10-20 — **Stable ids for everything but mesh elements.** Non-mesh entities (groups,
  instances, guides, dimensions, texts, images, section planes), definitions, materials, tags, tag
  folders, scenes and styles live in a `StableMap` (a slot map whose removed keys stay reserved):
  removal keeps the key, and undo puts the value back under the same id with `restore`. Deltas and
  records (a group's `definition`, a face's materials, a tag's folder) can therefore hold plain ids
  across undo/redo. Mesh edges and faces are re-created by position-based replay and get new ids;
  their pids are re-bound by the transaction and id-only references are resolved through
  `geom::ReplayIds`, kept per container across undo/redo (docs/03 Decisions). This refines the
  "EntityId values are not stable across undo/redo" entry above: that now only holds for edges and
  faces. Reserved keys cost one small slot each and are never handed out again.
- 2026-10-22 — Forking a definition (copy-on-edit, docs/05 Decisions) clones it with a fresh UUID
  and fresh pids for all its entities (pids are document-unique, §2); ids, order and side rows are
  kept. `Document::locked_step` finds the first locked group or instance on a context path.
- 2026-10-23 — `ComponentDefinition::usage_count` is maintained: recomputed after every command,
  undo and redo (it is derived data, not an undoable change) and by `repair`; `validate` warns when
  a stored count differs (`usage-count`). A group's private definition counts its group.
- 2026-10-23 — The nesting limit of §14 is enforced: a command that would nest deeper than 256
  levels (or create a cycle, §13) fails with `E1201` and changes nothing; `validate` reports a
  too deep chain as `nesting-too-deep`, distinct from `cyclic-nesting`.
- 2026-10-23 — Selections hold each occurrence once, in a canonical key form (pid when the entity
  has one, else id, for every step and the target): `#Model/FaceId…` and `@pid` naming the same
  face are one item for `add`, `remove` and `toggle` (§11).
- 2026-10-23 — Camera rays (§8): a view along the up vector (top/bottom view with up = +Z) takes
  its screen axes from the world axis least aligned with the view (a top view has +X right and +Y
  up, like a plan); two-point perspective uses a vertical image plane with the vertical shear that
  keeps the target centered, so vertical lines stay vertical.
- 2026-10-25 — **More validation, transactional repair** (§13). `validate` also reports
  inconsistent tolerances, references to definitions, materials, tags or images that do not exist,
  a definition order that misses or repeats a definition, and images used by watermarks and
  thumbnails count as used. `repair` runs through the command transaction like any edit, so pids
  and side tables follow the merged vertices and removed faces; dangling references fall back to
  the default material, untagged, removal of groups and instances of a missing definition, or
  dropping the texture.
- 2026-10-25 — `next_pid` is capped at 2⁶² (a file storing more is refused when read), so
  allocation can never wrap; a stored 0 reads as 1.
- 2026-10-26 — **Gluing (0.4).** A glued instance (`glued_to` set by `component-insert` with
  `on_face`) follows its face: when Move, Rotate or Scale moves every vertex of the face, the
  instance gets the same transform; when Push/Pull moves the face (without keeping the start
  face), the instance moves with the cap by the distance along the normal and is glued to the
  cap. Moving the instance alone does not move the face. `cuts_opening` is a display effect: the
  face shows a hole shaped like the component's opening outline (docs/06 Decisions); the face
  geometry is unchanged, so picking and exports still see the whole face.
- 2026-10-26 — **Persistent anchors.** `Anchor::kind` is `Start | End | Mid | Point(p)`: an end
  or the middle of the edge `Anchor::path` names by pid (mesh ids do not survive saving), or a
  fixed point. `path` is relative to the container holding the dimension or text (its steps
  start there), so annotations inside a component travel with it; copying a definition renames
  the pids its anchors use. A vertex is anchored as an end of its incident edge with the lowest
  pid. Linear dimensions gain `direction: Option<Vec3>` (the measuring axis; `None` = aligned).
  Moving or scaling a dimension or text moves its fixed points and offsets; edge anchors follow
  their edges. `ModelInfo::axes` holds the model's drawing axes (identity by default, not
  written then).
