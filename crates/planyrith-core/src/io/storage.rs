//! Implements docs/11-persistence-and-web.md §1 (storage abstraction): the
//! `Storage` trait plus the `MemoryStorage` and `NativeFs` backends.
//!
//! The spec writes the trait `async`; this draft keeps it **synchronous**
//! (docs/11 Decisions): desktop backends are blocking, and the web build wraps
//! IndexedDB calls behind the same signatures with a blocking façade or an
//! `async` adapter trait added at milestone 0.7. Converting to `async fn`
//! later is a mechanical change for implementations only.
//!
//! `NativeFs` is the **only** place in the crate allowed to touch `std::fs`
//! (docs/01 §1.1, AGENTS.md §2).
//!
//! Owner key: `io-native`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::io::IoError;

/// A storage key: a path for `NativeFs`, a name for browser storage.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StorageKey(pub String);

impl StorageKey {
    /// Wraps a key.
    pub fn new(key: impl Into<String>) -> Self {
        StorageKey(key.into())
    }

    /// The key text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Sibling key for the autosave journal (docs/11 §2 `<doc>.plyr.journal`).
    pub fn journal(&self) -> StorageKey {
        StorageKey(format!("{}.journal", self.0))
    }

    /// Sibling key for the autosave snapshot (docs/11 §2 `<doc>.autosave.plyr`).
    pub fn autosave(&self) -> StorageKey {
        StorageKey(format!(
            "{}.autosave.plyr",
            self.0.trim_end_matches(".plyr")
        ))
    }

    /// Sibling key for a rotating backup (docs/11 §6 `name.plyr~N`).
    pub fn backup(&self, n: u32) -> StorageKey {
        StorageKey(format!("{}~{n}", self.0))
    }
}

impl fmt::Display for StorageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for StorageKey {
    fn from(s: &str) -> Self {
        StorageKey(s.to_string())
    }
}

/// A listing row (docs/11 §1 `StorageEntry`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageEntry {
    /// Key.
    pub key: StorageKey,
    /// Size in bytes.
    pub size: u64,
    /// Modification time as Unix seconds, when the backend knows it.
    pub modified_unix_s: Option<i64>,
}

/// Backend capabilities (docs/11 §1 `StorageCaps`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageCaps {
    /// Parts can be read without reading the whole blob (lazy `.plyr` parts).
    pub random_access: bool,
    /// Maximum blob size, if limited.
    pub max_size: Option<u64>,
    /// Survives process/browser restarts.
    pub persistent: bool,
    /// Keys are paths another program can open.
    pub shareable_path: bool,
}

/// Byte-blob storage (docs/11 §1). Writes are atomic replacements.
pub trait Storage {
    /// Reads a whole blob.
    fn read(&self, key: &StorageKey) -> Result<Vec<u8>, IoError>;
    /// Atomically replaces a blob.
    fn write(&self, key: &StorageKey, bytes: &[u8]) -> Result<(), IoError>;
    /// Deletes a blob (`Ok` if absent).
    fn delete(&self, key: &StorageKey) -> Result<(), IoError>;
    /// Lists blobs whose key starts with `prefix`, sorted by key.
    fn list(&self, prefix: &str) -> Result<Vec<StorageEntry>, IoError>;
    /// `true` when the key exists.
    fn exists(&self, key: &StorageKey) -> Result<bool, IoError>;
    /// Backend capabilities.
    fn capabilities(&self) -> StorageCaps;
}

/// In-memory storage for tests and the clipboard (docs/11 §1).
#[derive(Debug, Default)]
pub struct MemoryStorage {
    blobs: Mutex<BTreeMap<StorageKey, Vec<u8>>>,
    read_only: bool,
}

impl MemoryStorage {
    /// Empty writable storage.
    pub fn new() -> Self {
        Self::default()
    }

    /// Storage that refuses writes (`E1501`), for read-only sessions.
    pub fn read_only(self) -> Self {
        MemoryStorage {
            read_only: true,
            ..self
        }
    }

