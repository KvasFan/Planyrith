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
