# 11 — Persistence, autosave and the web build

**Audience:** implementing agents. Normative. Lives in `planyrith-core::io::storage`,
`planyrith-gui::web`, and build tooling.

## 1. Storage abstraction

```rust
#[async_trait(?Send)]  // wasm has no Send; desktop impl is also fine single-threaded
pub trait Storage {
    async fn read(&self, key: &StorageKey) -> Result<Bytes, IoError>;
    async fn write(&self, key: &StorageKey, bytes: Bytes) -> Result<(), IoError>;   // atomic replace
    async fn delete(&self, key: &StorageKey) -> Result<(), IoError>;
    async fn list(&self, prefix: &str) -> Result<Vec<StorageEntry /*key, size, modified*/>, IoError>;
    async fn exists(&self, key: &StorageKey) -> Result<bool, IoError>;
    fn capabilities(&self) -> StorageCaps; // random_access, max_size, persistent, shareable_path
}
```
Implementations:
- `NativeFs` (desktop): keys are paths; atomic write = write to `.<name>.tmp` + fsync + rename;
  backups `<name>.plyr~` kept per preference.
- `MemoryStorage`: tests and clipboard.
- `IndexedDbStorage` (web, via `rexie`): object store `files` keyed by name with `{bytes, size,
  modified, thumbnail}`; large documents are chunked in 8 MB records (`files_chunks`) to stay
  under per-record limits in some browsers; a `meta` store holds recents and settings.
- `OpfsStorage` (web, Origin Private File System) — optional, used when available for faster
  streaming; falls back to IndexedDB. Post-1.0 if time is short.

The core never calls `std::fs` directly outside `NativeFs`. Open/save commands take
`&dyn Storage`, so the same command code serves desktop and web.

## 2. Autosave and recovery

- The engine appends every committed transaction to an **autosave journal**
  (`<doc>.plyr.journal` / IndexedDB `journals`): `{ seq, command, params, group }` plus a periodic
  snapshot every N = 200 commands or 2 minutes, whichever first (snapshot = full `.plyr` bytes
  to `<doc>.autosave.plyr`).
- On startup, if a journal newer than its document exists, the GUI offers **Recover** (replay
  snapshot + journal) or **Discard**. The CLI exposes `planyrith recover <doc>`.
- Journals are deleted on successful explicit save. Autosave never overwrites the user's file.
- Web: `beforeunload` flushes the journal synchronously (IndexedDB write is async; a small
  `localStorage` "dirty" flag marks the need to check on reload).

## 3. Recent files, settings, layouts

- Desktop: `<user config>/planyrith/settings.toml`, `layouts/*.json`, `recent.json`
  (via the `directories` crate, MIT/Apache-2.0).
- Web: IndexedDB `meta` store; export/import settings as JSON from Preferences.

## 4. WebAssembly build

### 4.1 Toolchain
- Target `wasm32-unknown-unknown`; `wasm-bindgen`; `trunk` (MIT/Apache-2.0) builds `index.html`
  with the eframe web runner; `wasm-opt` (Binaryen — a C++ *build tool*, not a dependency of the
  shipped binary; acceptable) for size.
- Threads: single-threaded in 1.0 (no `SharedArrayBuffer` requirement, simpler hosting). The
  `parallel` feature is off; long operations yield to the browser via `wasm-bindgen-futures`
  cooperative chunks (`Progress::yield_now().await` every ~8 ms of work).
- Size budget: ≤ 12 MB compressed (brotli) including fonts; `brep` feature optional per build.

### 4.2 Graphics
- WebGPU preferred; WebGL2 fallback automatically by `wgpu`. Feature differences handled in the
  renderer (`docs/06 §1`). Canvas is the whole page; HiDPI via `devicePixelRatio`.

### 4.3 Files in the browser
- Open: File System Access API (`showOpenFilePicker`) when available (Chromium), else
  `<input type=file>`. Save: `showSaveFilePicker` or download blob. "Save to browser" writes to
  IndexedDB; the Recent panel lists browser-stored documents with sizes and a storage quota meter
  (`navigator.storage.estimate`). Request persistent storage (`navigator.storage.persist`) on
  first save.
- Drag-and-drop of `.plyr`/interchange files onto the canvas imports/opens them.
- Textures referenced by path in imports are resolved from a multi-file picker or a dropped folder.

### 4.4 Hosting
- Static files only; no backend. Service worker caches the app for offline use (workbox-free,
  hand-written SW in JS, MIT). `Cross-Origin-Opener-Policy`/`Embedder-Policy` headers are *not*
  required (no threads).

### 4.5 Limits and differences (documented in Help › Web)
- Max document size ~ 1 GB (browser memory); textures downscaled to 4096 px.
- No shell access: `planyrith` CLI features are available through the in-app Console (same
  batch syntax) and through `script`.
- Clipboard between tabs uses the browser clipboard with the custom MIME type where permitted,
  else JSON text.

## 5. Large documents

- `.plyr` parts are read lazily: `document.msgpack` first, images on demand (by hash) with an
  LRU in-memory cache and GPU upload on first use.
