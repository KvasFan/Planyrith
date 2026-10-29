//! Implements docs/15 §§1–2: persistent paper-space drawings, millimeter
//! coordinates, linked model views and annotation anchors.

pub mod export;
pub mod layout;

use crate::document::{Anchor, Camera, ImageRef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A stable, page-local item identifier, preserved by saving and undo.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ItemId(String);
impl ItemId {
    /// Creates an identifier from a nonempty application-supplied name.
    pub fn named(name: impl Into<String>) -> Self {
        Self(name.into())
    }
    /// The externally addressable name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Paper rectangle in mm; origin at the lower left, positive Y upward.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rect {
    /// Left edge, mm.
    pub x: f64,
    /// Bottom edge, mm.
    pub y: f64,
    /// Width, mm.
    pub width: f64,
    /// Height, mm.
    pub height: f64,
}
impl Default for Rect {
    fn default() -> Self {
        Self {
            x: 20.0,
            y: 20.0,
            width: 160.0,
            height: 120.0,
        }
    }
}
impl Rect {
    /// Whether the rectangle has finite coordinates and positive size.
    pub fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|x| x.is_finite())
            && self.x.abs() <= 1e9
            && self.y.abs() <= 1e9
            && self.width > 0.0
            && self.height > 0.0
    }
    /// Contains a paper point, including its boundary.
    pub fn contains(self, p: [f64; 2]) -> bool {
        p[0] >= self.x
            && p[1] >= self.y
            && p[0] <= self.x + self.width
            && p[1] <= self.y + self.height
    }
}

/// Exact rational paper-to-model scale, such as 1:50.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Scale {
    /// Paper length numerator.
    pub numerator: u32,
    /// Model length denominator.
    pub denominator: u32,
}
impl Default for Scale {
    fn default() -> Self {
        Self {
            numerator: 1,
            denominator: 50,
        }
    }
}
impl Scale {
    /// Paper millimeters per model millimeter.
    pub fn factor(self) -> f64 {
        f64::from(self.numerator) / f64::from(self.denominator)
    }
    /// Parses a positive integer ratio, `1:50` or `1/50`.
    pub fn parse(text: &str) -> Option<Self> {
        let (a, b) = text.split_once([':', '/'])?;
        let s = Self {
            numerator: a.trim().parse().ok()?,
            denominator: b.trim().parse().ok()?,
        };
        (s.numerator > 0 && s.denominator > 0).then_some(s)
    }
}

/// Paper size in millimeters. Orientation is represented by width/height.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Paper {
    /// Width, mm.
    pub width: f64,
    /// Height, mm.
    pub height: f64,
    /// Printable inset on all sides, mm.
    pub margin: f64,
}
impl Default for Paper {
    fn default() -> Self {
        Self {
            width: 297.0,
            height: 210.0,
            margin: 10.0,
        }
    }
}
impl Paper {
    /// A0–A4, Letter, Tabloid, or `widthxheight` in millimeters.
    pub fn named(name: &str, landscape: bool) -> Option<Self> {
        let (w, h): (f64, f64) = match name.to_ascii_lowercase().as_str() {
            "a0" => (841.0, 1189.0),
            "a1" => (594.0, 841.0),
            "a2" => (420.0, 594.0),
            "a3" => (297.0, 420.0),
            "a4" => (210.0, 297.0),
            "letter" => (215.9, 279.4),
            "tabloid" => (279.4, 431.8),
            _ => {
                let (a, b) = name.split_once('x')?;
                (a.parse().ok()?, b.parse().ok()?)
            }
        };
        Some(Self {
            width: if landscape { w.max(h) } else { w.min(h) },
            height: if landscape { w.min(h) } else { w.max(h) },
            margin: 10.0,
        })
    }
    /// Valid finite paper bounds with a usable printable region.
    pub fn valid(&self) -> bool {
        [self.width, self.height, self.margin]
            .iter()
            .all(|v| v.is_finite())
            && self.width > 0.0
            && self.height > 0.0
            && self.width <= 5000.0
            && self.height <= 5000.0
            && self.margin >= 0.0
            && 2.0 * self.margin < self.width.min(self.height)
    }
}

