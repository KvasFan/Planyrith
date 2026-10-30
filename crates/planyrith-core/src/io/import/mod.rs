//! Implements docs/09-file-formats.md §5–§6 (importers): the `Importer`
//! contract, common options and reports. One sub-module per format
//! (`obj`, `stl`, `gltf`, `dxf`, `step` (feature `brep`), `image`, `csv`,
//! `plyr`) is added at milestone 0.5 (docs/13).
//!
//! Rules (docs/09 §6): boundary edges go through the kernel (`add_edge`);
//! interior triangles of large meshes use `HalfEdgeMesh::from_triangles_merged`;
//! unsupported entities yield `W2004`, never a failure.
//!
//! Owner key: `later`.

pub mod build;
pub mod csv;
pub mod dxf;
pub mod gltf;
pub mod image;
pub mod obj;
pub mod plyr;
pub mod skp;
pub mod stl;

use std::io::Read;

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::Transaction;
use crate::diagnostics::Warning;
use crate::ids::Pid;
use crate::io::{IoError, Progress};
use crate::units::{Angle, LengthUnit};

/// Import formats (docs/08 §2 `--format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImportFormat {
    /// Wavefront OBJ (+MTL).
    Obj,
    /// STL ASCII/binary.
    Stl,
    /// glTF 2.0 / GLB.
    Gltf,
    /// DXF.
    Dxf,
    /// STEP AP203/214 (feature `brep`).
    Step,
    /// PNG/JPEG/WebP as image entity or texture.
    Image,
    /// CSV points.
    Csv,
    /// Another `.plyr`/`.plyrc` merged as group/component.
    Plyr,
    /// SKP through the optional converter process.
    Skp,
}

impl ImportFormat {
    /// Every import format.
    pub const ALL: [ImportFormat; 9] = [
        ImportFormat::Obj,
        ImportFormat::Stl,
        ImportFormat::Gltf,
        ImportFormat::Dxf,
        ImportFormat::Step,
        ImportFormat::Image,
        ImportFormat::Csv,
        ImportFormat::Plyr,
        ImportFormat::Skp,
    ];

    /// Registered formats; SKP additionally requires a converter-capable backend.
    pub fn supported() -> Vec<ImportFormat> {
        Self::ALL
            .into_iter()
            .filter(|f| importer(*f).is_some())
            .collect()
    }

    /// Format from a file extension (case-insensitive), docs/08 §2 autodetection.
    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "obj" => ImportFormat::Obj,
            "stl" => ImportFormat::Stl,
            "gltf" | "glb" => ImportFormat::Gltf,
            "dxf" => ImportFormat::Dxf,
            "step" | "stp" => ImportFormat::Step,
            "png" | "jpg" | "jpeg" | "webp" => ImportFormat::Image,
            "csv" => ImportFormat::Csv,
            "plyr" | "plyrc" => ImportFormat::Plyr,
            "skp" => ImportFormat::Skp,
            _ => return None,
        })
    }
}

/// How confident an importer is that it can read some bytes (docs/09 §6 `probe`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Confidence(pub u8);

impl Confidence {
    /// Definitely not this format.
    pub const NONE: Confidence = Confidence(0);
    /// Extension matches but content was not checked.
    pub const EXTENSION: Confidence = Confidence(50);
    /// Magic bytes / header recognized.
    pub const CERTAIN: Confidence = Confidence(100);
}

/// What the import becomes (docs/08 §2 `--as`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImportAs {
    /// A group (default).
    Group,
    /// A component definition with this name plus one instance.
    Component(String),
    /// An image entity (image formats only).
    Image,
    /// A material with the image as its texture (image formats only).
    Texture,
}

