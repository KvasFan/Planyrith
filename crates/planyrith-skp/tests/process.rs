//! Exercises the actual optional process through core commands and Storage.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use planyrith_core::{
    commands::{CommandRegistry, Engine},
    document::Document,
    io::{
        NoProgress, Storage, StorageKey, model_file,
        native::{self, WriteOptions},
        storage::NativeFs,
    },
};
use serde_json::json;
use std::sync::Arc;

fn native_bytes(doc: &Document) -> Vec<u8> {
    let mut copy = doc.clone();
    copy.info.next_pid = planyrith_core::ids::PidAllocator::starting_at(1_000_000);
    native::write_document(
        &copy,
        &[],
        &WriteOptions {
            deterministic: true,
            ..Default::default()
        },
        &NoProgress,
    )
    .unwrap()
}
#[test]
fn converter_process_drives_import_export_open_save_and_exact_undo() {
    let dir = std::env::temp_dir().join(format!("planyrith-skp-process-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let storage = Arc::new(NativeFs {
        skp_adapter: Some(env!("CARGO_BIN_EXE_planyrith-skp").into()),
        ..NativeFs::rooted(&dir)
    });
    storage
        .write(
            &StorageKey::from("cube.skp"),
            include_bytes!("fixtures/box-2017.skp"),
        )
        .unwrap();
    storage
        .write(&StorageKey::from("broken.skp"), b"corrupt")
        .unwrap();
    let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    e.set_storage(storage.clone());
    let before = native_bytes(e.document());
    e.execute_erased("import", json!({"path":"cube.skp"}))
        .unwrap();
    let after = native_bytes(e.document());
    assert_eq!(e.document().model.groups().count(), 1);
    assert!((e.document().aabb().max.x - 1000.0).abs() < 1e-6);
    e.undo().unwrap();
    assert_eq!(native_bytes(e.document()), before);
    e.redo().unwrap();
    assert_eq!(native_bytes(e.document()), after);
    let before = native_bytes(e.document());
    assert!(
        e.execute_erased("import", json!({"path":"broken.skp"}))
            .is_err()
    );
    assert_eq!(native_bytes(e.document()), before);
    for version in [2017, 2026] {
        let output = e
            .execute_erased("export", json!({"path":"output.skp","skp_version":version}))
            .unwrap();
        assert_eq!(output["format"], "skp");
        let bytes = storage.read(&StorageKey::from("output.skp")).unwrap();
        assert!(bytes.starts_with(b"\xff\xfe\xff\x0e"));
        let opened = model_file::read(&bytes, "output.skp", storage.as_ref()).unwrap();
        assert!((opened.document.aabb().max.x - 1000.0).abs() < 1e-6);
    }
    let native = native_bytes(e.document());
    let save = model_file::for_save(native, "save.SKP", storage.as_ref()).unwrap();
    let opened = model_file::read(&save.bytes, "save.SKP", storage.as_ref()).unwrap();
    assert!((opened.document.aabb().max.x - 1000.0).abs() < 1e-6);
    let _ = std::fs::remove_dir_all(dir);
}
