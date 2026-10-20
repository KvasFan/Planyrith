//! Implements docs/09-file-formats.md §1 `manifest.json` and §7 (metadata).
//!
//! The manifest is the only part a reader has to trust before it extracts
//! anything else: `parts` names the entries in write order, `checksums` holds
//! the blake3 hex of every payload part, and `sizes` holds each payload part's
//! uncompressed byte count so that ZIP entries can be size-checked **before**
//! extraction (docs/09 §8). `mimetype` and the manifest itself have no
//! checksum or size row.
//!
//! Owner key: `io-native`.

#![warn(unused_variables, dead_code)]

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
    /// Uncompressed size in bytes per part (same key set as `checksums`);
    /// readers compare ZIP headers against it before extracting (docs/09 §8).
    #[serde(default)]
    pub sizes: BTreeMap<String, u64>,
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
            sizes: BTreeMap::new(),
        }
    }

    /// `true` when this build can read the file (docs/09 §4).
    pub fn is_readable(&self) -> bool {
        self.min_reader_version <= FORMAT_VERSION
    }

    /// Records a payload part with its blake3 hex and size.
    pub fn add_part(&mut self, name: impl Into<String>, bytes: &[u8]) {
        let name = name.into();
        self.checksums
            .insert(name.clone(), blake3::hash(bytes).to_hex().to_string());
        self.sizes.insert(name.clone(), bytes.len() as u64);
        self.parts.push(name);
    }

    /// Records a part that carries no checksum (`mimetype`, the manifest).
    pub fn add_unchecked_part(&mut self, name: impl Into<String>) {
        self.parts.push(name.into());
    }

    /// The declared uncompressed size of a part, when the writer recorded one.
    pub fn declared_size(&self, name: &str) -> Option<u64> {
        self.sizes.get(name).copied()
    }

    /// `true` when `bytes` match the recorded checksum of `name` (or when no
    /// checksum was recorded for it).
    pub fn checksum_matches(&self, name: &str, bytes: &[u8]) -> bool {
        match self.checksums.get(name) {
            Some(expected) => blake3::hash(bytes).to_hex().as_str() == expected,
            None => true,
        }
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
        assert_eq!(back.declared_size("document.msgpack"), Some(3));
        assert!(back.checksum_matches("document.msgpack", b"xyz"));
        assert!(!back.checksum_matches("document.msgpack", b"xyz!"));
        assert!(back.checksum_matches("unlisted", b""));
    }

    #[test]
    fn manifests_without_sizes_still_parse() {
        let json = serde_json::json!({
            "format_version": FORMAT_VERSION,
            "min_reader_version": FORMAT_VERSION,
            "generator": "planyrith 0.0.0",
            "created": Manifest::DETERMINISTIC_TIMESTAMP,
            "modified": Manifest::DETERMINISTIC_TIMESTAMP,
            "title": "",
            "parts": ["mimetype", "manifest.json"],
            "checksums": {}
        });
        let m: Manifest = serde_json::from_value(json).unwrap();
        assert!(m.sizes.is_empty());
        assert_eq!(m.declared_size("document.msgpack"), None);
    }
}
