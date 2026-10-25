//! The open document: engine, file metadata, journal and the caches the
//! viewport needs (docs/07 §1, docs/09). The GUI never mutates the document
//! itself: every change is a registered command (docs/01 §3.3).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use planyrith_core::commands::{CommandRegistry, Delta, Engine, Event};
use planyrith_core::document::Document;
use planyrith_core::io::NoProgress;
use planyrith_core::io::native::{
    HistoryEntry, LoadedDocument, WriteOptions, read_document, write_document,
};
use planyrith_core::io::storage::{NativeFs, Storage, StorageKey};
use planyrith_core::math::Ray;
use planyrith_core::spatial::{DocumentIndex, Hit, HitFilter};
use serde_json::Value;

/// An open document.
pub struct Session {
    /// The engine owning the document.
    pub engine: Engine,
    /// Where it was read from or last saved to.
    pub path: Option<PathBuf>,
    loaded: Option<LoadedDocument>,
    history: Vec<HistoryEntry>,
    saved_generation: u64,
    /// Deltas since the viewport last looked (render cache invalidation).
    deltas: Rc<RefCell<Vec<Delta>>>,
    /// Warnings the engine emitted, for the status bar.
    warnings: Rc<RefCell<Vec<String>>>,
    index: Option<(u64, DocumentIndex)>,
}

impl Session {
    /// A new, empty document.
    pub fn new() -> Session {
        Session::build(Document::new(), None, None)
    }

    /// Opens a `.plyr` file.
    pub fn open(path: &Path) -> Result<Session, String> {
        let bytes = NativeFs::new()
            .read(&key(path))
            .map_err(|e| e.to_string())?;
        let mut loaded = read_document(&bytes, &NoProgress).map_err(|e| e.to_string())?;
        let doc = std::mem::take(&mut loaded.document);
        let session = Session::build(doc, Some(loaded), Some(path.to_path_buf()));
        for w in session
            .loaded
            .as_ref()
            .map(|l| l.warnings.clone())
            .unwrap_or_default()
        {
            session.warnings.borrow_mut().push(w.message);
        }
        Ok(session)
    }

    fn build(doc: Document, loaded: Option<LoadedDocument>, path: Option<PathBuf>) -> Session {
        let history = loaded
            .as_ref()
            .map(|l| l.history.clone())
            .unwrap_or_default();
        let mut engine = Engine::new(doc, Arc::new(CommandRegistry::with_builtins()));
        if let Some(last) = history.last() {
            engine.set_last_created(last.created.clone());
        }
        let deltas: Rc<RefCell<Vec<Delta>>> = Rc::default();
        let warnings: Rc<RefCell<Vec<String>>> = Rc::default();
        let (d, w) = (Rc::clone(&deltas), Rc::clone(&warnings));
        engine
            .events_mut()
            .subscribe(Box::new(move |event| match event {
                Event::DocumentChanged { deltas, .. } => {
                    d.borrow_mut().extend(deltas.iter().cloned())
                }
                Event::Warning(warning) => w.borrow_mut().push(warning.message.clone()),
                _ => {}
            }));
        Session {
            engine,
            path,
            loaded,
            history,
            saved_generation: 0,
            deltas,
            warnings,
            index: None,
        }
    }

    /// The document.
    pub fn doc(&self) -> &Document {
        self.engine.document()
    }

    /// `true` when there are unsaved changes.
    pub fn dirty(&self) -> bool {
        self.engine.generation() != self.saved_generation
    }

