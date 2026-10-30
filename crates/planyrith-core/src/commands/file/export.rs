//! Implements docs/05-modeling-tools.md §10 `export { path, format, options }`
//! and docs/08-cli-specification.md §2 `planyrith export` for the 3D mesh
//! formats (docs/09 §5–§6): the visible model, or the occurrences a query
//! names, written through the engine's storage. It changes nothing in the
//! document, so it is not undoable.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::organize::libraries::storage;
use crate::commands::params::{command_context, select};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Document, Query};
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, exporter};
use crate::io::import::sibling;
use crate::io::{IoError, NoProgress, StorageKey};
use std::sync::Arc;

use serde_json::json;

use crate::commands::{CommandRegistry, Engine};
use crate::units::{Length, LengthUnit};

/// `export` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportParams {
    /// SKP file version: 2017 (default) or 2026; requires the optional converter.
    #[serde(default)]
    pub skp_version: Option<u32>,
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// File to write.
    pub path: String,
    /// Format (from the extension).
    #[serde(default)]
    pub format: Option<ExportFormat>,
    /// Only the occurrences these entities belong to (the whole model).
    #[serde(default)]
    pub targets: Option<Query>,
    /// Include hidden entities and tags.
    #[serde(default)]
    pub include_hidden: bool,
    /// Unit of unitless formats (`mm`, `cm`, `m`, `in`, `ft`; mm).
    #[serde(default)]
    pub file_units: Option<String>,
    /// ASCII STL instead of binary.
    #[serde(default)]
    pub ascii: bool,
    /// Keep Z up in formats that are Y-up by default (OBJ, STL).
    #[serde(default)]
    pub z_up: bool,
    /// A 2D drawing of the view, hidden lines removed (DXF; SVG and PDF
    /// always are).
    #[serde(default)]
    pub two_d: bool,
    /// Draw this scene's view (its camera, tags, hidden entities…) instead
    /// of the current one.
    #[serde(default)]
    pub scene: Option<String>,
    /// Paper scale of a parallel view, `1:50` (else the view fits the
    /// paper width).
    #[serde(default)]
    pub scale: Option<String>,
    /// Paper width of a fitted view (297 mm).
    #[serde(default)]
    pub paper_width: Option<Length>,
}

/// `export` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExportOutput {
    /// The format written.
    pub format: ExportFormat,
    /// What was written.
    #[serde(flatten)]
    pub report: ExportReport,
}

/// The `export` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Export;

fn units(p: &ExportParams) -> Result<LengthUnit, CommandError> {
    p.file_units
        .as_deref()
        .map_or(Ok(LengthUnit::Millimeter), |u| {
            u.parse().map_err(|_| {
                CommandError::invalid_param("file_units", format!("unknown unit `{u}`"))
            })
        })
}

/// `1:50`, `1/50` or a plain factor → paper per model length.
fn paper_scale(p: &ExportParams) -> Result<Option<f64>, CommandError> {
    let Some(text) = p.scale.as_deref().map(str::trim) else {
        return Ok(None);
    };
    let bad = || CommandError::invalid_param("scale", format!("`{text}`: expected 1:50"));
    let value = match text.split_once([':', '/']) {
        Some((a, b)) => {
            let (a, b): (f64, f64) = (
                a.trim().parse().map_err(|_| bad())?,
                b.trim().parse().map_err(|_| bad())?,
            );
            a / b
        }
        None => text.parse::<f64>().map_err(|_| bad())?,
    };
    if value.is_finite() && value > 0.0 {
        Ok(Some(value))
    } else {
        Err(bad())
    }
}

fn format(p: &ExportParams) -> Result<ExportFormat, CommandError> {
    p.format
        .or_else(|| {
            p.path
                .rsplit(['/', '\\'])
                .next()
                .and_then(|f| f.rsplit_once('.'))
                .and_then(|(_, ext)| ExportFormat::from_extension(ext))
        })
        .ok_or_else(|| {
            CommandError::Io(IoError::UnsupportedFormat(format!(
                "cannot tell the format of `{}`; pass `format`",
                p.path
            )))
        })
}

impl Command for Export {
    type Params = ExportParams;
    type Output = ExportOutput;

    fn name(&self) -> &'static str {
        "export"
    }

    fn validate(&self, _doc: &Document, p: &ExportParams) -> Result<(), CommandError> {
        if p.path.trim().is_empty() {
            return Err(CommandError::invalid_param("path", "must not be empty"));
        }
        units(p)?;
        format(p)?;
        if p.skp_version.is_some_and(|v| !matches!(v, 2017 | 2026)) {
            return Err(CommandError::invalid_param(
                "skp_version",
                "must be 2017 or 2026",
            ));
        }
        paper_scale(p)?;
        if let Some(w) = p.paper_width
            && !(w.as_mm() > 0.0 && w.as_mm().is_finite())
        {
            return Err(CommandError::OutOfRange {
                name: "paper_width".into(),
                reason: "must be a positive length".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ExportParams,
    ) -> Result<ExportOutput, CommandError> {
        let format = format(&p)?;
        let exporter = exporter(format).ok_or_else(|| {
            CommandError::Io(IoError::UnsupportedFormat(format!(
                "exporting {format:?} files"
            )))
        })?;
        let selection = match &p.targets {
            Some(q) => Some(select(tx, &command_context(tx, &p.common), "targets", q)?),
            None => None,
        };
        let scope = ExportScope {
            selection,
            scene: p.scene.clone(),
            include_hidden: p.include_hidden,
        };
        let opts = ExportOptions {
            skp_version: p.skp_version,
            units: units(&p)?,
            ascii: p.ascii,
            z_up: p.z_up,
            destination: p.path.clone(),
            two_d: p.two_d,
            scale: paper_scale(&p)?,
            paper_width_mm: p.paper_width.map(Length::as_mm),
            ..ExportOptions::default()
        };
        // A scene applies to a copy, as `scene-activate` would.
        let scened;
        let doc = match &p.scene {
            Some(name) => {
                let mut copy = Engine::new(
                    tx.document().clone(),
                    Arc::new(CommandRegistry::with_builtins()),
                );
                copy.execute_erased("scene-activate", json!({ "scene": name }))
                    .map_err(|e| match e {
                        CommandError::InvalidParam { reason, .. } => {
                            CommandError::invalid_param("scene", reason)
                        }
                        other => other,
                    })?;
                scened = copy.document().clone();
                &scened
            }
            None => tx.document(),
        };
        let mut bytes = Vec::new();
        let storage = storage(tx, &p.path)?;
        let report =
            exporter.export_with_storage(doc, &scope, &opts, &mut bytes, &NoProgress, storage)?;
        storage.write(&StorageKey::new(&p.path), &bytes)?;
        for (name, contents) in &report.side_files {
            storage.write(&StorageKey::new(sibling(&p.path, name)), contents)?;
        }
        if format == ExportFormat::Skp {
            for warning in &report.warnings {
                tx.warn(warning.clone());
            }
        }
        Ok(ExportOutput { format, report })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "export".into(),
            category: CommandCategory::File,
            label: "Export".into(),
            view_only: true,
            steps: Vec::new(),
        }
    }
}
