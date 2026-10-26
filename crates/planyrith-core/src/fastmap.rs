//! Hash maps for lookups on hot paths (welding, adjacency) with a fixed,
//! seedless multiplicative hasher: the same on every run and platform
//! (AGENTS.md determinism). They are only queried, never iterated where the
//! order could be observed; ordered results use sorted vectors or `BTree*`.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// A fast word-at-a-time hasher (rotate, xor, multiply).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct WordHasher(u64);

impl WordHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl Hasher for WordHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for w in words {
            self.add(u64::from_le_bytes(*w));
        }
        if !rest.is_empty() {
            let mut w = [0u8; 8];
            w[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(w));
        }
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_i64(&mut self, i: i64) {
        self.add(i as u64);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
}

/// A deterministic hash map (lookups only).
pub(crate) type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<WordHasher>>;

/// A deterministic hash set (lookups only).
pub(crate) type FastSet<K> = HashSet<K, BuildHasherDefault<WordHasher>>;
