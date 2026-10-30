//! Implements docs/09 §5: opening/saving native and adapter-backed model files.

use super::{IoError, NoProgress, Storage, adapter, native};

/// Whether the path names an SKP model (case insensitive).
pub fn is_skp(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("skp"))
}

/// Opens a whole model, keeping native metadata and converter warnings.
pub fn read(
    bytes: &[u8],
    path: &str,
    storage: &dyn Storage,
) -> Result<native::LoadedDocument, IoError> {
    if !is_skp(path) && !bytes.starts_with(b"\xff\xfe\xff\x0e") {
        return native::read_document(bytes, &NoProgress);
    }
    let converted = storage.convert_skp("decode", bytes)?;
    let mut loaded = native::read_document(&converted.bytes, &NoProgress)?;
    loaded
        .warnings
        .extend(
            converted
                .warnings
                .into_iter()
                .map(|message| crate::diagnostics::Warning {
                    code: crate::diagnostics::WarningCode::W2004,
                    message,
                }),
        );
    Ok(loaded)
}

/// Converts an already encoded native model to the requested file format.
/// Saving SKP uses the 2017 container for compatibility; explicit export may
/// choose 2026. Native snapshots/autosaves remain native, regardless of path.
pub fn for_save(
    bytes: Vec<u8>,
    path: &str,
    storage: &dyn Storage,
) -> Result<adapter::Converted, IoError> {
    if is_skp(path) {
        storage.convert_skp("encode-2017", &bytes)
    } else {
        Ok(adapter::Converted {
            bytes,
            warnings: Vec::new(),
        })
    }
}
