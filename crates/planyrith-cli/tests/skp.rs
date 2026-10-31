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
        .arg("-o")
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
fn backup_alias_defaults_to_native_without_overwriting_either_source_or_existing_copy() {
    let adapter = assert_cmd::cargo::cargo_bin("planyrith")
        .with_file_name(format!("planyrith-skp{}", std::env::consts::EXE_SUFFIX));
    if !adapter.is_file() {
        assert!(std::env::var_os("PLANYRITH_REQUIRE_SKP").is_none());
        return;
    }
    let dir = std::env::temp_dir().join(format!("planyrith-cli-legacy-skb-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let original = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../planyrith-skp/tests/fixtures/legacy/box-2013.skp"),
    )
    .unwrap();
    let source = dir.join("model.SKB");
    std::fs::write(&source, &original).unwrap();
    let output = cli()
        .args(["--json", "--deterministic", "-f"])
        .arg(&source)
        .args(["tool", "move", "--delta", "100,0,0", "--targets", "all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output: Value = serde_json::from_slice(&output).unwrap();
    let native = source.with_extension("plyr");
    assert_eq!(
        output["document"]["path"],
        native.to_string_lossy().as_ref()
    );
    assert!(std::fs::read(&native).unwrap().starts_with(b"PK"));
    assert_eq!(std::fs::read(&source).unwrap(), original);
    let saved = std::fs::read(&native).unwrap();
    cli()
        .arg("-f")
        .arg(&source)
        .args(["tool", "select", "--query", "all"])
        .assert()
        .failure();
    assert_eq!(std::fs::read(&native).unwrap(), saved);
    assert_eq!(std::fs::read(&source).unwrap(), original);
    let backup = dir.join("edited.skb");
    cli()
        .arg("-f")
        .arg(&native)
        .arg("-o")
        .arg(&backup)
        .args(["tool", "select", "--query", "all"])
        .assert()
        .success();
    assert!(
        std::fs::read(&backup)
            .unwrap()
            .starts_with(b"\xff\xfe\xff\x0e")
    );
    cli()
        .arg("-f")
        .arg(&backup)
        .arg("validate")
        .assert()
        .success();
    let exported = dir.join("exported.SKB");
    cli()
        .arg("-f")
        .arg(&native)
        .arg("export")
        .arg(&exported)
        .args(["--skp-version", "2026"])
        .assert()
        .success();
    cli()
        .arg("-f")
        .arg(&exported)
        .arg("validate")
        .assert()
        .success();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rpc_backup_sessions_default_to_native_and_keep_explicit_save_choices() {
    let adapter = assert_cmd::cargo::cargo_bin("planyrith")
        .with_file_name(format!("planyrith-skp{}", std::env::consts::EXE_SUFFIX));
    if !adapter.is_file() {
        assert!(std::env::var_os("PLANYRITH_REQUIRE_SKP").is_none());
        return;
    }
    let dir = std::env::temp_dir().join(format!("planyrith-rpc-legacy-skb-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let original = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../planyrith-skp/tests/fixtures/legacy/box-2013.skp"),
    )
    .unwrap();
    let source = dir.join("model.SKB");
    let native = source.with_extension("plyr");
    let backup = dir.join("restored.skb");
    std::fs::write(&source, &original).unwrap();
    let frames = [
        ("session.open", serde_json::json!({"path":source})),
        ("session.save", serde_json::json!({"doc_id":"d1"})),
        ("session.save", serde_json::json!({"doc_id":"d1"})),
        ("session.save", serde_json::json!({"doc_id":"d1","path":backup})),
        ("session.save", serde_json::json!({"doc_id":"d1"})),
        ("command.execute", serde_json::json!({"doc_id":"d1","name":"import","params":{"path":source,"at":[2000,0,0]}})),
        ("history.undo", serde_json::json!({"doc_id":"d1"})),
    ].into_iter().enumerate().map(|(id,(method,params))| format!("{}\n", serde_json::json!({"jsonrpc":"2.0","id":id+1,"method":method,"params":params}))).collect::<String>();
    let output = cli()
        .args(["serve", "--stdio"])
        .write_stdin(frames)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let replies = std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(replies.len(), 7);
    for reply in &replies {
        assert!(reply.get("error").is_none(), "{reply}");
    }
    for reply in &replies[1..3] {
        assert_eq!(reply["result"]["path"], native.to_string_lossy().as_ref());
    }
    for reply in &replies[3..5] {
        assert_eq!(reply["result"]["path"], backup.to_string_lossy().as_ref());
    }
    assert!(std::fs::read(native).unwrap().starts_with(b"PK"));
    assert_eq!(std::fs::read(source).unwrap(), original);
    assert_eq!(std::fs::read(backup).unwrap(), original);
    std::fs::remove_dir_all(dir).unwrap();
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
