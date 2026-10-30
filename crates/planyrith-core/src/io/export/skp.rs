//! Implements docs/09 §5: SKP export through a separately installed converter.
use super::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter};
use crate::document::Document;
use crate::io::{IoError, NoProgress, Progress, Storage, adapter, native};
use std::io::Write;

/// SKP exporter; conversion is available through `export_with_storage`.
pub struct SkpExporter;
impl Exporter for SkpExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Skp
    }
    fn export(
        &self,
        _doc: &Document,
        _scope: &ExportScope,
        _opts: &ExportOptions,
        _dst: &mut dyn Write,
        _progress: &dyn Progress,
    ) -> Result<ExportReport, IoError> {
        Err(adapter::unavailable())
    }
    fn export_with_storage(
        &self,
        doc: &Document,
        scope: &ExportScope,
        opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
        storage: &dyn Storage,
    ) -> Result<ExportReport, IoError> {
        // A model file preserves visibility rather than deleting hidden entities.
        // Selected-only export must not quietly write the whole document.
        if scope.selection.is_some() {
            return Err(IoError::UnsupportedFormat(
                "selected-only SKP export; export the whole model or create a component file first"
                    .into(),
            ));
        }
        let operation = match opts.skp_version.unwrap_or(2017) {
            2017 => "encode-2017",
            2026 => "encode-2026",
            _ => {
                return Err(IoError::UnsupportedFormat(
                    "SKP version must be 2017 or 2026".into(),
                ));
            }
        };
        progress.report(0.1, "converting SKP");
        let bytes = native::write_document(
            doc,
            &[],
            &native::WriteOptions {
                deterministic: true,
                include_history: false,
                ..Default::default()
            },
            &NoProgress,
        )?;
        let converted = storage.convert_skp(operation, &bytes)?;
        dst.write_all(&converted.bytes)
            .map_err(|e| IoError::storage("SKP output", e.to_string()))?;
        Ok(ExportReport {
            bytes_written: converted.bytes.len() as u64,
            entities: doc.model.len(),
            warnings: converted
                .warnings
                .into_iter()
                .map(|message| crate::diagnostics::Warning {
                    code: crate::diagnostics::WarningCode::W2004,
                    message,
                })
                .collect(),
            side_files: Vec::new(),
        })
    }
}
