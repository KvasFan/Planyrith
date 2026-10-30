//! The open document: engine, file metadata, journal, autosave (docs/11 §2)
//! and the caches the viewport needs (docs/07 §1, docs/09). The GUI never
//! mutates the document itself: every change is a registered command
//! (docs/01 §3.3).

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use planyrith_core::commands::{CommandRegistry, Delta, Engine, Event};
use planyrith_core::document::{Camera, ContainerRef, Document};
use planyrith_core::inference::{Inference, InferenceEngine, InferenceRequest};
use planyrith_core::io::NoProgress;
use planyrith_core::io::autosave::{self, Autosave, RowState};
#[cfg(not(target_arch = "wasm32"))]
use planyrith_core::io::native::read_document;
use planyrith_core::io::native::{HistoryEntry, LoadedDocument, WriteOptions, write_document};
use planyrith_core::io::storage::{Storage, StorageKey};
use planyrith_core::math::Ray;
use planyrith_core::spatial::{DocumentIndex, Hit, HitFilter};
use serde_json::Value;

use crate::platform::{self, now_rfc3339};

/// An open document.
pub struct Session {
    #[cfg(not(target_arch = "wasm32"))]
    remote: Option<crate::remote::Remote>,
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
    registry: Arc<CommandRegistry>,
    /// A camera change for the viewport to animate: from, to, seconds
    /// (a scene's transition, docs/07 §6).
    transition: Option<(Camera, Camera, f64)>,
    storage: Arc<dyn Storage + Send + Sync>,
    /// Journal and snapshots of unsaved work, when enabled.
    autosave: Option<Autosave>,
    /// A snapshot is due whatever the journal (after undo and redo, which
    /// the journal does not replay).
    snapshot_now: bool,
    /// The autosave clock started.
    clock_started: bool,
    /// This session wrote autosave files (only those are its to delete).
    autosaved: bool,
}

impl Session {
    /// A new, empty document.
    pub fn new() -> Session {
        Session::build(Document::new(), None, None)
    }

