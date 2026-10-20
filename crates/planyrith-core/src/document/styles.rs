//! Implements docs/04-document-model.md §7 (styles): named rendering presets
//! for edges, faces, background, watermarks, modeling colors and fog.

use serde::{Deserialize, Serialize};

use crate::document::materials::{ImageRef, Rgba8};
use crate::ids::StyleId;

/// How edge colors are chosen (docs/04 §7 `EdgeColorMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeColorMode {
    /// One color for all edges.
    All,
    /// Edge color from the adjacent material.
    ByMaterial,
    /// Axis-aligned edges in axis colors.
    ByAxis,
}

/// Edge rendering settings (docs/04 §7 `EdgeStyle`). Optional `u8` values are
/// pixel widths; `None` disables the effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeStyle {
    /// Draw edges at all.
    pub show: bool,
    /// Profile (silhouette) edge width.
    pub profiles: Option<u8>,
    /// Depth cue strength.
    pub depth_cue: Option<u8>,
    /// Extension length past vertices.
    pub extension: Option<u8>,
    /// Endpoint emphasis width.
    pub endpoints: Option<u8>,
    /// Sketchy jitter.
    pub jitter: bool,
    /// Color mode.
    pub color: EdgeColorMode,
    /// Draw back edges dashed.
    pub back_edges: bool,
}

/// Face rendering mode (docs/04 §7 `FaceStyle::mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaceMode {
    /// Edges only.
    Wireframe,
    /// White faces, hidden lines removed.
    HiddenLine,
    /// Shaded with material colors.
    Shaded,
    /// Shaded with textures.
    ShadedTextured,
    /// Default front/back colors only.
    Monochrome,
    /// Translucent faces.
    XRay,
}

/// Face rendering settings (docs/04 §7 `FaceStyle`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceStyle {
    /// Mode.
    pub mode: FaceMode,
    /// Default front color.
    pub front_default: Rgba8,
    /// Default back color.
    pub back_default: Rgba8,
}

/// Background settings (docs/04 §7 `Background`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Background {
    /// Base color.
    pub color: Rgba8,
    /// Sky gradient color.
    pub sky: Option<Rgba8>,
    /// Ground color and transparency.
    pub ground: Option<(Rgba8, f32)>,
    /// Show the ground plane when looking from below.
    pub show_ground_from_below: bool,
}

/// Corner anchoring for watermarks.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WatermarkPlacement {
    /// Stretched over the viewport.
    Stretched,
    /// Tiled.
    Tiled,
    /// Centered.
    Centered,
    /// Anchored at a corner: `(horizontal 0..1, vertical 0..1)`.
    Anchored(f32, f32),
}

/// A watermark image drawn over or behind the model (docs/04 §7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Watermark {
    /// Image.
    pub image: ImageRef,
    /// Placement.
    pub placement: WatermarkPlacement,
    /// Blend opacity `0..1`.
    pub opacity: f32,
    /// Scale relative to the viewport.
    pub scale: f32,
    /// Draw behind the model instead of over it.
    pub behind_model: bool,
}

/// Colors and widths used while modeling (docs/04 §7 `ModelingStyle`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelingStyle {
    /// Selection highlight.
    pub selection_color: Rgba8,
    /// Locked entity highlight.
    pub locked_color: Rgba8,
    /// Guide color.
    pub guide_color: Rgba8,
    /// Fade factor for entities outside the edit context `0..1`.
    pub inactive_fade: f32,
    /// Section cap fill color.
    pub section_fill: Rgba8,
    /// Section cut line color.
    pub section_line_color: Rgba8,
    /// Section cut line width in px.
    pub section_line_width_px: f32,
}

/// Distance fog (docs/04 §7 `Fog`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Fog {
    /// Start distance in mm.
    pub start: f64,
    /// Full-fog distance in mm.
    pub end: f64,
    /// Use the background color.
    pub use_background_color: bool,
    /// Explicit fog color.
    pub color: Rgba8,
}

/// A style preset (docs/04 §7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Style {
    /// Unique name.
    pub name: String,
    /// Edge settings.
    pub edges: EdgeStyle,
    /// Face settings.
    pub faces: FaceStyle,
    /// Background.
    pub background: Background,
    /// Watermarks.
    pub watermarks: Vec<Watermark>,
    /// Modeling colors.
    pub modeling: ModelingStyle,
    /// Fog.
    pub fog: Option<Fog>,
}

