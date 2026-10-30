//! Implements docs/09 §5: optional SKP conversion followed by native merge.
use super::{Confidence, ImportFormat, ImportOptions, ImportReport, Importer};
use crate::commands::{Transaction, params::Target};
use crate::diagnostics::{Warning, WarningCode};
use crate::io::{IoError, Progress, adapter};
use std::io::Read;

/// SKP importer; the backend provides an independent converter.
pub struct SkpImporter;
impl Importer for SkpImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Skp
    }
    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        if bytes.starts_with(b"\xff\xfe\xff\x0e") {
            Confidence::CERTAIN
        } else if crate::io::model_file::is_skp(name) {
            Confidence::EXTENSION
        } else {
            Confidence::NONE
        }
    }
    fn import(
        &self,
        src: &mut dyn Read,
        opts: &ImportOptions,
        tx: &mut Transaction<'_>,
        target: &Target,
        progress: &dyn Progress,
    ) -> Result<ImportReport, IoError> {
        let mut bytes = Vec::new();
        src.take(adapter::MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| IoError::corrupt("SKP", e.to_string()))?;
        if bytes.len() > adapter::MAX_BYTES {
            return Err(IoError::LimitExceeded("SKP input".into()));
        }
        let converted = tx
            .storage()
            .ok_or_else(adapter::unavailable)?
            .convert_skp("decode", &bytes)?;
        let mut report = super::plyr::PlyrImporter.import(
            &mut converted.bytes.as_slice(),
            opts,
            tx,
            target,
            progress,
        )?;
        report
            .warnings
            .extend(converted.warnings.into_iter().map(|message| Warning {
                code: WarningCode::W2004,
                message,
            }));
        Ok(report)
    }
}
