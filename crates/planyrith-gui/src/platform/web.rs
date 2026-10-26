//! Implements docs/11-persistence-and-web.md §1 and §4.3 for the browser:
//! `BrowserStorage`, the browser's own files (IndexedDB database `planyrith`,
//! object store `files`, one record `{bytes, modified}` per key), the file
//! system the file dialogs browse, uploading files from the computer and
//! downloading them back.
//!
//! The `Storage` trait is synchronous (docs/11 Decisions) and IndexedDB is
//! not: `BrowserStorage` keeps every file in memory, loaded once at startup,
//! and writes changes behind, in order, through one queue. Keys are absolute
//! paths (`/house.plyr`); folders are the keys' prefixes, and an empty folder
//! is a marker key ending in `/`.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use egui_file_dialog::{Disk, Disks, FileDialog, FileSystem, Metadata, UserDirectories};
use planyrith_core::io::IoError;
use planyrith_core::io::storage::{Storage, StorageCaps, StorageEntry, StorageKey};
use rexie::{ObjectStore, Rexie, TransactionMode};
use wasm_bindgen::{JsCast, JsValue};

/// The IndexedDB database.
const DATABASE: &str = "planyrith";
/// Its object store of files.
const FILES: &str = "files";

/// A file: its bytes and when it was last written (Unix seconds).
#[derive(Debug, Clone)]
struct Blob {
    bytes: Vec<u8>,
    modified: i64,
}

/// The browser's files (see the module documentation).
#[derive(Debug, Default)]
pub struct BrowserStorage {
    blobs: Mutex<BTreeMap<String, Blob>>,
    /// Write changes to IndexedDB (off for a scratch instance).
    persist: bool,
}

/// The storage of this page, set once [`install`] loaded it.
static STORAGE: OnceLock<Arc<BrowserStorage>> = OnceLock::new();

/// The page's storage (empty and not persisted before it is loaded).
pub fn storage() -> Arc<dyn Storage + Send + Sync> {
    let s: Arc<BrowserStorage> = Arc::clone(STORAGE.get_or_init(Arc::default));
    s
}

/// Loads the page's storage from IndexedDB; later calls of [`storage`] use
/// it. Call before the first [`storage`].
pub async fn install() -> Result<(), String> {
    let loaded = BrowserStorage::load().await?;
    STORAGE
        .set(Arc::new(loaded))
        .map_err(|_| "the storage is already in use".to_string())
}

impl BrowserStorage {
    /// The files stored in this browser, persisting further changes.
    pub async fn load() -> Result<BrowserStorage, String> {
        let db = database().await?;
        let tx = db
            .transaction(&[FILES], TransactionMode::ReadOnly)
            .map_err(js_error)?;
        let store = tx.store(FILES).map_err(js_error)?;
        let rows = store.scan(None, None, None, None).await.map_err(js_error)?;
        let mut blobs = BTreeMap::new();
        for (key, value) in rows {
            let (Some(key), Some(blob)) = (key.as_string(), decode(&value)) else {
                continue;
            };
            blobs.insert(key, blob);
        }
        Ok(BrowserStorage {
            blobs: Mutex::new(blobs),
            persist: true,
        })
    }

    /// An empty storage that is never written to IndexedDB.
    pub fn scratch() -> BrowserStorage {
        BrowserStorage::default()
    }

    fn blobs(&self) -> MutexGuard<'_, BTreeMap<String, Blob>> {
        self.blobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `true` when `path` is a folder: the root, a marker or a prefix of a key.
    pub fn is_folder(&self, path: &str) -> bool {
        let prefix = folder_prefix(path);
        prefix == "/"
            || self
                .blobs()
                .range(prefix.clone()..)
                .next()
                .is_some_and(|(k, _)| k.starts_with(&prefix))
    }

    /// The folders and files directly in folder `path`.
    pub fn children(&self, path: &str) -> Vec<String> {
        let prefix = folder_prefix(path);
        let mut out: Vec<String> = Vec::new();
        for key in self.blobs().keys() {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            let child = match rest.split_once('/') {
                Some((folder, _)) => format!("{prefix}{folder}"),
                None => format!("{prefix}{rest}"),
            };
            if !rest.is_empty() && out.last() != Some(&child) {
                out.push(child);
            }
        }
        out.dedup();
        out
    }

    fn put(&self, key: &str, bytes: &[u8]) {
        let blob = Blob {
            bytes: bytes.to_vec(),
            modified: super::unix_seconds(),
        };
        if self.persist {
            enqueue(Op::Put(key.to_string(), encode(&blob)));
        }
        self.blobs().insert(key.to_string(), blob);
    }
}

/// `path` as a folder prefix: `/`, or the path with a trailing `/`.
fn folder_prefix(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        format!("{trimmed}/")
    }
}

impl Storage for BrowserStorage {
    fn read(&self, key: &StorageKey) -> Result<Vec<u8>, IoError> {
        self.blobs()
            .get(key.as_str())
            .map(|b| b.bytes.clone())
            .ok_or_else(|| IoError::NotFound(key.to_string()))
    }

