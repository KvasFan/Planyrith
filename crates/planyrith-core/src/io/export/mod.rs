//! Implements docs/09-file-formats.md §5–§6 (exporters): the `Exporter`
//! contract, scope and options. `json` ships with the skeleton (milestone 0.1,
//! docs/13); `obj`, `stl`, `gltf`, `dxf`, `step`, `csv`, `svg`, `pdf` follow at
//! 0.5/0.9.
//!
//! Rules (docs/09 §6): exporters are deterministic (stable ordering by pid) and
//! stream their output.

pub mod json;

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
