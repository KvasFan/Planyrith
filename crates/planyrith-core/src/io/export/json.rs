//! Implements the `json` exporter of docs/09-file-formats.md §5 and the
//! canonical form of docs/08 §10: a full document dump (`--format json
//! --pretty` for debugging, `--canonical` for version control).
//!
//! Owner key: `io-native`.

use std::io::Write;

use crate::document::Document;
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter};
use crate::io::{IoError, Progress};

/// The JSON document exporter.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonExporter;

/// Serializes the document to a JSON value in the file layout of docs/09 §2
/// (image bytes excluded; meshes in indexed form).
pub fn document_to_json(doc: &Document) -> Result<serde_json::Value, IoError> {
    todo!("docs/09 §5 json document dump")
}

/// Canonicalizes in place: object keys sorted, floats printed with the shortest
/// round-trip representation, `-0.0` normalized to `0.0`, no NaN/∞
/// (docs/08 §10).
pub fn canonicalize(value: &mut serde_json::Value) {
    todo!("docs/08 §10 canonical JSON")
}

impl Exporter for JsonExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Json
    }

    fn export(
        &self,
        doc: &Document,
        scope: &ExportScope,
        opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<ExportReport, IoError> {
        todo!("docs/09 §5 JsonExporter::export")
    }
}
