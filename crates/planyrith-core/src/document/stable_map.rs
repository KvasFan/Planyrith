//! Implements the identity rule of docs/04-document-model.md §2 for
//! document-level collections and non-mesh entities: ids stay valid across
//! undo/redo (docs/04 Decisions).
//!
//! A [`StableMap`] is a slot map whose keys survive removal. Removing a value
//! keeps its key reserved, so undo can put the value back under the *same* key
//! with [`StableMap::restore`]. Deltas can therefore name tags, materials,
//! scenes, styles, definitions and non-mesh entities by id without alias
//! tables; only mesh edges and faces (re-created by position-based replay) need
//! the aliases of `geom::ReplayIds`.

#![warn(unused_variables, dead_code)]

use std::ops::{Index, IndexMut};

use serde::{Deserialize, Serialize};
use slotmap::{Key, SecondaryMap, SlotMap};

/// A slot map with reserved keys (see the module documentation). Iteration is
/// in slot order, which is deterministic for a given history.
///
/// Serialized as the sequence of live `(key, value)` pairs in slot order:
/// reserved keys of removed values are not saved (the undo history that could
/// restore them is not saved either), so a document whose changes were undone
/// writes the same bytes as before them (docs/12 §2). Loading rebuilds the
/// allocator so every saved key keeps its exact index and generation.
#[derive(Debug, Clone)]
pub struct StableMap<K: Key, V> {
    /// Every key ever handed out (values removed or not). The marker is a
    /// `u8`, not `()`: a unit value serializes like a vacant slot and would
    /// not round-trip.
    keys: SlotMap<K, u8>,
    /// Live values.
    values: SecondaryMap<K, V>,
}

impl<K: Key, V> Default for StableMap<K, V> {
    fn default() -> Self {
        StableMap {
            keys: SlotMap::with_key(),
            values: SecondaryMap::new(),
        }
    }
}

impl<K: Key, V> StableMap<K, V> {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty map (spelling of `SlotMap::with_key`).
    pub fn with_key() -> Self {
        Self::default()
    }

    /// Stores a value under a fresh key.
    pub fn insert(&mut self, value: V) -> K {
        let key = self.keys.insert(0);
        self.values.insert(key, value);
        key
    }

    /// Removes a value; its key stays reserved for [`StableMap::restore`].
    pub fn remove(&mut self, key: K) -> Option<V> {
        self.values.remove(key)
    }

    /// Puts a value back under a key this map handed out whose value was
    /// removed. Returns the value back when the key is unknown or occupied.
    pub fn restore(&mut self, key: K, value: V) -> Result<(), V> {
        if !self.keys.contains_key(key) || self.values.contains_key(key) {
            return Err(value);
        }
        self.values.insert(key, value);
        Ok(())
    }

    /// Value by key.
    pub fn get(&self, key: K) -> Option<&V> {
        self.values.get(key)
    }

    /// Mutable value by key.
    pub fn get_mut(&mut self, key: K) -> Option<&mut V> {
        self.values.get_mut(key)
    }

    /// `true` when the key holds a live value.
    pub fn contains_key(&self, key: K) -> bool {
        self.values.contains_key(key)
    }

    /// Number of live values.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// `true` when no value is live.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Live entries in slot order.
    pub fn iter(&self) -> impl Iterator<Item = (K, &V)> + '_ {
        self.values.iter()
    }

    /// Live entries in slot order, mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (K, &mut V)> + '_ {
        self.values.iter_mut()
    }

    /// Live keys in slot order.
    pub fn keys(&self) -> impl Iterator<Item = K> + '_ {
        self.values.keys()
    }

    /// Live values in slot order.
    pub fn values(&self) -> impl Iterator<Item = &V> + '_ {
        self.values.values()
    }

    /// Live values in slot order, mutably.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> + '_ {
        self.values.values_mut()
    }
}

impl<K: Key + Serialize, V: Serialize> Serialize for StableMap<K, V> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.values.iter())
    }
}

impl<'de, K: Key + Deserialize<'de>, V: Deserialize<'de>> Deserialize<'de> for StableMap<K, V> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let pairs: Vec<(K, V)> = Vec::deserialize(deserializer)?;
        StableMap::from_pairs(pairs).map_err(serde::de::Error::custom)
    }
}

/// Highest slot index a stored key may use (live and reserved keys of one
/// collection; docs/09 Decisions). Bounds the memory a hostile file can make
/// the loader allocate.
pub const MAX_SLOT_INDEX: usize = 1 << 22;

