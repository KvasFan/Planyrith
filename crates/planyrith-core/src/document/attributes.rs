//! Implements docs/04-document-model.md §9 (attribute dictionaries): the
//! namespaced key/value store attached to entities, definitions, materials and
//! the document.

use std::collections::BTreeMap;

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::materials::Rgba8;
use crate::ids::Pid;
use crate::units::Length;

/// Reserved dictionary-name prefixes (docs/04 §9).
pub const RESERVED_PREFIXES: [&str; 3] = ["plyr.", "sheets.", "ext."];

/// A typed attribute value (docs/04 §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum Value {
    /// Boolean.
    Bool(bool),
    /// Signed integer.
    Int(i64),
    /// Unitless float.
    Float(f64),
    /// Length in mm.
    Length(Length),
    /// UTF-8 string.
    Str(String),
    /// Opaque bytes.
    Bytes(Vec<u8>),
    /// 3D vector / point.
    Vec3(#[schemars(with = "[f64; 3]")] DVec3),
    /// Color.
    Color(Rgba8),
    /// Ordered list.
    Array(Vec<Value>),
    /// Nested map with sorted keys.
    Map(BTreeMap<String, Value>),
    /// Reference to another entity by persistent id.
    EntityRef(Pid),
}

/// `dictionary → key → value`, both levels sorted for deterministic iteration
/// and serialization (docs/04 §9).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct AttributeDictionaries(pub BTreeMap<String, BTreeMap<String, Value>>);

impl AttributeDictionaries {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when no dictionary exists.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of dictionaries.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Reads one value.
    pub fn get(&self, dict: &str, key: &str) -> Option<&Value> {
        self.0.get(dict).and_then(|d| d.get(key))
    }

    /// Writes one value, creating the dictionary; returns the previous value.
    pub fn set(&mut self, dict: &str, key: &str, value: Value) -> Option<Value> {
        self.0
            .entry(dict.to_string())
            .or_default()
            .insert(key.to_string(), value)
    }

    /// Removes one value; empty dictionaries are dropped.
    pub fn remove(&mut self, dict: &str, key: &str) -> Option<Value> {
        let removed = self.0.get_mut(dict).and_then(|d| d.remove(key));
        if self.0.get(dict).is_some_and(BTreeMap::is_empty) {
            self.0.remove(dict);
        }
        removed
    }

    /// Removes a whole dictionary.
    pub fn remove_dictionary(&mut self, dict: &str) -> Option<BTreeMap<String, Value>> {
        self.0.remove(dict)
    }

    /// One dictionary's entries.
    pub fn dictionary(&self, dict: &str) -> Option<&BTreeMap<String, Value>> {
        self.0.get(dict)
    }

    /// Dictionary names in sorted order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

/// `true` when `dict` starts with a reserved prefix (`plyr.`, `sheets.`, `ext.`).
pub fn is_reserved_dictionary(dict: &str) -> bool {
    RESERVED_PREFIXES.iter().any(|p| dict.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_remove() {
        let mut a = AttributeDictionaries::new();
        assert!(a.set("ext.demo", "k", Value::Int(1)).is_none());
        assert_eq!(a.get("ext.demo", "k"), Some(&Value::Int(1)));
        assert_eq!(
            a.set("ext.demo", "k", Value::Bool(true)),
            Some(Value::Int(1))
        );
        assert_eq!(a.remove("ext.demo", "k"), Some(Value::Bool(true)));
        assert!(a.is_empty());
    }

    #[test]
    fn reserved_prefixes() {
        assert!(is_reserved_dictionary("plyr.core"));
        assert!(is_reserved_dictionary("ext.com.example"));
        assert!(!is_reserved_dictionary("mine"));
    }

    #[test]
    fn value_round_trips_through_json() {
        let v = Value::Map(BTreeMap::from([(
            "p".to_string(),
            Value::Vec3(DVec3::new(1.0, 2.0, 3.0)),
        )]));
        let json = serde_json::to_string(&v).unwrap();
        let back: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }
}
