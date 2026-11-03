//! Implements docs/09-file-formats.md §4 (versioning and migration): pure
//! functions on the deserialized intermediate tree, applied in sequence from
//! the file's `format_version` to the current one. Every migration has a
//! fixture pair under `crates/planyrith-core/tests/fixtures/native/`.
//!
//! The intermediate tree is `serde_json::Value`. JSON has no byte strings,
//! which the document holds (component definition ids), so [`decode_tree`]
//! turns each into a one-entry object `{"$bytes": "<hex>"}` and
//! [`encode_tree`] turns it back: the migrated tree is re-encoded to
//! MessagePack and decoded from there, exactly as a current file would be.
//! Format version 1 is the first shipped schema, so the chain is currently
//! empty: a version-1 file is decoded directly and `migrate` is the identity
//! for it.
//!
//! Owner key: `io-native`.

#![warn(unused_variables, dead_code)]

use std::fmt;

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Error as _, SerializeMap, SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::document::FORMAT_VERSION;
use crate::io::IoError;

/// The key of the one-entry object standing for a MessagePack byte string in
/// the migration tree; its value is lowercase hex.
pub const BYTES_KEY: &str = "$bytes";

/// Decodes a MessagePack part into the migration tree, byte strings included.
pub fn decode_tree(bytes: &[u8]) -> Result<Value, rmp_serde::decode::Error> {
    rmp_serde::from_slice::<Tree>(bytes).map(|tree| tree.0)
}

/// Encodes a migration tree back to MessagePack in named-field form, restoring
/// the byte strings [`decode_tree`] turned into objects.
pub fn encode_tree(tree: &Value) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    rmp_serde::to_vec_named(&TreeRef(tree))
}

struct Tree(Value);

impl<'de> Deserialize<'de> for Tree {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(TreeVisitor)
    }
}

struct TreeVisitor;

impl<'de> Visitor<'de> for TreeVisitor {
    type Value = Tree;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a MessagePack value with string map keys")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Tree, E> {
        Ok(Tree(Value::Bool(v)))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Tree, E> {
        Ok(Tree(Value::from(v)))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Tree, E> {
        Ok(Tree(Value::from(v)))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Tree, E> {
        Number::from_f64(v)
            .map(|n| Tree(Value::Number(n)))
            .ok_or_else(|| E::custom("a non-finite number"))
    }
    fn visit_str<E>(self, v: &str) -> Result<Tree, E> {
        Ok(Tree(Value::String(v.to_owned())))
    }
    fn visit_bytes<E>(self, v: &[u8]) -> Result<Tree, E> {
        let hex: String = v.iter().map(|b| format!("{b:02x}")).collect();
        let mut object = Map::new();
        object.insert(BYTES_KEY.to_owned(), Value::String(hex));
        Ok(Tree(Value::Object(object)))
    }
    fn visit_unit<E>(self) -> Result<Tree, E> {
        Ok(Tree(Value::Null))
    }
    fn visit_none<E>(self) -> Result<Tree, E> {
        Ok(Tree(Value::Null))
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Tree, D::Error> {
        Tree::deserialize(d)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Tree, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(4096));
        while let Some(Tree(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Tree(Value::Array(items)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Tree, A::Error> {
        let mut object = Map::new();
        while let Some((key, Tree(value))) = map.next_entry::<String, Tree>()? {
            object.insert(key, value);
        }
        Ok(Tree(Value::Object(object)))
    }
}

struct TreeRef<'a>(&'a Value);

impl Serialize for TreeRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Null => serializer.serialize_unit(),
            Value::Bool(b) => serializer.serialize_bool(*b),
            Value::Number(n) => {
                if let Some(u) = n.as_u64() {
                    serializer.serialize_u64(u)
                } else if let Some(i) = n.as_i64() {
                    serializer.serialize_i64(i)
                } else {
                    serializer.serialize_f64(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            Value::String(s) => serializer.serialize_str(s),
            Value::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&TreeRef(item))?;
                }
                seq.end()
            }
            Value::Object(object) => {
                if let (1, Some(Value::String(hex))) = (object.len(), object.get(BYTES_KEY)) {
                    let bytes = unhex(hex).ok_or_else(|| S::Error::custom("invalid $bytes"))?;
                    return serializer.serialize_bytes(&bytes);
                }
                let mut map = serializer.serialize_map(Some(object.len()))?;
                for (key, value) in object {
                    map.serialize_entry(key, &TreeRef(value))?;
                }
                map.end()
            }
        }
    }
}

fn unhex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

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
    fn the_tree_keeps_byte_strings() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Part {
            id: uuid::Uuid,
            name: String,
            size: f64,
            parts: Vec<Option<u32>>,
        }
        let part = Part {
            id: uuid::Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
            name: "door".into(),
            size: -2.5,
            parts: vec![Some(3), None],
        };
        let bytes = rmp_serde::to_vec_named(&part).unwrap();
        let tree = decode_tree(&bytes).unwrap();
        assert_eq!(tree["id"][BYTES_KEY], "0123456789abcdef0123456789abcdef");
        let again = encode_tree(&tree).unwrap();
        assert_eq!(rmp_serde::from_slice::<Part>(&again).unwrap(), part);
        assert!(unhex("0").is_none() && unhex("zz").is_none());
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
