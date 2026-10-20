//! Implements docs/09-file-formats.md §4 (versioning and migration): pure
//! functions on the deserialized intermediate tree, applied in sequence from
//! the file's `format_version` to the current one. Every migration has a
//! fixture pair under `crates/planyrith-core/tests/fixtures/native/`.
//!
//! The intermediate tree is `serde_json::Value` (MessagePack maps with string
//! keys convert losslessly; binary parts are stored separately). Format
//! version 1 is the first shipped schema, so the chain is currently empty: a
//! version-1 file is decoded directly and `migrate` is the identity for it.
//!
//! Owner key: `io-native`.

#![warn(unused_variables, dead_code)]

use crate::document::FORMAT_VERSION;
use crate::io::IoError;

/// Oldest `format_version` this reader migrates from (docs/09 §4 policy:
/// every version since 0.1).
pub const MIN_SUPPORTED_VERSION: u32 = 1;

/// `true` when a file written at `from` must go through [`migrate`] before it
/// can be decoded into the current `Document` type.
pub const fn needs_migration(from: u32) -> bool {
    from != FORMAT_VERSION
}

/// Validates `from` against the supported range: `from > FORMAT_VERSION` is
/// `E1401`, `from < MIN_SUPPORTED_VERSION` is `E1402`.
pub fn check_version(from: u32) -> Result<(), IoError> {
    if from > FORMAT_VERSION {
        return Err(IoError::UnsupportedVersion {
            found: from,
            max: FORMAT_VERSION,
        });
    }
    if from < MIN_SUPPORTED_VERSION {
        return Err(IoError::corrupt(
            "document.msgpack",
            format!("format_version {from} predates 0.1"),
        ));
    }
    Ok(())
}

/// Migrates `tree` from `from` to [`FORMAT_VERSION`], one step at a time.
/// `from > FORMAT_VERSION` is `E1401`; `from < MIN_SUPPORTED_VERSION` is `E1402`.
pub fn migrate(tree: serde_json::Value, from: u32) -> Result<serde_json::Value, IoError> {
    check_version(from)?;
    let mut tree = tree;
    for version in from..FORMAT_VERSION {
        tree = step(tree, version)?;
    }
    Ok(tree)
}

/// One migration step `version → version + 1`. New steps are added here as
/// `vN_to_vN_plus_1` functions when the schema changes.
fn step(tree: serde_json::Value, version: u32) -> Result<serde_json::Value, IoError> {
    // No schema change has shipped yet, so every step is unreachable; the first
    // real migration becomes `if version == 1 { return v1_to_v2(tree); }`.
    let _ = tree;
    Err(IoError::corrupt(
        "document.msgpack",
        format!("no migration from format_version {version}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_is_identity() {
        let tree = serde_json::json!({ "format_version": FORMAT_VERSION });
        assert_eq!(migrate(tree.clone(), FORMAT_VERSION).unwrap(), tree);
        assert!(!needs_migration(FORMAT_VERSION));
        assert!(needs_migration(FORMAT_VERSION + 1));
    }

    #[test]
    fn newer_files_are_refused() {
        let err = migrate(serde_json::json!({}), FORMAT_VERSION + 1).unwrap_err();
        assert_eq!(err.code(), "E1401");
        assert_eq!(
            err,
            IoError::UnsupportedVersion {
                found: FORMAT_VERSION + 1,
                max: FORMAT_VERSION
            }
        );
        assert_eq!(
            migrate(serde_json::json!({}), 0).unwrap_err().code(),
            "E1402"
        );
        assert!(check_version(FORMAT_VERSION).is_ok());
        assert_eq!(check_version(0).unwrap_err().code(), "E1402");
    }
}
