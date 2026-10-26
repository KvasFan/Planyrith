//! Implements docs/11-persistence-and-web.md §7 and the 0.7 acceptance of
//! docs/13-roadmap.md in a headless browser (`wasm-bindgen-test`): the
//! storage conformance suite on the browser's storage, and the classroom
//! scenario — open an example, model, save to the browser, reload, recover.
//!
//! Run with `cargo test -p planyrith-gui --target wasm32-unknown-unknown
//! --test web` and `wasm-bindgen-test-runner` as the target's runner.

#![cfg(target_arch = "wasm32")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;

use planyrith_core::commands::CommandRegistry;
use planyrith_core::document::Document;
use planyrith_core::io::NoProgress;
use planyrith_core::io::autosave;
use planyrith_core::io::native::{WriteOptions, read_document, write_document};
use planyrith_core::io::storage::{Storage, StorageKey, conformance_check};
use planyrith_gui::platform::web::{self, BrowserStorage};
use planyrith_gui::samples;
use planyrith_gui::session::Session;
use serde_json::json;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// The document's bytes without clock or journal, for comparisons.
fn canonical(doc: &Document) -> Vec<u8> {
    let opts = WriteOptions {
        deterministic: true,
        ..WriteOptions::default()
    };
    write_document(doc, &[], &opts, &NoProgress).unwrap()
}

#[wasm_bindgen_test]
async fn browser_storage_conforms_and_persists() {
    conformance_check(&BrowserStorage::scratch()).unwrap();
    let persisted = BrowserStorage::load().await.unwrap();
    conformance_check(&persisted).unwrap();
    persisted.write(&"/kept/a.plyr".into(), b"kept").unwrap();
    web::flush().await;
    assert!(web::take_failure().is_none());
    let reloaded = BrowserStorage::load().await.unwrap();
    assert!(!reloaded.exists(&"conformance/probe.bin".into()).unwrap());
    assert_eq!(reloaded.read(&"/kept/a.plyr".into()).unwrap(), b"kept");
    assert!(reloaded.is_folder("/kept") && reloaded.is_folder("/"));
    assert!(reloaded.children("/").contains(&"/kept".to_string()));
    assert_eq!(reloaded.children("/kept"), vec!["/kept/a.plyr"]);
}

#[wasm_bindgen_test]
async fn classroom_scenario() {
    web::install().await.unwrap();

    // Open an example.
    let sample = samples::catalogue().into_iter().next().unwrap();
    let mut s = Session::new();
    for (name, params) in (sample.commands)() {
        s.execute(name, params).unwrap();
    }
    s.mark_saved();
    s.enable_autosave();

    // Model, save to the browser, model more.
    s.execute(
        "rectangle",
        json!({ "corner_a": [7000.0, 0.0, 0.0], "corner_b": [8000.0, 1000.0, 0.0] }),
    )
    .unwrap();
    let path = Path::new("/classroom.plyr");
    s.save(Some(path)).unwrap();
    let saved = canonical(s.doc());
    s.execute(
        "push-pull",
        json!({ "face": { "Query": "face & last" }, "distance": 500.0 }),
    )
    .unwrap();
    s.tick(0.0);
    s.undo().unwrap();
    s.redo().unwrap();
    s.tick(1.0); // undo and redo take a snapshot
    s.execute(
        "line",
        json!({ "points": [[7000.0, 0.0, 500.0], [8000.0, 1000.0, 500.0]] }),
    )
    .unwrap();
    // Selecting is not journaled; deleting the selection is.
    s.execute("select", json!({ "query": "face & normal:z" }))
        .unwrap();
    s.execute("delete", json!({ "targets": "selection" }))
        .unwrap();
    let expected = canonical(s.doc());
    assert_ne!(expected, saved);
    web::flush().await;
    assert!(web::take_failure().is_none());

    // Reload: a new page reads the browser's files back.
    let reloaded: Arc<dyn Storage + Send + Sync> = Arc::new(BrowserStorage::load().await.unwrap());
    let key = StorageKey::new("/classroom.plyr");
    let on_disk = read_document(&reloaded.read(&key).unwrap(), &NoProgress).unwrap();
    assert_eq!(canonical(&on_disk.document), saved, "the save is untouched");
    assert!(autosave::pending(reloaded.as_ref(), &key));

    // Recover.
    let back = autosave::recover(
        Arc::clone(&reloaded),
        &key,
        Arc::new(CommandRegistry::with_builtins()),
    )
    .unwrap();
    assert!(back.skipped.is_empty(), "{:?}", back.skipped);
    assert_eq!(back.replayed, 2, "line and delete");
    assert_eq!(canonical(back.engine.document()), expected);

    // Saving again leaves nothing to recover.
    s.save(None).unwrap();
    web::flush().await;
    let after = BrowserStorage::load().await.unwrap();
    assert!(!autosave::pending(&after, &key));
}
