//! CLI integration tests (AGENTS.md §4 step 4, docs/12): sessions, batches,
//! exit codes and a golden canonical export.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

fn planyrith() -> Command {
    Command::cargo_bin("planyrith").expect("binary builds")
}

/// A fresh scratch directory per test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("planyrith-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn json_of(output: &[u8]) -> Value {
    serde_json::from_slice(output).expect("one JSON document on stdout")
}

const TABLE: &str = "\
# A table: 30 mm top at 720 mm, four 50 mm legs.
rectangle --corner-a 0,0,690 --corner-b 1200,800,690
push-pull --face '{face & last}' --distance 30
rectangle --corner-a 20,20,690 --corner-b 70,70,690 ; push-pull --face '{face & last}' --distance 690
rectangle --corner-a 1130,20,690 --corner-b 1180,70,690 ; push-pull --face '{face & last}' --distance 690
rectangle --corner-a 20,730,690 --corner-b 70,780,690 ; push-pull --face '{face & last}' --distance 690
rectangle --corner-a 1130,730,690 --corner-b 1180,780,690 ; push-pull --face '{face & last}' --distance 690
";

fn build_table(dir: &Path, file: &str) -> PathBuf {
    let script = dir.join("table.plyrb");
    std::fs::write(&script, TABLE).expect("write batch");
    let doc = dir.join(file);
    planyrith()
        .args([
            "batch",
            script.to_str().unwrap(),
            "-f",
            doc.to_str().unwrap(),
            "--deterministic",
        ])
        .assert()
        .success();
    doc
}

