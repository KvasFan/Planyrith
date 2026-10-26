//! Implements docs/09-file-formats.md §5 `.plyr` export: a copy of the whole
//! document in the native container, without the edit journal and with
//! fixed timestamps (the core has no clock), like a library file.

use std::io::Write;

use crate::document::Document;
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter};
use crate::io::native::{WriteOptions, write_document};
use crate::io::{IoError, Progress};

/// The native document exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlyrExporter;

impl Exporter for PlyrExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Plyr
    }

    fn export(
        &self,
        doc: &Document,
        _scope: &ExportScope,
        _opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<ExportReport, IoError> {
        let opts = WriteOptions {
            deterministic: true,
            include_history: false,
            ..WriteOptions::default()
        };
        let bytes = write_document(doc, &[], &opts, progress)?;
        dst.write_all(&bytes).map_err(|e| IoError::Storage {
            key: "plyr".into(),
            reason: e.to_string(),
        })?;
        Ok(ExportReport {
            bytes_written: bytes.len() as u64,
            entities: doc.model.order().len(),
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}
