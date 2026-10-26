//! Implements docs/11-persistence-and-web.md §1–§4 for the GUI: what differs
//! between the desktop and the browser — the storage documents are read from
//! and written to, the file dialogs browsing it, the clock and where untitled
//! work is autosaved. Everything else in the GUI is the same code on both.

use std::sync::Arc;

use egui_file_dialog::FileDialog;
use planyrith_core::io::storage::{Storage, StorageKey};

#[cfg(target_arch = "wasm32")]
pub mod web;

/// The storage behind documents, file dialogs and file commands: the file
/// system on the desktop, the browser's storage on the web.
pub fn storage() -> Arc<dyn Storage + Send + Sync> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        Arc::new(planyrith_core::io::storage::NativeFs::new())
    }
    #[cfg(target_arch = "wasm32")]
    {
        web::storage()
    }
}

/// A file dialog browsing [`storage`].
pub fn file_dialog() -> FileDialog {
    #[cfg(not(target_arch = "wasm32"))]
    {
        FileDialog::new()
    }
    #[cfg(target_arch = "wasm32")]
    {
        web::file_dialog()
    }
}

/// Seconds since 1970-01-01 UTC.
pub fn unix_seconds() -> i64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
    }
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() / 1000.0) as i64
    }
}

/// Current UTC time as RFC 3339 (the GUI supplies the clock, not the core).
pub fn now_rfc3339() -> String {
    let secs = unix_seconds().max(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a proleptic Gregorian date.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The key an untitled document is autosaved under (docs/11 §2): in the
/// application's data folder on the desktop, a hidden name in the browser.
pub fn untitled_key() -> StorageKey {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = eframe::storage_dir("Planyrith").unwrap_or_else(std::env::temp_dir);
        StorageKey::new(dir.join("untitled.plyr").to_string_lossy().to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        StorageKey::new("/.untitled.plyr")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_reads_as_rfc3339() {
        let now = now_rfc3339();
        assert_eq!(now.len(), 20);
        assert!(now.ends_with('Z') && now.starts_with("20"));
    }
}
