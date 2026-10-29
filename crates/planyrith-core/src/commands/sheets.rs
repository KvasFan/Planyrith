//! Implements docs/15 §§4–5: one registry command per paper-space operation.

use crate::commands::transaction::Delta;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommandRegistry, CommonParams, Transaction,
};
use crate::document::{Camera, Document, Projection};
use crate::sheets::layout::{self, SheetError};
use crate::sheets::*;
use crate::units::Length;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn err(e: impl std::fmt::Display) -> CommandError {
    CommandError::invalid_param("sheet", e.to_string())
}
fn sheet_error(e: SheetError) -> CommandError {
    err(e)
}
fn page<'a>(doc: &'a Document, name: &str) -> Result<&'a Page, CommandError> {
    doc.sheets
        .page(name)
        .ok_or_else(|| CommandError::invalid_param("page", format!("unknown sheet page `{name}`")))
}
fn entry<'a>(doc: &'a Document, name: &str, id: &ItemId) -> Result<&'a Entry, CommandError> {
    let page = page(doc, name)?;
    let entry = page
        .items
        .iter()
        .find(|i| i.id == *id)
        .ok_or_else(|| err(format!("unknown sheet item `{}`", id.as_str())))?;
    if page
        .layers
        .iter()
        .any(|l| l.name == entry.layer && l.locked)
    {
        return Err(CommandError::NotApplicable("sheet layer is locked".into()));
    }
    Ok(entry)
}
fn paper(name: &str, landscape: bool, margin: Option<Length>) -> Result<Paper, CommandError> {
    let mut p = Paper::named(name, landscape)
        .ok_or_else(|| err("paper must be A0–A4, Letter, Tabloid or widthxheight in mm"))?;
    if let Some(m) = margin {
        p.margin = m.as_mm();
    }
    if !p.valid() {
        return Err(err("invalid paper dimensions or margin"));
    }
    Ok(p)
}
fn yes() -> bool {
    true
}

fn layer() -> String {
    "Drawing".into()
}
fn scale() -> String {
    "1:50".into()
}
fn height() -> Length {
    Length::mm(3.5)
}
fn black() -> Color {
    [0, 0, 0, 255]
}
fn dpi() -> u32 {
    300
}
fn offset() -> [f64; 2] {
    [0.0, 10.0]
}
fn start() -> SheetAnchor {
    SheetAnchor::Paper {
        point: [20.0, 20.0],
    }
}
fn end() -> SheetAnchor {
    SheetAnchor::Paper {
        point: [70.0, 20.0],
    }
}
fn columns() -> Vec<String> {
    vec!["name".into(), "length".into()]
}

