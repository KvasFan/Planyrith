//! Implements docs/15-sheets-2d-documentation.md §9 (engineering drawings,
//! milestone 1.13): the data of derived views, breaks and annotation
//! symbols. They are optional fields of existing sheet items, so the frozen
//! file format only grows (docs/09 §4) and older readers draw a derived
//! view with its last saved camera and an annotation as its plain text.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::sheets::ItemId;

/// Where a projected view sits next to its base view on paper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    /// To the left.
    Left,
    /// To the right.
    Right,
    /// Above.
    Above,
    /// Below.
    Below,
}

/// The projection method of a drawing (ISO 128 / ISO 5456-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Projection {
    /// First angle (ISO, Europe): the view placed to the right shows the
    /// object's left side.
    #[default]
    FirstAngle,
    /// Third angle (ANSI, North America): the view placed to the right
    /// shows the object's right side.
    ThirdAngle,
}

/// How a view follows another view of the same page (docs/15 §9.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ViewDerivation {
    /// An orthographic projection of the base view, turned a quarter turn.
    Projected {
        /// The base view.
        base: ItemId,
        /// Where it sits on paper.
        side: Side,
        /// First or third angle.
        projection: Projection,
    },
    /// A view along a model direction, aligned with the base view's up.
    Auxiliary {
        /// The base view.
        base: ItemId,
        /// The model direction the view looks along.
        direction: [f64; 3],
    },
    /// A section through a line drawn across the base view, looked at
    /// square to the cut.
    Section {
        /// The base view.
        base: ItemId,
        /// The cutting line on the base view, paper mm.
        line: [[f64; 2]; 2],
        /// Look at the cut from the other side.
        #[serde(default)]
        flip: bool,
        /// The section's letter (`A` gives SECTION A-A).
        label: String,
    },
    /// An enlarged circle of the base view.
    Detail {
        /// The base view.
        base: ItemId,
        /// The circle's center on the base view, paper mm.
        center: [f64; 2],
        /// Its radius on the base view, paper mm.
        radius: f64,
        /// The detail's letter.
        label: String,
    },
}

impl ViewDerivation {
    /// The base view.
    pub fn base(&self) -> &ItemId {
        match self {
            Self::Projected { base, .. }
            | Self::Auxiliary { base, .. }
            | Self::Section { base, .. }
            | Self::Detail { base, .. } => base,
        }
    }
}

/// The axis along which a broken view is shortened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BreakAxis {
    /// The band runs across the paper's width: a long horizontal part.
    Horizontal,
    /// The band runs across its height: a tall part.
    Vertical,
}

/// A band removed from a view (a broken view, docs/15 §9.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ViewBreak {
    /// Along which paper axis the view is shortened.
    pub axis: BreakAxis,
    /// Start of the removed band, paper mm (x for horizontal, y for
    /// vertical).
    pub from: f64,
    /// End of the removed band.
    pub to: f64,
    /// The gap left between the two break lines, mm.
    pub gap: f64,
}

/// A geometric characteristic (ISO 1101 / ASME Y14.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Characteristic {
    /// Straightness.
    Straightness,
    /// Flatness.
    Flatness,
    /// Circularity (roundness).
    Circularity,
    /// Cylindricity.
    Cylindricity,
    /// Profile of a line.
    ProfileLine,
    /// Profile of a surface.
    ProfileSurface,
    /// Angularity.
    Angularity,
    /// Perpendicularity.
    Perpendicularity,
    /// Parallelism.
    Parallelism,
    /// Position.
    Position,
    /// Concentricity (coaxiality).
    Concentricity,
    /// Symmetry.
    Symmetry,
    /// Circular run-out.
    CircularRunout,
    /// Total run-out.
    TotalRunout,
}

/// A material condition modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Modifier {
    /// Maximum material condition (Ⓜ).
    Maximum,
    /// Least material condition (Ⓛ).
    Least,
}

/// What a surface texture symbol requires (ISO 1302 / ISO 21920).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Machining {
    /// Any process.
    #[default]
    Any,
    /// Material must be removed.
    Required,
    /// Material must not be removed.
    Prohibited,
}

