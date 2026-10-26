//! Implements the `io/` entry of docs/01-architecture.md §2 and the error model
//! of docs/01 §7 for I/O: `IoError` (codes from docs/08 §9), the `Progress` /
//! `CancelToken` cooperation contract of docs/01 §6, and the sub-modules for
//! storage (docs/11 §1), the native container (docs/09 §1–§4), interchange
//! (docs/09 §5–§6) and render-mesh extraction (docs/06 §2).
//!
//! Owner key: `io-native` for this file. The `#![warn(unused_variables,
//! dead_code)]` opt-out of the skeleton allowance lives in the leaf modules
//! this owner implements (`storage`, `native`, `export::json`), not here,
//! because an attribute on this file would cascade into `render_mesh` and
//! `import`, which still carry `todo!()` bodies.

pub mod export;
pub mod import;
pub mod native;
pub mod overlay;
pub mod render_mesh;
pub mod storage;

pub use storage::{MemoryStorage, Storage, StorageCaps, StorageEntry, StorageKey};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// I/O and format failures (docs/01 §7) with stable codes (docs/08 §9; `E1403`
/// added for storage errors by docs/08 Decisions).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IoError {
    /// `E1401` — the file's `format_version` is newer than this reader supports.
    #[error("document format version {found} is newer than the supported {max}")]
    UnsupportedVersion {
        /// Version found in the file.
        found: u32,
        /// Highest version this build reads.
        max: u32,
    },
    /// `E1401` — the format or feature is not supported by this build
    /// (e.g. STEP without `brep`).
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    /// `E1402` — the container or a part is corrupt; `part` names the failing
    /// member (docs/09 §8).
    #[error("corrupt container: {part}: {reason}")]
    Corrupt {
        /// Container part or stream name.
        part: String,
        /// Explanation.
        reason: String,
    },
    /// `E1403` — the storage backend failed (missing key, unreadable, quota).
    #[error("storage error on `{key}`: {reason}")]
    Storage {
        /// Storage key or path.
        key: String,
        /// Explanation.
        reason: String,
    },
    /// `E1403` — the key does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// `E1501` — the session or backend is read-only.
    #[error("read-only: {0}")]
    ReadOnly(String),
    /// `E1402` — a size limit was exceeded (part > 4 GB, image > 16384 px).
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),
    /// `E1701` — cancelled through a [`CancelToken`].
    #[error("cancelled")]
    Cancelled,
}

impl IoError {
    /// Stable machine-readable code (docs/08 §9).
    pub const fn code(&self) -> &'static str {
        match self {
            IoError::UnsupportedVersion { .. } | IoError::UnsupportedFormat(_) => "E1401",
            IoError::Corrupt { .. } | IoError::LimitExceeded(_) => "E1402",
            IoError::Storage { .. } | IoError::NotFound(_) => "E1403",
            IoError::ReadOnly(_) => "E1501",
            IoError::Cancelled => "E1701",
        }
    }

    /// Builds a `Corrupt` error.
    pub fn corrupt(part: impl Into<String>, reason: impl Into<String>) -> Self {
        IoError::Corrupt {
            part: part.into(),
            reason: reason.into(),
        }
    }

    /// Builds a `Storage` error.
    pub fn storage(key: impl Into<String>, reason: impl Into<String>) -> Self {
        IoError::Storage {
            key: key.into(),
            reason: reason.into(),
        }
    }
}

/// Cooperative cancellation shared between a worker and the UI (docs/01 §6).
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// A token that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    /// `true` once cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// `Err(IoError::Cancelled)` once cancelled.
    pub fn check(&self) -> Result<(), IoError> {
        if self.is_cancelled() {
            Err(IoError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Progress reporting for long operations (docs/01 §6, docs/09 §6).
/// Implementations are frontend-specific; [`NoProgress`] discards everything.
pub trait Progress {
    /// Reports progress `fraction ∈ 0..=1` with a short message.
    fn report(&self, fraction: f32, message: &str);
    /// `true` when the operation should stop (`IoError::Cancelled`).
    fn is_cancelled(&self) -> bool;
    /// `Err(IoError::Cancelled)` when [`Progress::is_cancelled`] is `true`;
    /// long operations call it between steps.
    fn check_cancelled(&self) -> Result<(), IoError> {
        if self.is_cancelled() {
            Err(IoError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// A `Progress` that ignores reports and never cancels.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoProgress;

impl Progress for NoProgress {
    fn report(&self, _fraction: f32, _message: &str) {}

    fn is_cancelled(&self) -> bool {
        false
    }
}

impl Progress for CancelToken {
    fn report(&self, _fraction: f32, _message: &str) {}

    fn is_cancelled(&self) -> bool {
        CancelToken::is_cancelled(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_token_flips_once() {
        let t = CancelToken::new();
        let shared = t.clone();
        assert!(t.check().is_ok());
        assert!(Progress::check_cancelled(&t).is_ok());
        shared.cancel();
        assert!(t.is_cancelled());
        assert_eq!(t.check(), Err(IoError::Cancelled));
        assert!(Progress::is_cancelled(&t));
        assert_eq!(Progress::check_cancelled(&t), Err(IoError::Cancelled));
        assert!(NoProgress.check_cancelled().is_ok());
    }

    #[test]
    fn codes_are_stable() {
        assert_eq!(
            IoError::corrupt("document.msgpack", "truncated").code(),
            "E1402"
        );
        assert_eq!(IoError::NotFound("x".into()).code(), "E1403");
        assert_eq!(IoError::ReadOnly("session".into()).code(), "E1501");
        assert_eq!(
            IoError::UnsupportedVersion { found: 2, max: 1 }.code(),
            "E1401"
        );
        assert_eq!(IoError::LimitExceeded("x".into()).code(), "E1402");
        assert_eq!(IoError::Cancelled.code(), "E1701");
    }
}