impl<K: Key, V> StableMap<K, V> {
    /// Rebuilds a map whose live keys are exactly `pairs`' keys. The key
    /// allocator is reconstructed through `slotmap`'s own serialized form
    /// (one `(value, version)` slot per index, occupied slots carrying their
    /// odd generation), encoded compactly; indices above
    /// [`MAX_SLOT_INDEX`] are refused before anything is allocated.
    fn from_pairs(pairs: Vec<(K, V)>) -> Result<Self, String> {
        let mut highest = 0;
        for (k, _) in &pairs {
            let ffi = k.data().as_ffi();
            let (idx, version) = ((ffi & 0xffff_ffff) as usize, (ffi >> 32) as u32);
            if idx == 0 || version % 2 == 0 {
                return Err(format!("invalid key (slot {idx}, version {version})"));
            }
            if idx > MAX_SLOT_INDEX {
                return Err(format!(
                    "key slot {idx} exceeds the limit of {MAX_SLOT_INDEX} slots"
                ));
            }
            highest = highest.max(idx);
        }
        // (value, version) per slot; slot 0 is slotmap's sentinel.
        let mut slots: Vec<(Option<u8>, u32)> = vec![(None, 0); highest + 1];
        for (k, _) in &pairs {
            let ffi = k.data().as_ffi();
            let (idx, version) = ((ffi & 0xffff_ffff) as usize, (ffi >> 32) as u32);
            if slots[idx].0.is_some() {
                return Err(format!("key for slot {idx} given twice"));
            }
            slots[idx] = (Some(0), version);
        }
        let encoded = rmp_serde::to_vec(&slots).map_err(|e| e.to_string())?;
        drop(slots);
        let keys: SlotMap<K, u8> = rmp_serde::from_slice(&encoded).map_err(|e| e.to_string())?;
        let mut values = SecondaryMap::new();
        for (k, v) in pairs {
            values.insert(k, v);
        }
        Ok(StableMap { keys, values })
    }
}

impl<'a, K: Key, V> IntoIterator for &'a StableMap<K, V> {
    type Item = (K, &'a V);
    type IntoIter = slotmap::secondary::Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}

impl<'a, K: Key, V> IntoIterator for &'a mut StableMap<K, V> {
    type Item = (K, &'a mut V);
    type IntoIter = slotmap::secondary::IterMut<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.iter_mut()
    }
}

impl<K: Key, V> Index<K> for StableMap<K, V> {
    type Output = V;

    /// Panics when the key holds no value (like `SlotMap`'s indexing); library
    /// code uses [`StableMap::get`].
    fn index(&self, key: K) -> &V {
        match self.values.get(key) {
            Some(v) => v,
            None => panic!("StableMap: no value for this key"),
        }
    }
}

impl<K: Key, V> IndexMut<K> for StableMap<K, V> {
    fn index_mut(&mut self, key: K) -> &mut V {
        match self.values.get_mut(key) {
            Some(v) => v,
            None => panic!("StableMap: no value for this key"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TagId;

    #[test]
    fn removed_keys_stay_reserved_and_can_be_restored() {
        let mut m: StableMap<TagId, &str> = StableMap::new();
        let a = m.insert("a");
        let b = m.insert("b");
        assert_eq!(m.remove(a), Some("a"));
        assert!(!m.contains_key(a));
        let c = m.insert("c");
        assert_ne!(c, a, "a removed key is never handed out again");
        assert_eq!(m.restore(a, "a"), Ok(()));
        assert_eq!(m.restore(a, "again"), Err("again"));
        assert_eq!(m.restore(TagId::default(), "x"), Err("x"));
        assert_eq!(
            m.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(m[b], "b");
        let json = serde_json::to_string(&m).unwrap();
        let back: StableMap<TagId, String> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back.get(a).map(String::as_str), Some("a"));
        assert_eq!(back.get(c).map(String::as_str), Some("c"));
        // Removed-but-reserved keys are not saved: removing and restoring a
        // value gives the same bytes.
        let before = serde_json::to_string(&m).unwrap();
        let x = m.insert("x");
        m.remove(x);
        assert_eq!(serde_json::to_string(&m).unwrap(), before);
        // New keys after a load never collide with saved ones.
        let mut back = back;
        let d = back.insert("d".to_string());
        assert!(![a, b, c].contains(&d));
    }
}
