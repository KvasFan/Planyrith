//! Implements docs/09-file-formats.md §1–§4 and §7–§8: the `.plyr` ZIP
//! container (`mimetype`, `manifest.json`, `document.msgpack`, `images/*`,
//! `thumbnail.png`, `history.msgpack`, `ext/*`), deterministic writing, part
//! checksums, lazy reading and format migrations.
//!
//! Owner key: `io-native`.

pub mod manifest;
pub mod migrate;

pub use manifest::Manifest;

use serde::{Deserialize, Serialize};

use crate::diagnostics::Warning;
use crate::document::Document;
use crate::io::storage::{Storage, StorageKey};
use crate::io::{IoError, Progress};

/// Value of the `mimetype` entry (docs/09 §1).
pub const MIMETYPE: &str = "application/x-planyrith";

/// Container part names in the mandated write order (docs/09 §1).
pub mod parts {
    /// First entry, stored uncompressed for sniffing.
    pub const MIMETYPE: &str = "mimetype";
    /// Manifest.
    pub const MANIFEST: &str = "manifest.json";
    /// Document body.
    pub const DOCUMENT: &str = "document.msgpack";
    /// Image directory prefix (`images/<blake3-hex>.<ext>`).
    pub const IMAGES_DIR: &str = "images/";
    /// Optional preview.
    pub const THUMBNAIL: &str = "thumbnail.png";
    /// Optional command journal.
    pub const HISTORY: &str = "history.msgpack";
    /// Plugin payload prefix (`ext/<plugin-id>/…`).
    pub const EXT_DIR: &str = "ext/";
}

/// Maximum size of one part (ZIP64 enabled; docs/09 §1).
pub const MAX_PART_SIZE: u64 = 4 * 1024 * 1024 * 1024;

/// Default cap of the history journal (docs/09 §3).
pub const MAX_JOURNAL_SIZE: u64 = 64 * 1024 * 1024;

/// What a container holds, by extension (docs/09 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerKind {
    /// `.plyr` — a document.
    Document,
    /// `.plyrc` — one definition plus referenced materials.
    Component,
    /// `.plyrm` — material library.
    MaterialLibrary,
    /// `.plyrs` — style library.
    StyleLibrary,
    /// `.plyrt` — template (a document flagged `is_template`).
    Template,
}

impl ContainerKind {
    /// File extension without the dot.
    pub const fn extension(self) -> &'static str {
        match self {
            ContainerKind::Document => "plyr",
            ContainerKind::Component => "plyrc",
            ContainerKind::MaterialLibrary => "plyrm",
            ContainerKind::StyleLibrary => "plyrs",
            ContainerKind::Template => "plyrt",
        }
    }

    /// Kind from a file extension (case-insensitive).
    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "plyr" => ContainerKind::Document,
            "plyrc" => ContainerKind::Component,
            "plyrm" => ContainerKind::MaterialLibrary,
            "plyrs" => ContainerKind::StyleLibrary,
            "plyrt" => ContainerKind::Template,
            _ => return None,
        })
    }
}

/// Writer options (docs/09 §1 rules, §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOptions {
    /// Fixed timestamps and no extra fields so identical documents give
    /// identical bytes (`--deterministic`, docs/08 §10).
    pub deterministic: bool,
    /// Deflate parts (`false` = Stored).
    pub compress: bool,
    /// PNG bytes for `thumbnail.png` (`None` = omit).
    pub thumbnail: Option<Vec<u8>>,
    /// Write `history.msgpack` from the engine journal.
    pub include_history: bool,
    /// `generator` string for the manifest (`planyrith <version>`).
    pub generator: String,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            deterministic: false,
            compress: true,
            thumbnail: None,
            include_history: true,
            generator: format!("planyrith {}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// A journal row in `history.msgpack` (docs/09 §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Command name.
    pub command: String,
    /// Params as executed.
    pub params: serde_json::Value,
    /// Undo group label, if grouped.
    pub group: Option<String>,
    /// RFC 3339 timestamp, absent under `--deterministic`.
    pub timestamp: Option<String>,
}

/// Result of reading a container.
#[derive(Debug)]
pub struct LoadedDocument {
    /// The document (images loaded into `Document::images`).
    pub document: Document,
    /// The manifest.
    pub manifest: Manifest,
    /// Journal rows, if the part exists.
    pub history: Vec<HistoryEntry>,
    /// Non-fatal notes (unknown parts, missing planes recomputed, migrations).
    pub warnings: Vec<Warning>,
}

/// Parses a whole `.plyr` from memory: verifies `mimetype`, reads the
/// manifest, migrates `document.msgpack` to the current version, rebuilds the
/// half-edge meshes (`HalfEdgeMesh::from_indexed`) and loads images
/// (docs/09 §1–§2, §4).
pub fn read_document(bytes: &[u8], progress: &dyn Progress) -> Result<LoadedDocument, IoError> {
    todo!("docs/09 §1 read .plyr container")
}

/// Serializes a document into `.plyr` bytes in the mandated part order with
/// checksums (docs/09 §1).
pub fn write_document(
    doc: &Document,
    history: &[HistoryEntry],
    opts: &WriteOptions,
    progress: &dyn Progress,
) -> Result<Vec<u8>, IoError> {
    todo!("docs/09 §1 write .plyr container")
}

/// Reads only `manifest.json` (fast `info`, recent-files panel).
pub fn read_manifest(bytes: &[u8]) -> Result<Manifest, IoError> {
    todo!("docs/09 §1 read manifest")
}

/// Verifies every part's blake3 against the manifest; returns the names of
/// mismatching parts (docs/09 §1 `checksums`, `planyrith validate`).
pub fn verify_checksums(bytes: &[u8]) -> Result<Vec<String>, IoError> {
    todo!("docs/09 §1 verify checksums")
}

/// Rebuilds a document from `document.msgpack` alone (`repair --from-parts`,
/// docs/09 §8).
pub fn read_document_part(document_msgpack: &[u8]) -> Result<Document, IoError> {
    todo!("docs/09 §8 recover from document part")
}

/// Opens a document from storage (docs/11 §1: commands take `&dyn Storage`).
pub fn open(
    storage: &dyn Storage,
    key: &StorageKey,
    progress: &dyn Progress,
) -> Result<LoadedDocument, IoError> {
    let bytes = storage.read(key)?;
    read_document(&bytes, progress)
}

/// Saves a document to storage atomically.
pub fn save(
    storage: &dyn Storage,
    key: &StorageKey,
    doc: &Document,
    history: &[HistoryEntry],
    opts: &WriteOptions,
    progress: &dyn Progress,
) -> Result<(), IoError> {
    let bytes = write_document(doc, history, opts, progress)?;
    storage.write(key, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_kinds_by_extension() {
        assert_eq!(
            ContainerKind::from_extension("PLYR"),
            Some(ContainerKind::Document)
        );
        assert_eq!(
            ContainerKind::from_extension("plyrc"),
            Some(ContainerKind::Component)
        );
        assert_eq!(ContainerKind::from_extension("zip"), None);
        assert_eq!(ContainerKind::Template.extension(), "plyrt");
    }

    #[test]
    fn default_write_options_name_the_generator() {
        let o = WriteOptions::default();
        assert!(o.generator.starts_with("planyrith "));
        assert!(o.compress && o.include_history && !o.deterministic);
    }
}
