//! Implements docs/04-document-model.md §4 (materials, textures, UV maps, the
//! content-addressed image store) and the `Rgba8` color type shared by tags,
//! styles and attributes.

use std::collections::BTreeMap;
use std::fmt;

use glam::{DAffine2, DMat3, DVec2};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use slotmap::SlotMap;

use crate::document::attributes::AttributeDictionaries;
use crate::ids::{MaterialId, VertexId};
use crate::io::IoError;
use crate::math::Plane;

/// An 8-bit sRGB color with alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
pub struct Rgba8 {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha (255 = opaque).
    pub a: u8,
}

impl Rgba8 {
    /// Opaque white.
    pub const WHITE: Rgba8 = Rgba8::rgb(255, 255, 255);
    /// Opaque black.
    pub const BLACK: Rgba8 = Rgba8::rgb(0, 0, 0);

    /// Opaque color.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Rgba8 { r, g, b, a: 255 }
    }

    /// Color with alpha.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Rgba8 { r, g, b, a }
    }

    /// Parses `#RRGGBB` or `#RRGGBBAA` (case-insensitive, `#` optional).
    pub fn from_hex(text: &str) -> Option<Self> {
        let hex = text.strip_prefix('#').unwrap_or(text);
        let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        match hex.len() {
            6 => Some(Rgba8::rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => Some(Rgba8::rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => None,
        }
    }

    /// Linear `[r, g, b, a]` in `0..1` for GPU buffers (sRGB decoding is the
    /// renderer's job).
    pub fn to_f32_array(self) -> [f32; 4] {
        [
            f32::from(self.r) / 255.0,
            f32::from(self.g) / 255.0,
            f32::from(self.b) / 255.0,
            f32::from(self.a) / 255.0,
        ]
    }
}

impl fmt::Display for Rgba8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.a == 255 {
            write!(f, "#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
        } else {
            write!(
                f,
                "#{:02X}{:02X}{:02X}{:02X}",
                self.r, self.g, self.b, self.a
            )
        }
    }
}

/// Which side of a face (docs/04 §4 `FrontOrBack`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum FrontOrBack {
    /// Along the face normal.
    Front,
    /// Opposite the face normal.
    Back,
}

/// Content hash (blake3) of an image's bytes (docs/04 §4 `ImageRef(hash)`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ImageRef(pub [u8; 32]);

impl ImageRef {
    /// Hashes `bytes` with blake3.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        ImageRef(*blake3::hash(bytes).as_bytes())
    }

    /// Lowercase hex, as used for `images/<hex>.<ext>` in the container
    /// (docs/09 §1).
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl fmt::Display for ImageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// How a texture's colors are altered by the material color (docs/04 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ColorizeMode {
    /// Shift hue/saturation toward the material color.
    Shift,
    /// Multiply by the material color.
    Tint,
}

/// A texture reference with its real-world size (docs/04 §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Texture {
    /// Image in the document's [`ImageStore`].
    pub image: ImageRef,
    /// Real-world size of one tile in mm.
    pub size_mm: DVec2,
    /// Optional colorization.
    pub colorize: Option<ColorizeMode>,
}

/// Per-face UV mapping (docs/04 §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UvMap {
    /// Affine map in the face frame.
    Affine(DAffine2),
    /// Projected from a plane, then affine.
    Projected {
        /// Projection plane.
        plane: Plane,
        /// Affine map in the plane frame.
        affine: DAffine2,
    },
    /// Projective map (4-point pins that are not affine).
    Projective(DMat3),
    /// Explicit per-vertex UVs.
    Fixed(Vec<(VertexId, DVec2)>),
}

/// A material (docs/04 §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Material {
    /// Unique, user-visible name.
    pub name: String,
    /// Base color (`color.a` mirrors `opacity`).
    pub color: Rgba8,
    /// Opacity `0..1`.
    pub opacity: f32,
    /// Optional texture.
    pub texture: Option<Texture>,
    /// Set on the two built-in "Default" materials.
    pub is_default_for: Option<FrontOrBack>,
    /// Attributes.
    pub attrs: AttributeDictionaries,
}

impl Material {
    /// Name of the built-in default front material.
    pub const DEFAULT_FRONT_NAME: &'static str = "Default";
    /// Name of the built-in default back material.
    pub const DEFAULT_BACK_NAME: &'static str = "Default Back";

    /// An opaque, untextured material.
    pub fn solid(name: impl Into<String>, color: Rgba8) -> Self {
        Material {
            name: name.into(),
            color,
            opacity: 1.0,
            texture: None,
            is_default_for: None,
            attrs: AttributeDictionaries::default(),
        }
    }
}

/// Errors from material bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MaterialError {
    /// A material with this name exists.
    #[error("material name already used: {0}")]
    DuplicateName(String),
    /// The built-in defaults cannot be removed.
    #[error("the default materials cannot be removed")]
    DefaultProtected,
    /// Unknown id.
    #[error("material not found: {0}")]
    NotFound(MaterialId),
}

/// The document's material list (docs/04 §1 "Materials: name → Material").
/// Insertion order is kept in `order` for deterministic listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Materials {
    items: SlotMap<MaterialId, Material>,
    order: Vec<MaterialId>,
    default_front: MaterialId,
    default_back: MaterialId,
}