#[test]
fn session_verbs_create_draw_pull_and_report() {
    let dir = scratch("session");
    let doc = dir.join("house.plyr");
    let f = doc.to_str().unwrap();
    planyrith()
        .args(["new", "-f", f, "--title", "House"])
        .assert()
        .success();
    planyrith()
        .args(["new", "-f", f])
        .assert()
        .failure()
        .code(1);
    let out = planyrith()
        .args([
            "tool",
            "rectangle",
            "-f",
            f,
            "--corner-a",
            "0,0,0",
            "--corner-b",
            "8m,6m,0",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json_of(&out);
    assert_eq!(v["ok"], Value::Bool(true));
    assert_eq!(v["output"]["edges"].as_array().unwrap().len(), 4);
    planyrith()
        .args([
            "tool",
            "push-pull",
            "-f",
            f,
            "--face",
            "{face & last}",
            "--distance",
            "3m",
        ])
        .assert()
        .success();
    let out = planyrith()
        .args(["info", "-f", f, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json_of(&out);
    assert_eq!(v["output"]["counts"]["model"]["faces"], 6);
    assert_eq!(v["output"]["title"], "House");
    assert_eq!(v["output"]["bounds"]["max"][2], 3000.0);
    planyrith()
        .args(["query", "-f", f, "face & normal:z", "--count"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1"));
    planyrith().args(["validate", "-f", f]).assert().success();
}

#[test]
fn errors_map_to_exit_codes_and_json_errors() {
    let dir = scratch("errors");
    let doc = dir.join("d.plyr");
    let f = doc.to_str().unwrap();
    planyrith()
        .args(["tool", "no-such-tool", "-f", f])
        .assert()
        .code(1);
    planyrith()
        .args(["info", "-f", dir.join("missing.plyr").to_str().unwrap()])
        .assert()
        .code(4);
    let out = planyrith()
        .args([
            "tool",
            "push-pull",
            "-f",
            f,
            "--face",
            "@42",
            "--distance",
            "10",
            "--json",
        ])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let v = json_of(&out);
    assert_eq!(v["ok"], Value::Bool(false));
    assert_eq!(v["error"]["code"], "E1101");
    planyrith()
        .args([
            "tool", "circle", "-f", f, "--center", "0,0", "--radius", "5",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("E1001"));
    planyrith()
        .args(["tool", "rectangle", "-f", f])
        .assert()
        .code(1);
}

#[test]
fn batches_are_deterministic_and_validate_clean() {
    let dir = scratch("batch");
    let a = build_table(&dir, "a.plyr");
    let b = build_table(&dir, "b.plyr");
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    planyrith()
        .args(["validate", "-f", a.to_str().unwrap()])
        .assert()
        .success();
    let out = planyrith()
        .args([
            "query",
            "-f",
            a.to_str().unwrap(),
            "face & normal:-z",
            "--stats",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json_of(&out);
    assert_eq!(v["output"]["count"], 5);
    // Top underside (1200×800 minus four 50×50 legs) plus four leg feet.
    assert_eq!(v["output"]["area"], 1200.0 * 800.0);
}

#[test]
fn table_export_matches_the_golden_file() {
    let dir = scratch("golden");
    let doc = build_table(&dir, "t.plyr");
    let out = dir.join("t.json");
    planyrith()
        .args([
            "export",
            "-f",
            doc.to_str().unwrap(),
            out.to_str().unwrap(),
            "--canonical",
            "--pretty",
        ])
        .assert()
        .success();
    let got = std::fs::read_to_string(&out).unwrap();
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/table.json");
    if std::env::var_os("PLANYRITH_UPDATE_GOLDEN").is_some() || !golden.exists() {
        std::fs::write(&golden, &got).unwrap();
    }
    let want = std::fs::read_to_string(&golden).unwrap();
    assert!(
        got == want,
        "export differs from {} (set PLANYRITH_UPDATE_GOLDEN=1 to accept)",
        golden.display()
    );
}

#[test]
fn tool_help_lists_typed_options() {
    planyrith()
        .args(["tool", "push-pull", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--distance <LENGTH>").and(predicate::str::contains(
                "Length: number with optional unit",
            )),
        );
    planyrith()
        .args(["tool", "rectangle", "--schema"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"Point\""));
}

/// docs/13 milestone 0.1 acceptance: a batch builds a furnished room headless
/// (walls with openings, a table, chairs as components in an array); the file
/// reopens identically and validates clean.
#[test]
fn furnished_room_acceptance() {
    let dir = scratch("room");
    let batch = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/batches/furnished-room.plyrb");
    let room = dir.join("room.plyr");
    let r = room.to_str().unwrap();
    planyrith()
        .args(["batch", batch.to_str().unwrap(), "-f", r, "--deterministic"])
        .assert()
        .success();
    planyrith().args(["validate", "-f", r]).assert().success();

    let info = json_of(
        &planyrith()
            .args(["info", "-f", r, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let counts = &info["output"]["counts"];
    assert_eq!(counts["model"]["instances"], 4);
    assert_eq!(counts["model"]["groups"], 1);
    assert_eq!(counts["definitions"], 2);
    assert_eq!(info["output"]["bounds"]["max"][2], 2700.0);

    let chairs = json_of(
        &planyrith()
            .args([
                "query",
                "-f",
                r,
                "definition:Chair",
                "--fields",
                "pid,is_solid,bbox",
                "--json",
            ])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let chairs = chairs["output"].as_array().unwrap();
    assert_eq!(chairs.len(), 4);
    assert!(chairs.iter().all(|c| c["is_solid"] == Value::Bool(true)));
    let xs: Vec<f64> = chairs
        .iter()
        .map(|c| c["bbox"]["min"][0].as_f64().unwrap())
        .collect();
    assert_eq!(xs, vec![1000.0, 1600.0, 2200.0, 2800.0]);

    let table = json_of(
        &planyrith()
            .args([
                "query",
                "-f",
                r,
                "group",
                "--fields",
                "name,is_solid,volume",
                "--json",
            ])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    assert_eq!(table["output"][0]["name"], "Table");
    let expected = 2200.0 * 800.0 * 30.0 + 4.0 * 60.0 * 60.0 * 690.0;
    assert!((table["output"][0]["volume"].as_f64().unwrap() - expected).abs() < 1e-3);

    // Door and window cut through: the window has four reveal faces, the
    // door two jambs and a head (its threshold is coplanar with the floor
    // and merges into it, docs/03 §4.9 step 3).
    let reveals = json_of(
        &planyrith()
            .args([
                "query",
                "-f",
                r,
                "face & !tag:Walls & area<1m2",
                "--count",
                "--json",
            ])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    assert_eq!(reveals["output"], 3 + 4);

    // Reopen and re-save without changes: identical bytes.
    let again = dir.join("again.plyr");
    planyrith()
        .args([
            "repair",
            "-f",
            r,
            "-o",
            again.to_str().unwrap(),
            "--deterministic",
        ])
        .assert()
        .success();
    assert_eq!(
        std::fs::read(&room).unwrap(),
        std::fs::read(&again).unwrap()
    );
}

/// docs/13 milestone 0.4 acceptance: a cabinet built from components with a
/// textured veneer, saved as a component file and a material library; a
/// kitchen loads the library and places the cabinet twice (one definition,
/// found by its UUID); both validate clean and re-save identically.
#[test]
fn textured_cabinet_acceptance() {
    let dir = scratch("cabinet");
    std::fs::create_dir_all(dir.join("textures")).unwrap();
    // A striped veneer: 16 stripes across 64 × 32 pixels.
    image::RgbImage::from_fn(64, 32, |x, _| {
        if (x / 4) % 2 == 0 {
            image::Rgb([170, 110, 60])
        } else {
            image::Rgb([140, 85, 40])
        }
    })
    .save(dir.join("textures/oak.png"))
    .unwrap();
    let batches = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/batches");
    let run_batch = |name: &str, doc: &str| {
        planyrith()
            .current_dir(&dir)
            .args([
                "batch",
                batches.join(name).to_str().unwrap(),
                "-f",
                doc,
                "--deterministic",
            ])
            .assert()
            .success();
        planyrith()
            .current_dir(&dir)
            .args(["validate", "-f", doc])
            .assert()
            .success();
    };
    let info = |doc: &str| {
        json_of(
            &planyrith()
                .current_dir(&dir)
                .args(["info", "-f", doc, "--json"])
                .assert()
                .success()
                .get_output()
                .stdout,
        )["output"]
            .clone()
    };
    let count = |doc: &str, query: &str| {
        json_of(
            &planyrith()
                .current_dir(&dir)
                .args(["query", "-f", doc, query, "--count", "--json"])
                .assert()
                .success()
                .get_output()
                .stdout,
        )["output"]
            .as_u64()
            .unwrap()
    };

    run_batch("cabinet.plyrb", "cabinet.plyr");
    let cabinet = info("cabinet.plyr");
    // Side, Shelf, Back, Handle, Door, Door #2 and Cabinet.
    assert_eq!(cabinet["counts"]["definitions"], 7);
    assert_eq!(cabinet["counts"]["images"], 1);
    assert_eq!(
        cabinet["bounds"]["max"],
        serde_json::json!([600.0, 566.0, 720.0])
    );
    assert_eq!(
        cabinet["bounds"]["min"],
        serde_json::json!([0.0, -30.0, 0.0])
    );
    assert_eq!(count("cabinet.plyr", "definition:Cabinet"), 1);
    assert!(dir.join("parts/cabinet.plyrc").exists());
    assert!(dir.join("libs/cabinet.plyrm").exists());

    run_batch("kitchen.plyrb", "kitchen.plyr");
    let kitchen = info("kitchen.plyr");
    assert_eq!(
        kitchen["counts"]["definitions"], 7,
        "the second insert reuses the first"
    );
    assert_eq!(kitchen["counts"]["images"], 1, "one veneer image");
    assert_eq!(count("kitchen.plyr", "definition:Cabinet"), 2);
    assert_eq!(kitchen["bounds"]["max"][2], 720.0);

    // The library brought the materials over unchanged.
    let materials = |doc: &str| {
        let out = dir.join(format!("{doc}.json"));
        planyrith()
            .current_dir(&dir)
            .args(["export", "-f", doc, out.to_str().unwrap(), "--canonical"])
            .assert()
            .success();
        let v: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        let mut list: Vec<Value> = v["materials"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| pair[1].clone())
            .filter(|m| m["is_default_for"].is_null())
            .collect();
        list.sort_by_key(|m| m["name"].as_str().unwrap_or_default().to_string());
        list
    };
    assert_eq!(materials("cabinet.plyr"), materials("kitchen.plyr"));

    // Reopened and re-saved without changes: identical bytes.
    for doc in ["cabinet.plyr", "kitchen.plyr"] {
        let again = dir.join(format!("again-{doc}"));
        planyrith()
            .current_dir(&dir)
            .args([
                "repair",
                "-f",
                doc,
                "-o",
                again.to_str().unwrap(),
                "--deterministic",
            ])
            .assert()
            .success();
        assert_eq!(
            std::fs::read(dir.join(doc)).unwrap(),
            std::fs::read(&again).unwrap(),
            "{doc}"
        );
    }
}

/// AGENTS.md §4 step 4: every registered command has a batch under
/// `tests/commands/<name>.plyrb` whose result validates clean and whose
/// canonical export matches `tests/golden/commands/<name>.json`
/// (`PLANYRITH_UPDATE_GOLDEN=1` accepts changes).
#[test]
fn every_command_matches_its_golden_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let listing = json_of(
        &planyrith()
            .args(["tool", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let names: Vec<String> = listing["output"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    let mut batches: Vec<String> = std::fs::read_dir(root.join("commands"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "plyrb"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    batches.sort();
    let mut expected = names.clone();
    expected.sort();
    assert_eq!(batches, expected, "one batch per registered command");

    let update = std::env::var_os("PLANYRITH_UPDATE_GOLDEN").is_some();
    let dir = scratch("commands");
    // Batches run in the scratch directory, where these textures are.
    std::fs::create_dir_all(dir.join("textures")).unwrap();
    for (file, w, h, rgb) in [
        ("oak.png", 64, 32, [160, 82, 45]),
        ("tile.png", 16, 16, [200, 200, 210]),
    ] {
        image::RgbImage::from_pixel(w, h, image::Rgb(rgb))
            .save(dir.join("textures").join(file))
            .unwrap();
    }
    let mut mismatches = Vec::new();
    for name in &names {
        let batch = root.join("commands").join(format!("{name}.plyrb"));
        let doc = dir.join(format!("{name}.plyr"));
        let out = dir.join(format!("{name}.json"));
        let d = doc.to_str().unwrap();
        planyrith()
            .current_dir(&dir)
            .args(["batch", batch.to_str().unwrap(), "-f", d, "--deterministic"])
            .assert()
            .success();
        planyrith().args(["validate", "-f", d]).assert().success();
        planyrith()
            .args([
                "export",
                "-f",
                d,
                out.to_str().unwrap(),
                "--canonical",
                "--pretty",
            ])
            .assert()
            .success();
        let got = std::fs::read_to_string(&out).unwrap();
        let golden = root.join("golden/commands").join(format!("{name}.json"));
        if update || !golden.exists() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &got).unwrap();
        }
        if std::fs::read_to_string(&golden).unwrap() != got {
            mismatches.push(name.clone());
        }
    }
    assert!(
        mismatches.is_empty(),
        "exports differ from tests/golden/commands for {mismatches:?} \
         (set PLANYRITH_UPDATE_GOLDEN=1 to accept)"
    );
}

/// Runs `planyrith serve` with `frames` on standard input; returns the
/// replies by id and the event notifications, in order.
fn serve(args: &[&str], frames: &[Value]) -> (std::collections::BTreeMap<i64, Value>, Vec<Value>) {
    let input: String = frames.iter().map(|f| format!("{f}\n")).collect();
    let output = planyrith()
        .arg("serve")
        .args(args)
        .write_stdin(input)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let mut replies = std::collections::BTreeMap::new();
    let mut events = Vec::new();
    for line in String::from_utf8(output).unwrap().lines() {
        let frame: Value = serde_json::from_str(line).expect("one JSON frame per line");
        assert_eq!(frame["jsonrpc"], "2.0");
        if frame["method"] == "event" {
            events.push(frame);
        } else {
            replies.insert(frame["id"].as_i64().unwrap_or(-1), frame);
        }
    }
    (replies, events)
}

fn rpc(id: i64, method: &str, params: Value) -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// docs/08 §3 and AGENTS.md §2 (every command reachable via JSON-RPC).
#[test]
fn serve_answers_json_rpc_over_stdio() {
    use serde_json::json;
    let dir = scratch("serve");
    let file = dir.join("served.plyr");
    let f = file.to_str().unwrap();

    // Every registered command has a schema over JSON-RPC.
    let (r, _) = serve(&[], &[rpc(1, "command.list", json!({}))]);
    let names: Vec<String> = r[&1]["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.len() > 20 && names.contains(&"push-pull".to_string()));
    let frames: Vec<Value> = names
        .iter()
        .enumerate()
        .map(|(i, n)| rpc(i as i64, "command.schema", json!({ "name": n })))
        .collect();
    let (r, _) = serve(&[], &frames);
    for (i, name) in names.iter().enumerate() {
        let schema = &r[&(i as i64)]["result"];
        assert!(schema["params"].is_object(), "{name}: {schema}");
        assert_eq!(schema["meta"]["name"], name.as_str());
    }

    // A session: draw and pull as one undo step, undo, redo, query, save.
    let frames = [
        rpc(1, "server.capabilities", json!({})),
        rpc(2, "session.new", json!({ "path": f })),
        rpc(
            3,
            "events.subscribe",
            json!({ "kinds": ["history_changed"] }),
        ),
        rpc(
            4,
            "command.execute",
            json!({ "doc_id": "d1", "name": "rectangle", "group": "box",
            "params": { "corner_a": [0, 0, 0], "corner_b": [1000, 500, 0] } }),
        ),
        rpc(
            5,
            "command.execute",
            json!({ "doc_id": "d1", "name": "push-pull", "group": "box",
            "params": { "face": { "Query": "face & last" }, "distance": 300 } }),
        ),
        rpc(6, "history.undo", json!({ "doc_id": "d1" })),
        rpc(
            7,
            "document.query",
            json!({ "doc_id": "d1", "query": "face" }),
        ),
        rpc(8, "history.redo", json!({ "doc_id": "d1" })),
        rpc(
            9,
            "document.query",
            json!({ "doc_id": "d1", "query": "face & normal:z", "fields": ["pid", "area"] }),
        ),
        rpc(
            10,
            "selection.set",
            json!({ "doc_id": "d1", "query": "edge & last" }),
        ),
        rpc(11, "context.get", json!({ "doc_id": "d1" })),
        rpc(12, "document.get", json!({ "doc_id": "d1", "pid": 5 })),
        rpc(13, "session.save", json!({ "doc_id": "d1" })),
        rpc(14, "no.such.method", json!({})),
        rpc(15, "document.query", json!({ "query": "face" })),
        rpc(
            16,
            "command.execute",
            json!({ "doc_id": "d1", "name": "push-pull",
            "params": { "face": { "Pid": 999 }, "distance": 1 } }),
        ),
        json!({ "jsonrpc": "2.0", "method": "history.undo", "params": { "doc_id": "d1" } }),
    ];
    let (r, events) = serve(&["--deterministic"], &frames);
    assert_eq!(r[&1]["result"]["features"]["transports"][0], "stdio");
    assert_eq!(r[&2]["result"]["doc_id"], "d1");
    assert_eq!(r[&4]["result"]["output"]["face"], 5);
    assert_eq!(r[&5]["result"]["output"]["solid_after"], true);
    // One undo step removes both commands of the group.
    assert_eq!(r[&6]["result"]["applied"], 0);
    assert_eq!(r[&7]["result"], json!([]));
    assert_eq!(r[&8]["result"]["entries"], json!(["box"]));
    assert_eq!(r[&9]["result"].as_array().unwrap().len(), 1);
    assert_eq!(r[&9]["result"][0]["area"], 500_000.0);
    // After redoing the group, `last` is everything the group created.
    assert_eq!(r[&10]["result"]["selection"].as_array().unwrap().len(), 12);
    assert_eq!(r[&11]["result"]["context"], "#Model");
    assert_eq!(r[&12]["result"]["kind"], "face");
    assert_eq!(r[&13]["result"]["path"], f);
    assert_eq!(r[&14]["error"]["code"], -32601);
    assert_eq!(r[&15]["error"]["code"], -32602);
    assert_eq!(r[&16]["error"]["code"], 1101);
    assert_eq!(r[&16]["error"]["data"]["code"], "E1101");
    // The trailing notification got no reply, but its event was sent.
    assert!(!r.contains_key(&-1));
    assert!(
        events
            .iter()
            .all(|e| e["params"]["kind"] == "history_changed")
    );
    assert_eq!(events.last().unwrap()["params"]["data"]["can_redo"], true);
    planyrith().args(["validate", "-f", f]).assert().success();

    // Parse errors and read-only servers.
    let (r, _) = serve(
        &["--readonly", "-f", f],
        &[
            rpc(
                1,
                "command.execute",
                json!({ "doc_id": "d1", "name": "erase",
                "params": { "targets": "edge" } }),
            ),
            rpc(
                2,
                "document.query",
                json!({ "doc_id": "d1", "query": "face", "fields": ["pid"] }),
            ),
            rpc(3, "session.save", json!({ "doc_id": "d1" })),
        ],
    );
    assert_eq!(r[&1]["error"]["data"]["code"], "E1501");
    assert_eq!(r[&2]["result"].as_array().unwrap().len(), 6);
    assert_eq!(r[&3]["error"]["data"]["code"], "E1501");
    let out = planyrith()
        .arg("serve")
        .write_stdin("not json\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let frame: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(frame["error"]["code"], -32700);
}

/// docs/06 §11: `render` writes images of one view or of every scene.
#[cfg(feature = "render")]
#[test]
fn render_writes_images() {
    let dir = scratch("render");
    let file = build_table(&dir, "table.plyr");
    let f = file.to_str().unwrap();
    planyrith()
        .args([
            "tool",
            "camera-standard",
            "-f",
            f,
            "--view",
            "iso",
            "--extents",
        ])
        .assert()
        .success();
    planyrith()
        .args(["tool", "scene-add", "-f", f, "--name", "Iso"])
        .assert()
        .success();
    planyrith()
        .args([
            "tool",
            "camera-standard",
            "-f",
            f,
            "--view",
            "top",
            "--extents",
        ])
        .assert()
        .success();
    planyrith()
        .args(["tool", "scene-add", "-f", f, "--name", "Plan/Top"])
        .assert()
        .success();
    let png = dir.join("view.png");
    let result = planyrith()
        .args([
            "render",
            "-f",
            f,
            png.to_str().unwrap(),
            "--width",
            "160",
            "--height",
            "100",
        ])
        .args([
            "--camera",
            "eye=3m,-2m,2m;target=600,400,400;fov=40",
            "--json",
        ])
        .output()
        .unwrap();
    if !result.status.success() && std::env::var_os("PLANYRITH_REQUIRE_GPU").is_none() {
        eprintln!(
            "skipping: no graphics adapter ({})",
            String::from_utf8_lossy(&result.stdout)
        );
        return;
    }
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report = json_of(&result.stdout);
    assert_eq!(report["output"]["width"], 160);
    let image = image::open(&png).unwrap();
    assert_eq!((image.width(), image.height()), (160, 100));

    let scenes = dir.join("scenes");
    planyrith()
        .args(["render", "-f", f, "--all-scenes", scenes.to_str().unwrap()])
        .args(["--width", "80", "--height", "60", "--msaa", "1"])
        .assert()
        .success();
    assert!(scenes.join("Iso.png").exists());
    assert!(scenes.join("Plan_Top.png").exists());
    planyrith()
        .args([
            "render",
            "-f",
            f,
            png.to_str().unwrap(),
            "--camera",
            "eye=1,1,1;target=1,1,1",
        ])
        .assert()
        .code(2);
}

/// docs/08 §3: render buffers and previews over JSON-RPC.
#[test]
fn serve_extracts_render_meshes_and_previews_commands() {
    use serde_json::json;
    let frames = [
        rpc(1, "session.new", json!({})),
        rpc(
            2,
            "command.execute",
            json!({ "doc_id": "d1", "name": "rectangle",
            "params": { "corner_a": [0, 0, 0], "corner_b": [1000, 500, 0] } }),
        ),
        rpc(3, "document.render_mesh", json!({ "doc_id": "d1" })),
        rpc(
            4,
            "command.preview",
            json!({ "doc_id": "d1", "name": "push-pull",
            "params": { "face": { "Query": "face & last" }, "distance": 300 } }),
        ),
        rpc(
            5,
            "document.query",
            json!({ "doc_id": "d1", "query": "face" }),
        ),
        rpc(
            6,
            "document.render_mesh",
            json!({ "doc_id": "d1", "path": "#Model/@1" }),
        ),
    ];
    let (r, _) = serve(&[], &frames);
    let mesh = &r[&3]["result"];
    assert_eq!(mesh["container"], "model");
    // One face, both sides, four vertices each; 12 bytes per position.
    assert_eq!(mesh["vertex_count"], 8);
    assert_eq!(
        mesh["positions"].as_str().unwrap().len(),
        (8 * 12usize).div_ceil(3) * 4
    );
    assert_eq!(mesh["edges"]["count"], 4);
    assert_eq!(mesh["face_pids"], json!([5]));
    assert_eq!(mesh["ranges"].as_array().unwrap().len(), 2);
    let preview = &r[&4]["result"];
    assert_eq!(preview["output"]["solid_after"], true);
    assert_eq!(preview["containers"][0]["container"], "model");
    assert_eq!(preview["containers"][0]["vertex_count"], 6 * 4 * 2);
    // The preview ran on a copy.
    assert_eq!(r[&5]["result"].as_array().unwrap().len(), 1);
    assert_eq!(r[&6]["error"]["code"], -32602, "an edge opens no container");
}

/// Reads one length-prefixed frame.
fn read_frame(stream: &mut impl std::io::Read) -> Value {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).unwrap();
    let mut buf = vec![0u8; u32::from_le_bytes(len) as usize];
    stream.read_exact(&mut buf).unwrap();
    serde_json::from_slice(&buf).unwrap()
}

fn write_frame(stream: &mut impl std::io::Write, frame: &Value) {
    let bytes = frame.to_string().into_bytes();
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .unwrap();
    stream.write_all(&bytes).unwrap();
}

/// Sends a request and returns its reply (skipping event notifications).
fn call(stream: &mut (impl std::io::Read + std::io::Write), frame: Value) -> Value {
    write_frame(stream, &frame);
    loop {
        let reply = read_frame(stream);
        if reply["method"] != "event" {
            return reply;
        }
    }
}

/// Starts `planyrith serve` with `args`; returns the child and the address
/// it prints on standard error.
fn spawn_server(args: &[&str]) -> (std::process::Child, String) {
    use std::io::BufRead;
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("planyrith"))
        .arg("serve")
        .args(args)
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .trim()
        .strip_prefix("listening on ")
        .unwrap_or_else(|| panic!("unexpected server output: {line}"))
        .to_string();
    (child, addr)
}

/// docs/08 §3: the TCP transport needs `server.auth`; documents outlive a
/// connection.
#[test]
fn serve_over_tcp_requires_the_token() {
    use serde_json::json;
    planyrith()
        .args(["serve", "--socket", "0"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("--token"));
    let (mut child, addr) = spawn_server(&["--socket", "0", "--token", "s3cret"]);
    let result = std::panic::catch_unwind(|| {
        let mut stream = std::net::TcpStream::connect(&addr).unwrap();
        let r = call(&mut stream, rpc(1, "server.capabilities", json!({})));
        assert_eq!(r["error"]["code"], -32001);
        let r = call(
            &mut stream,
            rpc(2, "server.auth", json!({ "token": "wrong" })),
        );
        assert_eq!(r["error"]["code"], -32001);
        let r = call(
            &mut stream,
            rpc(3, "server.auth", json!({ "token": "s3cret" })),
        );
        assert_eq!(r["result"]["authenticated"], true);
        let r = call(&mut stream, rpc(4, "session.new", json!({})));
        assert_eq!(r["result"]["doc_id"], "d1");
        let r = call(
            &mut stream,
            rpc(
                5,
                "command.execute",
                json!({ "doc_id": "d1", "name": "rectangle",
                "params": { "corner_a": [0, 0, 0], "corner_b": [100, 100, 0] } }),
            ),
        );
        assert_eq!(r["result"]["output"]["face"], 5);
        drop(stream);
        // A new connection authenticates again and finds the document.
        let mut stream = std::net::TcpStream::connect(&addr).unwrap();
        let r = call(
            &mut stream,
            rpc(
                6,
                "document.query",
                json!({ "doc_id": "d1", "query": "face" }),
            ),
        );
        assert_eq!(r["error"]["code"], -32001);
        call(
            &mut stream,
            rpc(7, "server.auth", json!({ "token": "s3cret" })),
        );
        let r = call(
            &mut stream,
            rpc(
                8,
                "document.query",
                json!({ "doc_id": "d1", "query": "face" }),
            ),
        );
        assert_eq!(r["result"].as_array().unwrap().len(), 1);
    });
    let _ = child.kill();
    let _ = child.wait();
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// docs/08 §3: a Unix socket only this user may open.
#[cfg(unix)]
#[test]
fn serve_over_a_unix_socket() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    // Short: macOS limits socket paths to about 104 bytes.
    let path = std::env::temp_dir().join(format!("ply-{}.sock", std::process::id()));
    let (mut child, addr) = spawn_server(&["--socket", path.to_str().unwrap()]);
    let result = std::panic::catch_unwind(|| {
        assert_eq!(addr, path.display().to_string());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let mut stream = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let r = call(&mut stream, rpc(1, "server.capabilities", json!({})));
        assert_eq!(
            r["result"]["features"]["transports"],
            json!(["stdio", "tcp", "unix"])
        );
    });
    let _ = child.kill();
    let _ = child.wait();
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// docs/08 §3: `repl` runs lines as they come and saves at the end.
#[test]
fn repl_runs_lines_and_saves() {
    let dir = scratch("repl");
    let file = dir.join("repl.plyr");
    let f = file.to_str().unwrap();
    let input = "\
rectangle --corner-a 0,0,0 --corner-b 1m,600mm,0
push-pull --face '{face & last}' --distance 400
:undo
:json on
no-such-command
select --query face
:q
";
    let output = planyrith()
        .args(["repl", "-f", f])
        .write_stdin(input)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("\"undone\""), "{text}");
    assert!(text.contains("\"ok\":false"), "{text}");
    assert!(
        text.contains("\"count\":1"),
        "after the undo one face is left: {text}"
    );
    let info = json_of(
        &planyrith()
            .args(["info", "-f", f, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    assert_eq!(info["output"]["counts"]["model"]["faces"], 1);
    // `:q!` leaves the file as it was.
    planyrith()
        .args(["repl", "-f", f])
        .write_stdin("rectangle --corner-a 0,0,0 --corner-b 1,1,0\n:q!\n")
        .assert()
        .success();
    let info = json_of(
        &planyrith()
            .args(["info", "-f", f, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    assert_eq!(info["output"]["counts"]["model"]["faces"], 1);
}

/// docs/13 0.3 "inference golden tests": what rays at known places snap to
/// on the test table, in the order of docs/03 §7.
#[test]
fn inference_matches_the_golden_file() {
    let dir = scratch("infer");
    let file = build_table(&dir, "table.plyr");
    let f = file.to_str().unwrap();
    let queries: [&[&str]; 9] = [
        // Corner of the top, midpoint of a top edge, on an edge, on the top.
        &["--from", "1205,805,5000", "--dir", "0,0,-1"],
        &["--from", "603,4,5000", "--dir", "0,0,-1"],
        &["--from", "300,5,5000", "--dir", "0,0,-1"],
        &["--from", "400,400,5000", "--dir", "0,0,-1"],
        // Free on the ground, and the red axis from a reference point.
        &["--from", "3000,3000,5000", "--dir", "0,0,-1"],
        &[
            "--from",
            "3000,6,5000",
            "--dir",
            "0,0,-1",
            "--reference",
            "2000,0,0",
        ],
        // A leg's foot seen from the side, behind nothing.
        &["--from", "45,-5000,2", "--dir", "0,1,0"],
        // Locked to the blue axis at the reference: the top's height.
        &[
            "--from",
            "1205,805,5000",
            "--dir",
            "0,0,-1",
            "--reference",
            "2000,0,0",
            "--lock",
            "z",
        ],
        // Constrained to the plane z = 300 through the reference.
        &[
            "--from",
            "1205,805,5000",
            "--dir",
            "0,0,-1",
            "--reference",
            "0,0,300",
            "--plane",
            "0,0,1",
        ],
    ];
    let mut results = Vec::new();
    for q in queries {
        let out = json_of(
            &planyrith()
                .args(["infer", "-f", f, "--json"])
                .args(q)
                .assert()
                .success()
                .get_output()
                .stdout,
        );
        results.push(serde_json::json!({ "query": q.join(" "), "inference": out["output"] }));
    }
    let got = serde_json::to_string_pretty(&results).unwrap() + "\n";
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/infer.json");
    if std::env::var_os("PLANYRITH_UPDATE_GOLDEN").is_some() || !golden.exists() {
        std::fs::write(&golden, &got).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&golden).unwrap(),
        got,
        "inference differs from {} (set PLANYRITH_UPDATE_GOLDEN=1 to accept)",
        golden.display()
    );
}