    /// Opens a native or adapter-backed SKP model.
    pub fn open(path: &Path) -> Result<Session, String> {
        let bytes = platform::storage()
            .read(&key(path))
            .map_err(|e| e.to_string())?;
        let mut loaded = planyrith_core::io::model_file::read(
            &bytes,
            &path.to_string_lossy(),
            platform::storage().as_ref(),
        )
        .map_err(|e| e.to_string())?;
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
        let storage = platform::storage();
        let registry = Arc::new(
            crate::extensions::registry(storage.as_ref())
                .unwrap_or_else(|_| CommandRegistry::with_builtins()),
        );
        let mut engine = Engine::new(doc, Arc::clone(&registry));
        engine.set_storage(Arc::clone(&storage));
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
            #[cfg(not(target_arch = "wasm32"))]
            remote: None,
            engine,
            path,
            loaded,
            history,
            saved_generation: 0,
            deltas,
            warnings,
            index: None,
            registry,
            transition: None,
            storage,
            autosave: None,
            snapshot_now: false,
            clock_started: false,
            autosaved: false,
        }
    }

    /// The key this document's unsaved work is autosaved under.
    fn autosave_key(path: Option<&Path>) -> StorageKey {
        path.map_or_else(platform::untitled_key, key)
    }

    /// Journals commands and snapshots the document from now on (docs/11
    /// §2); the GUI turns it on, tests leave it off.
    pub fn enable_autosave(&mut self) {
        if self.is_attached() {
            return;
        }
        let key = Session::autosave_key(self.path.as_deref());
        self.autosave = Some(Autosave::new(key, 0.0));
    }

    /// Deletes this document's autosave (its changes were saved or
    /// discarded).
    pub fn drop_autosave(&mut self) {
        if !self.autosaved {
            return;
        }
        if let Some(autosave) = self.autosave.as_mut() {
            let key = autosave.doc_key().clone();
            if let Err(e) = autosave.reset(self.storage.as_ref(), key, 0.0) {
                self.warnings.borrow_mut().push(format!("autosave: {e}"));
            }
        }
    }

    /// Marks the current state as saved (an example just built).
    pub fn mark_saved(&mut self) {
        self.saved_generation = self.engine.generation();
    }

    /// The document as `.plyr` bytes (a download).
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        self.encode(&now_rfc3339())
    }

    /// `true` when the document at `path` (`None`: untitled) has unsaved work
    /// to recover.
    pub fn recoverable(path: Option<&Path>) -> bool {
        autosave::pending(platform::storage().as_ref(), &Session::autosave_key(path))
    }

    /// Drops the unsaved work of the document at `path`.
    pub fn discard_recovery(path: Option<&Path>) -> Result<(), String> {
        autosave::discard(platform::storage().as_ref(), &Session::autosave_key(path))
            .map_err(|e| e.to_string())
    }

    /// The document at `path` (`None`: untitled) with its unsaved work
    /// recovered: its snapshot with the journal replayed. Unsaved until
    /// saved; the commands that failed to replay are in the warnings.
    pub fn recover(path: Option<&Path>) -> Result<Session, String> {
        let registry = Arc::new(CommandRegistry::with_builtins());
        let recovered =
            autosave::recover(platform::storage(), &Session::autosave_key(path), registry)
                .map_err(|e| e.to_string())?;
        let doc = recovered.engine.document().clone();
        let mut session = Session::build(doc, recovered.loaded, path.map(Path::to_path_buf));
        session.history = recovered.history;
        if let Some(last) = session.history.last() {
            session.engine.set_last_created(last.created.clone());
        }
        // Unsaved: the dirty flag stays up until an explicit save.
        session.saved_generation = u64::MAX;
        session.snapshot_now = true;
        for skipped in recovered.skipped {
            session
                .warnings
                .borrow_mut()
                .push(format!("not recovered: {skipped}"));
        }
        Ok(session)
    }

    /// Takes a snapshot when one is due (docs/11 §2); call every frame with
    /// the frontend's clock in seconds. Failures go to the warnings.
    pub fn tick(&mut self, now_s: f64) {
        let Some(mut autosave) = self.autosave.take() else {
            return;
        };
        if !self.clock_started {
            autosave.restart_clock(now_s);
            self.clock_started = true;
        }
        if self.snapshot_now || autosave.due(now_s) {
            self.snapshot_now = false;
            self.autosaved = true;
            let result = if self.dirty() {
                self.encode(&now_rfc3339()).and_then(|bytes| {
                    autosave
                        .snapshot(self.storage.as_ref(), &bytes, now_s)
                        .map_err(|e| e.to_string())
                })
            } else {
                // Back to the saved state: nothing to recover.
                let key = autosave.doc_key().clone();
                autosave
                    .reset(self.storage.as_ref(), key, now_s)
                    .map_err(|e| e.to_string())
            };
            if let Err(e) = result {
                self.warnings.borrow_mut().push(format!("autosave: {e}"));
            }
        }
        self.autosave = Some(autosave);
    }

    /// The document as `.plyr` bytes, stamped `now`.
    fn encode(&self, now: &str) -> Result<Vec<u8>, String> {
        let mut opts = WriteOptions {
            created: now.to_string(),
            modified: now.to_string(),
            generator: format!("planyrith-gui {}", env!("CARGO_PKG_VERSION")),
            ..WriteOptions::default()
        };
        if let Some(loaded) = &self.loaded {
            opts = opts.carry_over(loaded);
        }
        write_document(self.engine.document(), &self.history, &opts, &NoProgress)
            .map_err(|e| e.to_string())
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
        #[cfg(not(target_arch = "wasm32"))]
        if self.remote.is_some() {
            let selection = self
                .engine
                .selection()
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>();
            let context = self.engine.context().path.to_string();
            let remote = self
                .remote
                .as_mut()
                .ok_or("Remote connection disappeared")?;
            remote.call("context.set", serde_json::json!({"path":context}))?;
            remote.call("selection.set",serde_json::json!({"query":if selection.is_empty() {"!all".into()} else {selection.join(" | ")}}))?;
            let output = remote.call(
                "command.execute",
                serde_json::json!({"name":name,"params":params}),
            )?;
            if let Some(warnings) = output["warnings"].as_array() {
                self.warnings.borrow_mut().extend(
                    warnings
                        .iter()
                        .filter_map(|w| w["message"].as_str().map(str::to_string)),
                );
            }
            self.refresh_remote(
                !name.starts_with("camera-")
                    && !name.starts_with("zoom-")
                    && name != "scene-activate",
            )?;
            return Ok(output["output"].clone());
        }
        let view_only = self
            .engine
            .registry()
            .get(name)
            .map(|c| c.meta().view_only)
            .unwrap_or(false);
        let camera_before = self.engine.document().camera;
        // What the command acts on, for the autosave journal.
        let state = self
            .autosave
            .is_some()
            .then(|| RowState::capture(&self.engine));
        let output = if name == "script-eval" {
            let p: planyrith_core::scripting::ScriptParams =
                serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
            let start = web_time::Instant::now();
            let control = planyrith_core::scripting::ScriptControl {
                interrupted: Some(Arc::new(move || start.elapsed().as_secs() >= 60)),
                ..Default::default()
            };
            serde_json::to_value(
                planyrith_core::scripting::eval(
                    &mut self.engine,
                    &p.source,
                    &p.args,
                    Default::default(),
                    control,
                )
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
        } else {
            self.engine
                .execute_erased(name, params.clone())
                .map_err(|e| format!("{name}: {e} [{}]", e.code()))?
        };
        // Scenes animate the camera unless asked not to.
        if name == "scene-activate"
            && params.get("no_transition").and_then(Value::as_bool) != Some(true)
            && self.engine.document().camera != camera_before
        {
            let doc = self.engine.document();
            let seconds = params
                .get("scene")
                .and_then(Value::as_str)
                .and_then(|n| doc.scenes.by_name(n))
                .and_then(|id| doc.scenes.get(id))
                .map_or(0.0, |s| f64::from(s.transition_s));
            if seconds > 0.0 {
                self.transition = Some((camera_before, doc.camera, seconds));
            }
        }
        if !view_only {
            if let Some(autosave) = self.autosave.as_mut() {
                self.autosaved = true;
                let state = state.unwrap_or_default();
                if let Err(e) = autosave.record(self.storage.as_ref(), name, &params, None, state) {
                    self.warnings.borrow_mut().push(format!("autosave: {e}"));
                }
            }
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
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(remote) = self.remote.as_mut() {
            remote.call("history.undo", serde_json::json!({}))?;
            return self.refresh_remote(true);
        }
        self.engine.undo().map_err(|e| e.to_string())?;
        self.snapshot_now = true;
        Ok(())
    }

    /// Redoes the last undone step.
    pub fn redo(&mut self) -> Result<(), String> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(remote) = self.remote.as_mut() {
            remote.call("history.redo", serde_json::json!({}))?;
            return self.refresh_remote(true);
        }
        self.engine.redo().map_err(|e| e.to_string())?;
        self.snapshot_now = true;
        Ok(())
    }

    /// Saves to `path` (or where it came from).
    pub fn save(&mut self, path: Option<&Path>) -> Result<PathBuf, String> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(remote) = self.remote.as_mut() {
            let result = remote.call(
                "session.save",
                serde_json::json!({"path":path.map(|p|p.to_string_lossy().to_string())}),
            )?;
            if let Some(warnings) = result["warnings"].as_array() {
                self.warnings.borrow_mut().extend(
                    warnings
                        .iter()
                        .filter_map(|w| w["message"].as_str().map(str::to_string)),
                );
            }
            self.refresh_remote(true)?;
            self.mark_saved();
            return self
                .path
                .clone()
                .ok_or_else(|| "Remote save did not return a path".into());
        }
        let target = path
            .map(Path::to_path_buf)
            .or_else(|| self.path.clone())
            .ok_or_else(|| "choose where to save".to_string())?;
        let bytes = self.encode(&now_rfc3339())?;
        let converted = planyrith_core::io::model_file::for_save(
            bytes,
            &target.to_string_lossy(),
            self.storage.as_ref(),
        )
        .map_err(|e| e.to_string())?;
        self.warnings.borrow_mut().extend(converted.warnings);
        self.storage
            .write(&key(&target), &converted.bytes)
            .map_err(|e| e.to_string())?;
        if let Some(autosave) = self.autosave.as_mut() {
            // Saved work needs no recovery; the new name autosaves from now.
            autosave
                .reset(self.storage.as_ref(), key(&target), 0.0)
                .map_err(|e| e.to_string())?;
            self.clock_started = false;
        }
        #[cfg(target_arch = "wasm32")]
        platform::web::request_persistence();
        self.path = Some(target.clone());
        self.saved_generation = self.engine.generation();
        Ok(target)
    }

    /// Runs a command on a copy of the document (a tool's preview, docs/07
    /// §3): the copy and the containers the command changed. The document
    /// itself, its history and its journal are untouched.
    pub fn preview(
        &self,
        name: &str,
        params: Value,
    ) -> Result<(Document, BTreeSet<ContainerRef>), String> {
        let mut fork = Engine::new(self.doc().clone(), Arc::clone(&self.registry));
        fork.set_storage(Arc::clone(&self.storage));
        fork.set_last_created(self.engine.last_created().to_vec());
        fork.set_context(self.engine.context().clone())
            .map_err(|e| e.to_string())?;
        fork.set_selection(self.engine.selection().clone());
        let touched: Rc<RefCell<BTreeSet<ContainerRef>>> = Rc::default();
        let everything = Rc::new(Cell::new(false));
        let (t, all) = (Rc::clone(&touched), Rc::clone(&everything));
        fork.events_mut().subscribe(Box::new(move |event| {
            if let Event::DocumentChanged { deltas, .. } = event {
                for delta in deltas.iter() {
                    match delta.container() {
                        Some(c) => {
                            t.borrow_mut().insert(c);
                        }
                        None => all.set(true),
                    }
                }
            }
        }));
        fork.execute_erased(name, params)
            .map_err(|e| format!("{name}: {e}"))?;
        let doc = fork.into_document();
        let mut touched = touched.take();
        if everything.get() {
            touched.extend(self.doc().containers());
            touched.extend(doc.containers());
        }
        Ok((doc, touched))
    }

    /// Refreshes tools after installing or enabling an extension.
    pub fn reload_extensions(&mut self) -> Result<(), String> {
        let registry = Arc::new(
            crate::extensions::registry(self.storage.as_ref()).map_err(|e| e.to_string())?,
        );
        self.engine.set_registry(registry.clone());
        self.registry = registry;
        Ok(())
    }

    /// Whether this session dispatches to a native local server.
    pub fn is_attached(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.remote.is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }
    /// Undo label from the authoritative history.
    pub fn undo_label(&self) -> Option<&str> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(remote) = &self.remote {
            return remote.undo.as_deref();
        }
        self.engine.history().undo_label()
    }
    /// Redo label from the authoritative history.
    pub fn redo_label(&self) -> Option<&str> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(remote) = &self.remote {
            return remote.redo.as_deref();
        }
        self.engine.history().redo_label()
    }
    /// Attaches to a local TCP port or Unix socket. Empty `doc_id` creates a
    /// server document; `d1` chooses the document opened with `serve -f`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn attach(address: &str, token: &str, doc_id: &str) -> Result<Self, String> {
        let mut session = Self::new();
        session.remote = Some(crate::remote::Remote::connect(address, token, doc_id)?);
        session.refresh_remote(false)?;
        session.mark_saved();
        Ok(session)
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn refresh_remote(&mut self, preserve_camera: bool) -> Result<(), String> {
        let remote = self.remote.as_mut().ok_or("No remote connection")?;
        let snapshot = remote.snapshot()?;
        let camera = self.engine.document().camera;
        if remote.generation != snapshot.generation {
            let loaded = read_document(&snapshot.bytes, &NoProgress).map_err(|e| e.to_string())?;
            self.engine.replace_document(loaded.document);
            remote.generation = snapshot.generation;
            self.index = None;
        } else if !preserve_camera {
            let loaded = read_document(&snapshot.bytes, &NoProgress).map_err(|e| e.to_string())?;
            self.engine.set_camera(loaded.document.camera);
        }
        if preserve_camera {
            self.engine.set_camera(camera);
        }
        self.engine
            .set_context(snapshot.context)
            .map_err(|e| e.to_string())?;
        self.engine.set_selection(snapshot.selection);
        self.engine.set_last_created(snapshot.last);
        self.path = snapshot.path;
        Ok(())
    }

    /// Takes the camera transition to animate, if any.
    pub fn take_transition(&mut self) -> Option<(Camera, Camera, f64)> {
        self.transition.take()
    }

    /// Takes the deltas since the last call.
    pub fn take_deltas(&self) -> Vec<Delta> {
        std::mem::take(&mut *self.deltas.borrow_mut())
    }

    /// Takes the warnings since the last call.
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.borrow_mut())
    }

    fn ensure_index(&mut self) {
        let generation = self.engine.generation();
        if self.index.as_ref().is_none_or(|(g, _)| *g != generation) {
            self.index = Some((generation, DocumentIndex::build(self.engine.document())));
        }
    }

    /// Ray cast for picking (docs/06 §8), with the spatial index rebuilt
    /// when the document changed.
    pub fn ray_cast(&mut self, ray: &Ray, filter: &HitFilter) -> Vec<Hit> {
        self.ensure_index();
        match &self.index {
            Some((_, index)) => index.ray_cast(self.engine.document(), ray, filter),
            None => Vec::new(),
        }
    }

    /// Every inference within tolerance, best first, one per point (`Tab`
    /// cycles through them, docs/07 §5).
    pub fn candidates(
        &mut self,
        engine: &InferenceEngine,
        req: &InferenceRequest,
    ) -> Vec<Inference> {
        self.ensure_index();
        let Some((_, index)) = &self.index else {
            return Vec::new();
        };
        let tol = self.engine.document().tolerances();
        let mut out: Vec<Inference> = Vec::new();
        for c in engine.candidates(self.engine.document(), index, req) {
            if out.iter().all(|o| o.point.distance(c.point) > tol.length) {
                out.push(c);
            }
        }
        out
    }

    /// The inference for a request (docs/03 §7).
    pub fn infer(&mut self, engine: &InferenceEngine, req: &InferenceRequest) -> Inference {
        self.ensure_index();
        match &self.index {
            Some((_, index)) => engine.infer(self.engine.document(), index, req),
            None => engine.infer(
                self.engine.document(),
                &DocumentIndex::build(self.engine.document()),
                req,
            ),
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

    #[test]
    fn unsaved_work_is_recovered_after_a_crash() {
        let dir = std::env::temp_dir().join(format!("planyrith-gui-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plan.plyr");
        let mut s = Session::new();
        s.enable_autosave();
        s.execute(
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [1000, 500, 0] }),
        )
        .unwrap();
        s.save(Some(&path)).unwrap();
        assert!(!Session::recoverable(Some(&path)));
        s.execute(
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 300.0 }),
        )
        .unwrap();
        s.tick(0.0);
        s.undo().unwrap();
        s.redo().unwrap();
        s.tick(1.0);
        s.execute("line", json!({ "points": [[0, 0, 300], [1000, 500, 300]] }))
            .unwrap();
        // Selecting is not journaled; deleting the selection is.
        s.execute("select", json!({ "query": "face & normal:z" }))
            .unwrap();
        s.execute("delete", json!({ "targets": "selection" }))
            .unwrap();
        // The process ends here without saving.
        let faces = s.doc().model.counts().faces;
        assert_eq!(faces, 5);
        drop(s);
        assert!(Session::recoverable(Some(&path)));
        let back = Session::recover(Some(&path)).unwrap();
        assert!(back.dirty());
        assert_eq!(back.doc().model.counts().faces, faces);
        assert_eq!(Session::open(&path).unwrap().doc().model.counts().faces, 1);
        Session::discard_recovery(Some(&path)).unwrap();
        assert!(!Session::recoverable(Some(&path)));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