impl Default for Materials {
    fn default() -> Self {
        let mut items = SlotMap::with_key();
        let mut front = Material::solid(Material::DEFAULT_FRONT_NAME, Rgba8::WHITE);
        front.is_default_for = Some(FrontOrBack::Front);
        let mut back = Material::solid(Material::DEFAULT_BACK_NAME, Rgba8::rgb(0xA0, 0xB4, 0xC8));
        back.is_default_for = Some(FrontOrBack::Back);
        let default_front = items.insert(front);
        let default_back = items.insert(back);
        Materials {
            items,
            order: vec![default_front, default_back],
            default_front,
            default_back,
        }
    }
}

impl Materials {
    /// The two default materials only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of materials (including the two defaults).
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Never true: the defaults always exist.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Material by id.
    pub fn get(&self, id: MaterialId) -> Option<&Material> {
        self.items.get(id)
    }

    /// Material by name.
    pub fn by_name(&self, name: &str) -> Option<MaterialId> {
        self.order
            .iter()
            .copied()
            .find(|id| self.items.get(*id).is_some_and(|m| m.name == name))
    }

    /// The default material for a side.
    pub fn default_for(&self, side: FrontOrBack) -> MaterialId {
        match side {
            FrontOrBack::Front => self.default_front,
            FrontOrBack::Back => self.default_back,
        }
    }

    /// Resolves `Option<MaterialId>` as faces store it (`None` = default).
    pub fn resolve(&self, id: Option<MaterialId>, side: FrontOrBack) -> MaterialId {
        id.unwrap_or_else(|| self.default_for(side))
    }

    /// Materials in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (MaterialId, &Material)> + '_ {
        self.order
            .iter()
            .filter_map(|id| self.items.get(*id).map(|m| (*id, m)))
    }

    /// Adds a material; names must be unique.
    pub(crate) fn insert(&mut self, material: Material) -> Result<MaterialId, MaterialError> {
        todo!("docs/04 §4 insert material")
    }

    /// Replaces a material's data (same id).
    pub(crate) fn replace(
        &mut self,
        id: MaterialId,
        material: Material,
    ) -> Result<Material, MaterialError> {
        todo!("docs/04 §4 replace material")
    }

    /// Removes a non-default material.
    pub(crate) fn remove(&mut self, id: MaterialId) -> Result<Material, MaterialError> {
        todo!("docs/04 §4 remove material")
    }
}

/// Supported image encodings (docs/04 §4, docs/09 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// WebP.
    WebP,
}

impl ImageFormat {
    /// File extension used inside the container (docs/09 §1).
    pub const fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::WebP => "webp",
        }
    }
}

/// Encoded image bytes plus decoded metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageData {
    /// Encoded bytes (never re-encoded).
    pub bytes: Vec<u8>,
    /// Encoding.
    pub format: ImageFormat,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
}

/// Content-addressed image store (docs/04 §4). Not serialized with the
/// document body: the native writer stores each image as its own container
/// part (docs/09 §1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageStore {
    images: BTreeMap<ImageRef, ImageData>,
}

impl ImageStore {
    /// Maximum side length in pixels (docs/04 §4).
    pub const MAX_SIDE_PX: u32 = 16_384;

    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds already-validated image data; returns its content hash.
    pub fn insert(&mut self, data: ImageData) -> ImageRef {
        let key = ImageRef::from_bytes(&data.bytes);
        self.images.entry(key).or_insert(data);
        key
    }

    /// Decodes the header of `bytes` (format, width, height) with the `image`
    /// crate and inserts; rejects sides above [`ImageStore::MAX_SIDE_PX`].
    pub fn insert_encoded(&mut self, bytes: Vec<u8>) -> Result<ImageRef, IoError> {
        todo!("docs/04 §4 image header decode and size cap")
    }

    /// Image by hash.
    pub fn get(&self, id: ImageRef) -> Option<&ImageData> {
        self.images.get(&id)
    }

    /// `true` when the hash is stored.
    pub fn contains(&self, id: ImageRef) -> bool {
        self.images.contains_key(&id)
    }

    /// Removes an image (callers ensure it is unreferenced; `purge-unused`).
    pub fn remove(&mut self, id: ImageRef) -> Option<ImageData> {
        self.images.remove(&id)
    }

    /// Images in hash order.
    pub fn iter(&self) -> impl Iterator<Item = (ImageRef, &ImageData)> + '_ {
        self.images.iter().map(|(k, v)| (*k, v))
    }

    /// Number of images.
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// `true` when empty.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_hex_round_trip() {
        let c = Rgba8::from_hex("#1A2b3C").unwrap();
        assert_eq!(c, Rgba8::rgb(0x1A, 0x2B, 0x3C));
        assert_eq!(c.to_string(), "#1A2B3C");
        assert_eq!(Rgba8::from_hex("1A2B3C80").unwrap().a, 0x80);
        assert!(Rgba8::from_hex("12345").is_none());
    }

    #[test]
    fn defaults_exist_and_resolve() {
        let m = Materials::new();
        assert_eq!(m.len(), 2);
        let front = m.default_for(FrontOrBack::Front);
        assert_eq!(m.by_name(Material::DEFAULT_FRONT_NAME), Some(front));
        assert_eq!(m.resolve(None, FrontOrBack::Front), front);
        assert_eq!(m.iter().count(), 2);
    }

    #[test]
    fn image_store_is_content_addressed() {
        let mut store = ImageStore::new();
        let data = ImageData {
            bytes: vec![1, 2, 3],
            format: ImageFormat::Png,
            width: 1,
            height: 1,
        };
        let a = store.insert(data.clone());
        let b = store.insert(data);
        assert_eq!(a, b);
        assert_eq!(store.len(), 1);
        assert_eq!(a.to_hex().len(), 64);
    }
}
