//! Desktop Open/Save As of adapter-backed models (docs/09 §5).
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use planyrith_gui::session::Session;
use serde_json::json;

#[test]
fn historical_backups_default_to_native_and_explicit_foreign_saves_keep_the_choice() {
    let adapter = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("planyrith-skp{}", std::env::consts::EXE_SUFFIX));
    if !adapter.is_file() {
        assert!(std::env::var_os("PLANYRITH_REQUIRE_SKP").is_none());
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "planyrith-gui-historical-skb-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for year in [2013, 2016, 2026] {
        let fixture = if year < 2017 {
            format!("legacy/box-{year}.skp")
        } else {
            format!("box-{year}.skp")
        };
        let original = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../planyrith-skp/tests/fixtures")
                .join(fixture),
        )
        .unwrap();
        let source = dir.join(format!("source-{year}.SKB"));
        std::fs::write(&source, &original).unwrap();
        let mut session = Session::open(&source).unwrap();
        assert!(session.needs_native_copy());
        let native = session.save(None).unwrap();
        assert_eq!(native, source.with_extension("plyr"));
        assert!(!session.needs_native_copy());
        assert!(std::fs::read(&native).unwrap().starts_with(b"PK"));
        assert_eq!(std::fs::read(&source).unwrap(), original);
        let mut session = Session::open(&native).unwrap();
        assert_eq!(session.doc().model.mesh().face_count(), 6);
        let backup = dir.join(format!("restored-{year}.skb"));
        session.save(Some(&backup)).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), original);
        session
            .execute("move", json!({"targets":"all","delta":[100,0,0]}))
            .unwrap();
        assert_eq!(session.save(None).unwrap(), backup);
        let opened = Session::open(&backup).unwrap();
        assert!((opened.doc().aabb().min.x - 100.0).abs() < 1e-6);
        let saved_native = std::fs::read(&native).unwrap();
        let mut collision = Session::open(&source).unwrap();
        assert!(collision.save(None).unwrap_err().contains("already exists"));
        assert_eq!(std::fs::read(&native).unwrap(), saved_native);
        assert_eq!(std::fs::read(&source).unwrap(), original);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn external_skp_opens_and_survives_native_working_copy_when_requested() {
    let Some(path) = std::env::var_os("PLANYRITH_SKP_TEST_FILE") else {
        return;
    };
    // Opt-in large/private compatibility probe; no model content is committed.
    let path = std::path::PathBuf::from(path);
    let mut session = Session::open(&path).unwrap();
    use planyrith_core::io::export::json::{canonicalize, document_to_json};
    let mut canonical = document_to_json(session.doc()).unwrap();
    canonicalize(&mut canonical);
    let dir =
        std::env::temp_dir().join(format!("planyrith-gui-external-skp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let working = dir.join("working.plyr");
    session.save(Some(&working)).unwrap();
    let mut reopened = Session::open(&working).unwrap();
    let mut actual = document_to_json(reopened.doc()).unwrap();
    canonicalize(&mut actual);
    assert_eq!(actual, canonical);
    let restored = dir.join("restored.skp");
    reopened.save(Some(&restored)).unwrap();
    assert_eq!(
        std::fs::read(&restored).unwrap(),
        std::fs::read(&path).unwrap()
    );
    std::fs::remove_dir_all(dir).unwrap();
}
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

#[test]
fn gui_annotation_edits_survive_skp_save_and_native_working_copies() {
    let exe = std::env::current_exe().unwrap();
    let adapter = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("planyrith-skp{}", std::env::consts::EXE_SUFFIX));
    if !adapter.is_file() {
        assert!(std::env::var_os("PLANYRITH_REQUIRE_SKP").is_none());
        return;
    }
    let dir = std::env::temp_dir().join(format!("planyrith-gui-skp-labels-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut session = Session::new();
    for (name, params) in [
        (
            "rectangle",
            json!({"corner_a":[0,0,0],"corner_b":[1000,1000,0]}),
        ),
        ("text", json!({"text":"Width","at":[500,0,0]})),
        (
            "dimension",
            json!({"start":[0,0,0],"end":[1000,0,0],"offset":[0,-200,0]}),
        ),
        (
            "label-style",
            json!({"targets":"text | dimension","font":{"family":"Inter","size_pt":18},"color":"#318ac0","arrow":"closed"}),
        ),
    ] {
        session.execute(name, params).unwrap();
    }
    let initial = dir.join("initial.skp");
    session.save(Some(&initial)).unwrap();
    let mut session = Session::open(&initial).unwrap();
    session
        .execute(
            "label-style",
            json!({"targets":"text | dimension","color":"#cc4400","arrow":"open"}),
        )
        .unwrap();
    let working = dir.join("working.plyr");
    session.save(Some(&working)).unwrap();
    let mut session = Session::open(&working).unwrap();
    let edited = dir.join("edited.skp");
    session.save(Some(&edited)).unwrap();
    assert_ne!(
        std::fs::read(&initial).unwrap(),
        std::fs::read(&edited).unwrap()
    );
    let opened = Session::open(&edited).unwrap();
    assert_eq!(opened.doc().model.texts().count(), 1);
    assert_eq!(opened.doc().model.dimensions().count(), 1);
    let layout = opened
        .doc()
        .model
        .texts()
        .next()
        .unwrap()
        .1
        .layout
        .as_ref()
        .unwrap();
    assert_eq!(layout.font.as_ref().unwrap().size_pt, 18.0);
    assert_eq!(
        layout.color,
        Some(planyrith_core::document::Rgba8::rgb(0xcc, 0x44, 0))
    );
    assert_eq!(layout.arrow, planyrith_core::document::ArrowStyle::Open);
    let _ = std::fs::remove_dir_all(dir);
}