    fn write(&self, key: &StorageKey, bytes: &[u8]) -> Result<(), IoError> {
        if key.as_str().ends_with('/') {
            return Err(IoError::Storage {
                key: key.to_string(),
                reason: "a file name cannot end with `/`".into(),
            });
        }
        self.put(key.as_str(), bytes);
        Ok(())
    }

    fn delete(&self, key: &StorageKey) -> Result<(), IoError> {
        if self.blobs().remove(key.as_str()).is_some() && self.persist {
            enqueue(Op::Delete(key.to_string()));
        }
        Ok(())
    }

    fn list(&self, prefix: &str) -> Result<Vec<StorageEntry>, IoError> {
        Ok(self
            .blobs()
            .range(prefix.to_string()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .filter(|(k, _)| !k.ends_with('/'))
            .map(|(k, b)| StorageEntry {
                key: StorageKey::new(k.clone()),
                size: b.bytes.len() as u64,
                modified_unix_s: Some(b.modified),
            })
            .collect())
    }

    fn exists(&self, key: &StorageKey) -> Result<bool, IoError> {
        Ok(self.blobs().contains_key(key.as_str()))
    }

    fn capabilities(&self) -> StorageCaps {
        StorageCaps {
            random_access: true,
            max_size: None,
            persistent: self.persist,
            shareable_path: false,
        }
    }
}

// ----- IndexedDB ----------------------------------------------------------------------------

/// A change waiting to be written.
enum Op {
    Put(String, JsValue),
    Delete(String),
}

thread_local! {
    static QUEUE: RefCell<VecDeque<Op>> = const { RefCell::new(VecDeque::new()) };
    static WRITING: Cell<bool> = const { Cell::new(false) };
    static DB: RefCell<Option<Rc<Rexie>>> = const { RefCell::new(None) };
    static FAILURE: RefCell<Option<String>> = const { RefCell::new(None) };
}

async fn database() -> Result<Rc<Rexie>, String> {
    if let Some(db) = DB.with(|d| d.borrow().clone()) {
        return Ok(db);
    }
    let db = Rc::new(
        Rexie::builder(DATABASE)
            .version(1)
            .add_object_store(ObjectStore::new(FILES))
            .build()
            .await
            .map_err(js_error)?,
    );
    DB.with(|d| *d.borrow_mut() = Some(Rc::clone(&db)));
    Ok(db)
}

fn enqueue(op: Op) {
    QUEUE.with(|q| q.borrow_mut().push_back(op));
    if !WRITING.with(Cell::get) {
        WRITING.with(|w| w.set(true));
        wasm_bindgen_futures::spawn_local(drain());
    }
}

/// Writes the queue in order.
async fn drain() {
    while let Some(op) = QUEUE.with(|q| q.borrow_mut().pop_front()) {
        if let Err(e) = apply(op).await {
            FAILURE.with(|f| *f.borrow_mut() = Some(e));
        }
    }
    WRITING.with(|w| w.set(false));
}

async fn apply(op: Op) -> Result<(), String> {
    let db = database().await?;
    let tx = db
        .transaction(&[FILES], TransactionMode::ReadWrite)
        .map_err(js_error)?;
    let store = tx.store(FILES).map_err(js_error)?;
    match op {
        Op::Put(key, value) => {
            store
                .put(&value, Some(&JsValue::from_str(&key)))
                .await
                .map_err(js_error)?;
        }
        Op::Delete(key) => store
            .delete(JsValue::from_str(&key))
            .await
            .map_err(js_error)?,
    }
    tx.done().await.map_err(js_error)?;
    Ok(())
}

/// Changes not yet written to IndexedDB (`true` while saving).
pub fn writing() -> bool {
    WRITING.with(Cell::get)
}

/// The last failed write, taken.
pub fn take_failure() -> Option<String> {
    FAILURE.with(|f| f.borrow_mut().take())
}

/// Waits until every change is written to IndexedDB.
pub async fn flush() {
    while writing() {
        sleep_ms(5).await;
    }
}

/// Resolves after `ms` milliseconds (lets the browser run its tasks).
pub async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

fn encode(blob: &Blob) -> JsValue {
    let record = js_sys::Object::new();
    let bytes = js_sys::Uint8Array::from(blob.bytes.as_slice());
    let _ = js_sys::Reflect::set(&record, &"bytes".into(), &bytes);
    let _ = js_sys::Reflect::set(&record, &"modified".into(), &(blob.modified as f64).into());
    record.into()
}

fn decode(value: &JsValue) -> Option<Blob> {
    let bytes = js_sys::Reflect::get(value, &"bytes".into()).ok()?;
    let bytes = bytes.dyn_into::<js_sys::Uint8Array>().ok()?.to_vec();
    let modified = js_sys::Reflect::get(value, &"modified".into())
        .ok()
        .and_then(|m| m.as_f64())
        .unwrap_or(0.0) as i64;
    Some(Blob { bytes, modified })
}

fn js_error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

// ----- the file dialogs' file system -------------------------------------------------------

/// The browser's files as the file dialogs see them.
struct BrowserFiles(Arc<BrowserStorage>);

impl BrowserFiles {
    fn key(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }
}

impl FileSystem for BrowserFiles {
    fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        let key = Self::key(path);
        let blobs = self.0.blobs();
        let blob = blobs
            .get(&key)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        Ok(Metadata::new(
            Some(blob.bytes.len() as u64),
            None,
            None,
            Some("file".to_string()),
        ))
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.0.is_folder(&Self::key(path))
    }

