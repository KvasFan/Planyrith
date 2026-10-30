//! Implements docs/11-persistence-and-web.md §1 (storage abstraction): the
//! `Storage` trait plus the `MemoryStorage` and `NativeFs` backends, and the
//! conformance suite of docs/11 §7.
//!
//! The spec writes the trait `async`; this draft keeps it **synchronous**
//! (docs/11 Decisions): desktop backends are blocking, and the web build wraps
//! IndexedDB calls behind the same signatures with a blocking façade or an
//! `async` adapter trait added at milestone 0.7. Converting to `async fn`
//! later is a mechanical change for implementations only.
//!
//! `NativeFs` is the **only** place in the crate allowed to touch `std::fs`
//! (docs/01 §1.1, AGENTS.md §2). Its atomic write is "write `.<name>.tmp` in
//! the target directory, `fsync`, rotate backups, rename over the target"
//! (docs/11 §1, §6).
//!
//! Owner key: `io-native`.

#![warn(unused_variables, dead_code)]

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
    /// Invokes the optional SKP converter through this backend's I/O boundary.
    /// `operation` is `decode`, `encode-preserve`, `encode-2017` or `encode-2026`.
    fn convert_skp(
        &self,
        _operation: &str,
        _bytes: &[u8],
    ) -> Result<super::adapter::Converted, IoError> {
        Err(super::adapter::unavailable())
    }
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
///
/// Keys use `/` as the separator on every platform; `list` looks at the
/// directory the prefix points into (not recursively) and reports entries in
/// key order. Missing parent directories are created on write.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Default)]
pub struct NativeFs {
    /// Optional absolute path to the separately installed SKP converter.
    pub skp_adapter: Option<std::path::PathBuf>,
    /// Base directory for relative keys (`None` = process working directory).
    pub root: Option<std::path::PathBuf>,
    /// Number of `name.plyr~N` backups to keep on overwrite (docs/11 §6).
    pub backups: u32,
    /// Refuse writes (`--readonly`).
    pub read_only: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeFs {
    /// Suffix of the temporary file an atomic write goes through.
    const TEMP_SUFFIX: &'static str = ".tmp";

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

    /// The sibling `.<name>.tmp` path an atomic write of `path` goes through.
    fn temp_path(path: &std::path::Path) -> std::path::PathBuf {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        path.with_file_name(format!(".{name}{}", Self::TEMP_SUFFIX))
    }

    /// `true` for the temporary files this backend creates, which `list`
    /// hides.
    fn is_temp_name(name: &str) -> bool {
        name.starts_with('.') && name.ends_with(Self::TEMP_SUFFIX)
    }

    /// Maps an OS error to the stable `E1403` variants without leaking the
    /// locale-dependent OS message.
    fn map_io(key: &StorageKey, err: &std::io::Error) -> IoError {
        if err.kind() == std::io::ErrorKind::NotFound {
            return IoError::NotFound(key.0.clone());
        }
        let reason = match err.raw_os_error() {
            Some(code) => format!("{} (os error {code})", err.kind()),
            None => err.kind().to_string(),
        };
        IoError::storage(key.as_str(), reason)
    }

    /// Rotates `path~1..N` so that `path` can become `path~1` (docs/11 §6).
    fn rotate_backups(&self, path: &std::path::Path) -> std::io::Result<()> {
        if self.backups == 0 || !path.is_file() {
            return Ok(());
        }
        let backup = |n: u32| {
            let mut os = path.as_os_str().to_owned();
            os.push(format!("~{n}"));
            std::path::PathBuf::from(os)
        };
        for n in (1..self.backups).rev() {
            match std::fs::rename(backup(n), backup(n + 1)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        std::fs::rename(path, backup(1))
    }

    /// Writes `bytes` to the temp sibling, syncs it and renames it over
    /// `path`. On failure the temp file is removed.
    fn write_atomic(&self, path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write as _;

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = Self::temp_path(path);
        let result = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            self.rotate_backups(path)?;
            std::fs::rename(&tmp, path)
        })();
        if result.is_err() {
            // Best effort: the original error is the one worth reporting.
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Storage for NativeFs {
    fn convert_skp(
        &self,
        operation: &str,
        bytes: &[u8],
    ) -> Result<super::adapter::Converted, IoError> {
        use super::adapter;
        use std::io::Write;
        use std::process::{Command, Stdio};
        if !matches!(
            operation,
            "decode" | "encode-preserve" | "encode-2017" | "encode-2026"
        ) {
            return Err(IoError::UnsupportedFormat(
                "unknown SKP converter operation".into(),
            ));
        }
        if bytes.len() > adapter::MAX_BYTES {
            return Err(IoError::LimitExceeded("SKP converter input".into()));
        }
        let executable = if let Some(path) = self
            .skp_adapter
            .clone()
            .map(std::path::PathBuf::into_os_string)
            .or_else(|| std::env::var_os("PLANYRITH_SKP_ADAPTER"))
        {
            let path = std::path::PathBuf::from(path);
            if !path.is_absolute() {
                return Err(IoError::storage(
                    "SKP adapter",
                    "PLANYRITH_SKP_ADAPTER must be an absolute path",
                ));
            }
            path
        } else {
            let current = std::env::current_exe().map_err(|_| adapter::unavailable())?;
            let parent = current.parent().ok_or_else(adapter::unavailable)?;
            // Cargo integration-test executables live under target/.../deps.
            let parent = if parent.file_name().is_some_and(|p| p == "deps") {
                parent.parent().unwrap_or(parent)
            } else {
                parent
            };
            parent.join(format!("planyrith-skp{}", std::env::consts::EXE_SUFFIX))
        };
        let mut child = Command::new(&executable)
            .arg(operation)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    adapter::unavailable()
                } else {
                    IoError::storage("SKP adapter", e.to_string())
                }
            })?;
        // The converter reads its complete input before producing output.
        // Close stdin even after a write error, then reap the process.
        let written = child
            .stdin
            .take()
            .ok_or_else(adapter::unavailable)
            .and_then(|mut input| {
                input
                    .write_all(bytes)
                    .map_err(|e| IoError::storage("SKP adapter input", e.to_string()))
            });
        let output = child
            .wait_with_output()
            .map_err(|e| IoError::storage("SKP adapter", e.to_string()))?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr);
            return Err(IoError::corrupt(
                "SKP",
                message.chars().take(4096).collect::<String>(),
            ));
        }
        written?;
        adapter::decode(&output.stdout)
    }

    fn read(&self, key: &StorageKey) -> Result<Vec<u8>, IoError> {
        std::fs::read(self.path_for(key)).map_err(|e| Self::map_io(key, &e))
    }

    fn write(&self, key: &StorageKey, bytes: &[u8]) -> Result<(), IoError> {
        if self.read_only {
            return Err(IoError::ReadOnly(key.0.clone()));
        }
        self.write_atomic(&self.path_for(key), bytes)
            .map_err(|e| Self::map_io(key, &e))
    }

    fn delete(&self, key: &StorageKey) -> Result<(), IoError> {
        if self.read_only {
            return Err(IoError::ReadOnly(key.0.clone()));
        }
        match std::fs::remove_file(self.path_for(key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Self::map_io(key, &e)),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<StorageEntry>, IoError> {
        let dir_part = prefix.rsplit_once('/').map_or("", |(d, _)| d);
        let dir_key = if dir_part.is_empty() {
            String::new()
        } else {
            format!("{dir_part}/")
        };
        let dir = match &self.root {
            Some(root) if dir_part.is_empty() => root.clone(),
            Some(root) => root.join(dir_part),
            None if dir_part.is_empty() => std::path::PathBuf::from("."),
            None => std::path::PathBuf::from(dir_part),
        };
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Self::map_io(&StorageKey::new(prefix), &e)),
        };
        let mut rows = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|e| Self::map_io(&StorageKey::new(prefix), &e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if Self::is_temp_name(&name) {
                continue;
            }
            let key = format!("{dir_key}{name}");
            if !key.starts_with(prefix) {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(Self::map_io(&StorageKey::new(&key), &e)),
            };
            if !meta.is_file() {
                continue;
            }
            let modified_unix_s = meta.modified().ok().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .and_then(|d| i64::try_from(d.as_secs()).ok())
            });
            rows.push(StorageEntry {
                key: StorageKey(key),
                size: meta.len(),
                modified_unix_s,
            });
        }
        rows.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(rows)
    }

    fn exists(&self, key: &StorageKey) -> Result<bool, IoError> {
        match std::fs::metadata(self.path_for(key)) {
            Ok(meta) => Ok(meta.is_file()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Self::map_io(key, &e)),
        }
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

    /// A fresh scratch directory per test (tests may touch `std::fs`).
    #[cfg(not(target_arch = "wasm32"))]
    struct Scratch(std::path::PathBuf);

    #[cfg(not(target_arch = "wasm32"))]
    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "planyrith-core-storage-{}-{tag}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_fs_passes_conformance() {
        let scratch = Scratch::new("conformance");
        let fs = NativeFs::rooted(&scratch.0);
        conformance_check(&fs).unwrap();
        assert!(fs.capabilities().persistent && fs.capabilities().shareable_path);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_fs_write_is_atomic_and_rotates_backups() {
        let scratch = Scratch::new("atomic");
        let fs = NativeFs {
            backups: 2,
            ..NativeFs::rooted(&scratch.0)
        };
        let key = StorageKey::from("docs/house.plyr");
        fs.write(&key, b"v1").unwrap();
        fs.write(&key, b"v2").unwrap();
        fs.write(&key, b"v3").unwrap();
        fs.write(&key, b"v4").unwrap();
        assert_eq!(fs.read(&key).unwrap(), b"v4");
        assert_eq!(fs.read(&key.backup(1)).unwrap(), b"v3");
        assert_eq!(fs.read(&key.backup(2)).unwrap(), b"v2");
        assert_eq!(fs.read(&key.backup(3)).unwrap_err().code(), "E1403");
        // No temp file is left behind; listing is by key prefix, in key order.
        let names: Vec<String> = fs
            .list("docs/")
            .unwrap()
            .into_iter()
            .map(|e| e.key.0)
            .collect();
        assert_eq!(
            names,
            vec!["docs/house.plyr", "docs/house.plyr~1", "docs/house.plyr~2"]
        );
        assert!(!NativeFs::temp_path(&fs.path_for(&key)).exists());
        assert_eq!(fs.list("docs/house.plyr~").unwrap().len(), 2);
        assert!(fs.list("missing/").unwrap().is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_fs_read_only_and_not_found() {
        let scratch = Scratch::new("readonly");
        let fs = NativeFs {
            read_only: true,
            ..NativeFs::rooted(&scratch.0)
        };
        let key = StorageKey::from("a.plyr");
        assert_eq!(fs.write(&key, b"x").unwrap_err().code(), "E1501");
        assert_eq!(fs.delete(&key).unwrap_err().code(), "E1501");
        assert_eq!(
            fs.read(&key).unwrap_err(),
            IoError::NotFound("a.plyr".into())
        );
        assert!(!fs.exists(&key).unwrap());
    }
}
