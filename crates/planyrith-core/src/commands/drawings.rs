//! Implements docs/15-sheets-2d-documentation.md §9 (engineering drawings,
//! milestone 1.13): derived views (projected, auxiliary, section, detail),
//! broken views, annotation symbols (geometric tolerances, datums, surface
//! texture, welds), balloons numbered like the bill of materials, and
//! standard sheet formats with border, zones, title block and projection
//! symbol.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::sheets::{EditOutput, entry, err, insert, layer, page, replace, sheet_error};
use crate::commands::transaction::Delta;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ContainerRef, Document};
use crate::sheets::{
    Alignment, Annotation, BreakAxis, Entry, ItemId, Page, Paper, ProjectionMethod, Rect,
    ShapeKind, SheetItem, Side, Stroke, TextRun, ViewBreak, ViewDerivation, layout,
};

fn meta(name: &str) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        category: CommandCategory::Sheet,
        label: name.replace('-', " "),
        view_only: false,
        steps: Vec::new(),
    }
}

fn gap() -> f64 {
    10.0
}

fn break_gap() -> f64 {
    4.0
}

/// The base view's entry, which must be a model view.
fn base_view<'a>(
    doc: &'a Document,
    page_name: &str,
    id: &ItemId,
) -> Result<&'a Entry, CommandError> {
    let e = entry(doc, page_name, id)?;
    if !matches!(e.item, SheetItem::ModelView { .. }) {
        return Err(err("the base must be a model view"));
    }
    Ok(e)
}

/// A new view like `base` (scale, style, display) at `frame`, derived by
/// `derived`, with its camera computed now.
fn derived_view(
    doc: &Document,
    page: &Page,
    base: &SheetItem,
    frame: Rect,
    scale: Option<crate::sheets::Scale>,
    derived: ViewDerivation,
    clip: Vec<[f64; 2]>,
) -> Result<SheetItem, CommandError> {
    let SheetItem::ModelView {
        scene,
        camera,
        scale: base_scale,
        style,
        render,
        tags_override,
        section_override,
        ..
    } = base
    else {
        return Err(err("the base must be a model view"));
    };
    let item = SheetItem::ModelView {
        scene: scene.clone(),
        camera: *camera,
        camera_override: true,
        scale: scale.unwrap_or(*base_scale),
        style: style.clone(),
        render: *render,
        frame,
        clip,
        tags_override: tags_override.clone(),
        section_override: section_override.clone(),
        frozen: None,
        derived: Some(derived),
        breaks: Vec::new(),
    };
    // The camera saved for readers that do not derive it.
    let (resolved, _) = layout::derived_item(doc, page, &item, 0).map_err(sheet_error)?;
    layout::validate_item(&resolved).map_err(sheet_error)?;
    Ok(resolved)
}

/// The paper size, at `scale`, that holds the whole model seen by the
/// derived `item`'s camera around its target: twice the farthest corner,
/// plus a margin.
fn content_size(doc: &Document, page: &Page, item: &SheetItem) -> Result<(f64, f64), CommandError> {
    let (resolved, _) = layout::derived_item(doc, page, item, 0).map_err(sheet_error)?;
    let SheetItem::ModelView { camera, scale, .. } = &resolved else {
        return Err(err("not a model view"));
    };
    let bb = doc.aabb();
    if bb.is_empty() {
        return Ok((20.0, 20.0));
    }
    let right = camera.right();
    let up = right.cross(camera.forward()).normalize_or(camera.up);
    let (mut w, mut h) = (0.0f64, 0.0f64);
    for i in 0..8 {
        let corner = glam::DVec3::new(
            if i & 1 == 0 { bb.min.x } else { bb.max.x },
            if i & 2 == 0 { bb.min.y } else { bb.max.y },
            if i & 4 == 0 { bb.min.z } else { bb.max.z },
        ) - camera.target;
        w = w.max(corner.dot(right).abs());
        h = h.max(corner.dot(up).abs());
    }
    let k = scale.factor();
    Ok((
        (2.0 * w * k + 10.0).max(20.0),
        (2.0 * h * k + 10.0).max(20.0),
    ))
}

/// A derived view placed beside its base, its frame fitted to what it
/// shows: the side shared with the base keeps the base's size so the views
/// line up.
fn fitted_view(
    doc: &Document,
    page: &Page,
    base: &SheetItem,
    side: Side,
    gap: f64,
    derived: ViewDerivation,
) -> Result<SheetItem, CommandError> {
    let base_frame = frame_of(base)?;
    let probe = derived_view(
        doc,
        page,
        base,
        beside(base_frame, side, gap, None),
        None,
        derived.clone(),
        Vec::new(),
    )?;
    let (w, h) = content_size(doc, page, &probe)?;
    let size = match side {
        Side::Left | Side::Right => (w, base_frame.height),
        Side::Above | Side::Below => (base_frame.width, h),
    };
    derived_view(
        doc,
        page,
        base,
        beside(base_frame, side, gap, Some(size)),
        None,
        derived,
        Vec::new(),
    )
}

/// The frame next to `base` on `side`, `gap` apart, of the same size.
fn beside(base: Rect, side: Side, gap: f64, size: Option<(f64, f64)>) -> Rect {
    let (w, h) = size.unwrap_or((base.width, base.height));
    match side {
        Side::Right => Rect {
            x: base.x + base.width + gap,
            y: base.y + (base.height - h) / 2.0,
            width: w,
            height: h,
        },
        Side::Left => Rect {
            x: base.x - gap - w,
            y: base.y + (base.height - h) / 2.0,
            width: w,
            height: h,
        },
        Side::Above => Rect {
            x: base.x + (base.width - w) / 2.0,
            y: base.y + base.height + gap,
            width: w,
            height: h,
        },
        Side::Below => Rect {
            x: base.x + (base.width - w) / 2.0,
            y: base.y - gap - h,
            width: w,
            height: h,
        },
    }
}

