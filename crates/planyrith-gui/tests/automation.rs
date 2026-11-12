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

/// docs/05 §8.18–§8.19: the GUI and CLI share geometry and persisted metadata.
#[test]
fn molds_and_routing_from_cli_and_gui_have_identical_canonical_state() {
    let dir = scratch("molds-routing");
    let model = dir.join("parts.plyr");
    let _ = std::fs::remove_file(&model);
    let mut gui = Session::new();
    for (name, params) in [
        (
            "rectangle",
            json!({"corner_a": [0,0,0], "corner_b": [100,60,0]}),
        ),
        (
            "push-pull",
            json!({"face": {"Query": "face & last"}, "distance": 40}),
        ),
        (
            "make-group",
            json!({"targets": "face|edge", "name": "Part"}),
        ),
        (
            "mold-split",
            json!({"body": "name:Part", "point": [50,30,20], "normal": [0,0,1], "shrink_factor": 1.02}),
        ),
        (
            "route",
            json!({"points": [[200,0,0],[300,0,0],[300,100,0]], "segments": 8, "bend_segments": 2}),
        ),
        (
            "route-fitting",
            json!({"kind": "reducer", "size": "32x2", "outlet_diameter": 20, "position": [400,0,0]}),
        ),
    ] {
        gui.execute(name, params.clone()).unwrap();
        let output = Command::new(cli())
            .arg("--deterministic")
            .arg("-f")
            .arg(&model)
            .args(["tool", name, "--params-json"])
            .arg(params.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let loaded = planyrith_core::io::native::read_document(
        &std::fs::read(&model).unwrap(),
        &planyrith_core::io::NoProgress,
    )
    .unwrap();
    let mut cli = document_to_json(&loaded.document).unwrap();
    let mut actual = document_to_json(gui.doc()).unwrap();
    canonicalize(&mut cli);
    canonicalize(&mut actual);
    assert_eq!(actual, cli);
}

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

/// docs/08 §3.1: two desktop sessions join one document by address and
/// name; one draws and selects, the other sees the change and the
/// selection, then leaves with a local copy.
#[test]
fn two_gui_sessions_work_together() {
    let mut server = Server(
        Command::new(cli())
            .args(["serve", "--socket", "127.0.0.1:0", "--token", "t0ken"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stderr = BufReader::new(server.0.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    let rest = line.trim().strip_prefix("listening on ").unwrap();
    assert!(rest.ends_with("(encrypted)"), "{rest}");
    let address = rest.split_whitespace().next().unwrap().to_string();
    // A wrong token fails at the encrypted handshake.
    assert!(Session::join(&address, "wrong", "", "Eve").is_err());
    let mut anna = Session::join(&address, "t0ken", "", "Anna").unwrap();
    let doc_id = "d1";
    let mut bruno = Session::join(&address, "t0ken", doc_id, "Bruno").unwrap();
    assert_eq!(bruno.participants.len(), 2);
    anna.execute(
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [800, 400, 0] }),
    )
    .unwrap();
    anna.execute("select", json!({ "query": "face" })).unwrap();
    bruno.poll_remote(1000.0).unwrap();
    assert_eq!(
        bruno.doc().model.mesh().faces().count(),
        1,
        "Anna's face arrived"
    );
    assert!(
        bruno.engine.selection().is_empty(),
        "Bruno's selection is his own"
    );
    let anna_row = bruno
        .participants
        .iter()
        .find(|p| p.name == "Anna")
        .expect("Anna is listed");
    assert!(!anna_row.you);
    assert_eq!(anna_row.selection.len(), 1);
    assert_eq!(
        anna_row.selection[0].bounds,
        Some([[0.0, 0.0, 0.0], [800.0, 400.0, 0.0]])
    );

    // Undo is one's own (1.7): Bruno draws after Anna, Anna undoes, and
    // her rectangle goes while his stays.
    bruno
        .execute(
            "rectangle",
            json!({ "corner_a": [2000, 0, 0], "corner_b": [2400, 400, 0] }),
        )
        .unwrap();
    anna.poll_remote(2000.0).unwrap();
    assert_eq!(anna.undo_label(), Some("Rectangle"));
    anna.undo().unwrap();
    assert_eq!(anna.undo_label(), None, "nothing more of Anna's");
    assert_eq!(anna.redo_label(), Some("Rectangle"));
    bruno.poll_remote(3000.0).unwrap();
    let faces: Vec<_> = bruno.doc().model.mesh().faces().map(|(f, _)| f).collect();
    assert_eq!(faces.len(), 1);
    let bounds = bruno.doc().model.mesh().face_aabb(faces[0]).unwrap();
    assert!(bounds.min.x >= 2000.0, "Bruno's rectangle stayed");
    assert_eq!(bruno.undo_label(), Some("Rectangle"));
    anna.redo().unwrap();
    assert_eq!(anna.doc().model.mesh().faces().count(), 2);

    let local = bruno.leave();
    assert!(!local.is_attached());
    assert_eq!(local.doc().model.mesh().faces().count(), 1);
}

/// A browser's WebSocket, played by a native one.
struct NativeSocket(
    tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    Option<String>,
);

impl planyrith_gui::link::Socket for NativeSocket {
    fn send(&mut self, bytes: Vec<u8>) {
        if let Err(e) = self.0.send(tungstenite::Message::binary(bytes)) {
            self.1 = Some(e.to_string());
        }
    }

    fn receive(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            match self.0.read() {
                Ok(tungstenite::Message::Binary(b)) => out.push(b.to_vec()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return out;
                }
                Err(e) => {
                    self.1 = Some(e.to_string());
                    return out;
                }
            }
        }
    }

    fn open(&self) -> Result<bool, String> {
        match &self.1 {
            Some(e) => Err(e.clone()),
            None => Ok(true),
        }
    }
}

/// Polls a session joined from the browser until `done` holds (five
/// seconds at most); `clock` is its time in seconds.
fn wait_for(carla: &mut Session, clock: &mut f64, done: impl Fn(&Session) -> bool) {
    for _ in 0..500 {
        *clock += 0.1;
        carla.poll_joined(*clock).unwrap();
        if done(carla) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the browser session did not catch up");
}

/// docs/08 §3.3: a browser joins the document a desktop session works on,
/// over `serve --web`; changes and undo go both ways, without the browser
/// ever waiting for an answer.
#[test]
fn a_browser_session_works_with_a_desktop_one() {
    let mut server = Server(
        Command::new(cli())
            .args([
                "serve",
                "--socket",
                "127.0.0.1:0",
                "--web",
                "127.0.0.1:0",
                "--token",
                "t0ken",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stderr = BufReader::new(server.0.stderr.take().unwrap());
    let mut socket_line = String::new();
    stderr.read_line(&mut socket_line).unwrap();
    let mut web_line = String::new();
    stderr.read_line(&mut web_line).unwrap();
    let socket = socket_line
        .trim()
        .strip_prefix("listening on ")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let web = web_line
        .trim()
        .strip_prefix("browsers join at http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .to_string();

    let mut anna = Session::join(&socket, "t0ken", "", "Anna").unwrap();
    anna.execute(
        "rectangle",
        json!({ "corner_a": [0, 0, 0], "corner_b": [800, 400, 0] }),
    )
    .unwrap();

    let connect = || {
        let (ws, _) = tungstenite::connect(format!("ws://{web}/rpc")).unwrap();
        if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
            s.set_read_timeout(Some(std::time::Duration::from_millis(5)))
                .unwrap();
        }
        Box::new(NativeSocket(ws, None))
    };
    // The wrong token is refused.
    let mut eve = Session::join_over(connect(), "wrong", "d1", "Eve");
    let mut refused = false;
    for i in 0..300 {
        if eve.poll_joined(f64::from(i)).is_err() {
            refused = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(refused, "a wrong token must not join");

    let mut carla = Session::join_over(connect(), "t0ken", "d1", "Carla");
    assert!(carla.is_attached());
    let mut clock = 0.0;
    wait_for(&mut carla, &mut clock, |c| {
        !c.is_joining() && c.doc().model.mesh().faces().count() == 1
    });
    wait_for(&mut carla, &mut clock, |c| c.participants.len() == 2);

    // Carla draws; Anna sees it.
    carla
        .execute(
            "rectangle",
            json!({ "corner_a": [2000, 0, 0], "corner_b": [2400, 400, 0] }),
        )
        .unwrap();
    wait_for(&mut carla, &mut clock, |c| {
        c.doc().model.mesh().faces().count() == 2
    });
    assert_eq!(carla.undo_label(), Some("Rectangle"));
    anna.poll_remote(100.0).unwrap();
    assert_eq!(anna.doc().model.mesh().faces().count(), 2);

    // Carla's undo takes back her rectangle, not Anna's.
    carla.undo().unwrap();
    wait_for(&mut carla, &mut clock, |c| {
        c.doc().model.mesh().faces().count() == 1 && c.redo_label() == Some("Rectangle")
    });
    let face = carla.doc().model.mesh().faces().next().unwrap().0;
    let bounds = carla.doc().model.mesh().face_aabb(face).unwrap();
    assert!(bounds.max.x <= 800.0, "Anna's rectangle stayed");

    // A failed command comes back as an error.
    carla
        .execute(
            "push-pull",
            json!({ "face": { "Pid": 99999 }, "distance": 10 }),
        )
        .unwrap();
    let mut failed = false;
    for _ in 0..300 {
        clock += 0.1;
        if carla.poll_joined(clock).is_err() {
            failed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(failed, "the server's refusal reaches the browser");

    let local = carla.leave();
    assert!(!local.is_attached());
    assert_eq!(local.doc().model.mesh().faces().count(), 1);
}
