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
use crate::io::{IoError, NoProgress, StorageKey};
use crate::units::LengthUnit;

/// `export` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportParams {
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
            scene: None,
            include_hidden: p.include_hidden,
        };
        let opts = ExportOptions {
            units: units(&p)?,
            ascii: p.ascii,
            z_up: p.z_up,
            ..ExportOptions::default()
        };
        let mut bytes = Vec::new();
        let report = exporter.export(tx.document(), &scope, &opts, &mut bytes, &NoProgress)?;
        storage(tx, &p.path)?.write(&StorageKey::new(&p.path), &bytes)?;
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