/// Rendering mode of a model viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RenderMode {
    /// Hidden-line vector paths.
    #[default]
    Vector,
    /// Raster faces and edges at the requested DPI.
    Raster,
    /// Raster faces with vector linework.
    Hybrid,
}
/// RGBA color used by paper graphics.
pub type Color = [u8; 4];
/// Pen and dash pattern in paper millimeters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Stroke {
    /// Color.
    pub color: Color,
    /// Width, mm.
    pub width: f64,
    /// Alternating dash/gap lengths, mm; empty means solid.
    #[serde(default)]
    pub dash: Vec<f64>,
}
impl Default for Stroke {
    fn default() -> Self {
        Self {
            color: [0, 0, 0, 255],
            width: 0.25,
            dash: Vec::new(),
        }
    }
}
/// Document-wide sheet drawing defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetStyles {
    /// Ordinary edge pen.
    pub edge: Stroke,
    /// Silhouette pen.
    pub profile: Stroke,
    /// Pen overrides by model tag name.
    #[serde(default)]
    pub pens: BTreeMap<String, Stroke>,
    /// Annotation text height, mm.
    pub text_height: f64,
}
impl Default for SheetStyles {
    fn default() -> Self {
        Self {
            edge: Stroke::default(),
            profile: Stroke {
                width: 0.5,
                ..Stroke::default()
            },
            pens: BTreeMap::new(),
            text_height: 3.5,
        }
    }
}
/// A paper-space layer. Item order supplies stacking within it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Layer {
    /// Unique layer name.
    pub name: String,
    /// Export and display visibility.
    pub visible: bool,
    /// Whether the editor permits changes to its items.
    pub locked: bool,
}
impl Default for Layer {
    fn default() -> Self {
        Self {
            name: "Drawing".into(),
            visible: true,
            locked: false,
        }
    }
}
/// A reusable title-block field template.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TitleBlockTemplate {
    /// Named explicit fields; no clock or user-account lookup.
    pub fields: BTreeMap<String, String>,
}
/// All sheets saved inside the model document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct SheetSet {
    /// Pages in print order.
    pub pages: Vec<Page>,
    /// Paper used for new pages.
    pub paper_defaults: Paper,
    /// Optional reusable title block.
    pub title_block: Option<TitleBlockTemplate>,
    /// Pen and text defaults.
    pub styles: SheetStyles,
}
impl SheetSet {
    /// An empty set does not add parts to older documents.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
    /// Every embedded image used by a sheet item.
    pub fn image_refs(&self) -> impl Iterator<Item = ImageRef> + '_ {
        self.pages
            .iter()
            .flat_map(|p| &p.items)
            .filter_map(|e| match &e.item {
                SheetItem::Image { image, .. } => Some(*image),
                SheetItem::ModelView { frozen, .. } => *frozen,
                _ => None,
            })
    }
    /// Looks up a page by unique name.
    pub fn page(&self, name: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.name == name)
    }
}
/// Sheet defaults, separated from pages so undo copies only metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetSettings {
    /// Paper for newly added pages.
    pub paper_defaults: Paper,
    /// Reusable title-block fields.
    pub title_block: Option<TitleBlockTemplate>,
    /// Text and pen defaults.
    pub styles: SheetStyles,
}
impl From<&SheetSet> for SheetSettings {
    fn from(s: &SheetSet) -> Self {
        Self {
            paper_defaults: s.paper_defaults.clone(),
            title_block: s.title_block.clone(),
            styles: s.styles.clone(),
        }
    }
}
/// One paper page, with items in stacking order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Page {
    /// Unique page name and print label.
    pub name: String,
    /// Paper dimensions and margin.
    pub paper: Paper,
    /// Items in back-to-front order.
    pub items: Vec<Entry>,
    /// Layers in back-to-front order.
    pub layers: Vec<Layer>,
}
/// One identifiable item assigned to a layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Entry {
    /// Stable identity.
    pub id: ItemId,
    /// Existing layer name.
    pub layer: String,
    /// Graphic content.
    pub item: SheetItem,
}
/// A point on paper, or a live anchor in the model through a viewport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SheetAnchor {
    /// Absolute paper point, mm.
    Paper {
        /// Position.
        point: [f64; 2],
    },
    /// Model anchor resolved on every layout.
    Model {
        /// Persistent model reference.
        anchor: Anchor,
    },
}
/// Dimension measurement direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DimensionKind {
    /// True point-to-point distance.
    #[default]
    Aligned,
    /// Horizontal projected distance.
    Horizontal,
    /// Vertical projected distance.
    Vertical,
    /// Radius from the first point to the second.
    Radius,
    /// Twice the distance from the first point to the second.
    Diameter,
}
/// A dimension's archival measurement while its view is frozen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FrozenDimension {
    /// Endpoint paper offsets from the viewport's lower-left corner.
    pub points: [[f64; 2]; 2],
    /// Original measured model length, mm.
    pub value_mm: f64,
    /// Formatted text at freeze time.
    pub text: String,
}
/// One styled text run, with height in millimeters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TextRun {
    /// Unicode text.
    pub text: String,
    /// Font size expressed as cap-independent em height, mm.
    pub height: f64,
    /// Text color.
    pub color: Color,
}
/// Text alignment inside its frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Alignment {
    /// Left aligned.
    #[default]
    Left,
    /// Centered.
    Center,
    /// Right aligned.
    Right,
}
/// Shape outline interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ShapeKind {
    /// Open or closed polygonal path.
    #[default]
    Polyline,
    /// Opposite corners define a rectangle.
    Rectangle,
    /// Opposite corners define an ellipse.
    Ellipse,
}
/// Paper-space graphics and linked model content (docs/15 §2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SheetItem {
    /// Scaled, optionally scene-linked model viewport.
    ModelView {
        /// Scene name; resolved at refresh/export.
        scene: Option<String>,
        /// Saved camera; a linked scene's camera takes precedence.
        camera: Camera,
        /// Pin the camera while retaining the linked scene's display state.
        #[serde(default)]
        camera_override: bool,
        /// Rational paper/model scale.
        scale: Scale,
        /// Optional named style override.
        style: Option<String>,
        /// Drawing mode.
        render: RenderMode,
        /// Viewport bounds in mm.
        frame: Rect,
        /// Optional convex clipping polygon in paper coordinates.
        #[serde(default)]
        clip: Vec<[f64; 2]>,
        /// Tag visibility overrides by name.
        #[serde(default)]
        tags_override: BTreeMap<String, bool>,
        /// Section activation overrides by persistent id string.
        #[serde(default)]
        section_override: BTreeMap<String, bool>,
        /// Stored raster snapshot; frozen views do not follow model changes.
        #[serde(default)]
        frozen: Option<ImageRef>,
    },
    /// A live dimension measured through a model view, or on paper.
    Dimension {
        /// View used by model anchors.
        view: Option<ItemId>,
        /// First anchor.
        start: SheetAnchor,
        /// Second anchor.
        end: SheetAnchor,
        /// Measurement kind.
        dimension_kind: DimensionKind,
        /// Archival measurement, controlled by freezing the associated view.
        #[serde(default)]
        frozen: Option<FrozenDimension>,
        /// Offset from the measured points in paper mm.
        offset: [f64; 2],
        /// Text override; `<>` stands for the current measurement.
        text: Option<String>,
    },
    /// Rich text within a paper rectangle.
    Text {
        /// Runs in reading order; newline starts another line.
        runs: Vec<TextRun>,
        /// Text rectangle.
        frame: Rect,
        /// Horizontal alignment.
        alignment: Alignment,
        /// Optional leader endpoints in paper coordinates.
        leader: Option<[[f64; 2]; 2]>,
    },
    /// Explicit cells or a live query table.
    Table {
        /// Bounds.
        frame: Rect,
        /// Literal cells; first row is the heading.
        rows: Vec<Vec<String>>,
        /// Entity query, or `component-report` for component quantities.
        query: Option<String>,
        /// Query fields: pid, kind, name, length, area, volume, attr.dict.key.
        columns: Vec<String>,
    },
    /// Embedded image.
    Image {
        /// Content-addressed stored image.
        image: ImageRef,
        /// Paper bounds.
        frame: Rect,
    },
    /// Polyline, rectangle or ellipse.
    Shape {
        /// Outline kind.
        shape: ShapeKind,
        /// Paper control points.
        points: Vec<[f64; 2]>,
        /// Close the polygonal path.
        closed: bool,
        /// Pen.
        stroke: Stroke,
        /// Optional fill color.
        fill: Option<Color>,
    },
    /// Explicit metadata laid out as labeled rows.
    TitleBlock {
        /// Paper bounds.
        frame: Rect,
        /// Project, sheet, author, date, revision, etc.
        fields: BTreeMap<String, String>,
    },
    /// Live text bound to a model attribute or view's scene name.
    Label {
        /// Paper bounds.
        frame: Rect,
        /// `title`, `author`, `scene:<item-id>` or `attr.<dictionary>.<key>`.
        binding: String,
        /// Prefix.
        prefix: String,
    },
}
impl SheetItem {
    /// The bounding frame when the item has a rectangular frame.
    pub fn frame(&self) -> Option<Rect> {
        match self {
            Self::ModelView { frame, .. }
            | Self::Text { frame, .. }
            | Self::Table { frame, .. }
            | Self::Image { frame, .. }
            | Self::TitleBlock { frame, .. }
            | Self::Label { frame, .. } => Some(*frame),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
