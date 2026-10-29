//! Implements docs/15 §§2–3, 8: live view projection, paper layout, query
//! tables and dimensions that resolve persistent model anchors on read.

use super::*;
use crate::commands::{CommandRegistry, Engine};
use crate::document::{
    ContainerRef, Document, EditContext, FrontOrBack, Projection, Query, QueryEnv, Selection,
};
use crate::io::export::{ExportScope, view2d};
use glam::{DVec2, DVec3};
use std::collections::BTreeSet;
use std::sync::Arc;

/// A drawing cannot be laid out without losing meaning or precision.
#[derive(Debug, thiserror::Error)]
pub enum SheetError {
    /// Invalid sheet data or unresolved reference.
    #[error("{0}")]
    Invalid(String),
    /// A bounded operation would exceed the sheet resource budget.
    #[error("{0}")]
    Limit(String),
}
/// Layout result.
pub type Result<T> = std::result::Result<T, SheetError>;
/// An item-level paper graphic, consumed identically by the GUI and exporters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Graphic {
    /// Open or closed path with paper-space pen and optional fill.
    Path {
        /// Points, mm.
        points: Vec<[f64; 2]>,
        /// Closed path.
        closed: bool,
        /// Pen.
        stroke: Stroke,
        /// Fill.
        fill: Option<Color>,
        /// Original model tag, for SVG/DXF grouping and pen tables.
        tag: Option<String>,
    },
    /// One Unicode text line with its baseline on paper.
    Text {
        /// Text.
        text: String,
        /// Baseline, mm.
        at: [f64; 2],
        /// Em height, mm.
        height: f64,
        /// Color.
        color: Color,
    },
    /// An embedded image, as encoded PNG/JPEG/WebP bytes.
    Image {
        /// Encoded pixels.
        bytes: Vec<u8>,
        /// Rectangle, mm.
        frame: Rect,
    },
}
/// Graphics in one layer and clipping boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    /// Paper layer name.
    pub layer: String,
    /// Item identity.
    pub item: ItemId,
    /// Convex clipping polygon, empty for no clip.
    pub clip: Vec<[f64; 2]>,
    /// Graphics in paint order.
    pub graphics: Vec<Graphic>,
}
/// A fully resolved printable page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drawing {
    /// Print label.
    pub name: String,
    /// Paper dimensions.
    pub paper: Paper,
    /// Groups in layer and item stacking order.
    pub groups: Vec<Group>,
    /// Detached anchors, missing bindings or missing images.
    pub warnings: Vec<String>,
}
fn invalid(message: impl Into<String>) -> SheetError {
    SheetError::Invalid(message.into())
}
fn finite(p: [f64; 2]) -> bool {
    p.iter().all(|x| x.is_finite() && x.abs() <= 1e9)
}
/// Validates a sheet set independently of a model, including hard resource limits.
pub fn validate_set(set: &SheetSet) -> Result<()> {
    if set.pages.len() > 4096 {
        return Err(SheetError::Limit("at most 4096 sheet pages".into()));
    }
    if !set.paper_defaults.valid() {
        return Err(invalid("invalid default paper"));
    }
    if !set.styles.text_height.is_finite()
        || set.styles.text_height <= 0.0
        || set.styles.text_height > 1000.0
    {
        return Err(invalid("invalid sheet text height"));
    }
    for pen in std::iter::once(&set.styles.edge)
        .chain(std::iter::once(&set.styles.profile))
        .chain(set.styles.pens.values())
    {
        if !pen.width.is_finite()
            || pen.width <= 0.0
            || pen.width > 10.0
            || pen.dash.len() > 64
            || pen.dash.iter().any(|v| !v.is_finite() || *v <= 0.0)
        {
            return Err(invalid("invalid pen table"));
        }
    }
    let mut names = BTreeSet::new();
    for page in &set.pages {
        if page.name.trim().is_empty() || !names.insert(&page.name) || !page.paper.valid() {
            return Err(invalid(
                "page names must be unique and paper dimensions valid",
            ));
        }
        if page.items.len() > 100_000 || page.layers.len() > 1024 {
            return Err(SheetError::Limit("sheet item/layer limit".into()));
        }
        let mut layers = BTreeSet::new();
        for layer in &page.layers {
            if layer.name.is_empty() || !layers.insert(&layer.name) {
                return Err(invalid("layer names must be unique and nonempty"));
            }
        }
        let mut ids = BTreeSet::new();
        for entry in &page.items {
            if entry.id.as_str().is_empty()
                || !ids.insert(&entry.id)
                || !layers.contains(&entry.layer)
            {
                return Err(invalid(
                    "item ids must be unique, nonempty, and assigned to an existing layer",
                ));
            }
            validate_item(&entry.item)?;
        }
    }
    Ok(())
}
/// Validates graphic values without changing the document.
pub fn validate_item(item: &SheetItem) -> Result<()> {
    if item
        .frame()
        .is_some_and(|r| !r.valid() || r.width > 5000.0 || r.height > 5000.0)
    {
        return Err(invalid(
            "item frame must be finite and have positive size, at most 5000 mm",
        ));
    }
    match item {
        SheetItem::ModelView {
            camera,
            scale,
            clip,
            ..
        } => {
            if scale.numerator == 0 || scale.denominator == 0 {
                return Err(invalid("scale must be a positive integer ratio"));
            }
            if !camera.eye.is_finite()
                || !camera.target.is_finite()
                || !camera.up.is_finite()
                || camera.eye.abs().max_element() > 1e12
                || camera.target.abs().max_element() > 1e12
                || camera.up.abs().max_element() > 1e12
                || (camera.eye - camera.target).length_squared() < 1e-12
                || camera.up.length_squared() < 1e-12
            {
                return Err(invalid("invalid view camera"));
            }
            if !matches!(camera.projection, Projection::Parallel { .. }) {
                return Err(invalid("scaled sheet views require parallel projection"));
            }
            if !clip.is_empty() {
                validate_convex(clip)?;
            }
        }
        SheetItem::Dimension {
            start,
            end,
            offset,
            frozen,
            ..
        } => {
            if let Some(snapshot) = frozen
                && (!snapshot.points.iter().copied().all(finite)
                    || !snapshot.value_mm.is_finite()
                    || snapshot.value_mm < 0.0
                    || snapshot.text.len() > 1024 * 1024)
            {
                return Err(invalid("invalid frozen dimension"));
            }
            for a in [start, end] {
                match a {
                    SheetAnchor::Paper { point } if !finite(*point) => {
                        return Err(invalid("invalid paper anchor"));
                    }
                    SheetAnchor::Model { anchor } => {
                        if let crate::document::AnchorKind::Point(p) = anchor.kind
                            && !p.is_finite()
                        {
                            return Err(invalid("invalid model point"));
                        }
                    }
                    _ => {}
                }
            }
            if !finite(*offset) {
                return Err(invalid("invalid dimension offset"));
            }
        }
        SheetItem::Text { runs, leader, .. } => {
            if runs.len() > 4096
                || runs.iter().any(|r| {
                    !r.height.is_finite()
                        || r.height <= 0.0
                        || r.height > 1000.0
                        || r.text.len() > 1024 * 1024
                })
            {
                return Err(invalid("invalid text run or text limit"));
            }
            if leader.is_some_and(|l| !l.into_iter().all(finite)) {
                return Err(invalid("invalid leader"));
            }
        }
        SheetItem::Shape { points, stroke, .. } => {
            if points.len() < 2 || points.len() > 65536 || !points.iter().copied().all(finite) {
                return Err(invalid("a shape needs 2–65536 finite points"));
            }
            if !stroke.width.is_finite()
                || stroke.width <= 0.0
                || stroke.dash.len() > 64
                || stroke.dash.iter().any(|x| !x.is_finite() || *x <= 0.0)
            {
                return Err(invalid("invalid pen width or dash pattern"));
            }
        }
        SheetItem::Table {
            rows,
            columns,
            query,
            ..
        } => {
            if rows.len() > 10000
                || columns.len() > 256
                || rows
                    .iter()
                    .any(|r| r.len() > 256 || r.iter().any(|s| s.len() > 65536))
            {
                return Err(SheetError::Limit("table limit".into()));
            }
            if let Some(q) = query
                && q != "component-report"
            {
                Query(q.clone())
                    .parse()
                    .map_err(|e| invalid(e.to_string()))?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn validate_convex(points: &[[f64; 2]]) -> Result<()> {
    if points.len() < 3 || points.len() > 4096 || !points.iter().copied().all(finite) {
        return Err(invalid("clip must be a finite convex polygon"));
    }
    let mut sign = 0.0f64;
    for i in 0..points.len() {
        let (a, b, c) = (
            DVec2::from(points[i]),
            DVec2::from(points[(i + 1) % points.len()]),
            DVec2::from(points[(i + 2) % points.len()]),
        );
        let cross = (b - a).perp_dot(c - b);
        if cross.abs() < 1e-10 {
            continue;
        }
        if sign.abs() > 0.0 && sign * cross < 0.0 {
            return Err(invalid("clip must be convex"));
        }
        sign = cross;
    }
    if sign.abs() < 1e-10 {
        return Err(invalid("degenerate clip polygon"));
    }
    Ok(())
}
/// The effective model and camera of a linked view. No source document is mutated.
pub fn view_document(doc: &Document, item: &SheetItem) -> Result<(Document, Camera)> {
    let SheetItem::ModelView {
        scene,
        camera,
        camera_override,
        scale,
        frame,
        style,
        tags_override,
        section_override,
        ..
    } = item
    else {
        return Err(invalid("item is not a model view"));
    };
    let mut engine = Engine::new(doc.clone(), Arc::new(CommandRegistry::with_builtins()));
    if let Some(scene) = scene {
        engine
            .execute_erased("scene-activate", serde_json::json!({"scene":scene}))
            .map_err(|e| invalid(e.to_string()))?;
    }
    if let Some(style) = style {
        engine
            .execute_erased("style-activate", serde_json::json!({"style":style}))
            .map_err(|e| invalid(e.to_string()))?;
    }
    for (tag, visible) in tags_override {
        engine
            .execute_erased(
                "tag-set-visible",
                serde_json::json!({"tag":tag,"visible":visible}),
            )
            .map_err(|e| invalid(e.to_string()))?;
    }
    for (pid, active) in section_override {
        let pid = crate::ids::Pid(
            pid.parse()
                .map_err(|_| invalid("section override key must be a persistent id"))?,
        );
        let (container, target) = engine
            .document()
            .locate_pid(pid)
            .ok_or_else(|| invalid(format!("unknown section {pid}")))?;
        if !matches!(target, crate::ids::EntityId::SectionPlane(_)) {
            return Err(invalid("section override must reference a section plane"));
        }
        let metadata = crate::commands::CommandMeta {
            name: "sheet-section-display".into(),
            category: crate::commands::CommandCategory::View,
            label: "Sheet section display".into(),
            view_only: true,
            steps: Vec::new(),
        };
        engine
            .run(&metadata, |tx| {
                let sections = tx
                    .document()
                    .container(container)
                    .ok_or_else(|| {
                        crate::commands::CommandError::Internal("section container missing".into())
                    })?
                    .section_planes()
                    .map(|(id, plane)| (id, plane.clone()))
                    .collect::<Vec<_>>();
                for (id, mut plane) in sections {
                    let id = crate::ids::EntityId::SectionPlane(id);
                    if id == target {
                        plane.active = *active;
                    } else if *active {
                        plane.active = false;
                    }
                    tx.replace_entity(
                        container,
                        id,
                        crate::document::EntityRecord::SectionPlane(plane),
                    )?;
                }
                Ok(())
            })
            .map_err(|e| invalid(e.to_string()))?;
    }
    let model = engine.document().clone();
    let mut camera = if scene.is_some() && !*camera_override {
        model.camera
    } else {
        *camera
    };
    camera.aspect = frame.width / frame.height;
    camera.projection = Projection::Parallel {
        height_mm: frame.height / scale.factor(),
    };
    Ok((model, camera))
}
/// Projects a world point into a sheet viewport, preserving its exact scale.
pub fn project(camera: &Camera, frame: Rect, scale: Scale, p: DVec3) -> [f64; 2] {
    let right = camera.right();
    let up = right.cross(camera.forward()).normalize_or(camera.up);
    let p = p - camera.target;
    [
        frame.x + frame.width / 2.0 + p.dot(right) * scale.factor(),
        frame.y + frame.height / 2.0 + p.dot(up) * scale.factor(),
    ]
}
/// A rectangle as a counterclockwise polygon.
pub fn rectangle(r: Rect) -> Vec<[f64; 2]> {
    vec![
        [r.x, r.y],
        [r.x + r.width, r.y],
        [r.x + r.width, r.y + r.height],
        [r.x, r.y + r.height],
    ]
}
/// Clips a line segment against a convex polygon; used by all exporters.
pub fn clip_segment(a: [f64; 2], b: [f64; 2], polygon: &[[f64; 2]]) -> Option<[[f64; 2]; 2]> {
    if polygon.is_empty() {
        return Some([a, b]);
    }
    let area: f64 = polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .take(polygon.len())
        .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
        .sum();
    let sign = area.signum();
    let mut a = DVec2::from(a);
    let mut b = DVec2::from(b);
    for (p, q) in polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .take(polygon.len())
    {
        let p = DVec2::from(*p);
        let edge = DVec2::from(*q) - p;
        let (da, db) = (edge.perp_dot(a - p) * sign, edge.perp_dot(b - p) * sign);
        if da < 0.0 && db < 0.0 {
            return None;
        }
        if da < 0.0 {
            a = a + (b - a) * (da / (da - db));
        } else if db < 0.0 {
            b = a + (b - a) * (da / (da - db));
        }
    }
    Some([a.to_array(), b.to_array()])
}
/// Clips a filled polygon to a convex clipping boundary.
pub fn clip_polygon(points: &[[f64; 2]], boundary: &[[f64; 2]]) -> Vec<[f64; 2]> {
    if boundary.is_empty() {
        return points.to_vec();
    }
    let sign = boundary
        .iter()
        .zip(boundary.iter().cycle().skip(1))
        .take(boundary.len())
        .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
        .sum::<f64>()
        .signum();
    let mut polygon = points.to_vec();
    for (a, b) in boundary
        .iter()
        .zip(boundary.iter().cycle().skip(1))
        .take(boundary.len())
    {
        let a = DVec2::from(*a);
        let edge = DVec2::from(*b) - a;
        let mut result = Vec::new();
        for (p, q) in polygon
            .iter()
            .zip(polygon.iter().cycle().skip(1))
            .take(polygon.len())
        {
            let p = DVec2::from(*p);
            let q = DVec2::from(*q);
            let (dp, dq) = (edge.perp_dot(p - a) * sign, edge.perp_dot(q - a) * sign);
            if dp >= 0.0 {
                result.push(p.to_array());
            }
            if (dp > 0.0 && dq < 0.0) || (dp < 0.0 && dq > 0.0) {
                result.push((p + (q - p) * (dp / (dp - dq))).to_array());
            }
        }
        polygon = result;
    }
    polygon
}
fn path(points: Vec<[f64; 2]>, closed: bool, stroke: Stroke, fill: Option<Color>) -> Graphic {
    Graphic::Path {
        points,
        closed,
        stroke,
        fill,
        tag: None,
    }
}
fn line(a: [f64; 2], b: [f64; 2], stroke: &Stroke) -> Graphic {
    path(vec![a, b], false, stroke.clone(), None)
}
/// Width of a Unicode string in paper mm using the bundled font's advances.
pub fn text_width(text: &str, height: f64) -> f64 {
    let Ok(face) = ttf_parser::Face::parse(crate::fonts::INTER, 0) else {
        return 0.0;
    };
    let units = f64::from(face.units_per_em());
    text.chars()
        .map(|c| {
            face.glyph_index(c)
                .and_then(|g| face.glyph_hor_advance(g))
                .map_or(units / 2.0, f64::from)
        })
        .sum::<f64>()
        * height
        / units
}
fn text_graphics(runs: &[TextRun], frame: Rect, alignment: Alignment) -> Vec<Graphic> {
    let mut lines: Vec<Vec<TextRun>> = vec![Vec::new()];
    let mut width = 0.0;
    for run in runs {
        for c in run.text.chars() {
            let w = text_width(&c.to_string(), run.height);
            if c == '\n' || width + w > frame.width {
                lines.push(Vec::new());
                width = 0.0;
                if c == '\n' {
                    continue;
                }
            }
            if let Some(current) = lines.last_mut() {
                if let Some(last) = current.last_mut()
                    && last.height.to_bits() == run.height.to_bits()
                    && last.color == run.color
                {
                    last.text.push(c);
                } else {
                    current.push(TextRun {
                        text: c.to_string(),
                        height: run.height,
                        color: run.color,
                    });
                }
            }
            width += w;
        }
    }
    let mut y = frame.y + frame.height;
    let mut out = Vec::new();
    for runs in lines {
        let h = runs.iter().map(|r| r.height).fold(0.0, f64::max);
        y -= h * 1.2;
        if y < frame.y {
            break;
        }
        let width = runs
            .iter()
            .map(|r| text_width(&r.text, r.height))
            .sum::<f64>();
        let mut x = frame.x
            + match alignment {
                Alignment::Left => 0.0,
                Alignment::Center => (frame.width - width) / 2.0,
                Alignment::Right => frame.width - width,
            };
        for run in runs {
            let w = text_width(&run.text, run.height);
            out.push(Graphic::Text {
                text: run.text,
                at: [x, y],
                height: run.height,
                color: run.color,
            });
            x += w;
        }
    }
    out
}
fn text(text: String, frame: Rect, height: f64) -> Vec<Graphic> {
    text_graphics(
        &[TextRun {
            text,
            height,
            color: [0, 0, 0, 255],
        }],
        frame,
        Alignment::Left,
    )
}
fn attr_text(value: &crate::document::Value) -> String {
    use crate::document::Value;
    match value {
        Value::Str(x) => x.clone(),
        Value::Bool(x) => x.to_string(),
        Value::Int(x) => x.to_string(),
        Value::Float(x) => x.to_string(),
        Value::Length(x) => x.as_mm().to_string(),
        _ => format!("{value:?}"),
    }
}
/// Live table rows, bounded and ordered by the shared query engine.
pub fn table_rows(
    doc: &Document,
    rows: &[Vec<String>],
    query: Option<&str>,
    columns: &[String],
) -> Result<Vec<Vec<String>>> {
    let Some(query) = query else {
        return Ok(rows.to_vec());
    };
    let mut out = vec![columns.to_vec()];
    if query == "component-report" {
        let scene = crate::io::export::scene::collect(doc, &ExportScope::default());
        let mut counts = BTreeMap::new();
        for occ in &scene.occurrences {
            if let ContainerRef::Definition(id) = occ.container
                && let Some(def) = doc.definition(id)
                && !def.is_internal_group
            {
                *counts.entry(def.name.clone()).or_insert(0usize) += 1;
            }
        }
        return Ok(std::iter::once(vec!["Component".into(), "Count".into()])
            .chain(counts.into_iter().map(|(n, c)| vec![n, c.to_string()]))
            .collect());
    }
    let sel = Selection::new();
    let context = EditContext::model();
    let env = QueryEnv {
        selection: &sel,
        last: &[],
        context: &context,
    };
    let selected = Query(query.into())
        .parse()
        .map_err(|e| invalid(e.to_string()))?
        .eval(doc, &env)
        .map_err(|e| invalid(e.to_string()))?;
    if selected.len() > 10000 {
        return Err(SheetError::Limit("query table exceeds 10000 rows".into()));
    }
    for path in selected.iter() {
        let resolved = doc.resolve(path).map_err(|e| invalid(e.to_string()))?;
        let Some(entities) = doc.container(resolved.container) else {
            continue;
        };
        let measures = crate::document::measure_occurrence(doc, &env, path);
        let mut row = Vec::new();
        for field in columns {
            row.push(match field.as_str() {
                "pid" => resolved.pid.to_string(),
                "kind" => format!("{:?}", resolved.entity.kind()),
                "name" => match entities.view(resolved.entity) {
                    Some(crate::document::EntityView::Group(g)) => g.name.clone(),
                    Some(crate::document::EntityView::Instance(i)) => {
                        i.name.clone().unwrap_or_default()
                    }
                    _ => String::new(),
                },
                "length" => measures
                    .as_ref()
                    .and_then(|m| m.length)
                    .map(|x| format!("{x:.3}"))
                    .unwrap_or_default(),
                "area" => measures
                    .as_ref()
                    .and_then(|m| m.area)
                    .map(|x| format!("{x:.3}"))
                    .unwrap_or_default(),
                "volume" => measures
                    .as_ref()
                    .and_then(|m| m.volume)
                    .map(|x| format!("{x:.3}"))
                    .unwrap_or_default(),
                other => other
                    .strip_prefix("attr.")
                    .and_then(|x| x.rsplit_once('.'))
                    .and_then(|(d, k)| entities.attrs(resolved.entity)?.get(d, k))
                    .map(attr_text)
                    .unwrap_or_default(),
            });
        }
        out.push(row);
    }
    Ok(out)
}
/// A dimension's paper coordinates, model measurement and display text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DimensionLayout {
    /// Measured endpoints on paper.
    pub points: [[f64; 2]; 2],
    /// Measured length in model mm, or paper mm for paper anchors.
    pub value: f64,
    /// Current text.
    pub text: String,
}
/// Resolves a sheet dimension. Detached anchors produce an explicit error.
pub fn dimension(doc: &Document, page: &Page, item: &SheetItem) -> Result<DimensionLayout> {
    let SheetItem::Dimension {
        view,
        start,
        end,
        dimension_kind,
        frozen,
        text,
        ..
    } = item
    else {
        return Err(invalid("not a dimension"));
    };
    let target = match view {
        Some(id) => Some(
            page.items
                .iter()
                .find(|e| e.id == *id)
                .ok_or_else(|| invalid("dimension viewport is missing"))?,
        ),
        None => None,
    };
    if let Some(snapshot) = frozen {
        let Some(Entry {
            item: SheetItem::ModelView { frame, .. },
            ..
        }) = target
        else {
            return Err(invalid("frozen dimension viewport is missing"));
        };
        return Ok(DimensionLayout {
            points: snapshot.points.map(|p| [p[0] + frame.x, p[1] + frame.y]),
            value: snapshot.value_mm,
            text: snapshot.text.clone(),
        });
    }
    let v = target.map(|e| view_document(doc, &e.item)).transpose()?;
    let resolve = |a: &SheetAnchor| -> Result<(DVec3, [f64; 2], bool)> {
        match a {
            SheetAnchor::Paper { point } => {
                Ok((DVec3::new(point[0], point[1], 0.0), *point, false))
            }
            SheetAnchor::Model { anchor } => {
                let p =
                    crate::document::annotations::anchor_position(doc, ContainerRef::Model, anchor)
                        .ok_or_else(|| invalid("detached sheet dimension anchor"))?;
                let Some((_, camera)) = &v else {
                    return Err(invalid("model anchors require a viewport"));
                };
                let Some(Entry {
                    item: SheetItem::ModelView { frame, scale, .. },
                    ..
                }) = target
                else {
                    return Err(invalid("dimension references a non-view item"));
                };
                Ok((p, project(camera, *frame, *scale, p), true))
            }
        }
    };
    let (a, pa, model_a) = resolve(start)?;
    let (b, pb, model_b) = resolve(end)?;
    if model_a != model_b {
        return Err(invalid(
            "dimension endpoints must both be on paper or both in the model",
        ));
    }
    let delta = b - a;
    let value = match dimension_kind {
        DimensionKind::Horizontal | DimensionKind::Vertical => {
            let d = if let Some((_, camera)) = &v {
                if *dimension_kind == DimensionKind::Horizontal {
                    camera.right()
                } else {
                    camera
                        .right()
                        .cross(camera.forward())
                        .normalize_or(camera.up)
                }
            } else if *dimension_kind == DimensionKind::Horizontal {
                DVec3::X
            } else {
                DVec3::Y
            };
            delta.dot(d).abs()
        }
        DimensionKind::Diameter => delta.length() * 2.0,
        _ => delta.length(),
    };
    let prefix = match dimension_kind {
        DimensionKind::Radius => "R",
        DimensionKind::Diameter => "Ø",
        _ => "",
    };
    let measured = if model_a {
        crate::units::format_length(
            crate::units::Length::mm(value),
            doc.info.display_units,
            doc.info.precision,
        )
    } else {
        format!("{value:.2} mm")
    };
    let measured = format!("{prefix}{measured}");
    Ok(DimensionLayout {
        points: [pa, pb],
        value,
        text: text
            .as_ref()
            .map_or_else(|| measured.clone(), |s| s.replace("<>", &measured)),
    })
}
/// Resolves one printable page, including current scene cameras and live bindings.
pub fn layout_page(
    doc: &Document,
    page: &Page,
    dpi: u32,
    mode: Option<RenderMode>,
) -> Result<Drawing> {
    if !(36..=1200).contains(&dpi) {
        return Err(invalid("DPI must be 36–1200"));
    }
    let mut out = Drawing {
        name: page.name.clone(),
        paper: page.paper.clone(),
        groups: Vec::new(),
        warnings: Vec::new(),
    };
    for layer in page.layers.iter().filter(|l| l.visible) {
        for entry in page.items.iter().filter(|e| e.layer == layer.name) {
            let mut group = Group {
                layer: layer.name.clone(),
                item: entry.id.clone(),
                clip: Vec::new(),
                graphics: Vec::new(),
            };
            let h = doc.sheets.styles.text_height;
            let pen = &doc.sheets.styles.edge;
            match &entry.item {
                SheetItem::ModelView {
                    frame,
                    scale,
                    render,
                    frozen,
                    clip,
                    ..
                } => {
                    group.clip = if clip.is_empty() {
                        rectangle(*frame)
                    } else {
                        clip.clone()
                    };
                    if let Some(image) = frozen {
                        let image = doc
                            .images
                            .get(*image)
                            .ok_or_else(|| invalid("frozen viewport image is missing"))?;
                        group.graphics.push(Graphic::Image {
                            bytes: image.bytes.clone(),
                            frame: *frame,
                        });
                    } else {
                        let (model, camera) = view_document(doc, &entry.item)?;
                        let draw = view2d::draw_view(
                            &model,
                            &ExportScope::default(),
                            &camera,
                            view2d::Paper {
                                scale: Some(scale.factor()),
                                width_mm: frame.width,
                            },
                        );
                        let mode = mode.unwrap_or(*render);
                        if mode == RenderMode::Vector {
                            for occurrence in crate::io::render_mesh::draw_list(
                                &model,
                                &Selection::new(),
                                &EditContext::model(),
                                false,
                            ) {
                                if let (Some(cut), Some(entities)) =
                                    (occurrence.section, model.container(occurrence.container))
                                {
                                    let cap = crate::io::render_mesh::section_cap(
                                        entities,
                                        &cut.plane,
                                        cut.fill,
                                        model.tolerances(),
                                    );
                                    for triangle in cap.triangles.as_chunks::<6>().0.iter() {
                                        let points = triangle[..3]
                                            .iter()
                                            .map(|p| {
                                                project(
                                                    &camera,
                                                    *frame,
                                                    *scale,
                                                    occurrence.world.transform_point3(
                                                        glam::Vec3::from_array(*p).as_dvec3(),
                                                    ),
                                                )
                                            })
                                            .collect::<Vec<_>>();
                                        let points = clip_polygon(
                                            &clip_polygon(&points, &rectangle(*frame)),
                                            &group.clip,
                                        );
                                        if points.len() >= 3 {
                                            group.graphics.push(path(
                                                points,
                                                true,
                                                Stroke {
                                                    width: 0.01,
                                                    color: [160, 160, 160, 255],
                                                    dash: Vec::new(),
                                                },
                                                Some([160, 160, 160, 255]),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        if mode != RenderMode::Vector {
                            let image = raster_view(
                                &model,
                                &camera,
                                *frame,
                                *scale,
                                dpi,
                                mode == RenderMode::Raster,
                                &draw,
                            )?;
                            group.graphics.push(Graphic::Image {
                                bytes: image,
                                frame: *frame,
                            });
                        }
                        if mode != RenderMode::Raster {
                            for (i, (a, b, kind)) in draw.lines.iter().enumerate() {
                                let a = [a.x + frame.x, a.y + frame.y];
                                let b = [b.x + frame.x, b.y + frame.y];
                                let tag = draw
                                    .line_tags
                                    .get(i)
                                    .copied()
                                    .flatten()
                                    .and_then(|t| model.tags.get(t));
                                let base = if *kind == view2d::LineKind::Profile {
                                    &doc.sheets.styles.profile
                                } else {
                                    pen
                                };
                                let mut pen = tag
                                    .and_then(|t| doc.sheets.styles.pens.get(&t.name))
                                    .unwrap_or(base)
                                    .clone();
                                if pen.dash.is_empty()
                                    && tag.is_none_or(|t| {
                                        !doc.sheets.styles.pens.contains_key(&t.name)
                                    })
                                    && let Some(t) = tag
                                {
                                    use crate::document::LineStyle;
                                    pen.dash = match t.line_style {
                                        LineStyle::Solid => Vec::new(),
                                        LineStyle::Dashed => vec![4.0, 2.0],
                                        LineStyle::Dotted => vec![0.25, 1.5],
                                        LineStyle::DashDot => vec![4.0, 1.0, 0.25, 1.0],
                                    };
                                }
                                if let Some([a, b]) = clip_segment(a, b, &group.clip) {
                                    let mut graphic = line(a, b, &pen);
                                    if let Graphic::Path { tag: name, .. } = &mut graphic {
                                        *name = tag.map(|t| t.name.clone());
                                    }
                                    group.graphics.push(graphic);
                                }
                            }
                        }
                    }
                }
                SheetItem::Dimension {
                    offset,
                    dimension_kind,
                    ..
                } => match dimension(doc, page, &entry.item) {
                    Ok(d) => {
                        let [a, b] = d.points;
                        let mut ca = [a[0] + offset[0], a[1] + offset[1]];
                        let mut cb = [b[0] + offset[0], b[1] + offset[1]];
                        match dimension_kind {
                            DimensionKind::Horizontal => cb[1] = ca[1],
                            DimensionKind::Vertical => cb[0] = ca[0],
                            DimensionKind::Radius | DimensionKind::Diameter => {
                                ca = a;
                                cb = b;
                            }
                            _ => {}
                        }
                        group.graphics.extend([
                            line(a, ca, pen),
                            line(ca, cb, pen),
                            line(b, cb, pen),
                        ]);
                        let dir = (DVec2::from(cb) - DVec2::from(ca)).normalize_or_zero();
                        for (p, sign) in [(ca, 1.0), (cb, -1.0)] {
                            let p = DVec2::from(p);
                            let tip = p + dir * 2.0 * sign;
                            let side = dir.perp() * 0.7;
                            group.graphics.push(path(
                                vec![
                                    p.to_array(),
                                    (tip + side).to_array(),
                                    (tip - side).to_array(),
                                ],
                                true,
                                pen.clone(),
                                Some([0, 0, 0, 255]),
                            ));
                        }
                        group.graphics.push(Graphic::Text {
                            text: d.text,
                            at: [(ca[0] + cb[0]) / 2.0, (ca[1] + cb[1]) / 2.0 + 1.0],
                            height: h,
                            color: [0, 0, 0, 255],
                        });
                    }
                    Err(e) => {
                        out.warnings.push(e.to_string());
                        group.graphics.extend(text(
                            "Detached dimension".into(),
                            Rect {
                                x: 10.0,
                                y: 10.0,
                                width: 80.0,
                                height: 10.0,
                            },
                            h,
                        ));
                    }
                },
                SheetItem::Text {
                    runs,
                    frame,
                    alignment,
                    leader,
                } => {
                    group.clip = rectangle(*frame);
                    group.graphics = text_graphics(runs, *frame, *alignment);
                    if let Some([a, b]) = leader {
                        group.clip.clear();
                        group.graphics.insert(0, line(*a, *b, pen));
                    }
                }
                SheetItem::Table {
                    frame,
                    rows,
                    query,
                    columns,
                } => {
                    let rows = table_rows(doc, rows, query.as_deref(), columns)?;
                    group.graphics = draw_table(&rows, *frame, h, pen);
                    group.clip = rectangle(*frame);
                }
                SheetItem::Image { image, frame } => {
                    let image = doc
                        .images
                        .get(*image)
                        .ok_or_else(|| invalid("sheet image is missing"))?;
                    group.graphics.push(Graphic::Image {
                        bytes: image.bytes.clone(),
                        frame: *frame,
                    });
                }
                SheetItem::Shape {
                    shape,
                    points,
                    closed,
                    stroke,
                    fill,
                } => {
                    let points = match shape {
                        ShapeKind::Polyline => points.clone(),
                        ShapeKind::Rectangle => {
                            let (a, b) = (points[0], points[1]);
                            rectangle(Rect {
                                x: a[0].min(b[0]),
                                y: a[1].min(b[1]),
                                width: (a[0] - b[0]).abs(),
                                height: (a[1] - b[1]).abs(),
                            })
                        }
                        ShapeKind::Ellipse => {
                            let (a, b) = (DVec2::from(points[0]), DVec2::from(points[1]));
                            let center = (a + b) / 2.0;
                            let radius = (b - a).abs() / 2.0;
                            (0..96)
                                .map(|i| {
                                    let t = f64::from(i) * std::f64::consts::TAU / 96.0;
                                    (center + radius * DVec2::new(libm::cos(t), libm::sin(t)))
                                        .to_array()
                                })
                                .collect()
                        }
                    };
                    group.graphics.push(path(
                        points,
                        *closed || *shape != ShapeKind::Polyline,
                        stroke.clone(),
                        *fill,
                    ));
                }
                SheetItem::TitleBlock { frame, fields } => {
                    let mut fields = fields.clone();
                    fields
                        .entry("Sheet".into())
                        .or_insert_with(|| page.name.clone());
                    fields
                        .entry("Project".into())
                        .or_insert_with(|| doc.info.title.clone());
                    let rows = fields
                        .into_iter()
                        .map(|(k, v)| vec![k, v])
                        .collect::<Vec<_>>();
                    group.graphics = draw_table(&rows, *frame, h, pen);
                }
                SheetItem::Label {
                    frame,
                    binding,
                    prefix,
                } => {
                    let value = match binding.as_str() {
                        "title" => Some(doc.info.title.clone()),
                        "author" => Some(doc.info.author.clone()),
                        b if b.starts_with("scene:") => page
                            .items
                            .iter()
                            .find(|e| e.id.as_str() == &b[6..])
                            .and_then(|e| {
                                if let SheetItem::ModelView { scene, .. } = &e.item {
                                    scene.clone()
                                } else {
                                    None
                                }
                            }),
                        b => b
                            .strip_prefix("attr.")
                            .and_then(|b| b.rsplit_once('.'))
                            .and_then(|(d, k)| {
                                doc.attrs.get(d, k).or_else(|| doc.info.attrs.get(d, k))
                            })
                            .map(attr_text),
                    };
                    if value.is_none() {
                        out.warnings
                            .push(format!("unresolved label binding {binding}"));
                    }
                    group.graphics =
                        text(format!("{prefix}{}", value.unwrap_or_default()), *frame, h);
                }
            }
            out.groups.push(group);
        }
    }
    Ok(out)
}
fn draw_table(rows: &[Vec<String>], frame: Rect, height: f64, pen: &Stroke) -> Vec<Graphic> {
    let mut out = Vec::new();
    let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let n = rows.len().max(1);
    let cw = frame.width / cols as f64;
    let rh = frame.height / n as f64;
    out.push(path(rectangle(frame), true, pen.clone(), None));
    for r in 1..n {
        let y = frame.y + rh * r as f64;
        out.push(line([frame.x, y], [frame.x + frame.width, y], pen));
    }
    for c in 1..cols {
        let x = frame.x + cw * c as f64;
        out.push(line([x, frame.y], [x, frame.y + frame.height], pen));
    }
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            out.extend(text(
                cell.clone(),
                Rect {
                    x: frame.x + cw * c as f64 + 1.0,
                    y: frame.y + frame.height - rh * (r + 1) as f64,
                    width: (cw - 2.0).max(0.1),
                    height: rh,
                },
                height.min(rh / 1.5),
            ));
        }
    }
    out
}

/// A deterministic raster view with bounded memory, used by raster/hybrid exports.
pub fn raster_view(
    doc: &Document,
    camera: &Camera,
    frame: Rect,
    scale: Scale,
    dpi: u32,
    edges: bool,
    drawing: &view2d::Drawing,
) -> Result<Vec<u8>> {
    let width = libm::ceil(frame.width * f64::from(dpi) / 25.4).max(1.0) as u32;
    let height = libm::ceil(frame.height * f64::from(dpi) / 25.4).max(1.0) as u32;
    if width > 8192 || height > 8192 || u64::from(width) * u64::from(height) > 16_777_216 {
        return Err(SheetError::Limit(
            "raster viewport exceeds 8192 px or 16 megapixels; reduce DPI or frame size".into(),
        ));
    }
    let mut image = image::RgbImage::from_pixel(width, height, image::Rgb([255, 255, 255]));
    let mut depth = vec![f64::INFINITY; (width * height) as usize];
    let scene = crate::io::export::scene::collect(doc, &ExportScope::default());
    let map = |p: DVec3| {
        let p = project(
            camera,
            Rect {
                x: 0.0,
                y: 0.0,
                ..frame
            },
            scale,
            p,
        );
        DVec2::new(
            p[0] * f64::from(width) / frame.width,
            f64::from(height) - p[1] * f64::from(height) / frame.height,
        )
    };
    let mut texture_cache: Option<(ImageRef, image::RgbaImage)> = None;
    for occ in &scene.occurrences {
        let Some(mesh) = scene.mesh(occ.container) else {
            continue;
        };
        let cut = crate::io::render_mesh::occurrence_section(doc, &occ.path, &occ.world);
        for face in &mesh.faces {
            let material = doc.materials.get(
                doc.materials
                    .resolve(face.material.or(occ.material_override), FrontOrBack::Front),
            );
            let color = material.map_or([190, 195, 204], |m| [m.color.r, m.color.g, m.color.b]);
            let normal = occ.world.matrix3.inverse().transpose() * face.normal;
            let light = 0.35
                + 0.65
                    * normal
                        .normalize_or_zero()
                        .dot(DVec3::new(0.4, -0.5, 0.8).normalize())
                        .abs();
            let textured = matches!(
                doc.styles
                    .active_style()
                    .map_or(crate::document::FaceMode::Shaded, |s| s.faces.mode),
                crate::document::FaceMode::ShadedTextured
            );
            let texture = if textured {
                material.and_then(|m| m.texture.as_ref())
            } else {
                None
            };
            if let Some(texture) = texture
                && texture_cache
                    .as_ref()
                    .is_none_or(|(id, _)| *id != texture.image)
            {
                let source = doc
                    .images
                    .get(texture.image)
                    .ok_or_else(|| invalid("model texture image is missing"))?;
                if u64::from(source.width) * u64::from(source.height) > 16_777_216 {
                    return Err(SheetError::Limit(
                        "model texture exceeds 16 megapixels".into(),
                    ));
                }
                texture_cache = Some((
                    texture.image,
                    image::load_from_memory(&source.bytes)
                        .map_err(|e| invalid(e.to_string()))?
                        .to_rgba8(),
                ));
            }
            let pixels = texture.and_then(|t| {
                texture_cache
                    .as_ref()
                    .filter(|(id, _)| *id == t.image)
                    .map(|(_, image)| image)
            });
            let opacity = material.map_or(1.0, |m| f64::from(m.opacity));
            let mode = doc
                .styles
                .active_style()
                .map_or(crate::document::FaceMode::Shaded, |s| s.faces.mode);
            if matches!(
                mode,
                crate::document::FaceMode::Wireframe | crate::document::FaceMode::HiddenLine
            ) {
                continue;
            }
            for t in &face.triangles {
                let original = t.map(|i| face.positions[i as usize]);
                let original_uv = t.map(|i| face.uvs.get(i as usize).copied().unwrap_or_default());
                let poly = cut.map_or_else(
                    || original.to_vec(),
                    |cut| view2d::clip_section_polygon(&original, cut.plane),
                );
                for i in 1..poly.len().saturating_sub(1) {
                    let local = [poly[0], poly[i], poly[i + 1]];
                    let uv = local.map(|p| interpolated_uv(original, original_uv, p));
                    let p = local.map(|p| occ.world.transform_point3(p));
                    let screen = p.map(map);
                    let z = p.map(|p| (p - camera.eye).dot(camera.forward()));
                    let paint = RasterPaint {
                        color,
                        texture: pixels,
                        uv,
                        light,
                        opacity,
                    };
                    raster_triangle(&mut image, &mut depth, screen, z, paint);
                }
            }
        }
        if let (Some(cut), Some(entities)) = (cut, doc.container(occ.container)) {
            let cap = crate::io::render_mesh::section_cap(
                entities,
                &cut.plane,
                cut.fill,
                doc.tolerances(),
            );
            for tri in cap.triangles.as_chunks::<3>().0.iter() {
                let p = [tri[0], tri[1], tri[2]].map(|p| {
                    occ.world
                        .transform_point3(glam::Vec3::from_array(p).as_dvec3())
                });
                raster_triangle(
                    &mut image,
                    &mut depth,
                    p.map(map),
                    p.map(|p| (p - camera.eye).dot(camera.forward())),
                    RasterPaint::solid([100, 100, 100]),
                );
            }
        }
    }
    if edges {
        for (a, b, _) in &drawing.lines {
            let a = DVec2::new(
                a.x / frame.width * f64::from(width),
                (1.0 - a.y / frame.height) * f64::from(height),
            );
            let b = DVec2::new(
                b.x / frame.width * f64::from(width),
                (1.0 - b.y / frame.height) * f64::from(height),
            );
            let n = libm::ceil((b - a).abs().max_element()) as u32;
            for i in 0..=n.min(16384) {
                let p = a + (b - a) * (f64::from(i) / f64::from(n.max(1)));
                if p.x >= 0.0 && p.y >= 0.0 && p.x < f64::from(width) && p.y < f64::from(height) {
                    image.put_pixel(p.x as u32, p.y as u32, image::Rgb([0, 0, 0]));
                }
            }
        }
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|e| invalid(e.to_string()))?;
    Ok(bytes.into_inner())
}
struct RasterPaint<'a> {
    color: [u8; 3],
    texture: Option<&'a image::RgbaImage>,
    uv: [DVec2; 3],
    light: f64,
    opacity: f64,
}
impl RasterPaint<'_> {
    fn solid(color: [u8; 3]) -> Self {
        Self {
            color,
            texture: None,
            uv: [DVec2::ZERO; 3],
            light: 1.0,
            opacity: 1.0,
        }
    }
}
fn interpolated_uv(p: [DVec3; 3], uv: [DVec2; 3], q: DVec3) -> DVec2 {
    let (a, b, d) = (p[1] - p[0], p[2] - p[0], q - p[0]);
    let (aa, ab, bb) = (a.dot(a), a.dot(b), b.dot(b));
    let det = aa * bb - ab * ab;
    if det.abs() < 1e-24 {
        return uv[0];
    }
    let u = (d.dot(a) * bb - d.dot(b) * ab) / det;
    let v = (d.dot(b) * aa - d.dot(a) * ab) / det;
    uv[0] + (uv[1] - uv[0]) * u + (uv[2] - uv[0]) * v
}
fn raster_triangle(
    image: &mut image::RgbImage,
    depth: &mut [f64],
    p: [DVec2; 3],
    z: [f64; 3],
    paint: RasterPaint<'_>,
) {
    let area = (p[1] - p[0]).perp_dot(p[2] - p[0]);
    if area.abs() < 1e-10 {
        return;
    }
    let lo = p[0].min(p[1]).min(p[2]).max(DVec2::ZERO);
    let hi = p[0].max(p[1]).max(p[2]).min(DVec2::new(
        f64::from(image.width()) - 1.0,
        f64::from(image.height()) - 1.0,
    ));
    if lo.x > hi.x || lo.y > hi.y {
        return;
    }
    for y in lo.y as u32..=hi.y as u32 {
        for x in lo.x as u32..=hi.x as u32 {
            let q = DVec2::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
            let w = [
                (p[1] - q).perp_dot(p[2] - q) / area,
                (p[2] - q).perp_dot(p[0] - q) / area,
                (p[0] - q).perp_dot(p[1] - q) / area,
            ];
            if w.iter().all(|v| *v >= -1e-10) {
                let d = w[0] * z[0] + w[1] * z[1] + w[2] * z[2];
                let index = (y * image.width() + x) as usize;
                if d < depth[index] {
                    let mut rgba = [paint.color[0], paint.color[1], paint.color[2], 255];
                    if let Some(texture) = paint.texture {
                        let uv = paint.uv[0] * w[0] + paint.uv[1] * w[1] + paint.uv[2] * w[2];
                        let tx = (uv.x.rem_euclid(1.0) * f64::from(texture.width())) as u32;
                        let ty =
                            ((1.0 - uv.y).rem_euclid(1.0) * f64::from(texture.height())) as u32;
                        rgba = texture
                            .get_pixel(tx.min(texture.width() - 1), ty.min(texture.height() - 1))
                            .0;
                    }
                    let alpha = f64::from(rgba[3]) / 255.0 * paint.opacity;
                    if alpha > 0.0 {
                        let background = image.get_pixel(x, y).0;
                        let color = [0, 1, 2].map(|i| {
                            (f64::from(rgba[i]) * paint.light * alpha
                                + f64::from(background[i]) * (1.0 - alpha))
                                .clamp(0.0, 255.0) as u8
                        });
                        depth[index] = d;
                        image.put_pixel(x, y, image::Rgb(color));
                    }
                }
            }
        }
    }
}

/// Paper bounds used consistently for picking, alignment and movement.
pub fn item_bounds(doc: &Document, page: &Page, item: &SheetItem) -> Result<Rect> {
    if let Some(frame) = item.frame() {
        return Ok(frame);
    }
    let points = match item {
        SheetItem::Shape { points, .. } => points.clone(),
        SheetItem::Dimension { offset, .. } => {
            let d = dimension(doc, page, item)?;
            d.points
                .into_iter()
                .flat_map(|p| [p, [p[0] + offset[0], p[1] + offset[1]]])
                .collect()
        }
        _ => Vec::new(),
    };
    if points.is_empty() {
        return Err(invalid("item has no paper bounds"));
    }
    let lo = points
        .iter()
        .fold(DVec2::splat(f64::INFINITY), |a, p| a.min(DVec2::from(*p)));
    let hi = points.iter().fold(DVec2::splat(f64::NEG_INFINITY), |a, p| {
        a.max(DVec2::from(*p))
    });
    Ok(Rect {
        x: lo.x,
        y: lo.y,
        width: (hi.x - lo.x).max(1.0),
        height: (hi.y - lo.y).max(1.0),
    })
}
/// Translates paper graphics. Dimensions retain their anchors and move their line.
pub fn translated(item: &SheetItem, delta: [f64; 2]) -> Result<SheetItem> {
    if !finite(delta) {
        return Err(invalid("invalid sheet translation"));
    }
    let mut item = item.clone();
    let point = |p: &mut [f64; 2]| {
        p[0] += delta[0];
        p[1] += delta[1];
    };
    let frame = |r: &mut Rect| {
        r.x += delta[0];
        r.y += delta[1];
    };
    match &mut item {
        SheetItem::ModelView { frame: r, clip, .. } => {
            frame(r);
            for p in clip {
                point(p);
            }
        }
        SheetItem::Text {
            frame: r, leader, ..
        } => {
            frame(r);
            if let Some(leader) = leader {
                for p in leader {
                    point(p);
                }
            }
        }
        SheetItem::Table { frame: r, .. }
        | SheetItem::Image { frame: r, .. }
        | SheetItem::TitleBlock { frame: r, .. }
        | SheetItem::Label { frame: r, .. } => frame(r),
        SheetItem::Shape { points, .. } => {
            for p in points {
                point(p);
            }
        }
        SheetItem::Dimension { offset, .. } => point(offset),
    }
    validate_item(&item)?;
    Ok(item)
}
/// Picks a persistent model anchor through a sheet viewport, snapping vertices
/// within 1.5 paper mm. A hit elsewhere becomes a fixed model point.
pub fn pick_anchor(doc: &Document, item: &SheetItem, paper: [f64; 2]) -> Result<SheetAnchor> {
    let SheetItem::ModelView { frame, scale, .. } = item else {
        return Err(invalid("anchor picking needs a model view"));
    };
    let (model, camera) = view_document(doc, item)?;
    let scene = crate::io::export::scene::collect(&model, &ExportScope::default());
    let mut best: Option<(f64, f64, DVec3)> = None;
    for occ in &scene.occurrences {
        let Some(entities) = model.container(occ.container) else {
            continue;
        };
        let cut = crate::io::render_mesh::occurrence_section(&model, &occ.path, &occ.world);
        for (_, vertex) in entities.mesh().vertices() {
            let local = vertex.position;
            if cut.is_some_and(|c| c.plane.signed_distance(local) > doc.tolerances().length) {
                continue;
            }
            let world = occ.world.transform_point3(local);
            let p = project(&camera, *frame, *scale, world);
            let distance = (DVec2::from(p) - DVec2::from(paper)).length();
            let depth = (world - camera.eye).dot(camera.forward());
            if distance <= 1.5
                && best.as_ref().is_none_or(|(d, z, _)| {
                    distance < *d - 1e-6 || ((distance - *d).abs() < 1e-6 && depth < *z)
                })
            {
                best = Some((distance, depth, world));
            }
        }
    }
    let position = if let Some((_, _, point)) = best {
        point
    } else {
        let right = camera.right();
        let up = right.cross(camera.forward()).normalize_or(camera.up);
        let p = camera.target
            + right * ((paper[0] - frame.x - frame.width / 2.0) / scale.factor())
            + up * ((paper[1] - frame.y - frame.height / 2.0) / scale.factor());
        let ray = crate::math::Ray::new(p - camera.forward() * camera.distance(), camera.forward());
        let mut best: Option<(f64, DVec3)> = None;
        for occ in &scene.occurrences {
            let Some(entities) = model.container(occ.container) else {
                continue;
            };
            let inverse = occ.world.inverse();
            let local = crate::math::Ray::new(
                inverse.transform_point3(ray.origin),
                inverse.transform_vector3(ray.dir),
            );
            let cut = crate::io::render_mesh::occurrence_section(&model, &occ.path, &occ.world);
            for (face, _) in entities.mesh().faces() {
                if let Ok(Some(hit)) = crate::geom::intersect::ray_face(
                    &local,
                    entities.mesh(),
                    face,
                    doc.tolerances(),
                ) {
                    if cut.is_some_and(|c| {
                        c.plane.signed_distance(hit.point) > doc.tolerances().length
                    }) {
                        continue;
                    }
                    let world = occ.world.transform_point3(hit.point);
                    let t = (world - ray.origin).dot(ray.dir);
                    if best.is_none_or(|(old, _)| t < old) {
                        best = Some((t, world));
                    }
                }
            }
        }
        best.map_or(p, |(_, p)| p)
    };
    Ok(SheetAnchor::Model {
        anchor: crate::document::annotations::find_anchor(doc, ContainerRef::Model, position),
    })
}