/// Stable paper command result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EditOutput {
    /// Page name.
    pub page: String,
    /// Added/edited item, absent for page operations.
    pub id: Option<ItemId>,
}
fn changed(page: &str, id: Option<ItemId>) -> EditOutput {
    EditOutput {
        page: page.into(),
        id,
    }
}
fn insert(
    tx: &mut Transaction<'_>,
    name: &str,
    id: Option<ItemId>,
    layer: &str,
    item: SheetItem,
) -> Result<EditOutput, CommandError> {
    layout::validate_item(&item).map_err(sheet_error)?;
    for image in images(&item) {
        if !tx.document().images.contains(image) {
            return Err(err("sheet image is not in the document"));
        }
    }
    let p = page(tx.document(), name)?;
    if p.items.len() >= 100_000 {
        return Err(err("sheet item limit"));
    }
    let l = p
        .layers
        .iter()
        .find(|l| l.name == layer)
        .ok_or_else(|| err("unknown sheet layer"))?;
    if l.locked {
        return Err(CommandError::NotApplicable("sheet layer is locked".into()));
    }
    let id = id.unwrap_or_else(|| {
        let mut i = 1;
        loop {
            let id = ItemId::named(format!("item-{i}"));
            if !p.items.iter().any(|e| e.id == id) {
                break id;
            }
            i += 1;
        }
    });
    if id.as_str().is_empty() || p.items.iter().any(|e| e.id == id) {
        return Err(err("sheet item id must be nonempty and unique"));
    }
    let index = p.items.len();
    let value = Entry {
        id: id.clone(),
        layer: layer.into(),
        item,
    };
    tx.sheet_delta(Delta::SheetItem {
        page: name.into(),
        index,
        from: None,
        to: Some(Box::new(value)),
    })?;
    Ok(changed(name, Some(id)))
}
fn images(item: &SheetItem) -> Vec<crate::document::ImageRef> {
    match item {
        SheetItem::Image { image, .. } => vec![*image],
        SheetItem::ModelView {
            frozen: Some(image),
            ..
        } => vec![*image],
        _ => Vec::new(),
    }
}
fn replace(
    tx: &mut Transaction<'_>,
    name: &str,
    id: &ItemId,
    item: Option<Entry>,
) -> Result<EditOutput, CommandError> {
    let old = entry(tx.document(), name, id)?.clone();
    let p = page(tx.document(), name)?;
    let index = p
        .items
        .iter()
        .position(|e| e.id == *id)
        .ok_or_else(|| err("unknown item"))?;
    if let Some(new) = &item {
        if let (
            SheetItem::ModelView {
                frame: a,
                scale: sa,
                frozen: Some(_),
                ..
            },
            SheetItem::ModelView {
                frame: b,
                scale: sb,
                frozen: Some(_),
                ..
            },
        ) = (&old.item, &new.item)
            && ((a.width - b.width).abs() > tx.tolerances().length
                || (a.height - b.height).abs() > tx.tolerances().length
                || sa != sb)
        {
            return Err(err(
                "follow the model before changing a frozen viewport's size or scale",
            ));
        }
        layout::validate_item(&new.item).map_err(sheet_error)?;
        for image in images(&new.item) {
            if !tx.document().images.contains(image) {
                return Err(err("sheet image is not in the document"));
            }
        }
        if new.id != *id {
            return Err(err("sheet-edit-item preserves its id"));
        }
        if !p.layers.iter().any(|l| l.name == new.layer && !l.locked) {
            return Err(err("destination sheet layer missing or locked"));
        }
    }
    let unfreezing = matches!(
        (&old.item, item.as_ref().map(|e| &e.item)),
        (
            SheetItem::ModelView {
                frozen: Some(_),
                ..
            },
            Some(SheetItem::ModelView { frozen: None, .. })
        )
    );
    if unfreezing {
        let dimensions=page(tx.document(),name)?.items.iter().filter(|e|matches!(&e.item,SheetItem::Dimension{view:Some(view),frozen:Some(_),..} if view==id)).cloned().collect::<Vec<_>>();
        for mut dimension in dimensions {
            if let SheetItem::Dimension { frozen, .. } = &mut dimension.item {
                *frozen = None;
            }
            let id = dimension.id.clone();
            replace(tx, name, &id, Some(dimension))?;
        }
    }
    let released = images(&old.item);
    tx.sheet_delta(Delta::SheetItem {
        page: name.into(),
        index,
        from: Some(Box::new(old)),
        to: item.map(Box::new),
    })?;
    tx.release_images(&released);
    Ok(changed(name, Some(id.clone())))
}
fn meta(name: &str, read: bool) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        category: CommandCategory::Sheet,
        label: name.replace('-', " "),
        view_only: read,
        steps: Vec::new(),
    }
}
macro_rules! command {
    ($ty:ident,$params:ident,$output:ty,$name:literal,$read:expr,$validate:expr,$apply:expr) => {
        #[doc=concat!("The `",$name,"` paper-space command (docs/15).")]
        #[derive(Debug, Default, Clone, Copy)]
        pub struct $ty;
        impl Command for $ty {
            type Params = $params;
            type Output = $output;
            fn name(&self) -> &'static str {
                $name
            }
            fn validate(&self, doc: &Document, p: &Self::Params) -> Result<(), CommandError> {
                ($validate)(doc, p)
            }
            fn apply(
                &self,
                tx: &mut Transaction<'_>,
                p: Self::Params,
            ) -> Result<Self::Output, CommandError> {
                ($apply)(tx, p)
            }
            fn meta(&self) -> CommandMeta {
                meta($name, $read)
            }
        }
    };
}
/// Parameters for adding a sheet page.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddPageParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Unique page name.
    pub page: String,
    /// A0–A4, Letter, Tabloid or widthxheight in millimeters.
    #[serde(default)]
    pub paper: Option<String>,
    /// Optional orientation; an inherited paper keeps its orientation.
    #[serde(default)]
    pub landscape: Option<bool>,
    /// Margin, mm (default 10 mm).
    #[serde(default)]
    pub margin: Option<Length>,
}
command!(
    AddPage,
    AddPageParams,
    EditOutput,
    "sheet-add-page",
    false,
    |doc: &Document, p: &AddPageParams| {
        if p.page.trim().is_empty() || doc.sheets.page(&p.page).is_some() {
            return Err(err("page name must be nonempty and unique"));
        }
        if doc.sheets.pages.len() >= 4096 {
            return Err(err("sheet page limit"));
        }
        page_paper(doc, p)?;
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: AddPageParams| {
        let page = Page {
            name: p.page.clone(),
            paper: page_paper(tx.document(), &p)?,
            items: Vec::new(),
            layers: vec![Layer::default()],
        };
        let index = tx.document().sheets.pages.len();
        tx.sheet_delta(Delta::SheetPage {
            index,
            from: None,
            to: Some(Box::new(page)),
        })?;
        Ok(changed(&p.page, None))
    }
);
fn page_paper(doc: &Document, p: &AddPageParams) -> Result<Paper, CommandError> {
    if let Some(name) = &p.paper {
        return paper(name, p.landscape.unwrap_or(true), p.margin);
    }
    let mut paper = doc.sheets.page(&p.page).map_or_else(
        || doc.sheets.paper_defaults.clone(),
        |page| page.paper.clone(),
    );
    if let Some(landscape) = p.landscape {
        let (w, h) = (paper.width, paper.height);
        paper.width = if landscape { w.max(h) } else { w.min(h) };
        paper.height = if landscape { w.min(h) } else { w.max(h) };
    }
    if let Some(margin) = p.margin {
        paper.margin = margin.as_mm();
    }
    if !paper.valid() {
        return Err(err("invalid paper defaults"));
    }
    Ok(paper)
}
/// A page name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PageParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page name.
    pub page: String,
}
command!(
    RemovePage,
    PageParams,
    EditOutput,
    "sheet-remove-page",
    false,
    |doc: &Document, p: &PageParams| {
        page(doc, &p.page)?;
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: PageParams| {
        let old = page(tx.document(), &p.page)?.clone();
        let index = tx
            .document()
            .sheets
            .pages
            .iter()
            .position(|p| p.name == old.name)
            .ok_or_else(|| err("missing page"))?;
        let released = old
            .items
            .iter()
            .flat_map(|e| images(&e.item))
            .collect::<Vec<_>>();
        tx.sheet_delta(Delta::SheetPage {
            index,
            from: Some(Box::new(old)),
            to: None,
        })?;
        tx.release_images(&released);
        Ok(changed(&p.page, None))
    }
);
/// Parameters for changing paper without changing items.
pub type PaperParams = AddPageParams;
command!(
    SetPaper,
    PaperParams,
    EditOutput,
    "sheet-set-paper",
    false,
    |doc: &Document, p: &PaperParams| {
        page(doc, &p.page)?;
        page_paper(doc, p)?;
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: PaperParams| {
        let old = page(tx.document(), &p.page)?.paper.clone();
        let new = page_paper(tx.document(), &p)?;
        tx.sheet_delta(Delta::SheetPaper {
            page: p.page.clone(),
            from: old,
            to: new,
        })?;
        Ok(changed(&p.page, None))
    }
);
/// Model view parameters (paper lengths in mm).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddViewParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page name.
    pub page: String,
    /// Optional stable item name.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer name.
    #[serde(default = "layer")]
    pub layer: String,
    /// Linked scene name; its camera and display state update on export.
    #[serde(default)]
    pub scene: Option<String>,
    /// Named style override.
    #[serde(default)]
    pub style: Option<String>,
    /// Camera snapshot (default: current document camera).
    #[serde(default)]
    pub camera: Option<Camera>,
    /// Use the supplied camera instead of the scene camera, retaining scene appearance.
    #[serde(default)]
    pub camera_override: bool,
    /// Rational paper/model scale, e.g. 1:50.
    #[serde(default = "scale")]
    pub scale: String,
    /// Vector, raster or hybrid.
    #[serde(default)]
    pub render: RenderMode,
    /// Paper rectangle in mm.
    #[serde(default)]
    pub frame: Rect,
    /// Optional convex paper-space clip polygon.
    #[serde(default)]
    pub clip: Vec<[f64; 2]>,
    /// Tag visibility by name.
    #[serde(default)]
    pub tags_override: BTreeMap<String, bool>,
    /// Section active flags by persistent id string.
    #[serde(default)]
    pub section_override: BTreeMap<String, bool>,
}
fn view_item(doc: &Document, p: &AddViewParams) -> Result<SheetItem, CommandError> {
    let mut camera = p.camera.unwrap_or(doc.camera);
    camera.projection = Projection::Parallel {
        height_mm: p.frame.height
            / Scale::parse(&p.scale)
                .ok_or_else(|| err("scale must be a positive integer ratio, e.g. 1:50"))?
                .factor(),
    };
    let item = SheetItem::ModelView {
        scene: p.scene.clone(),
        camera,
        camera_override: p.camera_override,
        scale: Scale::parse(&p.scale).ok_or_else(|| err("invalid scale"))?,
        style: p.style.clone(),
        render: p.render,
        frame: p.frame,
        clip: p.clip.clone(),
        tags_override: p.tags_override.clone(),
        section_override: p.section_override.clone(),
        frozen: None,
    };
    layout::validate_item(&item).map_err(sheet_error)?;
    layout::view_document(doc, &item).map_err(sheet_error)?;
    Ok(item)
}
command!(
    AddView,
    AddViewParams,
    EditOutput,
    "sheet-add-view",
    false,
    |doc: &Document, p: &AddViewParams| {
        page(doc, &p.page)?;
        view_item(doc, p)?;
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: AddViewParams| {
        let item = view_item(tx.document(), &p)?;
        insert(tx, &p.page, p.id, &p.layer, item)
    }
);
/// Rich text parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddTextParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// Optional stable item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
    /// Plain text, used if runs are empty.
    #[serde(default)]
    pub text: String,
    /// Styled runs (override plain text).
    #[serde(default)]
    pub runs: Vec<TextRun>,
    /// Text frame, paper mm.
    #[serde(default)]
    pub frame: Rect,
    /// Em height, mm.
    #[serde(default = "height")]
    pub height: Length,
    /// Text color.
    #[serde(default = "black")]
    pub color: Color,
    /// Alignment.
    #[serde(default)]
    pub alignment: Alignment,
    /// Leader endpoints on paper, mm.
    #[serde(default)]
    pub leader: Option<[[f64; 2]; 2]>,
}
command!(
    AddText,
    AddTextParams,
    EditOutput,
    "sheet-add-text",
    false,
    |doc: &Document, p: &AddTextParams| {
        page(doc, &p.page)?;
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: AddTextParams| {
        let runs = if p.runs.is_empty() {
            vec![TextRun {
                text: p.text,
                height: p.height.as_mm(),
                color: p.color,
            }]
        } else {
            p.runs
        };
        insert(
            tx,
            &p.page,
            p.id,
            &p.layer,
            SheetItem::Text {
                runs,
                frame: p.frame,
                alignment: p.alignment,
                leader: p.leader,
            },
        )
    }
);
/// Sheet dimension parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddDimensionParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// Optional item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
    /// Model view for anchors in model coordinates.
    #[serde(default)]
    pub view: Option<ItemId>,
    /// First paper/model anchor.
    #[serde(default = "start")]
    pub start: SheetAnchor,
    /// Second paper/model anchor.
    #[serde(default = "end")]
    pub end: SheetAnchor,
    /// Optional first world point, snapped to a persistent model edge.
    #[serde(default)]
    pub model_start: Option<[f64; 3]>,
    /// Optional second world point, snapped to a persistent model edge.
    #[serde(default)]
    pub model_end: Option<[f64; 3]>,
    /// Aligned, horizontal, vertical, radius or diameter.
    #[serde(default)]
    pub dimension_kind: DimensionKind,
    /// Offset in paper mm.
    #[serde(default = "offset")]
    pub offset: [f64; 2],
    /// Optional override (`<>` is the measured value).
    #[serde(default)]
    pub text: Option<String>,
}
command!(
    AddDimension,
    AddDimensionParams,
    EditOutput,
    "sheet-add-dimension",
    false,
    |doc: &Document, p: &AddDimensionParams| {
        page(doc, &p.page)?;
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: AddDimensionParams| {
        let snap = |q: [f64; 3]| SheetAnchor::Model {
            anchor: crate::document::annotations::find_anchor(
                tx.document(),
                crate::document::ContainerRef::Model,
                glam::DVec3::from(q),
            ),
        };
        let item = SheetItem::Dimension {
            view: p.view,
            start: p.model_start.map_or(p.start, snap),
            end: p.model_end.map_or(p.end, snap),
            dimension_kind: p.dimension_kind,
            frozen: None,
            offset: p.offset,
            text: p.text,
        };
        if let SheetItem::Dimension {
            view: Some(view),
            start,
            end,
            ..
        } = &item
            && (matches!(start, SheetAnchor::Model { .. })
                || matches!(end, SheetAnchor::Model { .. }))
            && page(tx.document(), &p.page)?.items.iter().any(|e| {
                e.id == *view
                    && matches!(
                        e.item,
                        SheetItem::ModelView {
                            frozen: Some(_),
                            ..
                        }
                    )
            })
        {
            return Err(err(
                "follow the model before adding linked dimensions to a frozen view",
            ));
        }
        layout::dimension(tx.document(), page(tx.document(), &p.page)?, &item)
            .map_err(sheet_error)?;
        insert(tx, &p.page, p.id, &p.layer, item)
    }
);
/// Table parameters, literal or query-bound.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddTableParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// Item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
    /// Frame, paper mm.
    #[serde(default)]
    pub frame: Rect,
    /// Literal cells.
    #[serde(default)]
    pub rows: Vec<Vec<String>>,
    /// Live entity query or component-report.
    #[serde(default)]
    pub query: Option<String>,
    /// Query columns (name, pid, kind, length, area, volume, attr.dict.key).
    #[serde(default = "columns")]
    pub columns: Vec<String>,
}
command!(
    AddTable,
    AddTableParams,
    EditOutput,
    "sheet-add-table",
    false,
    |doc: &Document, p: &AddTableParams| {
        page(doc, &p.page)?;
        layout::table_rows(doc, &p.rows, p.query.as_deref(), &p.columns).map_err(sheet_error)?;
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: AddTableParams| insert(
        tx,
        &p.page,
        p.id,
        &p.layer,
        SheetItem::Table {
            frame: p.frame,
            rows: p.rows,
            query: p.query,
            columns: p.columns
        }
    )
);
/// Image parameters; bytes are loaded only through Storage.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddImageParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// Item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
    /// Source PNG, JPEG or WebP.
    pub path: String,
    /// Frame, mm.
    #[serde(default)]
    pub frame: Rect,
}
command!(
    AddImage,
    AddImageParams,
    EditOutput,
    "sheet-add-image",
    false,
    |doc: &Document, p: &AddImageParams| {
        page(doc, &p.page)?;
        if !p.frame.valid() {
            return Err(err("invalid image frame"));
        }
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: AddImageParams| {
        let bytes = crate::commands::organize::libraries::storage(tx, &p.path)?
            .read(&crate::io::StorageKey::new(&p.path))?;
        let image = tx.insert_image(bytes)?;
        insert(
            tx,
            &p.page,
            p.id,
            &p.layer,
            SheetItem::Image {
                image,
                frame: p.frame,
            },
        )
    }
);
/// Shape parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddShapeParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// Item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
    /// Polyline, rectangle or ellipse.
    #[serde(default)]
    pub shape: ShapeKind,
    /// Control points, paper mm; two opposite corners for rectangles/ellipses.
    pub points: Vec<[f64; 2]>,
    /// Close a polyline.
    #[serde(default)]
    pub closed: bool,
    /// Pen in paper mm.
    #[serde(default)]
    pub stroke: Stroke,
    /// Optional fill RGBA.
    #[serde(default)]
    pub fill: Option<Color>,
}
command!(
    AddShape,
    AddShapeParams,
    EditOutput,
    "sheet-add-shape",
    false,
    |doc: &Document, p: &AddShapeParams| {
        page(doc, &p.page)?;
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: AddShapeParams| insert(
        tx,
        &p.page,
        p.id,
        &p.layer,
        SheetItem::Shape {
            shape: p.shape,
            points: p.points,
            closed: p.closed,
            stroke: p.stroke,
            fill: p.fill
        }
    )
);
/// Explicit title-block fields.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddTitleBlockParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// Item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
    /// Paper frame.
    #[serde(default)]
    pub frame: Rect,
    /// Explicit metadata; date and author are never read from the host.
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}
command!(
    AddTitleBlock,
    AddTitleBlockParams,
    EditOutput,
    "sheet-add-title-block",
    false,
    |doc: &Document, p: &AddTitleBlockParams| {
        page(doc, &p.page)?;
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: AddTitleBlockParams| {
        let mut fields = tx
            .document()
            .sheets
            .title_block
            .as_ref()
            .map(|t| t.fields.clone())
            .unwrap_or_default();
        fields.extend(p.fields);
        insert(
            tx,
            &p.page,
            p.id,
            &p.layer,
            SheetItem::TitleBlock {
                frame: p.frame,
                fields,
            },
        )
    }
);
/// Attribute/scene label parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddLabelParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// Item id.
    #[serde(default)]
    pub id: Option<ItemId>,
    /// Layer.
    #[serde(default = "layer")]
    pub layer: String,
    /// Paper frame.
    #[serde(default)]
    pub frame: Rect,
    /// `title`, `author`, `scene:<item-id>`, or `attr.<dictionary>.<key>`.
    pub binding: String,
    /// Text prefix.
    #[serde(default)]
    pub prefix: String,
}
command!(
    AddLabel,
    AddLabelParams,
    EditOutput,
    "sheet-add-label",
    false,
    |doc: &Document, p: &AddLabelParams| {
        page(doc, &p.page)?;
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: AddLabelParams| insert(
        tx,
        &p.page,
        p.id,
        &p.layer,
        SheetItem::Label {
            frame: p.frame,
            binding: p.binding,
            prefix: p.prefix
        }
    )
);
/// Item replacement parameters (all paper fields remain typed).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EditItemParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// Existing item id.
    pub id: ItemId,
    /// New item value.
    pub item: SheetItem,
    /// Destination layer, otherwise the current layer.
    #[serde(default)]
    pub layer: Option<String>,
}
command!(
    EditItem,
    EditItemParams,
    EditOutput,
    "sheet-edit-item",
    false,
    |doc: &Document, p: &EditItemParams| {
        entry(doc, &p.page, &p.id)?;
        layout::validate_item(&p.item).map_err(sheet_error)
    },
    |tx: &mut Transaction<'_>, p: EditItemParams| {
        let old = entry(tx.document(), &p.page, &p.id)?;
        let new = Entry {
            id: p.id.clone(),
            layer: p.layer.unwrap_or_else(|| old.layer.clone()),
            item: p.item,
        };
        replace(tx, &p.page, &p.id, Some(new))
    }
);
/// Translation parameters in paper millimeters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MoveItemParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page name.
    pub page: String,
    /// Item id.
    pub id: ItemId,
    /// Horizontal/vertical translation, mm.
    pub delta: [f64; 2],
}
command!(
    MoveItem,
    MoveItemParams,
    EditOutput,
    "sheet-move-item",
    false,
    |doc: &Document, p: &MoveItemParams| {
        let e = entry(doc, &p.page, &p.id)?;
        layout::translated(&e.item, p.delta).map_err(sheet_error)?;
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: MoveItemParams| {
        let mut e = entry(tx.document(), &p.page, &p.id)?.clone();
        e.item = layout::translated(&e.item, p.delta).map_err(sheet_error)?;
        replace(tx, &p.page, &p.id, Some(e))
    }
);
/// Parameters naming an existing item.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ItemParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// Item id.
    pub id: ItemId,
}
command!(
    RemoveItem,
    ItemParams,
    EditOutput,
    "sheet-remove-item",
    false,
    |doc: &Document, p: &ItemParams| {
        entry(doc, &p.page, &p.id)?;
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: ItemParams| replace(tx, &p.page, &p.id, None)
);
/// Layer settings; absent flags keep their previous values.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LayerParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// Existing/new layer name.
    pub layer: String,
    /// Visibility.
    #[serde(default)]
    pub visible: Option<bool>,
    /// Lock editing.
    #[serde(default)]
    pub locked: Option<bool>,
}
command!(
    SetLayer,
    LayerParams,
    EditOutput,
    "sheet-layer-set",
    false,
    |doc: &Document, p: &LayerParams| {
        page(doc, &p.page)?;
        if p.layer.is_empty() {
            return Err(err("empty layer name"));
        }
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: LayerParams| {
        let from = page(tx.document(), &p.page)?.layers.clone();
        let mut to = from.clone();
        if !to.iter().any(|l| l.name == p.layer) {
            if to.len() >= 1024 {
                return Err(err("layer limit"));
            }
            to.push(Layer {
                name: p.layer.clone(),
                visible: true,
                locked: false,
            });
        }
        if let Some(layer) = to.iter_mut().find(|l| l.name == p.layer) {
            if let Some(v) = p.visible {
                layer.visible = v;
            }
            if let Some(v) = p.locked {
                layer.locked = v;
            }
        }
        tx.sheet_delta(Delta::SheetLayers {
            page: p.page.clone(),
            from,
            to,
        })?;
        Ok(changed(&p.page, None))
    }
);
/// Raster snapshot parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FreezeViewParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// View item id.
    pub id: ItemId,
    /// Freeze current image, or false to follow the model again.
    #[serde(default = "yes")]
    pub frozen: bool,
    /// Pixels per inch (36–1200, default 300).
    #[serde(default = "dpi")]
    pub dpi: u32,
}
command!(
    FreezeView,
    FreezeViewParams,
    EditOutput,
    "sheet-freeze-view",
    false,
    |doc: &Document, p: &FreezeViewParams| {
        let e = entry(doc, &p.page, &p.id)?;
        if !matches!(e.item, SheetItem::ModelView { .. }) {
            return Err(err("item must be a model view"));
        }
        if !(36..=1200).contains(&p.dpi) {
            return Err(err("DPI must be 36–1200"));
        }
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: FreezeViewParams| {
        let mut value = entry(tx.document(), &p.page, &p.id)?.clone();
        let image = if p.frozen {
            let (model, camera) =
                layout::view_document(tx.document(), &value.item).map_err(sheet_error)?;
            let SheetItem::ModelView { frame, scale, .. } = &value.item else {
                return Err(err("not a view"));
            };
            let draw = crate::io::export::view2d::draw_view(
                &model,
                &crate::io::export::ExportScope::default(),
                &camera,
                crate::io::export::view2d::Paper {
                    scale: Some(scale.factor()),
                    width_mm: frame.width,
                },
            );
            let bytes = layout::raster_view(&model, &camera, *frame, *scale, p.dpi, true, &draw)
                .map_err(sheet_error)?;
            Some(tx.insert_image(bytes)?)
        } else {
            None
        };
        let associated = page(tx.document(), &p.page)?
            .items
            .iter()
            .filter(|e| matches!(&e.item,SheetItem::Dimension{view:Some(view),..} if *view==p.id))
            .cloned()
            .collect::<Vec<_>>();
        for mut dimension in associated {
            if let SheetItem::Dimension { frozen, .. } = &mut dimension.item {
                *frozen = None;
            }
            let frozen_value = if p.frozen {
                let d = layout::dimension(
                    tx.document(),
                    page(tx.document(), &p.page)?,
                    &dimension.item,
                )
                .map_err(sheet_error)?;
                let SheetItem::ModelView { frame, .. } = &value.item else {
                    return Err(err("not a view"));
                };
                Some(FrozenDimension {
                    points: d
                        .points
                        .map(|point| [point[0] - frame.x, point[1] - frame.y]),
                    value_mm: d.value,
                    text: d.text,
                })
            } else {
                None
            };
            if let SheetItem::Dimension { frozen, .. } = &mut dimension.item {
                *frozen = frozen_value;
            }
            let id = dimension.id.clone();
            replace(tx, &p.page, &id, Some(dimension))?;
        }
        if let SheetItem::ModelView { frozen, .. } = &mut value.item {
            *frozen = image;
        }
        replace(tx, &p.page, &p.id, Some(value))
    }
);
/// Read-only list/refresh parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Optional page name, otherwise all pages.
    #[serde(default)]
    pub page: Option<String>,
}
/// The current drawing set, including all typed item parameters.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListOutput {
    /// Pages in print order.
    pub pages: Vec<Page>,
}
command!(
    List,
    ReadParams,
    ListOutput,
    "sheet-list",
    true,
    |doc: &Document, p: &ReadParams| {
        if let Some(name) = &p.page {
            page(doc, name)?;
        }
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: ReadParams| Ok(ListOutput {
        pages: tx
            .document()
            .sheets
            .pages
            .iter()
            .filter(|page| p.page.as_ref().is_none_or(|name| page.name == *name))
            .cloned()
            .collect()
    })
);
/// Refreshed live measurements and detached-reference warnings.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RefreshOutput {
    /// Per-dimension page/id/value/text records.
    pub dimensions: Vec<serde_json::Value>,
    /// Unresolved references.
    pub warnings: Vec<String>,
}
command!(
    Refresh,
    ReadParams,
    RefreshOutput,
    "sheet-refresh",
    true,
    |doc: &Document, p: &ReadParams| {
        if let Some(name) = &p.page {
            page(doc, name)?;
        }
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: ReadParams| {
        let doc = tx.document();
        let mut result = RefreshOutput {
            dimensions: Vec::new(),
            warnings: Vec::new(),
        };
        for page in doc
            .sheets
            .pages
            .iter()
            .filter(|page| p.page.as_ref().is_none_or(|name| page.name == *name))
        {
            for entry in &page.items {
                if matches!(entry.item, SheetItem::Dimension { .. }) {
                    match layout::dimension(doc,page,&entry.item){Ok(d)=>result.dimensions.push(serde_json::json!({"page":page.name,"id":entry.id,"value_mm":d.value,"text":d.text})),Err(e)=>result.warnings.push(format!("{} / {}: {e}",page.name,entry.id.as_str()))}
                }
            }
        }
        Ok(result)
    }
);
/// Export selected sheet pages, writing through Storage.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExportParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Destination .pdf, .svg or .dxf. PDF includes every selected page.
    pub path: String,
    /// Single page, otherwise pages/all.
    #[serde(default)]
    pub page: Option<String>,
    /// Named pages, or all (default), in document print order.
    #[serde(default)]
    pub pages: Vec<String>,
    /// Raster DPI, 36–1200.
    #[serde(default = "dpi")]
    pub dpi: u32,
    /// Override model-view render modes.
    #[serde(default)]
    pub render: Option<RenderMode>,
}
/// Exported paths and annotation warnings.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ExportOutput {
    /// Written files including raster side files.
    pub files: Vec<String>,
    /// Detached annotations or missing bindings.
    pub warnings: Vec<String>,
}
command!(
    Export,
    ExportParams,
    ExportOutput,
    "sheet-export",
    true,
    |doc: &Document, p: &ExportParams| {
        if let Some(name) = &p.page {
            page(doc, name)?;
        }
        for name in &p.pages {
            if name != "all" {
                page(doc, name)?;
            }
        }
        if !(36..=1200).contains(&p.dpi) {
            return Err(err("DPI must be 36–1200"));
        }
        if p.path.is_empty() {
            return Err(err("export path is empty"));
        }
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: ExportParams| {
        let doc = tx.document();
        let pages = doc
            .sheets
            .pages
            .iter()
            .filter(|page| {
                p.page.as_ref().map_or_else(
                    || {
                        p.pages.is_empty()
                            || p.pages
                                .iter()
                                .any(|name| name == "all" || name == &page.name)
                    },
                    |name| page.name == *name,
                )
            })
            .map(|page| layout::layout_page(doc, page, p.dpi, p.render).map_err(sheet_error))
            .collect::<Result<Vec<_>, _>>()?;
        let warnings = pages.iter().flat_map(|p| p.warnings.clone()).collect();
        let files =
            crate::sheets::export::files(&pages, &p.path, &doc.info.title, &doc.info.author)
                .map_err(sheet_error)?;
        let storage = crate::commands::organize::libraries::storage(tx, &p.path)?;
        for (path, bytes) in &files {
            storage.write(&crate::io::StorageKey::new(path), bytes)?;
        }
        Ok(ExportOutput {
            files: files.into_iter().map(|(name, _)| name).collect(),
            warnings,
        })
    }
);

/// Sheet-wide defaults; omitted values remain unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SettingsParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Paper used by future pages.
    #[serde(default)]
    pub paper_defaults: Option<Paper>,
    /// Pen table and text defaults.
    #[serde(default)]
    pub styles: Option<SheetStyles>,
    /// Reusable explicit title-block fields.
    #[serde(default)]
    pub title_block: Option<TitleBlockTemplate>,
    /// Remove the title-block template.
    #[serde(default)]
    pub clear_title_block: bool,
}
command!(
    Settings,
    SettingsParams,
    SheetSettings,
    "sheet-settings",
    false,
    |_doc: &Document, _p: &SettingsParams| Ok(()),
    |tx: &mut Transaction<'_>, p: SettingsParams| {
        let from = SheetSettings::from(&tx.document().sheets);
        let mut to = from.clone();
        if let Some(paper) = p.paper_defaults {
            to.paper_defaults = paper;
        }
        if let Some(styles) = p.styles {
            to.styles = styles;
        }
        if let Some(template) = p.title_block {
            to.title_block = Some(template);
        }
        if p.clear_title_block {
            to.title_block = None;
        }
        layout::validate_set(&SheetSet {
            pages: Vec::new(),
            paper_defaults: to.paper_defaults.clone(),
            title_block: to.title_block.clone(),
            styles: to.styles.clone(),
        })
        .map_err(sheet_error)?;
        tx.sheet_delta(Delta::SheetSettings {
            from: Box::new(from),
            to: Box::new(to.clone()),
        })?;
        Ok(to)
    }
);
/// Rename an existing page.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenamePageParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Existing page name.
    pub page: String,
    /// New unique page name.
    pub name: String,
}
command!(
    RenamePage,
    RenamePageParams,
    EditOutput,
    "sheet-rename-page",
    false,
    |doc: &Document, p: &RenamePageParams| {
        page(doc, &p.page)?;
        if p.name.trim().is_empty()
            || doc
                .sheets
                .pages
                .iter()
                .any(|page| page.name == p.name && page.name != p.page)
        {
            return Err(err("page name must be nonempty and unique"));
        }
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: RenamePageParams| {
        tx.sheet_delta(Delta::SheetPageName {
            from: p.page,
            to: p.name.clone(),
        })?;
        Ok(changed(&p.name, None))
    }
);
/// Reorders a page in print order (zero-based index).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PageOrderParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Existing page.
    pub page: String,
    /// New zero-based print position.
    pub index: usize,
}
command!(
    MovePage,
    PageOrderParams,
    EditOutput,
    "sheet-move-page",
    false,
    |doc: &Document, p: &PageOrderParams| {
        page(doc, &p.page)?;
        if p.index >= doc.sheets.pages.len() {
            return Err(err("print index outside page list"));
        }
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: PageOrderParams| {
        let from = tx
            .document()
            .sheets
            .pages
            .iter()
            .position(|page| page.name == p.page)
            .ok_or_else(|| err("page missing"))?;
        tx.sheet_delta(Delta::SheetPageOrder { from, to: p.index })?;
        Ok(changed(&p.page, None))
    }
);
/// Reorders one item without changing its content.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ItemOrderParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Page.
    pub page: String,
    /// Existing item.
    pub id: ItemId,
    /// New zero-based stacking position.
    pub index: usize,
}
command!(
    OrderItem,
    ItemOrderParams,
    EditOutput,
    "sheet-order-item",
    false,
    |doc: &Document, p: &ItemOrderParams| {
        entry(doc, &p.page, &p.id)?;
        if p.index >= page(doc, &p.page)?.items.len() {
            return Err(err("item index outside page"));
        }
        Ok(())
    },
    |tx: &mut Transaction<'_>, p: ItemOrderParams| {
        let e = entry(tx.document(), &p.page, &p.id)?.clone();
        let from = page(tx.document(), &p.page)?
            .items
            .iter()
            .position(|e| e.id == p.id)
            .ok_or_else(|| err("item missing"))?;
        tx.sheet_delta(Delta::SheetItem {
            page: p.page.clone(),
            index: from,
            from: Some(Box::new(e.clone())),
            to: None,
        })?;
        tx.sheet_delta(Delta::SheetItem {
            page: p.page.clone(),
            index: p.index,
            from: None,
            to: Some(Box::new(e)),
        })?;
        Ok(changed(&p.page, Some(p.id)))
    }
);

/// Registers every paper-space operation, also making it available to RPC/Rhai.
pub(crate) fn register(registry: &mut CommandRegistry) {
    registry.register(Settings);
    registry.register(RenamePage);
    registry.register(MovePage);
    registry.register(OrderItem);
    registry.register(AddPage);
    registry.register(RemovePage);
    registry.register(SetPaper);
    registry.register(AddView);
    registry.register(AddDimension);
    registry.register(AddText);
    registry.register(AddTable);
    registry.register(AddImage);
    registry.register(AddShape);
    registry.register(AddTitleBlock);
    registry.register(AddLabel);
    registry.register(EditItem);
    registry.register(RemoveItem);
    registry.register(MoveItem);
    registry.register(SetLayer);
    registry.register(FreezeView);
    registry.register(List);
    registry.register(Refresh);
    registry.register(Export);
}