    /// The file name for the window title.
    pub fn title(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_name()).map_or_else(
            || "Untitled".to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// Runs a command and records it in the journal (docs/09 §3).
    pub fn execute(&mut self, name: &str, params: Value) -> Result<Value, String> {
        let view_only = self
            .engine
            .registry()
            .get(name)
            .map(|c| c.meta().view_only)
            .unwrap_or(false);
        let output = self
            .engine
            .execute_erased(name, params.clone())
            .map_err(|e| format!("{name}: {e} [{}]", e.code()))?;
        if !view_only {
            self.history.push(HistoryEntry {
                command: name.to_string(),
                params,
                group: None,
                timestamp: Some(now_rfc3339()),
                created: self.engine.last_created().to_vec(),
            });
        }
        Ok(output)
    }

    /// Undoes the last step.
    pub fn undo(&mut self) -> Result<(), String> {
        self.engine.undo().map_err(|e| e.to_string())
    }

    /// Redoes the last undone step.
    pub fn redo(&mut self) -> Result<(), String> {
        self.engine.redo().map_err(|e| e.to_string())
    }

    /// Saves to `path` (or where it came from).
    pub fn save(&mut self, path: Option<&Path>) -> Result<PathBuf, String> {
        let target = path
            .map(Path::to_path_buf)
            .or_else(|| self.path.clone())
            .ok_or_else(|| "choose where to save".to_string())?;
        let now = now_rfc3339();
        let mut opts = WriteOptions {
            created: now.clone(),
            modified: now,
            generator: format!("planyrith-gui {}", env!("CARGO_PKG_VERSION")),
            ..WriteOptions::default()
        };
        if let Some(loaded) = &self.loaded {
            opts = opts.carry_over(loaded);
        }
        let bytes = write_document(self.engine.document(), &self.history, &opts, &NoProgress)
            .map_err(|e| e.to_string())?;
        NativeFs::new()
            .write(&key(&target), &bytes)
            .map_err(|e| e.to_string())?;
        self.path = Some(target.clone());
        self.saved_generation = self.engine.generation();
        Ok(target)
    }

    /// Takes the deltas since the last call.
    pub fn take_deltas(&self) -> Vec<Delta> {
        std::mem::take(&mut *self.deltas.borrow_mut())
    }

    /// Takes the warnings since the last call.
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.borrow_mut())
    }

    /// Ray cast for picking (docs/06 §8), with the spatial index rebuilt
    /// when the document changed.
    pub fn ray_cast(&mut self, ray: &Ray, filter: &HitFilter) -> Vec<Hit> {
        let generation = self.engine.generation();
        if self.index.as_ref().is_none_or(|(g, _)| *g != generation) {
            self.index = Some((generation, DocumentIndex::build(self.engine.document())));
        }
        match &self.index {
            Some((_, index)) => index.ray_cast(self.engine.document(), ray, filter),
            None => Vec::new(),
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

fn key(path: &Path) -> StorageKey {
    StorageKey::new(path.to_string_lossy().to_string())
}

/// Current UTC time as RFC 3339 (the GUI supplies the clock, not the core).
fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a proleptic Gregorian date.
    let z = days as i64 + 719_468;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    #[test]
    fn commands_are_journaled_and_saving_clears_the_dirty_flag() {
        let dir = std::env::temp_dir().join(format!("planyrith-gui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("box.plyr");
        let mut s = Session::new();
        assert!(!s.dirty());
        s.execute(
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [1000, 500, 0] }),
        )
        .unwrap();
        s.execute("camera-standard", json!({ "view": "top" }))
            .unwrap();
        assert!(s.dirty());
        assert_eq!(s.history.len(), 1, "view commands are not journaled");
        assert!(!s.take_deltas().is_empty());
        assert!(
            s.execute(
                "push-pull",
                json!({ "face": { "Pid": 999 }, "distance": 1 })
            )
            .is_err()
        );
        s.save(Some(&path)).unwrap();
        assert!(!s.dirty());
        assert_eq!(s.title(), "box.plyr");

        let mut reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.doc().model.counts().faces, 1);
        // `last` survives through the journal (docs/08 Decisions).
        reopened
            .execute(
                "push-pull",
                json!({ "face": { "Query": "face & last" }, "distance": 300.0 }),
            )
            .unwrap();
        assert_eq!(reopened.doc().model.counts().faces, 6);
        reopened
            .execute("select", json!({ "query": "face & normal:z" }))
            .unwrap();
        reopened
            .execute("delete", json!({ "targets": "selection" }))
            .unwrap();
        assert_eq!(reopened.doc().model.counts().faces, 5);
        let ray = planyrith_core::math::Ray::new(
            glam::DVec3::new(500.0, 250.0, 5000.0),
            glam::DVec3::NEG_Z,
        );
        let hits = reopened.ray_cast(&ray, &HitFilter::default());
        assert!(!hits.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
