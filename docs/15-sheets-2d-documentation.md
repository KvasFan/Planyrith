# 15 — Sheets: 2D documentation module

**Audience:** implementing agents. Normative for milestone 0.9. Lives in
`planyrith-core::sheets` (data + layout + export) and `planyrith-gui::sheets` (paper-space editor).

## 1. Purpose

Produce construction-style drawings from the 3D model: plans, sections, elevations, details and
schedules laid out on paper-size sheets with scale, dimensions, text, tables and title blocks, and
export them to PDF/SVG/DXF. Sheets live **inside the `.plyr` document** (`sheets` part, see §6) so
a single file carries model and drawings and keeps them in sync.

## 2. Data model

```rust
pub struct SheetSet { pages: Vec<Page>, paper_defaults: Paper, title_block: Option<TitleBlockTemplate>, styles: SheetStyles }
pub struct Page { name: String, paper: Paper { size: A0..A4|Letter|Tabloid|Custom(w_mm,h_mm), orientation, margins }, items: Vec<SheetItem>, layers: Vec<SheetLayer> }
pub enum SheetItem {
    ModelView { scene: Option<SceneRef>, camera: Camera /* parallel for scaled views */, scale: Scale(1, 50), style: StyleRef, render: Vector|Raster|Hybrid, frame: Rect_mm, clip: Option<Polygon>, tags_override, section_override, layer },
    Dimension { .. as model dims but anchored to ModelView entities or paper points .. },
    Text { rich text runs, box, alignment, leader },
    Table { rows/cols with cell text or a bound query (`component-report`, `attribute table`), style },
    Image { path/hash, frame },
    Shape { polyline/rect/ellipse/path, stroke, fill },
    TitleBlock { template fields: project, sheet number, date, author, revision },
    Label { auto text bound to model attribute or scene name }
}
pub struct Scale(u32, u32); // 1:50 → Scale(1,50)
```

- `ModelView` keeps a **link** to a scene: updating the model updates the view (on demand or
  auto). Views can be "frozen" (raster snapshot stored) for archival sheets.
- Dimensions in sheets anchor to model entities **through** the view (3D anchors projected), so
  they re-measure when the model changes; or to paper points.

## 3. Rendering a model view

- **Vector** mode: project edges with hidden-line removal (software; `render::hlr`): compute
  visibility of edge segments against faces using the view's depth; output polylines grouped
  by tag/line style; section cut lines and fills included; profile edges thick.
- **Raster** mode: `planyrith-render` headless at the required DPI (default 300) for shaded/
  textured views, embedded as an image with correct scale.
- **Hybrid**: raster faces + vector edges (best for shaded plans with crisp lines).
- Line weights per tag/style (`Tag.line_style`), pen tables for export.
- Scale is exact: a 1 m edge at 1:50 measures 20 mm on paper and in exported PDF units.

## 4. Editor (GUI)

A `Sheets` workspace tab: page list, paper canvas (egui painter, zoom/pan), item tools (select,
view, dimension, text, table, image, shapes, title block), layers, alignment/snap to paper grid,
properties panel. Model views open the 3D viewport in a "view editing" mode to set camera/scale.
Command names are prefixed `sheet-*` (e.g. `sheet-add-page`, `sheet-add-view`, `sheet-export`).

## 5. CLI

`planyrith sheet list|add-page|add-view|export -f m.plyr --page "A101" --out plan.pdf
[--pages all] [--dpi 300] [--vector|--raster|--hybrid]`. All item operations are commands.

### 5.1 Command surface (0.9)

All commands are category `sheet`, have serde/JSON schemas, and are available as
`planyrith tool sheet-<operation>`, `planyrith sheet <operation>`, RPC
`command.execute`, and generated Rhai `sheet_<operation>()` functions.

| Operation | Principal parameters |
|---|---|
| `settings` | `paper_defaults`, `styles` (edge/profile pens, `pens` by tag, text height), `title_block.fields`, `clear_title_block` |
| `add-page` | unique `page`, optional `paper` A0–A4/Letter/Tabloid/`widthxheight`, `landscape`, `margin` |
| `rename-page`, `move-page`, `remove-page` | `page`; new `name`, zero-based print `index`, or removal |
| `set-paper` | `page`, paper/orientation/margin; existing items keep their paper positions |
| `add-view` | `page`, optional `id`, `layer`, linked `scene`, snapshot `camera`, `camera_override`, `scale` (`1:50`), `style`, `render`, `frame`, convex `clip`, `tags_override`, `section_override` |
| `add-dimension` | `page`, `view` for model anchors, `start`/`end`, optional `model_start`/`model_end` snapped to model edges, `dimension_kind`, paper `offset`, optional `text` (`<>` = measured value) |
| `add-text` | `text` or styled `runs`, frame, height, color, alignment, optional leader |
| `add-table` | literal `rows`, or `query` + `columns`; `component-report` gives component counts |
| `add-image` | stored `path` (PNG/JPEG/WebP), frame |
| `add-shape` | polyline/rectangle/ellipse `shape`, paper `points`, `closed`, `stroke`, `fill` |
| `add-title-block` | frame, explicit fields overlaid on the reusable template |
| `add-label` | frame, `binding` (`title`, `author`, `scene:<view-id>`, `attr.dictionary.key`), prefix |
| `edit-item`, `move-item`, `order-item`, `remove-item` | page, stable item `id`; typed replacement `item`, paper `delta`, stacking `index`, or removal |
| `layer-set` | page, layer name, optional visible/locked flags; creates missing layers |
| `freeze-view` | page, view id, frozen flag, DPI; stores a snapshot or resumes live model updates |
| `list`, `refresh` | optional page; typed page data or current dimension measurements/warnings |
| `export` | destination `path`, optional page or pages (`all`), DPI and render-mode override |