/// Common import options (docs/09 §6, docs/08 §2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImportOptions {
    /// Source unit; `None`: the file's own (glTF: meters, DXF: its
    /// header) or millimeters.
    pub units: Option<LengthUnit>,
    /// Extra scale factor.
    pub scale: f64,
    /// Swap Y/Z (Y-up sources); `None` = autodetect per format.
    pub swap_yz: Option<bool>,
    /// Merge coplanar adjacent triangles into faces.
    pub merge_coplanar: bool,
    /// Edges between faces below this angle become `SOFT | SMOOTH`.
    pub soften_angle: Angle,
    /// Target kind.
    pub import_as: ImportAs,
    /// Placement of the result.
    #[schemars(with = "[f64; 3]")]
    pub at: DVec3,
    /// Storage key of the file read: names the import and locates side
    /// files (an OBJ's MTL and textures) next to it.
    #[serde(default)]
    pub source: String,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            units: None,
            scale: 1.0,
            swap_yz: None,
            merge_coplanar: true,
            soften_angle: Angle::degrees(20.0),
            import_as: ImportAs::Group,
            at: DVec3::ZERO,
            source: String::new(),
        }
    }
}

/// Import output (docs/08 §2 "lists created entities and warnings").
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ImportReport {
    /// Top-level entities created (the group/instance/image).
    pub created: Vec<Pid>,
    /// Faces created.
    pub faces: usize,
    /// Edges created.
    pub edges: usize,
    /// Unsupported entities skipped (`W2004` details in `warnings`).
    pub skipped: usize,
    /// Warnings.
    pub warnings: Vec<Warning>,
}

/// An importer (docs/09 §6).
pub trait Importer: Send + Sync {
    /// The format handled.
    fn format(&self) -> ImportFormat;
    /// How confidently `bytes` (first KiB suffice) named `name` can be read.
    fn probe(&self, bytes: &[u8], name: &str) -> Confidence;
    /// Imports into `target` (the edit context the command resolved).
    fn import(
        &self,
        src: &mut dyn Read,
        opts: &ImportOptions,
        tx: &mut Transaction<'_>,
        target: &crate::commands::params::Target,
        progress: &dyn Progress,
    ) -> Result<ImportReport, IoError>;
}

/// The file name of `source` without directory and extension (`fallback`
/// when empty).
pub fn source_stem(source: &str, fallback: &str) -> String {
    let file = source.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    if stem.is_empty() {
        fallback.to_string()
    } else {
        stem.to_string()
    }
}

/// The storage key of `name` (relative, `/` or `\\` separated) next to
/// `source`.
pub fn sibling(source: &str, name: &str) -> String {
    let name = name.trim().replace('\\', "/");
    if name.starts_with('/') {
        return name;
    }
    match source.rfind(['/', '\\']) {
        Some(i) => format!("{}/{name}", &source[..i]),
        None => name,
    }
}

/// The importer of `format`, when this build has one.
pub fn importer(format: ImportFormat) -> Option<Box<dyn Importer>> {
    match format {
        #[cfg(feature = "brep")]
        ImportFormat::Step => Some(Box::new(crate::io::step::StepImporter)),
        ImportFormat::Csv => Some(Box::new(csv::CsvImporter)),
        ImportFormat::Dxf => Some(Box::new(dxf::DxfImporter)),
        ImportFormat::Gltf => Some(Box::new(gltf::GltfImporter)),
        ImportFormat::Image => Some(Box::new(image::ImageImporter)),
        ImportFormat::Plyr => Some(Box::new(plyr::PlyrImporter)),
        ImportFormat::Skp => Some(Box::new(skp::SkpImporter)),
        ImportFormat::Obj => Some(Box::new(obj::ObjImporter)),
        ImportFormat::Stl => Some(Box::new(stl::StlImporter)),
        #[cfg(not(feature = "brep"))]
        ImportFormat::Step => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_formats() {
        assert_eq!(
            ImportFormat::from_extension("GLB"),
            Some(ImportFormat::Gltf)
        );
        assert_eq!(
            ImportFormat::from_extension("stp"),
            Some(ImportFormat::Step)
        );
        assert_eq!(ImportFormat::from_extension("txt"), None);
        assert!(Confidence::CERTAIN > Confidence::EXTENSION);
    }
}