fn frame_of(item: &SheetItem) -> Result<Rect, CommandError> {
    item.frame().ok_or_else(|| err("the item has no frame"))
}

/// The page's projection method: its title block's `Projection` field, else
/// first angle.
fn page_projection(page: &Page) -> ProjectionMethod {
    for e in &page.items {
        if let SheetItem::TitleBlock { fields, .. } = &e.item
            && fields
                .get("Projection")
                .is_some_and(|v| v.to_ascii_lowercase().contains("third"))
        {
            return ProjectionMethod::ThirdAngle;
        }
    }
    ProjectionMethod::FirstAngle
}

fn text_item(text: &str, at: [f64; 2], height: f64) -> SheetItem {
    SheetItem::Text {
        runs: vec![TextRun {
            text: text.into(),
            height,
            color: [0, 0, 0, 255],
        }],
        frame: Rect {
            x: at[0],
            y: at[1],
            width: layout::text_width(text, height).max(1.0) + 1.0,
            height: height * 1.6,
        },
        alignment: Alignment::Left,
        leader: None,
        annotation: None,
    }
}

fn shape(points: Vec<[f64; 2]>, closed: bool, stroke: Stroke) -> SheetItem {
    SheetItem::Shape {
        shape: ShapeKind::Polyline,
        points,
        closed,
        stroke,
        fill: None,
    }
}

fn add(
    tx: &mut Transaction<'_>,
    page: &str,
    id: String,
    layer: &str,
    item: SheetItem,
) -> Result<(), CommandError> {
    insert(tx, page, Some(ItemId::named(id)), layer, item).map(|_| ())
}

/// A view id: the one given, or the next free `<prefix>-N`.
fn new_id(page: &Page, given: Option<ItemId>, prefix: &str) -> ItemId {
    given.unwrap_or_else(|| {
        let mut i = 1;
        loop {
            let id = ItemId::named(format!("{prefix}-{i}"));
            if !page.items.iter().any(|e| e.id == id) {
                break id;
            }
            i += 1;
        }
    })
}

// ----- projected and auxiliary views ----------------------------------------------------

/// `sheet-add-projected-view` parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProjectedViewParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// The base view.
    pub base: ItemId,
    /// Where the new view sits.
    pub side: Side,
    /// First or third angle (the page title block's `Projection`, else
    /// first angle).
    #[serde(default)]
    pub projection: Option<ProjectionMethod>,
    /// Space between the views, mm.
    #[serde(default = "gap")]
    pub gap: f64,
    /// Optional item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
}

/// The `sheet-add-projected-view` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct AddProjectedView;

impl Command for AddProjectedView {
    type Params = ProjectedViewParams;
    type Output = EditOutput;