    fn blobs(&self) -> std::sync::MutexGuard<'_, BTreeMap<StorageKey, Vec<u8>>> {
        self.blobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Storage for MemoryStorage {
    fn read(&self, key: &StorageKey) -> Result<Vec<u8>, IoError> {
        self.blobs()
            .get(key)
            .cloned()
            .ok_or_else(|| IoError::NotFound(key.0.clone()))
    }

    fn write(&self, key: &StorageKey, bytes: &[u8]) -> Result<(), IoError> {
        if self.read_only {
            return Err(IoError::ReadOnly(key.0.clone()));
        }
        self.blobs().insert(key.clone(), bytes.to_vec());
        Ok(())
    }

    fn delete(&self, key: &StorageKey) -> Result<(), IoError> {
        if self.read_only {
            return Err(IoError::ReadOnly(key.0.clone()));
        }
        self.blobs().remove(key);
        Ok(())
    }

    fn list(&self, prefix: &str) -> Result<Vec<StorageEntry>, IoError> {
        Ok(self
            .blobs()
            .iter()
            .filter(|(k, _)| k.0.starts_with(prefix))
            .map(|(k, v)| StorageEntry {
                key: k.clone(),
                size: v.len() as u64,
                modified_unix_s: None,
            })
            .collect())
    }

    fn exists(&self, key: &StorageKey) -> Result<bool, IoError> {
        Ok(self.blobs().contains_key(key))
    }

    fn capabilities(&self) -> StorageCaps {
        StorageCaps {
            random_access: true,
            max_size: None,
            persistent: false,
            shareable_path: false,
        }
    }
}

/// Desktop file-system storage (docs/11 §1 `NativeFs`): keys are paths; atomic
/// write = write `.<name>.tmp` + fsync + rename; optional rotating backups.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Default)]
pub struct NativeFs {
    /// Base directory for relative keys (`None` = process working directory).
    pub root: Option<std::path::PathBuf>,
    /// Number of `name.plyr~N` backups to keep on overwrite (docs/11 §6).
    pub backups: u32,
    /// Refuse writes (`--readonly`).
    pub read_only: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeFs {
    /// Storage rooted at the working directory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Storage rooted at `root`.
    pub fn rooted(root: impl Into<std::path::PathBuf>) -> Self {
        NativeFs {
            root: Some(root.into()),
            ..Self::default()
        }
    }

    /// Resolves a key to an absolute or root-relative path.
    pub fn path_for(&self, key: &StorageKey) -> std::path::PathBuf {
        match &self.root {
            Some(root) => root.join(&key.0),
            None => std::path::PathBuf::from(&key.0),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Storage for NativeFs {
    fn read(&self, key: &StorageKey) -> Result<Vec<u8>, IoError> {
        todo!("docs/11 §1 NativeFs::read")
    }

    fn write(&self, key: &StorageKey, bytes: &[u8]) -> Result<(), IoError> {
        todo!("docs/11 §1 NativeFs::write (tmp + fsync + rename, backups)")
    }

    fn delete(&self, key: &StorageKey) -> Result<(), IoError> {
        todo!("docs/11 §1 NativeFs::delete")
    }

    fn list(&self, prefix: &str) -> Result<Vec<StorageEntry>, IoError> {
        todo!("docs/11 §1 NativeFs::list")
    }

    fn exists(&self, key: &StorageKey) -> Result<bool, IoError> {
        todo!("docs/11 §1 NativeFs::exists")
    }

    fn capabilities(&self) -> StorageCaps {
        StorageCaps {
            random_access: true,
            max_size: None,
            persistent: true,
            shareable_path: true,
        }
    }
}

/// Conformance checks every backend must pass (docs/11 §7). Returns the first
/// failing assertion as an error.
pub fn conformance_check(storage: &dyn Storage) -> Result<(), IoError> {
    let key = StorageKey::new("conformance/probe.bin");
    storage.write(&key, b"abc")?;
    if !storage.exists(&key)? {
        return Err(IoError::storage(key.as_str(), "exists() false after write"));
    }
    if storage.read(&key)? != b"abc" {
        return Err(IoError::storage(
            key.as_str(),
            "read() differs from written bytes",
        ));
    }
    storage.write(&key, b"abcd")?;
    if storage.read(&key)?.len() != 4 {
        return Err(IoError::storage(key.as_str(), "write() did not replace"));
    }
    let listed = storage.list("conformance/")?;
    if listed.len() != 1 || listed[0].size != 4 {
        return Err(IoError::storage(
            key.as_str(),
            "list() did not report the blob",
        ));
    }
    storage.delete(&key)?;
    if storage.exists(&key)? {
        return Err(IoError::storage(key.as_str(), "exists() true after delete"));
    }
    storage.delete(&key)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_storage_passes_conformance() {
        conformance_check(&MemoryStorage::new()).unwrap();
    }

    #[test]
    fn read_only_memory_storage_refuses_writes() {
        let s = MemoryStorage::new().read_only();
        let err = s.write(&StorageKey::from("x"), b"1").unwrap_err();
        assert_eq!(err.code(), "E1501");
        assert_eq!(s.read(&StorageKey::from("x")).unwrap_err().code(), "E1403");
    }

    #[test]
    fn derived_keys() {
        let k = StorageKey::from("house.plyr");
        assert_eq!(k.journal().as_str(), "house.plyr.journal");
        assert_eq!(k.autosave().as_str(), "house.autosave.plyr");
        assert_eq!(k.backup(2).as_str(), "house.plyr~2");
    }
}
