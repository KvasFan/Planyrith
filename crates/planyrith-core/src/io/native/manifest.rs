//! Implements docs/09-file-formats.md §1 `manifest.json` and §7 (metadata).
//!
//! Owner key: `io-native`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::document::FORMAT_VERSION;

/// `manifest.json` (docs/09 §1, §7). Unknown fields are ignored on read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Version the writer produced.
    pub format_version: u32,
    /// Oldest reader that can load the file.
    pub min_reader_version: u32,
    /// `planyrith <version>`.
    pub generator: String,
    /// RFC 3339 creation time (fixed under `--deterministic`).
    pub created: String,
    /// RFC 3339 modification time (fixed under `--deterministic`).
    pub modified: String,
    /// Document title.
    pub title: String,
    /// Author.
    #[serde(default)]
    pub author: String,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// `true` for `.plyrt` templates.
    #[serde(default)]
    pub is_template: bool,
    /// Part names in write order.
    pub parts: Vec<String>,
    /// blake3 hex per part (excluding `mimetype` and the manifest itself).
    pub checksums: BTreeMap<String, String>,
}

impl Manifest {
    /// The timestamp written under `--deterministic` (docs/08 §10).
    pub const DETERMINISTIC_TIMESTAMP: &'static str = "2000-01-01T00:00:00Z";

    /// A manifest for the current format with no parts yet.
    pub fn new(generator: impl Into<String>, title: impl Into<String>) -> Self {
        Manifest {
            format_version: FORMAT_VERSION,
            min_reader_version: FORMAT_VERSION,
            generator: generator.into(),
            created: Self::DETERMINISTIC_TIMESTAMP.to_string(),
            modified: Self::DETERMINISTIC_TIMESTAMP.to_string(),
            title: title.into(),
            author: String::new(),
            description: String::new(),
            is_template: false,
            parts: Vec::new(),
            checksums: BTreeMap::new(),
        }
    }

    /// `true` when this build can read the file (docs/09 §4).
    pub fn is_readable(&self) -> bool {
        self.min_reader_version <= FORMAT_VERSION
    }

    /// Records a part and its blake3 hex.
    pub fn add_part(&mut self, name: impl Into<String>, bytes: &[u8]) {
        let name = name.into();
        self.checksums
            .insert(name.clone(), blake3::hash(bytes).to_hex().to_string());
        self.parts.push(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_json_round_trip_ignores_unknown_fields() {
        let mut m = Manifest::new("planyrith 0.0.0", "House");
        m.add_part("document.msgpack", b"xyz");
        let mut json = serde_json::to_value(&m).unwrap();
        json["future_field"] = serde_json::json!(42);
        let back: Manifest = serde_json::from_value(json).unwrap();
        assert_eq!(back, m);
        assert!(back.is_readable());
        assert_eq!(back.checksums["document.msgpack"].len(), 64);
    }
}