    fn name(&self) -> &'static str {
        "sheet-add-projected-view"
    }

    fn validate(&self, doc: &Document, p: &ProjectedViewParams) -> Result<(), CommandError> {
        base_view(doc, &p.page, &p.base)?;
        if !(p.gap.is_finite() && p.gap >= 0.0) {
            return Err(err("the gap must be zero or more"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ProjectedViewParams,
    ) -> Result<EditOutput, CommandError> {
        let doc = tx.document();
        let pg = page(doc, &p.page)?;
        let base = base_view(doc, &p.page, &p.base)?;
        let projection = p.projection.unwrap_or_else(|| page_projection(pg));
        let item = fitted_view(
            doc,
            pg,
            &base.item,
            p.side,
            p.gap,
            ViewDerivation::Projected {
                base: p.base.clone(),
                side: p.side,
                projection,
            },
        )?;
        let id = new_id(pg, p.id, "view");
        insert(tx, &p.page, Some(id), &p.layer, item)
    }

    fn meta(&self) -> CommandMeta {
        meta("sheet-add-projected-view")
    }
}

/// `sheet-add-auxiliary-view` parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuxiliaryViewParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// The base view.
    pub base: ItemId,
    /// The model direction the view looks along (square to an inclined
    /// face, for instance).
    pub direction: [f64; 3],
    /// Where the view sits.
    #[serde(default = "right")]
    pub side: Side,
    /// Space between the views, mm.
    #[serde(default = "gap")]
    pub gap: f64,
    /// Optional item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
}

fn right() -> Side {
    Side::Right
}

/// The `sheet-add-auxiliary-view` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct AddAuxiliaryView;

impl Command for AddAuxiliaryView {
    type Params = AuxiliaryViewParams;
    type Output = EditOutput;

    fn name(&self) -> &'static str {
        "sheet-add-auxiliary-view"
    }

    fn validate(&self, doc: &Document, p: &AuxiliaryViewParams) -> Result<(), CommandError> {
        base_view(doc, &p.page, &p.base)?;
        let d = glam::DVec3::from_array(p.direction);
        if !d.is_finite() || d.length() < 1e-9 {
            return Err(err("the direction must be non-zero"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: AuxiliaryViewParams,
    ) -> Result<EditOutput, CommandError> {
        let doc = tx.document();
        let pg = page(doc, &p.page)?;
        let base = base_view(doc, &p.page, &p.base)?;
        let item = fitted_view(
            doc,
            pg,
            &base.item,
            p.side,
            p.gap,
            ViewDerivation::Auxiliary {
                base: p.base.clone(),
                direction: p.direction,
            },
        )?;
        let id = new_id(pg, p.id, "view");
        insert(tx, &p.page, Some(id), &p.layer, item)
    }

    fn meta(&self) -> CommandMeta {
        meta("sheet-add-auxiliary-view")
    }
}

// ----- section views --------------------------------------------------------------------

/// `sheet-add-section-view` parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SectionViewParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// The base view.
    pub base: ItemId,
    /// The cutting line across the base view, paper mm.
    pub line: [[f64; 2]; 2],
    /// The section's letter.
    #[serde(default = "letter_a")]
    pub label: String,
    /// Look at the cut from the other side.
    #[serde(default)]
    pub flip: bool,
    /// Where the section view sits.
    #[serde(default = "right")]
    pub side: Side,
    /// Space between the views, mm.
    #[serde(default = "gap")]
    pub gap: f64,
    /// Optional item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
}

fn letter_a() -> String {
    "A".into()
}

/// The `sheet-add-section-view` command: the section view, the cutting line
/// with its arrows and letters on the base view, and the view's title.
#[derive(Debug, Default, Clone, Copy)]
pub struct AddSectionView;

impl Command for AddSectionView {
    type Params = SectionViewParams;
    type Output = EditOutput;

    fn name(&self) -> &'static str {
        "sheet-add-section-view"
    }

    fn validate(&self, doc: &Document, p: &SectionViewParams) -> Result<(), CommandError> {
        base_view(doc, &p.page, &p.base)?;
        let [a, b] = p.line;
        if ![a, b].iter().flatten().all(|v| v.is_finite())
            || libm::hypot(a[0] - b[0], a[1] - b[1]) < 1.0
        {
            return Err(err("the cutting line needs two points at least 1 mm apart"));
        }
        if p.label.trim().is_empty() || p.label.chars().count() > 3 {
            return Err(err("the section's letter is one to three characters"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SectionViewParams,
    ) -> Result<EditOutput, CommandError> {
        let doc = tx.document();
        let pg = page(doc, &p.page)?;
        let base = base_view(doc, &p.page, &p.base)?;
        let item = fitted_view(
            doc,
            pg,
            &base.item,
            p.side,
            p.gap,
            ViewDerivation::Section {
                base: p.base.clone(),
                line: p.line,
                flip: p.flip,
                label: p.label.clone(),
            },
        )?;
        let frame = frame_of(&item)?;
        let h = doc.sheets.styles.text_height;
        let id = new_id(pg, p.id, "section");
        let out = insert(tx, &p.page, Some(id.clone()), &p.layer, item)?;
        // The cutting line: thin chain line with thick ends, arrows square
        // to it toward the viewing direction, and the letters.
        let [a, b] = p.line;
        let len = libm::hypot(b[0] - a[0], b[1] - a[1]);
        let u = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
        let mut n = [-u[1], u[0]];
        if p.flip {
            n = [-n[0], -n[1]];
        }
        let thin = Stroke {
            width: 0.25,
            dash: vec![8.0, 1.5, 1.0, 1.5],
            ..Stroke::default()
        };
        let thick = Stroke {
            width: 0.7,
            ..Stroke::default()
        };
        let name = id.as_str().to_string();
        add(
            tx,
            &p.page,
            format!("{name}-cut"),
            &p.layer,
            shape(vec![a, b], false, thin),
        )?;
        for (k, end, sign) in [("a", a, 1.0), ("b", b, -1.0)] {
            let inner = [end[0] + sign * u[0] * 6.0, end[1] + sign * u[1] * 6.0];
            add(
                tx,
                &p.page,
                format!("{name}-end-{k}"),
                &p.layer,
                shape(vec![end, inner], false, thick.clone()),
            )?;
            let tip = [end[0] + n[0] * 8.0, end[1] + n[1] * 8.0];
            add(
                tx,
                &p.page,
                format!("{name}-arrow-{k}"),
                &p.layer,
                SheetItem::Shape {
                    shape: ShapeKind::Polyline,
                    points: vec![
                        tip,
                        [
                            tip[0] - n[0] * 2.5 + u[0] * 1.0,
                            tip[1] - n[1] * 2.5 + u[1] * 1.0,
                        ],
                        [
                            tip[0] - n[0] * 2.5 - u[0] * 1.0,
                            tip[1] - n[1] * 2.5 - u[1] * 1.0,
                        ],
                    ],
                    closed: true,
                    stroke: Stroke::default(),
                    fill: Some([0, 0, 0, 255]),
                },
            )?;
            add(
                tx,
                &p.page,
                format!("{name}-stem-{k}"),
                &p.layer,
                shape(vec![end, tip], false, Stroke::default()),
            )?;
            add(
                tx,
                &p.page,
                format!("{name}-letter-{k}"),
                &p.layer,
                text_item(
                    &p.label,
                    [tip[0] + n[0] * 2.0 - h * 0.35, tip[1] + n[1] * 2.0],
                    h * 1.4,
                ),
            )?;
        }
        let title = format!("SECTION {0}-{0}", p.label);
        add(
            tx,
            &p.page,
            format!("{name}-title"),
            &p.layer,
            text_item(&title, [frame.x, frame.y - h * 2.0], h * 1.4),
        )?;
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        meta("sheet-add-section-view")
    }
}

// ----- detail views ---------------------------------------------------------------------

/// `sheet-add-detail-view` parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DetailViewParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// The base view.
    pub base: ItemId,
    /// The circle's center on the base view, paper mm.
    pub center: [f64; 2],
    /// Its radius on the base view, paper mm.
    pub radius: f64,
    /// The detail's scale (`2:1`).
    pub scale: String,
    /// The detail's letter.
    #[serde(default = "letter_b")]
    pub label: String,
    /// Where the detail view sits.
    #[serde(default = "right")]
    pub side: Side,
    /// Space between the views, mm.
    #[serde(default = "gap")]
    pub gap: f64,
    /// Optional item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
}

fn letter_b() -> String {
    "B".into()
}

/// The `sheet-add-detail-view` command: a circular enlargement, its circle
/// and letter on the base view, and its title.
#[derive(Debug, Default, Clone, Copy)]
pub struct AddDetailView;

impl Command for AddDetailView {
    type Params = DetailViewParams;
    type Output = EditOutput;

    fn name(&self) -> &'static str {
        "sheet-add-detail-view"
    }

    fn validate(&self, doc: &Document, p: &DetailViewParams) -> Result<(), CommandError> {
        base_view(doc, &p.page, &p.base)?;
        if !(p.radius.is_finite() && p.radius >= 1.0) {
            return Err(err("the radius must be at least 1 mm"));
        }
        crate::sheets::Scale::parse(&p.scale)
            .ok_or_else(|| err("scale must be a positive integer ratio, e.g. 2:1"))?;
        if p.label.trim().is_empty() || p.label.chars().count() > 3 {
            return Err(err("the detail's letter is one to three characters"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: DetailViewParams,
    ) -> Result<EditOutput, CommandError> {
        let doc = tx.document();
        let pg = page(doc, &p.page)?;
        let base = base_view(doc, &p.page, &p.base)?;
        let SheetItem::ModelView {
            scale: base_scale, ..
        } = &base.item
        else {
            return Err(err("the base must be a model view"));
        };
        let scale = crate::sheets::Scale::parse(&p.scale).ok_or_else(|| err("invalid scale"))?;
        let ratio = scale.factor() / base_scale.factor();
        let r = p.radius * ratio;
        let size = 2.0 * r + 4.0;
        let frame = beside(frame_of(&base.item)?, p.side, p.gap, Some((size, size)));
        let center = [frame.x + frame.width / 2.0, frame.y + frame.height / 2.0];
        let clip: Vec<[f64; 2]> = (0..48)
            .map(|i| {
                let t = f64::from(i) * std::f64::consts::TAU / 48.0;
                [center[0] + r * libm::cos(t), center[1] + r * libm::sin(t)]
            })
            .collect();
        let item = derived_view(
            doc,
            pg,
            &base.item,
            frame,
            Some(scale),
            ViewDerivation::Detail {
                base: p.base.clone(),
                center: p.center,
                radius: p.radius,
                label: p.label.clone(),
            },
            clip.clone(),
        )?;
        let h = doc.sheets.styles.text_height;
        let id = new_id(pg, p.id, "detail");
        let out = insert(tx, &p.page, Some(id.clone()), &p.layer, item)?;
        let name = id.as_str().to_string();
        let thin = Stroke {
            width: 0.35,
            ..Stroke::default()
        };
        let mark = |c: [f64; 2], radius: f64| SheetItem::Shape {
            shape: ShapeKind::Ellipse,
            points: vec![
                [c[0] - radius, c[1] - radius],
                [c[0] + radius, c[1] + radius],
            ],
            closed: true,
            stroke: thin.clone(),
            fill: None,
        };
        add(
            tx,
            &p.page,
            format!("{name}-circle"),
            &p.layer,
            mark(p.center, p.radius),
        )?;
        add(
            tx,
            &p.page,
            format!("{name}-letter"),
            &p.layer,
            text_item(
                &p.label,
                [p.center[0] + p.radius * 0.75, p.center[1] + p.radius * 0.75],
                h * 1.4,
            ),
        )?;
        add(
            tx,
            &p.page,
            format!("{name}-outline"),
            &p.layer,
            mark(center, r),
        )?;
        let title = format!("DETAIL {} ({})", p.label, p.scale);
        add(
            tx,
            &p.page,
            format!("{name}-title"),
            &p.layer,
            text_item(&title, [frame.x, frame.y - h * 2.0], h * 1.4),
        )?;
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        meta("sheet-add-detail-view")
    }
}

// ----- broken views ---------------------------------------------------------------------

/// `sheet-add-break` parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddBreakParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// The view to shorten.
    pub view: ItemId,
    /// Which way the view is shortened.
    pub axis: BreakAxis,
    /// The band removed: from this paper coordinate…
    pub from: f64,
    /// …to this one (x for horizontal, y for vertical), mm.
    pub to: f64,
    /// The gap left between the break lines, mm.
    #[serde(default = "break_gap")]
    pub gap: f64,
}

/// The `sheet-add-break` command (a broken view, docs/15 §9.2).
#[derive(Debug, Default, Clone, Copy)]
pub struct AddBreak;

impl Command for AddBreak {
    type Params = AddBreakParams;
    type Output = EditOutput;

    fn name(&self) -> &'static str {
        "sheet-add-break"
    }

    fn validate(&self, doc: &Document, p: &AddBreakParams) -> Result<(), CommandError> {
        base_view(doc, &p.page, &p.view)?;
        if !(p.from.is_finite() && p.to.is_finite() && p.gap.is_finite() && p.gap >= 0.0) {
            return Err(err("the band and the gap must be finite"));
        }
        if (p.to - p.from).abs() <= p.gap {
            return Err(err("the band must be wider than the gap"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: AddBreakParams,
    ) -> Result<EditOutput, CommandError> {
        let mut e = base_view(tx.document(), &p.page, &p.view)?.clone();
        if let SheetItem::ModelView { breaks, frame, .. } = &mut e.item {
            let (lo, hi) = (p.from.min(p.to), p.from.max(p.to));
            let (start, end) = match p.axis {
                BreakAxis::Horizontal => (frame.x, frame.x + frame.width),
                BreakAxis::Vertical => (frame.y, frame.y + frame.height),
            };
            if lo < start || hi > end {
                return Err(err("the band must lie inside the view"));
            }
            breaks.push(ViewBreak {
                axis: p.axis,
                from: lo,
                to: hi,
                gap: p.gap,
            });
        }
        replace(tx, &p.page, &p.view, Some(e))
    }

    fn meta(&self) -> CommandMeta {
        meta("sheet-add-break")
    }
}

// ----- annotations ----------------------------------------------------------------------

/// `sheet-add-annotation` parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddAnnotationParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// The symbol.
    pub annotation: Annotation,
    /// Its lower-left corner, paper mm.
    pub at: [f64; 2],
    /// A leader from the symbol to this paper point.
    #[serde(default)]
    pub leader: Option<[f64; 2]>,
    /// Optional item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
}

fn annotation_item(
    doc: &Document,
    a: Annotation,
    at: [f64; 2],
    leader: Option<[f64; 2]>,
) -> SheetItem {
    let h = doc.sheets.styles.text_height;
    let plain = a.plain();
    SheetItem::Text {
        runs: vec![TextRun {
            text: plain.clone(),
            height: h,
            color: [0, 0, 0, 255],
        }],
        frame: Rect {
            x: at[0],
            y: at[1],
            width: layout::text_width(&plain, h).max(2.0 * h) + h,
            height: 2.0 * h,
        },
        alignment: Alignment::Left,
        leader: leader.map(|to| [at, to]),
        annotation: Some(a),
    }
}

/// The `sheet-add-annotation` command: a geometric tolerance frame, datum,
/// surface texture symbol or weld symbol (docs/15 §9.3).
#[derive(Debug, Default, Clone, Copy)]
pub struct AddAnnotation;

impl Command for AddAnnotation {
    type Params = AddAnnotationParams;
    type Output = EditOutput;

    fn name(&self) -> &'static str {
        "sheet-add-annotation"
    }

    fn validate(&self, doc: &Document, p: &AddAnnotationParams) -> Result<(), CommandError> {
        page(doc, &p.page)?;
        p.annotation.check().map_err(err)?;
        if !p
            .at
            .iter()
            .chain(p.leader.iter().flatten())
            .all(|v| v.is_finite())
        {
            return Err(err("points must be finite"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: AddAnnotationParams,
    ) -> Result<EditOutput, CommandError> {
        let item = annotation_item(tx.document(), p.annotation, p.at, p.leader);
        let id = new_id(page(tx.document(), &p.page)?, p.id, "note");
        insert(tx, &p.page, Some(id), &p.layer, item)
    }

    fn meta(&self) -> CommandMeta {
        meta("sheet-add-annotation")
    }
}

/// `sheet-add-balloon` parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddBalloonParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// The balloon's lower-left corner, paper mm.
    pub at: [f64; 2],
    /// The point on the part the leader touches, paper mm.
    pub leader: [f64; 2],
    /// The component (or group) name: the balloon shows its item number
    /// in the bill of materials.
    #[serde(default)]
    pub component: Option<String>,
    /// The text instead of an item number.
    #[serde(default)]
    pub text: Option<String>,
    /// Optional item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
}

/// The `sheet-add-balloon` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct AddBalloon;

impl Command for AddBalloon {
    type Params = AddBalloonParams;
    type Output = EditOutput;

    fn name(&self) -> &'static str {
        "sheet-add-balloon"
    }

    fn validate(&self, doc: &Document, p: &AddBalloonParams) -> Result<(), CommandError> {
        page(doc, &p.page)?;
        if p.component.is_none() == p.text.is_none() {
            return Err(err("give a component or a text"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: AddBalloonParams,
    ) -> Result<EditOutput, CommandError> {
        let text = match (&p.component, &p.text) {
            (_, Some(t)) => t.clone(),
            (Some(name), None) => {
                crate::commands::assembly::bom_rows(tx.document(), ContainerRef::Model, true)
                    .into_iter()
                    .find(|r| &r.name == name)
                    .map(|r| r.item.to_string())
                    .ok_or_else(|| err(format!("{name} is not in the bill of materials")))?
            }
            (None, None) => return Err(err("give a component or a text")),
        };
        let a = Annotation::Balloon { text };
        a.check().map_err(err)?;
        let item = annotation_item(tx.document(), a, p.at, Some(p.leader));
        let id = new_id(page(tx.document(), &p.page)?, p.id, "balloon");
        insert(tx, &p.page, Some(id), &p.layer, item)
    }

    fn meta(&self) -> CommandMeta {
        meta("sheet-add-balloon")
    }
}

// ----- sheet formats --------------------------------------------------------------------

/// A drawing standard for sheet formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Standard {
    /// ISO 5457 sheets (A0–A4), ISO 7200 title block, first angle.
    Iso,
    /// ANSI/ASME Y14.1 sheets (A–E), third angle.
    Ansi,
}

/// `sheet-format` parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SheetFormatParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// The standard.
    pub standard: Standard,
    /// The size: `A0`–`A4` (ISO) or `A`–`E` (ANSI).
    pub size: String,
    /// Landscape (the usual for all but A4 and ANSI A).
    #[serde(default)]
    pub landscape: Option<bool>,
    /// Title block fields (ISO 7200 names; missing ones stay empty).
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    /// The projection method (the standard's by default).
    #[serde(default)]
    pub projection: Option<ProjectionMethod>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
}

/// The paper of a standard size.
pub fn standard_paper(standard: Standard, size: &str, landscape: Option<bool>) -> Option<Paper> {
    let size = size.trim().to_ascii_uppercase();
    let (w, h, margin) = match standard {
        Standard::Iso => {
            let p = Paper::named(&size, true)?;
            if !matches!(size.as_str(), "A0" | "A1" | "A2" | "A3" | "A4") {
                return None;
            }
            (p.width, p.height, 10.0)
        }
        Standard::Ansi => {
            let (a, b) = match size.as_str() {
                "A" => (279.4, 215.9),
                "B" => (431.8, 279.4),
                "C" => (558.8, 431.8),
                "D" => (863.6, 558.8),
                "E" => (1117.6, 863.6),
                _ => return None,
            };
            (a, b, 12.7)
        }
    };
    let default_landscape = !matches!((standard, size.as_str()), (Standard::Iso, "A4"));
    let landscape = landscape.unwrap_or(default_landscape);
    let (long, short) = (w.max(h), w.min(h));
    Some(Paper {
        width: if landscape { long } else { short },
        height: if landscape { short } else { long },
        margin,
    })
}

/// The ids `sheet-format` owns on a page.
fn format_items(page: &Page) -> Vec<ItemId> {
    page.items
        .iter()
        .filter(|e| e.id.as_str().starts_with("format-"))
        .map(|e| e.id.clone())
        .collect()
}

/// The `sheet-format` command: paper, border, zones, title block and
/// projection symbol of a standard sheet (docs/15 §9.5). Running it again
/// replaces them.
#[derive(Debug, Default, Clone, Copy)]
pub struct SheetFormat;

impl Command for SheetFormat {
    type Params = SheetFormatParams;
    type Output = EditOutput;

    fn name(&self) -> &'static str {
        "sheet-format"
    }

    fn validate(&self, doc: &Document, p: &SheetFormatParams) -> Result<(), CommandError> {
        page(doc, &p.page)?;
        standard_paper(p.standard, &p.size, p.landscape)
            .ok_or_else(|| err("ISO sizes are A0–A4, ANSI sizes A–E"))?;
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SheetFormatParams,
    ) -> Result<EditOutput, CommandError> {
        let paper = standard_paper(p.standard, &p.size, p.landscape)
            .ok_or_else(|| err("ISO sizes are A0–A4, ANSI sizes A–E"))?;
        for id in format_items(page(tx.document(), &p.page)?) {
            replace(tx, &p.page, &id, None)?;
        }
        let old = page(tx.document(), &p.page)?.paper.clone();
        tx.sheet_delta(Delta::SheetPaper {
            page: p.page.clone(),
            from: old,
            to: paper.clone(),
        })?;
        let (w, h) = (paper.width, paper.height);
        // The drawing frame: ISO 5457 leaves 20 mm on the left for filing.
        let (left, other) = match p.standard {
            Standard::Iso => (20.0, 10.0),
            Standard::Ansi => (12.7, 12.7),
        };
        let frame = Rect {
            x: left,
            y: other,
            width: w - left - other,
            height: h - 2.0 * other,
        };
        let thick = Stroke {
            width: 0.7,
            ..Stroke::default()
        };
        let thin = Stroke {
            width: 0.35,
            ..Stroke::default()
        };
        add(
            tx,
            &p.page,
            "format-border".into(),
            &p.layer,
            SheetItem::Shape {
                shape: ShapeKind::Rectangle,
                points: vec![
                    [frame.x, frame.y],
                    [frame.x + frame.width, frame.y + frame.height],
                ],
                closed: true,
                stroke: thick,
                fill: None,
            },
        )?;
        // Zones about 50 mm wide, numbered left to right and lettered top
        // down, written in the margin.
        let th = 2.5;
        let cols = ((frame.width / 50.0).round() as u32).max(2);
        let rows = ((frame.height / 50.0).round() as u32).max(2);
        for i in 0..=cols {
            let x = frame.x + frame.width * f64::from(i) / f64::from(cols);
            for (y0, y1) in [
                (frame.y, frame.y - other * 0.6),
                (frame.y + frame.height, frame.y + frame.height + other * 0.6),
            ] {
                add(
                    tx,
                    &p.page,
                    format!("format-tick-x{i}-{}", if y1 < y0 { "b" } else { "t" }),
                    &p.layer,
                    shape(vec![[x, y0], [x, y1]], false, thin.clone()),
                )?;
            }
            if i < cols {
                let label = (i + 1).to_string();
                let cx =
                    x + frame.width / f64::from(cols) / 2.0 - layout::text_width(&label, th) / 2.0;
                add(
                    tx,
                    &p.page,
                    format!("format-zone-x{}", i + 1),
                    &p.layer,
                    text_item(&label, [cx, frame.y + frame.height + other * 0.15], th),
                )?;
            }
        }
        for j in 0..=rows {
            let y = frame.y + frame.height * f64::from(j) / f64::from(rows);
            add(
                tx,
                &p.page,
                format!("format-tick-y{j}"),
                &p.layer,
                shape(
                    vec![[frame.x, y], [frame.x - left * 0.3, y]],
                    false,
                    thin.clone(),
                ),
            )?;
            if j < rows {
                let letter = char::from(b'A' + (rows - 1 - j).min(25) as u8).to_string();
                let cy = y + frame.height / f64::from(rows) / 2.0 - th / 2.0;
                add(
                    tx,
                    &p.page,
                    format!("format-zone-y{letter}"),
                    &p.layer,
                    text_item(&letter, [frame.x - left * 0.6, cy], th),
                )?;
            }
        }
        // The title block, lower right: 180 × 56 mm (ISO 7200's width).
        let projection = p.projection.unwrap_or(match p.standard {
            Standard::Iso => ProjectionMethod::FirstAngle,
            Standard::Ansi => ProjectionMethod::ThirdAngle,
        });
        let mut fields: BTreeMap<String, String> = [
            "Title",
            "Drawing number",
            "Owner",
            "Created by",
            "Approved by",
            "Document type",
            "Date of issue",
            "Revision",
            "Scale",
        ]
        .into_iter()
        .map(|k| (k.to_string(), String::new()))
        .collect();
        fields.extend(p.fields.clone());
        fields.insert(
            "Projection".into(),
            match projection {
                ProjectionMethod::FirstAngle => "first angle".into(),
                ProjectionMethod::ThirdAngle => "third angle".into(),
            },
        );
        let tw = 180.0f64.min(frame.width);
        let block = Rect {
            x: frame.x + frame.width - tw,
            y: frame.y,
            width: tw,
            height: 56.0f64.min(frame.height / 2.0),
        };
        add(
            tx,
            &p.page,
            "format-title".into(),
            &p.layer,
            SheetItem::TitleBlock {
                frame: block,
                fields,
            },
        )?;
        // The projection symbol: a truncated cone's side and end views,
        // the end view on the right for first angle, on the left for third.
        let sy = block.y + block.height + 4.0;
        let sx = block.x + block.width - 40.0;
        let side_view = |x: f64| {
            vec![
                [x, sy + 2.0],
                [x + 14.0, sy],
                [x + 14.0, sy + 12.0],
                [x, sy + 10.0],
            ]
        };
        let (cone_x, end_x) = match projection {
            ProjectionMethod::FirstAngle => (sx, sx + 26.0),
            ProjectionMethod::ThirdAngle => (sx + 12.0, sx + 3.0),
        };
        add(
            tx,
            &p.page,
            "format-projection-side".into(),
            &p.layer,
            shape(side_view(cone_x), true, thin.clone()),
        )?;
        for (k, r) in [("outer", 6.0), ("inner", 4.0)] {
            add(
                tx,
                &p.page,
                format!("format-projection-{k}"),
                &p.layer,
                SheetItem::Shape {
                    shape: ShapeKind::Ellipse,
                    points: vec![[end_x - r, sy + 6.0 - r], [end_x + r, sy + 6.0 + r]],
                    closed: true,
                    stroke: thin.clone(),
                    fill: None,
                },
            )?;
        }
        Ok(EditOutput {
            page: p.page,
            id: Some(ItemId::named("format-title".to_string())),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("sheet-format")
    }
}

/// Registers the drawing commands.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(AddProjectedView);
    registry.register(AddAuxiliaryView);
    registry.register(AddSectionView);
    registry.register(AddDetailView);
    registry.register(AddBreak);
    registry.register(AddAnnotation);
    registry.register(AddBalloon);
    registry.register(SheetFormat);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::{Camera, Document, Projection};
    use crate::sheets::layout::{Graphic, layout_page};
    use glam::DVec3;
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn engine() -> Engine {
        Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
    }

    fn run(e: &mut Engine, name: &str, p: Value) -> Value {
        e.execute_erased(name, p)
            .unwrap_or_else(|err| panic!("{name}: {err}"))
    }

    /// A 100 × 60 × 40 box at the origin, a page and a front view of it at
    /// 1:2 (looking along +Y, up +Z).
    fn front_view(e: &mut Engine) {
        run(
            e,
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [100, 60, 0] }),
        );
        run(
            e,
            "push-pull",
            json!({ "face": { "Query": "face" }, "distance": 40 }),
        );
        run(
            e,
            "sheet-add-page",
            json!({ "page": "P1", "paper": "a3", "landscape": true }),
        );
        let camera = Camera {
            eye: DVec3::new(50.0, -1000.0, 20.0),
            target: DVec3::new(50.0, 30.0, 20.0),
            up: DVec3::Z,
            projection: Projection::Parallel { height_mm: 200.0 },
            aspect: 1.0,
        };
        run(
            e,
            "sheet-add-view",
            json!({ "page": "P1", "id": "front", "camera": camera, "scale": "1:2",
                    "frame": { "x": 40, "y": 120, "width": 80, "height": 60 } }),
        );
    }

    /// The paper bounds of the paths an item draws.
    fn extent(e: &Engine, id: &str) -> ([f64; 2], [f64; 2]) {
        let page = &e.document().sheets.pages[0];
        let d = layout_page(e.document(), page, 150, None).unwrap();
        let g = d.groups.iter().find(|g| g.item.as_str() == id).unwrap();
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for gr in &g.graphics {
            if let Graphic::Path { points, .. } = gr {
                for p in points {
                    for k in 0..2 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
            }
        }
        (lo, hi)
    }

    fn size(e: &Engine, id: &str) -> [f64; 2] {
        let (lo, hi) = extent(e, id);
        [hi[0] - lo[0], hi[1] - lo[1]]
    }

    #[test]
    fn projected_views_follow_their_base() {
        let mut e = engine();
        front_view(&mut e);
        let s = size(&e, "front");
        assert!(
            (s[0] - 50.0).abs() < 1e-6 && (s[1] - 20.0).abs() < 1e-6,
            "{s:?}"
        );
        run(
            &mut e,
            "sheet-add-projected-view",
            json!({ "page": "P1", "base": "front", "side": "right", "id": "side" }),
        );
        run(
            &mut e,
            "sheet-add-projected-view",
            json!({ "page": "P1", "base": "front", "side": "below", "id": "plan" }),
        );
        // The side view shows the depth across, the plan the width.
        let s = size(&e, "side");
        assert!(
            (s[0] - 30.0).abs() < 1e-6 && (s[1] - 20.0).abs() < 1e-6,
            "{s:?}"
        );
        let s = size(&e, "plan");
        assert!(
            (s[0] - 50.0).abs() < 1e-6 && (s[1] - 30.0).abs() < 1e-6,
            "{s:?}"
        );
        // Views line up with the base.
        let (front_lo, front_hi) = extent(&e, "front");
        let (plan_lo, plan_hi) = extent(&e, "plan");
        assert!((front_lo[0] - plan_lo[0]).abs() < 1e-6 && (front_hi[0] - plan_hi[0]).abs() < 1e-6);
        let (side_lo, _) = extent(&e, "side");
        assert!((side_lo[1] - front_lo[1]).abs() < 1e-6);
        // Turning the base turns the projections at the next layout.
        let mut base = e.document().sheets.pages[0].items[0].clone();
        if let crate::sheets::SheetItem::ModelView { camera, .. } = &mut base.item {
            camera.eye = DVec3::new(-1000.0, 30.0, 20.0);
        }
        run(
            &mut e,
            "sheet-edit-item",
            json!({ "page": "P1", "id": "front", "item": base.item }),
        );
        // The side view now looks along the green axis: wider than the
        // 30 mm depth it showed, up to its frame (fitted when it was made).
        let s = size(&e, "side");
        assert!(s[0] > 35.0, "the side now shows the width: {s:?}");
    }

    #[test]
    fn a_section_cuts_only_its_view_and_a_detail_enlarges() {
        let mut e = engine();
        front_view(&mut e);
        let before = e.document().model.iter().count();
        // A horizontal cut through the middle of the front view.
        run(
            &mut e,
            "sheet-add-section-view",
            json!({ "page": "P1", "base": "front", "line": [[30, 150], [130, 150]], "side": "right" }),
        );
        assert_eq!(
            e.document().model.iter().count(),
            before,
            "the model is not cut"
        );
        let page = &e.document().sheets.pages[0];
        let d = layout_page(e.document(), page, 150, None).unwrap();
        let g = d
            .groups
            .iter()
            .find(|g| g.item.as_str() == "section-1")
            .unwrap();
        // The cut face is hatched with thin 45° lines.
        let hatch = g
            .graphics
            .iter()
            .filter(|gr| {
                matches!(gr, Graphic::Path { points, stroke, fill: None, .. }
                    if points.len() == 2 && (stroke.width - 0.18).abs() < 1e-9
                        && ((points[1][0] - points[0][0]) - (points[1][1] - points[0][1])).abs() < 1e-6)
            })
            .count();
        assert!(hatch > 5, "{hatch}");
        assert!(
            page.items
                .iter()
                .any(|i| i.id.as_str() == "section-1-title")
        );
        // A 2:1 detail of a corner.
        run(
            &mut e,
            "sheet-add-detail-view",
            json!({ "page": "P1", "base": "front", "center": [55, 140], "radius": 10, "scale": "2:1", "side": "above" }),
        );
        let s = size(&e, "detail-1");
        assert!(s[0] <= 81.0 && s[0] > 30.0, "{s:?}");
    }

    #[test]
    fn a_broken_view_closes_up() {
        let mut e = engine();
        front_view(&mut e);
        run(
            &mut e,
            "sheet-add-break",
            json!({ "page": "P1", "view": "front", "axis": "horizontal", "from": 70, "to": 90, "gap": 4 }),
        );
        let s = size(&e, "front");
        // 50 mm wide less the 20 mm band, plus the 4 mm gap; zigzags span
        // the frame's height.
        assert!((s[0] - 34.0).abs() < 1.3, "{s:?}");
        assert!(
            e.execute_erased(
                "sheet-add-break",
                json!({ "page": "P1", "view": "front", "axis": "horizontal", "from": 70, "to": 72, "gap": 4 })
            )
            .is_err()
        );
    }

    #[test]
    fn annotations_balloons_and_formats() {
        let mut e = engine();
        front_view(&mut e);
        run(
            &mut e,
            "sheet-add-annotation",
            json!({ "page": "P1", "at": [200, 200], "leader": [180, 150],
                    "annotation": { "type": "feature-control", "characteristic": "position",
                                    "tolerance": "0.1", "diameter": true, "modifier": "maximum",
                                    "datums": ["A", "B"] } }),
        );
        for a in [
            json!({ "type": "datum", "letter": "A" }),
            json!({ "type": "surface-finish", "value": "Ra 1.6", "machining": "required" }),
            json!({ "type": "weld", "kind": "fillet", "size": "a5", "length": "100", "all_around": true }),
        ] {
            run(
                &mut e,
                "sheet-add-annotation",
                json!({ "page": "P1", "at": [250, 200], "annotation": a }),
            );
        }
        assert!(
            e.execute_erased(
                "sheet-add-annotation",
                json!({ "page": "P1", "at": [0, 0], "annotation": { "type": "datum", "letter": "" } })
            )
            .is_err()
        );
        let page = &e.document().sheets.pages[0];
        let d = layout_page(e.document(), page, 150, None).unwrap();
        let frame = d
            .groups
            .iter()
            .find(|g| g.item.as_str() == "note-1")
            .unwrap();
        let texts: Vec<&str> = frame
            .graphics
            .iter()
            .filter_map(|g| match g {
                Graphic::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["0.1", "M", "A", "B"]);

        // Balloons count like the bill of materials.
        let mut e = engine();
        for (name, at) in [("Plate", 0.0), ("Bolt", 500.0)] {
            run(
                &mut e,
                "rectangle",
                json!({ "corner_a": [at, 0, 0], "corner_b": [at + 10.0, 10, 0] }),
            );
            run(
                &mut e,
                "push-pull",
                json!({ "face": { "Query": "face & last" }, "distance": 10 }),
            );
            run(
                &mut e,
                "make-component",
                json!({ "targets": "face & !instance | edge & !instance", "name": name }),
            );
        }
        run(&mut e, "sheet-add-page", json!({ "page": "P1" }));
        run(
            &mut e,
            "sheet-add-balloon",
            json!({ "page": "P1", "at": [50, 50], "leader": [40, 40], "component": "Plate" }),
        );
        let item = &e.document().sheets.pages[0].items[0].item;
        let crate::sheets::SheetItem::Text {
            annotation: Some(crate::sheets::Annotation::Balloon { text }),
            ..
        } = item
        else {
            panic!("{item:?}");
        };
        assert_eq!(text, "2", "Bolt is item 1, Plate item 2");
        run(
            &mut e,
            "sheet-add-table",
            json!({ "page": "P1", "query": "bill-of-materials",
            "frame": { "x": 100, "y": 100, "width": 120, "height": 30 } }),
        );

        // A standard format, replaced by another.
        run(
            &mut e,
            "sheet-format",
            json!({ "page": "P1", "standard": "iso", "size": "A3",
            "fields": { "Title": "Bracket" } }),
        );
        let page = &e.document().sheets.pages[0];
        assert_eq!((page.paper.width, page.paper.height), (420.0, 297.0));
        let count = page.items.len();
        run(
            &mut e,
            "sheet-format",
            json!({ "page": "P1", "standard": "ansi", "size": "B" }),
        );
        let page = &e.document().sheets.pages[0];
        assert!((page.paper.width - 431.8).abs() < 1e-9);
        let titles = page
            .items
            .iter()
            .filter(|i| i.id.as_str() == "format-title")
            .count();
        assert_eq!(titles, 1);
        assert!(page.items.len() <= count + 10, "the old format is replaced");
        let crate::sheets::SheetItem::TitleBlock { fields, .. } = &page
            .items
            .iter()
            .find(|i| i.id.as_str() == "format-title")
            .unwrap()
            .item
        else {
            panic!()
        };
        assert_eq!(fields["Projection"], "third angle");
        layout_page(e.document(), page, 150, None).unwrap();
    }
}
