//! Desktop SKP entry points (docs/09 §5), with the separately built converter.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use assert_cmd::Command;
use serde_json::Value;
fn cli() -> Command {
    Command::cargo_bin("planyrith").unwrap()
}

#[test]
fn native_working_copy_retains_the_complete_unedited_skp_source() {
    let adapter = assert_cmd::cargo::cargo_bin("planyrith")
        .with_file_name(format!("planyrith-skp{}", std::env::consts::EXE_SUFFIX));
    if !adapter.is_file() {
        assert!(std::env::var_os("PLANYRITH_REQUIRE_SKP").is_none());
        return;
    }
    let dir = std::env::temp_dir().join(format!("planyrith-cli-skp-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for version in [2017, 2026] {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../planyrith-skp/tests/fixtures/box-{version}.skp"));
        let native = dir.join(format!("working-{version}.plyr"));
        let restored = dir.join(format!("restored-{version}.skp"));
        cli()
            .arg("-f")
            .arg(&source)
            .arg("-o")
            .arg(&native)
            .args(["tool", "select", "--query", "all"])
            .assert()
            .success();
        assert!(std::fs::read(&native).unwrap().starts_with(b"PK"));
        cli()
            .arg("-f")
            .arg(&native)
            .arg("-o")
            .arg(&restored)
            .args(["tool", "select", "--query", "all"])
            .assert()
            .success();
        assert_eq!(
            std::fs::read(&restored).unwrap(),
            std::fs::read(&source).unwrap()
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}
#[test]
fn cli_opens_modifies_and_saves_real_skp_without_renaming_native_bytes() {
    let adapter = assert_cmd::cargo::cargo_bin("planyrith")
        .with_file_name(format!("planyrith-skp{}", std::env::consts::EXE_SUFFIX));
    if !adapter.is_file() {
        assert!(
            std::env::var_os("PLANYRITH_REQUIRE_SKP").is_none(),
            "build the optional planyrith-skp converter before this acceptance test"
        );
        return;
    }
    let dir = std::env::temp_dir().join(format!("planyrith-cli-skp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let skp = dir.join("cube.skp");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../planyrith-skp/tests/fixtures/box-2017.skp"),
        &skp,
    )
    .unwrap();
    cli()
        .args(["--deterministic", "-f"])
        .arg(&skp)
        .args([
            "tool",
            "move",
            "--params-json",
            r#"{"targets":"all","delta":[100,0,0]}"#,
        ])
        .assert()
        .success();
    assert!(
        std::fs::read(&skp)
            .unwrap()
            .starts_with(b"\xff\xfe\xff\x0e")
    );
    let out = cli()
        .arg("-f")
        .arg(&skp)
        .args(["info", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let info: Value = serde_json::from_slice(&out).unwrap();
    assert!((info["output"]["bounds"]["min"][0].as_f64().unwrap() - 100.0).abs() < 1e-6);
    let native = dir.join("copy.plyr");
    cli()
        .arg("-f")
        .arg(&skp)
        .arg("-o")
        .arg(&native)
        .args(["tool", "tag-create", "--name", "Exported"])
        .assert()
        .success();
    assert!(std::fs::read(&native).unwrap().starts_with(b"PK"));
    let modern = dir.join("modern.skp");
    cli()
        .arg("-f")
        .arg(&native)
        .arg("export")
        .arg(&modern)
        .args(["--skp-version", "2026"])
        .assert()
        .success();
    cli()
        .arg("-f")
        .arg(&modern)
        .arg("validate")
        .assert()
        .success();
    cli().args(["export", "--help"]).assert().success();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rpc_and_rhai_share_skp_support_and_return_compatibility_notices() {
    use std::io::{BufRead, BufReader, Write};
    let adapter = assert_cmd::cargo::cargo_bin("planyrith")
        .with_file_name(format!("planyrith-skp{}", std::env::consts::EXE_SUFFIX));
    if !adapter.is_file() {
        assert!(std::env::var_os("PLANYRITH_REQUIRE_SKP").is_none());
        return;
    }
    let dir = std::env::temp_dir().join(format!("planyrith-rpc-skp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../planyrith-skp/tests/fixtures/box-2017.skp");
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("planyrith"))
        .args(["serve", "--stdio"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    for (id, method, params) in [
        (1, "session.open", serde_json::json!({"path":source})),
        (
            2,
            "script.eval",
            serde_json::json!({"doc_id":"d1","source":"freehand(#{points:[[4000.0,0.0,0.0],[4100.0,100.0,0.0],[4200.0,0.0,0.0]]}); rotated_rectangle(#{origin:[5000.0,0.0,0.0],edge_end:[5100.0,100.0,0.0],width:100.0});"}),
        ),
        (
            3,
            "command.execute",
            serde_json::json!({"doc_id":"d1","name":"export","params":{"path":dir.join("exported.skp")}}),
        ),
        (
            4,
            "session.save",
            serde_json::json!({"doc_id":"d1","path":dir.join("saved.skp")}),
        ),
    ] {
        writeln!(
            input,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert!(response.get("error").is_none(), "{response}");
        if id == 3 || id == 4 {
            assert!(
                !response["result"]["warnings"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
    assert!(
        std::fs::read(dir.join("saved.skp"))
            .unwrap()
            .starts_with(b"\xff\xfe\xff\x0e")
    );
    let _ = std::fs::remove_dir_all(dir);
}