- Save streams parts directly into the ZIP writer; memory peak ≤ size of `document.msgpack`
  plus one image.
- Component definitions from libraries are loaded on first instantiation and cached by UUID.

## 6. Backups and versioning hooks

- Preference "keep N backups" rotates `name.plyr~1..N`.
- `planyrith export --format json --canonical` plus the history journal make Planyrith documents
  git-friendly; a `git textconv` recipe is documented in the user manual.

## 7. Testing

- `Storage` conformance test suite run against all implementations (web via `wasm-bindgen-test`
  in headless Chrome in CI).
- Recovery test: kill the process mid-journal (desktop) / simulate reload (web) → recover
  produces the same document as the in-memory state before the kill.

## Decisions

- 2026-10-20 — Single-threaded wasm build in 1.0; no SharedArrayBuffer requirement.
- 2026-10-20 — IndexedDB via `rexie` behind `Storage`; OPFS optional.
- 2026-10-20 — Autosave is journal + periodic snapshot, never in-place.
- 2026-10-20 — Skeleton contract: the `Storage` trait is **synchronous** in the first draft
  (`fn read(&self, key) -> Result<Vec<u8>, IoError>` etc., `Bytes` = `Vec<u8>`); desktop
  backends are blocking and the web build wraps IndexedDB behind the same signatures (or an
  `async` adapter) at milestone 0.7. `MemoryStorage` and `conformance_check` ship with the
  skeleton; `NativeFs` is the only module allowed to use `std::fs`.
- 2026-10-26 — **Browser storage as built (0.7).** `BrowserStorage` (`planyrith-gui::platform::web`)
  keeps the synchronous `Storage` trait: every file is held in memory, loaded from IndexedDB
  (database `planyrith`, store `files`, one record `{bytes, modified}` per key) before the
  application starts, and changes are written behind through one ordered queue. Keys are
  absolute paths (`/house.plyr`); folders are key prefixes (an empty folder is a marker key
  ending in `/`). The GUI's file dialogs browse this storage, so Open, Save As, Import, Export
  and the panels' files work as on the desktop; files reach the computer through File ›
  Upload from Computer… (`<input type=file>`, several at once) and dropping files on the page,
  and leave it through File › Download This Document / Download a File… (a blob download). Not
  in 0.7: the File System Access API, the 8 MB record chunking (current browsers store records of
  hundreds of MB), the `meta` store (settings, layouts and recent files use the page's
  `localStorage` through the GUI's persistence), the quota meter, dropped folders and the
  "Export all" zip. `navigator.storage.persist()` is requested on every save.
- 2026-10-26 — **Autosave as built (0.7).** `planyrith-core::io::autosave`: the journal
  (`<doc>.plyr.journal`) is JSON lines `{seq, command, params, group, selection, context}` (the
  selection and open container the command ran with, by persistent ids, restored before it is
  replayed: selecting is not a journaled command, yet `delete` of the selection depends on it)
  rewritten whole after each
  journaled command (at most 200 rows); a snapshot (`<doc>.autosave.plyr`, the full container with
  its history) is taken after 200 commands or 2 minutes with commands journaled, and after every
  undo or redo (the journal replays commands only, so an undo past the snapshot could not be
  replayed). Recovery replays the journal on the snapshot, else on the document. An explicit save,
  and discarding changes in the unsaved-changes prompt, delete both. A torn last journal line is
  dropped. Untitled documents autosave as `untitled.plyr` in the application's data folder on the
  desktop and as `/.untitled.plyr` in the browser. On startup the GUI offers Recover / Discard for
  the untitled document and the recent files, and when a file with unsaved work is opened. The
  per-command journal write makes the `localStorage` dirty flag and a synchronous flush on
  `beforeunload` unnecessary; at most the commands of the last few milliseconds before a tab
  closes can be lost.
- 2026-10-26 — **Web build without trunk.** `tools/build-web.sh` runs `cargo build` for
  `wasm32-unknown-unknown`, the `wasm-bindgen` CLI (`--target web`) and, when installed,
  `wasm-opt`, then copies `crates/planyrith-gui/web/` (page, manifest, icon, service worker stamped
  with the build): the same static output with one tool fewer. The size budget is checked with
  brotli (4.5 MB on 2026-10-26 without `wasm-opt`, budget 12 MB). WebGL2 is enabled next to
  WebGPU; the renderer already keeps to downlevel limits. Reading the id buffer back (hover) does
  not block in the browser: it is started in one frame and read in a later one.
- 2026-10-28 — **Service worker: network first.** The service worker fetches the application's
  files from the server first (revalidating, `cache: "no-cache"`) and keeps a copy; the cache
  answers only when the network fails. The first, cache-first worker kept a browser on the
  previous build until a second reload, which hid fixes (the page stayed black after the WebGL2
  fix). Pages that installed it get the new worker on their next load; a hard reload
  (Ctrl+Shift+R) bypasses any worker. The page and the worker carry the build id, and the page
  logs it with the graphics API in use.
