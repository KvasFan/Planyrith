//! Reproduces the native golden counterpart for docs/09 §5 SKP round trips.
//! Run: cargo run -p planyrith-core --example generate-skp-fixture
use planyrith_core::{
    commands::{CommandRegistry, Engine},
    document::Document,
    io::{
        NoProgress, Storage, StorageKey,
        native::{WriteOptions, write_document},
        storage::NativeFs,
    },
};
use serde_json::json;
use std::sync::Arc;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
    e.execute_erased(
        "rectangle",
        json!({"corner_a":[0,0,0],"corner_b":[1000,1000,0]}),
    )?;
    e.execute_erased(
        "push-pull",
        json!({"face":{"Query":"face"},"distance":1000}),
    )?;
    let bytes = write_document(
        e.document(),
        &[],
        &WriteOptions {
            deterministic: true,
            include_history: false,
            ..Default::default()
        },
        &NoProgress,
    )?;
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skp/cube.plyr");
    NativeFs::new().write(&StorageKey::new(root.to_string_lossy()), &bytes)?;
    Ok(())
}
