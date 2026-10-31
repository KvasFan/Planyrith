//! Acceptance tests for docs/13 0.8: frontend parity and native server attach.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use planyrith_core::io::export::json::{canonicalize, document_to_json};
use planyrith_gui::session::Session;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

fn cli() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(if cfg!(windows) {
            "planyrith.exe"
        } else {
            "planyrith"
        })
}
fn scratch(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("planyrith-08-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}
#[test]
fn stairs_from_cli_and_gui_have_identical_canonical_exports() {
    let dir = scratch("stairs");
    let path = dir.join("cli.plyr");
    let export = dir.join("cli.json");
    let output = Command::new(cli())
        .args([
            "--deterministic",
            "tool",
            "ext:org.planyrith.stairs/stairs",
            "--steps",
            "7",
            "--rise",
            "1400",
            "--run",
            "2100",
            "--width",
            "900",
            "-f",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = Command::new(cli())
        .arg("-f")
        .arg(&path)
        .arg("export")
        .arg(&export)
        .args(["--canonical", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut gui = Session::new();
    gui.execute(
        "ext:org.planyrith.stairs/stairs",
        json!({"steps":7,"rise":1400,"run":2100,"width":900}),
    )
    .unwrap();
    let mut expected =
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(export).unwrap()).unwrap();
    let mut actual = document_to_json(gui.doc()).unwrap();
    canonicalize(&mut expected);
    canonicalize(&mut actual);
    assert_eq!(actual, expected);
    gui.undo().unwrap();
    assert!(gui.doc().model.is_empty());
    gui.redo().unwrap();
    assert!(!gui.doc().model.is_empty());
}
struct Server(Child);

#[test]
fn annotation_style_from_cli_and_gui_has_identical_canonical_state() {
    let dir = scratch("labels");
    let model = dir.join("labels.plyr");
    let params = json!({"targets":"text | dimension","font":{"family":"Inter","size_pt":18,"bold":true},"color":"#318ac0","arrow":"open","leader_visible":false});
    let mut gui = Session::new();
    for (name, p) in [
        (
            "rectangle",
            json!({"corner_a":[0,0,0],"corner_b":[2000,1000,0]}),
        ),
        ("text", json!({"text":"Width","at":[1000,0,0]})),
        (
            "dimension",
            json!({"start":[0,0,0],"end":[2000,0,0],"offset":[0,-200,0]}),
        ),
        ("label-style", params),
    ] {
        gui.execute(name, p.clone()).unwrap();
        let result = Command::new(cli())
            .arg("--deterministic")
            .arg("-f")
            .arg(&model)
            .args(["tool", name, "--params-json"])
            .arg(p.to_string())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let loaded = planyrith_core::io::native::read_document(
        &std::fs::read(&model).unwrap(),
        &planyrith_core::io::NoProgress,
    )
    .unwrap();
    let mut expected = document_to_json(&loaded.document).unwrap();
    let mut actual = document_to_json(gui.doc()).unwrap();
    canonicalize(&mut expected);
    canonicalize(&mut actual);
    assert_eq!(actual, expected);
    gui.undo().unwrap();
    let before = gui.doc().model.texts().next().unwrap().1;
    assert!(before.layout.is_none());
    gui.redo().unwrap();
    assert!(
        gui.doc()
            .model
            .texts()
            .next()
            .unwrap()
            .1
            .layout
            .as_ref()
            .unwrap()
            .font
            .as_ref()
            .unwrap()
            .bold
    );
    let _ = std::fs::remove_dir_all(dir);
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn attached_gui_dispatches_history_scripts_and_save_to_server() {
    let mut server = Server(
        Command::new(cli())
            .args(["serve", "--socket", "0", "--token", "test-token"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stderr = BufReader::new(server.0.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    let address = line.trim().strip_prefix("listening on ").unwrap();
    let port = address.rsplit(':').next().unwrap();
    assert!(Session::attach(port, "wrong", "").is_err());
    let mut gui = Session::attach(port, "test-token", "").unwrap();
    assert!(gui.is_attached());
    gui.execute("script-eval",json!({"source":"rectangle(#{corner_a:pt(0,0,0),corner_b:pt(100,100,0)}); push_pull(#{face:sel(\"face & last\"),distance:50});"})).unwrap();
    assert!(gui.doc().model.mesh().is_solid());
    assert!(gui.undo_label().is_some());
    gui.undo().unwrap();
    assert!(gui.doc().model.is_empty());
    assert!(gui.redo_label().is_some());
    gui.redo().unwrap();
    assert!(gui.doc().model.mesh().is_solid());
    let file = scratch("remote").join("saved.plyr");
    gui.save(Some(&file)).unwrap();
    assert!(file.exists());
    assert!(!gui.dirty());
    let loaded = Session::open(&file).unwrap();
    assert!(loaded.doc().model.mesh().is_solid());
}