/// A weld's shape (ISO 2553 / AWS A2.4, basic symbols).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WeldKind {
    /// Fillet.
    Fillet,
    /// Square butt.
    Square,
    /// Single-V butt.
    V,
    /// Single-bevel butt.
    Bevel,
    /// Plug or slot.
    Plug,
    /// Spot.
    Spot,
}

/// A structured annotation drawn as vector symbols (docs/15 §9.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Annotation {
    /// A feature control frame.
    FeatureControl {
        /// The characteristic.
        characteristic: Characteristic,
        /// The tolerance value, as written (`0.05`).
        tolerance: String,
        /// A cylindrical (⌀) tolerance zone.
        #[serde(default)]
        diameter: bool,
        /// A material condition modifier.
        #[serde(default)]
        modifier: Option<Modifier>,
        /// Datum references in order (`A`, `B`, `C`).
        #[serde(default)]
        datums: Vec<String>,
    },
    /// A datum feature symbol.
    Datum {
        /// The datum's letter.
        letter: String,
    },
    /// A surface texture symbol.
    SurfaceFinish {
        /// The requirement, as written (`Ra 1.6`).
        value: String,
        /// Whether material is removed.
        #[serde(default)]
        machining: Machining,
    },
    /// A weld symbol on a reference line.
    Weld {
        /// The weld's shape.
        kind: WeldKind,
        /// Its size, as written (`a5`).
        #[serde(default)]
        size: String,
        /// Its length, as written (`100`).
        #[serde(default)]
        length: String,
        /// The weld is on the other side of the joint.
        #[serde(default)]
        other_side: bool,
        /// Weld all around.
        #[serde(default)]
        all_around: bool,
        /// Weld on site (a flag).
        #[serde(default)]
        field: bool,
    },
    /// An item balloon.
    Balloon {
        /// The item number.
        text: String,
    },
}

impl Annotation {
    /// The plain-text form, kept as the text item's runs for readers that
    /// do not draw symbols.
    pub fn plain(&self) -> String {
        match self {
            Self::FeatureControl {
                characteristic,
                tolerance,
                diameter,
                modifier,
                datums,
            } => {
                let mut s = format!(
                    "[{characteristic:?} | {}{tolerance}",
                    if *diameter { "⌀" } else { "" }
                );
                match modifier {
                    Some(Modifier::Maximum) => s.push_str(" (M)"),
                    Some(Modifier::Least) => s.push_str(" (L)"),
                    None => {}
                }
                for d in datums {
                    s.push_str(&format!(" | {d}"));
                }
                s.push(']');
                s
            }
            Self::Datum { letter } => format!("[{letter}]"),
            Self::SurfaceFinish { value, .. } => format!("√ {value}"),
            Self::Weld {
                kind, size, length, ..
            } => format!("{kind:?} {size} {length}").trim().to_string(),
            Self::Balloon { text } => format!("({text})"),
        }
    }

    /// Checks the written values.
    pub fn check(&self) -> Result<(), String> {
        let short = |s: &str, what: &str| {
            if s.chars().count() > 40 {
                Err(format!("{what} is too long"))
            } else {
                Ok(())
            }
        };
        match self {
            Self::FeatureControl {
                tolerance, datums, ..
            } => {
                if tolerance.trim().is_empty() {
                    return Err("give the tolerance".into());
                }
                short(tolerance, "the tolerance")?;
                if datums.len() > 3
                    || datums
                        .iter()
                        .any(|d| d.trim().is_empty() || d.chars().count() > 3)
                {
                    return Err("up to three datums of one to three letters".into());
                }
                Ok(())
            }
            Self::Datum { letter } => {
                if letter.trim().is_empty() || letter.chars().count() > 3 {
                    return Err("a datum is one to three letters".into());
                }
                Ok(())
            }
            Self::SurfaceFinish { value, .. } => short(value, "the value"),
            Self::Weld { size, length, .. } => {
                short(size, "the size")?;
                short(length, "the length")
            }
            Self::Balloon { text } => {
                if text.trim().is_empty() {
                    return Err("a balloon needs its item number".into());
                }
                short(text, "the text")
            }
        }
    }
}