impl Default for Style {
    /// The built-in "Default" style: shaded with textures, thin dark edges,
    /// light background.
    fn default() -> Self {
        Style {
            name: "Default".to_string(),
            edges: EdgeStyle {
                show: true,
                profiles: Some(2),
                depth_cue: None,
                extension: None,
                endpoints: None,
                jitter: false,
                color: EdgeColorMode::All,
                back_edges: false,
            },
            faces: FaceStyle {
                mode: FaceMode::ShadedTextured,
                front_default: Rgba8::WHITE,
                back_default: Rgba8::rgb(0xA0, 0xB4, 0xC8),
            },
            background: Background {
                color: Rgba8::rgb(0xFF, 0xFF, 0xFF),
                sky: Some(Rgba8::rgb(0xCE, 0xE1, 0xF2)),
                ground: None,
                show_ground_from_below: false,
            },
            watermarks: Vec::new(),
            modeling: ModelingStyle {
                selection_color: Rgba8::rgb(0x1E, 0x90, 0xFF),
                locked_color: Rgba8::rgb(0xD0, 0x30, 0x30),
                guide_color: Rgba8::rgb(0x40, 0x40, 0x40),
                inactive_fade: 0.5,
                section_fill: Rgba8::rgb(0x40, 0x40, 0x40),
                section_line_color: Rgba8::BLACK,
                section_line_width_px: 3.0,
            },
            fog: None,
        }
    }
}

/// The document's styles plus the active one (docs/04 §1 "Styles … (+ the
/// active one)").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Styles {
    items: crate::document::StableMap<StyleId, Style>,
    order: Vec<StyleId>,
    active: StyleId,
}

impl Default for Styles {
    fn default() -> Self {
        let mut items = crate::document::StableMap::with_key();
        let active = items.insert(Style::default());
        Styles {
            items,
            order: vec![active],
            active,
        }
    }
}

impl Styles {
    /// One default style, active.
    pub fn new() -> Self {
        Self::default()
    }

    /// The active style id.
    pub fn active(&self) -> StyleId {
        self.active
    }

    /// The active style.
    pub fn active_style(&self) -> Option<&Style> {
        self.items.get(self.active)
    }

    /// Style by id.
    pub fn get(&self, id: StyleId) -> Option<&Style> {
        self.items.get(id)
    }

    /// Style by name.
    pub fn by_name(&self, name: &str) -> Option<StyleId> {
        self.order
            .iter()
            .copied()
            .find(|id| self.items.get(*id).is_some_and(|s| s.name == name))
    }

    /// Styles in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (StyleId, &Style)> + '_ {
        self.order
            .iter()
            .filter_map(|id| self.items.get(*id).map(|s| (*id, s)))
    }

    /// Number of styles (≥ 1).
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Never true.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Adds a style.
    pub(crate) fn insert(&mut self, style: Style) -> StyleId {
        let id = self.items.insert(style);
        self.order.push(id);
        id
    }

    /// Makes `id` active; returns the previous active id or `None` if unknown.
    pub(crate) fn set_active(&mut self, id: StyleId) -> Option<StyleId> {
        self.items
            .contains_key(id)
            .then(|| std::mem::replace(&mut self.active, id))
    }

    /// Replaces a style's data.
    pub(crate) fn replace(&mut self, id: StyleId, style: Style) -> Option<Style> {
        self.items
            .get_mut(id)
            .map(|slot| std::mem::replace(slot, style))
    }

    /// Position of a style in the insertion order.
    pub fn position(&self, id: StyleId) -> Option<usize> {
        self.order.iter().position(|s| *s == id)
    }

    /// Puts a removed style back under its id at `index` (undo of a
    /// removal); `false` when `id` is not a removed style of this list.
    pub(crate) fn restore(&mut self, id: StyleId, style: Style, index: usize) -> bool {
        if self.items.restore(id, style).is_err() {
            return false;
        }
        self.order.insert(index.min(self.order.len()), id);
        true
    }

    /// Removes a non-active style; its id stays reserved for
    /// [`Styles::restore`].
    pub(crate) fn remove(&mut self, id: StyleId) -> Option<Style> {
        if id == self.active {
            return None;
        }
        let index = self.order.iter().position(|s| *s == id)?;
        self.order.remove(index);
        self.items.remove(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_style_is_active_and_protected() {
        let mut styles = Styles::new();
        let active = styles.active();
        assert_eq!(styles.by_name("Default"), Some(active));
        assert!(styles.remove(active).is_none());
        let other = styles.insert(Style {
            name: "Other".into(),
            ..Style::default()
        });
        assert_eq!(styles.set_active(other), Some(active));
        assert!(styles.remove(active).is_some());
        assert_eq!(styles.len(), 1);
    }
}