Coordinates and frames are bottom-left paper millimeters. Bare CLI paper lengths
are millimeters even when the model displays meters or feet; explicit unit
suffixes are accepted for semantic lengths (`height`, `margin`). Item identifiers
are page-local strings; unnamed items receive deterministic `item-N` identifiers.
Layer locks reject item edits. Failed commands roll back all changes, including
image loads. Each item edit has its own inverse delta; paper and print-order edits
do not copy unrelated items. Read/list/refresh/export do not add undo entries.

```sh
planyrith -f house.plyr sheet add-page --page A101 --paper A4
planyrith -f house.plyr sheet add-view --page A101 --scene Plan --scale 1:50
planyrith -f house.plyr sheet add-text --page A101 --text 'Ground floor' --height 4mm
planyrith -f house.plyr sheet export --pages all --vector --out house.pdf
```

The `--out` alias belongs to the sheet export destination, leaving the native
model path intact. Multiple SVG/DXF pages use `<stem>-NN-<page>.<ext>` names;
DXF images are PNG side files beside the drawing. Query columns are `pid`, `kind`,
`name`, `length`, `area`, `volume`, or `attr.<dictionary>.<key>`.

## 6. Storage

`sheets.msgpack` part in the `.plyr` container (`docs/09 §1`), frozen rasters under
`sheets/frozen/<hash>.png`. Old readers ignore the part.

## 7. Export

- **PDF**: vector pages (`pdf-writer`), embedded fonts (subset), raster images as JPEG/PNG,
  layers as optional content groups, page labels, metadata.
- **SVG**: one file per page, groups per layer/tag, `mm` user units.
- **DXF**: 2D entities in paper space or model space at scale, layers from tags/sheet layers.

## 8. Tests

- Scale accuracy test: exported PDF of a 1 m cube at 1:20 → path bbox 50 mm (± 0.01 mm).
- HLR correctness goldens on fixtures (house plan/section/elevation).
- Re-measure test: move a wall → sheet dimension updates after `sheet-refresh`.

## Decisions

- 2026-10-29 — **0.9 implementation.** Pages, typed item values, settings and
  stable string item ids live in the document's Sheets set. `sheets.msgpack` is
  optional and omitted for an untouched empty set; old files retain their native
  bytes. Frozen PNG assets use `sheets/frozen/<hash>.png`; live images keep the
  ordinary image store's content-addressed references. Native reads verify
  checksums and validate Sheets, with a 64 MiB cap on its MessagePack part.
- 2026-10-29 — **Headless layout and raster.** The software HLR now clips
  occurrences and occluders against section planes and includes cap outlines.
  Vector sheets include cap fills and tag-aware pens. Raster/hybrid use a bounded,
  deterministic software Z buffer in the core, keeping the same command available
  without a graphics adapter on CLI, RPC and web. It supports shaded material
  colors, textured views with UVs, and section caps; viewport post effects
  (sun-shadow maps, fog and watermarks) are not reproduced in this paper renderer.
  A raster viewport is capped at 8192 pixels per side and 16 megapixels; DPI is
  36–1200, default 300. Higher requests fail explicitly instead of changing scale.
- 2026-10-29 — **PDF and DXF.** PDF uses `pdf-writer` with one mm-to-point
  transform, embedded subset Inter, a Unicode CMap, layers, page labels and
  caller-supplied metadata; no timestamps are read. SVG embeds its font/images.
  DXF is ASCII R2000 in model space at printed millimeter scale, one file per page,
  with sheet/tag layers, SOLID fills, Unicode escapes and PNG image references.
  Paper boundaries and filled paths are clipped before DXF serialization.
- 2026-10-29 — **Frozen annotations.** Freezing a viewport snapshots its linked
  dimensions in the same undo step; later model edits cannot change their values
  or detach them. Moving the viewport moves their projected anchor positions.
  Follow Model restores live anchors. Frozen views must be unfrozen before their
  scale or dimensions change. A deleted live anchor is reported by validation and
  refresh; layout retains an explicit detached-dimension notice.
- 2026-10-29 — **GUI.** The central workspace has Model/Sheets tabs. File › Send
  to Sheets places the current camera. The paper canvas supports zoom, middle-drag
  pan, grid snapping, two-click or dragged frames, polyline completion, item
  movement/stacking, paper alignment, layers and typed properties. Edit Camera in
  Model enters the 3D workspace; Use This View pins the new camera while keeping
  the linked scene’s appearance. Browser image picks upload bytes and sheet exports
  download to the computer. The native editor writes through Storage.
- 2026-10-29 — **Acceptance fixtures.** `tools/generate-sheets.rs` reproduces the
  small house native fixture and plan/section/elevation layout goldens, plus
  PDF/SVG/DXF outputs under `target/tmp/sheets`. Tests independently measure PDF
  stroke operands: a 1 m cube at 1:20 has 50 mm edges (±0.01 mm). Other tests cover
  model dimension re-measurement, native round-trips, byte-exact undo, freeze,
  CLI/RPC/Rhai entry points, and synthetic mouse input on the paper canvas.
- 2026-10-20 — Sheets are stored inside the document, not as a separate file type.
- 2026-10-20 — Vector HLR is implemented in software for determinism and headless use.
