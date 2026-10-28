//! Implements docs/09-file-formats.md §5–§6 (exporters): the `Exporter`
//! contract, scope and options. `json` ships with the skeleton (milestone 0.1,
//! docs/13); `obj`, `stl`, `gltf`, `dxf`, `step`, `csv`, `svg`, `pdf` follow at
//! 0.5/0.9.
//!
//! Rules (docs/09 §6): exporters are deterministic (stable ordering by pid) and
//! stream their output.

pub mod csv;
pub mod dxf;
pub mod gltf;
pub mod json;
pub mod obj;
pub mod plyr;
pub mod scene;
pub mod stl;
pub mod view2d;

use std::io::Write;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::diagnostics::Warning;
use crate::document::{Document, Selection};
use crate::io::{IoError, Progress};
use crate::units::LengthUnit;

/// Export formats (docs/08 §2 `export --format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    /// Wavefront OBJ (+MTL).
    Obj,
    /// STL (binary default).
    Stl,
    /// glTF 2.0 (JSON + buffers).
    Gltf,
    /// GLB.
    Glb,
    /// DXF 3D or 2D.
    Dxf,
    /// STEP (feature `brep`).
    Step,
    /// Native container.
    Plyr,
    /// Full document dump.
    Json,
    /// 2D vector view/sheet.
    Svg,
    /// 2D vector view/sheet.
    Pdf,
    /// Component report.
    Csv,
}

impl ExportFormat {
    /// Every export format.
    pub const ALL: [ExportFormat; 11] = [
        ExportFormat::Obj,
        ExportFormat::Stl,
        ExportFormat::Gltf,
        ExportFormat::Glb,
        ExportFormat::Dxf,
        ExportFormat::Step,
        ExportFormat::Plyr,
        ExportFormat::Json,
        ExportFormat::Svg,
        ExportFormat::Pdf,
        ExportFormat::Csv,
    ];

    /// The formats this build exports.
    pub fn supported() -> Vec<ExportFormat> {
        Self::ALL
            .into_iter()
            .filter(|f| exporter(*f).is_some())
            .collect()
    }

    /// Format from a file extension (case-insensitive).
    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "obj" => ExportFormat::Obj,
            "stl" => ExportFormat::Stl,
            "gltf" => ExportFormat::Gltf,
            "glb" => ExportFormat::Glb,
            "dxf" => ExportFormat::Dxf,
            "step" | "stp" => ExportFormat::Step,
            "plyr" => ExportFormat::Plyr,
            "json" => ExportFormat::Json,
            "svg" => ExportFormat::Svg,
            "pdf" => ExportFormat::Pdf,
            "csv" => ExportFormat::Csv,
            _ => return None,
        })
    }
}

/// What to export (docs/08 §2 `--scene`, `--selection-only`, `--include-hidden`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExportScope {
    /// Only these occurrences (`None` = whole model).
    pub selection: Option<Selection>,
    /// Apply this scene's visibility/camera first.
    pub scene: Option<String>,
    /// Include hidden entities and hidden tags.
    pub include_hidden: bool,
}

/// Common export options (docs/09 §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportOptions {
    /// Output unit for unitless formats.
    pub units: LengthUnit,
    /// Triangulate faces.
    pub triangulate: bool,
    /// One object/layer per tag.
    pub per_tag: bool,
    /// Copy textures next to the output into this directory.
    pub copy_textures: Option<String>,
    /// Canonical output (sorted keys, fixed float formatting) for diffing
    /// (docs/08 §10 `--canonical`).
    pub canonical: bool,
    /// Pretty-print text formats.
    pub pretty: bool,
    /// ASCII instead of binary (STL).
    pub ascii: bool,
    /// Keep Z up in formats that are Y-up by default (OBJ, STL).
    pub z_up: bool,
    /// Storage key of the file written: names side files (an OBJ's MTL
    /// and textures).
    #[serde(default)]
    pub destination: String,
    /// A 2D drawing of the view instead of 3D geometry (DXF; SVG and PDF
    /// are always 2D).
    #[serde(default)]
    pub two_d: bool,
    /// Paper per model length of a parallel view (`1:50` → 0.02).
    #[serde(default)]
    pub scale: Option<f64>,
    /// Paper width of a fitted view, mm.
    #[serde(default)]
    pub paper_width_mm: Option<f64>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        ExportOptions {
            units: LengthUnit::Millimeter,
            triangulate: false,
            per_tag: false,
            copy_textures: None,
            canonical: false,
            pretty: false,
            ascii: false,
            z_up: false,
            destination: String::new(),
            two_d: false,
            scale: None,
            paper_width_mm: None,
        }
    }
}

/// Export output.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ExportReport {
    /// Bytes written.
    pub bytes_written: u64,
    /// Entities exported.
    pub entities: usize,
    /// Warnings.
    pub warnings: Vec<Warning>,
    /// Files to write next to the output (name relative to its folder,
    /// contents), e.g. an OBJ's MTL and textures.
    #[serde(skip)]
    pub side_files: Vec<(String, Vec<u8>)>,
}

/// An exporter (docs/09 §6).
pub trait Exporter: Send + Sync {
    /// The format produced.
    fn format(&self) -> ExportFormat;
    /// Streams the export into `dst`.
    fn export(
        &self,
        doc: &Document,
        scope: &ExportScope,
        opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<ExportReport, IoError>;
}

/// The exporter of `format`, when this build has one.
pub fn exporter(format: ExportFormat) -> Option<Box<dyn Exporter>> {
    match format {
        #[cfg(feature = "brep")]
        ExportFormat::Step => Some(Box::new(crate::io::step::StepExporter)),
        ExportFormat::Csv => Some(Box::new(csv::CsvExporter)),
        ExportFormat::Dxf => Some(Box::new(dxf::DxfExporter)),
        ExportFormat::Gltf => Some(Box::new(gltf::GltfExporter { binary: false })),
        ExportFormat::Glb => Some(Box::new(gltf::GltfExporter { binary: true })),
        ExportFormat::Json => Some(Box::new(json::JsonExporter)),
        ExportFormat::Obj => Some(Box::new(obj::ObjExporter)),
        ExportFormat::Plyr => Some(Box::new(plyr::PlyrExporter)),
        ExportFormat::Stl => Some(Box::new(stl::StlExporter)),
        ExportFormat::Svg => Some(Box::new(view2d::SvgExporter)),
        ExportFormat::Pdf => Some(Box::new(view2d::PdfExporter)),
        #[cfg(not(feature = "brep"))]
        ExportFormat::Step => None,
    }
}

/// Document (mm, Z-up) → output coordinates: `opts.units`, and Y-up
/// unless `z_up` for formats that are Y-up by default.
pub fn output_transform(opts: &ExportOptions, y_up_format: bool) -> glam::DAffine3 {
    let axes = if y_up_format && !opts.z_up {
        // (x, y, z) → (x, z, −y): Z-up to Y-up.
        glam::DMat3::from_cols(glam::DVec3::X, -glam::DVec3::Z, glam::DVec3::Y)
    } else {
        glam::DMat3::IDENTITY
    };
    glam::DAffine3::from_mat3(axes / opts.units.millimeters())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_formats() {
        assert_eq!(
            ExportFormat::from_extension("JSON"),
            Some(ExportFormat::Json)
        );
        assert_eq!(ExportFormat::from_extension("glb"), Some(ExportFormat::Glb));
        assert_eq!(ExportFormat::from_extension("bin"), None);
    }
}
