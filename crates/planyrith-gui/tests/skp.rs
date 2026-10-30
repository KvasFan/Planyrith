//! Desktop Open/Save As of adapter-backed models (docs/09 §5).
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use planyrith_gui::session::Session;
use serde_json::json;
#[test]
fn gui_session_opens_and_saves_skp_through_the_same_converter() {
    let exe = std::env::current_exe().unwrap();
    let adapter = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("planyrith-skp{}", std::env::consts::EXE_SUFFIX));
    if !adapter.is_file() {
        assert!(
            std::env::var_os("PLANYRITH_REQUIRE_SKP").is_none(),
            "build planyrith-skp first"
        );
        return;
    }
    let dir = std::env::temp_dir().join(format!("planyrith-gui-skp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("cube.skp");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../planyrith-skp/tests/fixtures/box-2026.skp"),
        &source,
    )
    .unwrap();
    let mut session = Session::open(&source).unwrap();
    assert_eq!(session.doc().model.mesh().face_count(), 6);
    let original = std::fs::read(&source).unwrap();
    let untouched = dir.join("untouched.skp");
    session.save(Some(&untouched)).unwrap();
    assert_eq!(std::fs::read(&untouched).unwrap(), original);
    let working = dir.join("working.plyr");
    session.save(Some(&working)).unwrap();
    assert!(std::fs::read(&working).unwrap().starts_with(b"PK"));
    session = Session::open(&working).unwrap();
    let restored = dir.join("restored.skp");
    session.save(Some(&restored)).unwrap();
    assert_eq!(std::fs::read(&restored).unwrap(), original);
    session
        .execute("move", json!({"targets":"all","delta":[100,0,0]}))
        .unwrap();
    let output = dir.join("saved.skp");
    session.save(Some(&output)).unwrap();
    let opened = Session::open(&output).unwrap();
    assert!((opened.doc().aabb().max.x - 1100.0).abs() < 1e-6);
    assert!(
        std::fs::read(&output)
            .unwrap()
            .starts_with(b"\xff\xfe\xff\x0e")
    );
    let _ = std::fs::remove_dir_all(dir);
}
