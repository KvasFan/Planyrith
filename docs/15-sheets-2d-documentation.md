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

- 2026-10-20 — Sheets are stored inside the document, not as a separate file type.
- 2026-10-20 — Vector HLR is implemented in software for determinism and headless use.