    fn is_file(&self, path: &Path) -> bool {
        self.0.blobs().contains_key(&Self::key(path))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        Ok(self
            .0
            .children(&Self::key(path))
            .into_iter()
            .map(PathBuf::from)
            .collect())
    }

    fn get_disks(&self, _canonicalize_paths: bool) -> Disks {
        Disks::new(vec![Disk::new(
            Some("This browser"),
            Path::new("/"),
            false,
            false,
        )])
    }

    fn is_path_hidden(&self, path: &Path) -> bool {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        name.starts_with('.') || name.ends_with(".journal") || name.ends_with(".autosave.plyr")
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let marker = folder_prefix(&Self::key(path));
        self.0.put(&marker, &[]);
        Ok(())
    }

    fn user_dirs(&self, _canonicalize_paths: bool) -> Option<UserDirectories> {
        None
    }

    fn current_dir(&self) -> io::Result<PathBuf> {
        Ok(PathBuf::from("/"))
    }
}

/// A file dialog over the browser's files.
pub fn file_dialog() -> FileDialog {
    let browser = Arc::clone(STORAGE.get_or_init(Arc::default));
    FileDialog::with_file_system(Arc::new(BrowserFiles(browser)))
        .load_via_thread(false)
        .canonicalize_paths(false)
        .initial_directory(PathBuf::from("/"))
        .show_devices(false)
        .show_pinned_folders(false)
}

// ----- files from and to the computer -------------------------------------------------------

/// Files the user picked to upload, read when ready: name and bytes.
pub type Uploads = Rc<RefCell<Vec<(String, Vec<u8>)>>>;

/// Asks the browser for files from the computer (`accept`: extensions like
/// `.plyr,.obj`); each one read lands in `into`.
pub fn upload(accept: &str, into: Uploads) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Ok(input) = document
        .create_element("input")
        .map(|e| e.unchecked_into::<web_sys::HtmlInputElement>())
    else {
        return;
    };
    input.set_type("file");
    input.set_multiple(true);
    input.set_accept(accept);
    let picked = input.clone();
    let on_change = wasm_bindgen::closure::Closure::once_into_js(move || {
        let Some(files) = picked.files() else {
            return;
        };
        for i in 0..files.length() {
            let Some(file) = files.get(i) else { continue };
            let into = Rc::clone(&into);
            wasm_bindgen_futures::spawn_local(async move {
                let name = file.name();
                if let Ok(buffer) = wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await
                {
                    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                    into.borrow_mut().push((name, bytes));
                }
            });
        }
    });
    input.set_onchange(Some(on_change.unchecked_ref()));
    input.click();
}

/// Hands `bytes` to the browser as a download named `name`.
pub fn download(name: &str, bytes: &[u8]) -> Result<(), String> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("application/octet-stream");
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options)
        .map_err(|e| format!("{e:?}"))?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(|e| format!("{e:?}"))?;
    let anchor = document
        .create_element("a")
        .map_err(|e| format!("{e:?}"))?
        .unchecked_into::<web_sys::HtmlAnchorElement>();
    anchor.set_href(&url);
    anchor.set_download(name);
    anchor.click();
    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(())
}

/// Whether the open document has unsaved changes, for the page's
/// `beforeunload` guard.
static UNSAVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tells the page's `beforeunload` guard whether there are unsaved changes.
pub fn set_unsaved(unsaved: bool) {
    UNSAVED.store(unsaved, std::sync::atomic::Ordering::Relaxed);
}

/// Makes the browser ask before the page is closed or reloaded with unsaved
/// changes (docs/07 §13); the autosave keeps them either way.
pub fn guard_unload() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let guard = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::BeforeUnloadEvent)>::new(
        |event: web_sys::BeforeUnloadEvent| {
            if UNSAVED.load(std::sync::atomic::Ordering::Relaxed) {
                event.prevent_default();
                event.set_return_value("unsaved changes");
            }
        },
    );
    let _ = window.add_event_listener_with_callback("beforeunload", guard.as_ref().unchecked_ref());
    // The page keeps the listener for its whole life.
    guard.forget();
}

/// Asks the browser to keep this site's storage (docs/11 §4.3, on the first
/// save).
pub fn request_persistence() {
    if let Some(window) = web_sys::window() {
        let storage = window.navigator().storage();
        if let Ok(promise) = storage.persist() {
            wasm_bindgen_futures::spawn_local(async move {
                let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
            });
        }
    }
}
