//! Implements docs/10 §9 and docs/13 0.8 automation acceptance.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
fn cli() -> Command {
    Command::cargo_bin("planyrith").unwrap()
}
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("planyrith-cli-08-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
#[test]
fn bundled_extensions_execute_from_cli_and_reject_invalid_parameters() {
    let dir = scratch("bundled");
    for name in [
        "ext:org.planyrith.stairs/stairs",
        "ext:org.planyrith.cabinet/cabinet",
        "ext:org.planyrith.csv-points/points",
    ] {
        let file = dir
            .join(name.rsplit('/').next().unwrap())
            .with_extension("plyr");
        cli()
            .args(["tool", name, "-f"])
            .arg(&file)
            .arg("--deterministic")
            .assert()
            .success();
        cli().args(["validate", "-f"]).arg(&file).assert().success();
    }
    cli()
        .args([
            "tool",
            "ext:org.planyrith.stairs/stairs",
            "--steps",
            "0",
            "--no-save",
            "--json",
        ])
        .assert()
        .failure()
        .code(2);
    cli()
        .args([
            "tool",
            "ext:org.planyrith.stairs/stairs",
            "--steps",
            "3",
            "--width",
            "1m",
            "--rise",
            "600mm",
            "--run",
            "90cm",
            "--no-save",
        ])
        .assert()
        .success();
}
#[test]
fn scripts_batches_rpc_and_attribute_helpers_work_headlessly() {
    let dir = scratch("scripting");
    let source = dir.join("box.rhai");
    let doc = dir.join("box.plyr");
    std::fs::write(&source,r#"rectangle(#{corner_a:pt(0,0,0),corner_b:pt(args.width,100,0)});push_pull(#{face:sel("face & last"),distance:50});let f=query("face")[0];set_attr(f,"ext.test.kind","panel");get_attr(f,"ext.test.kind")"#).unwrap();
    let result = cli()
        .args(["script", "--arg", "width=200", "--json", "-f"])
        .arg(&doc)
        .arg(&source)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let result: Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(result["output"]["value"], "panel");
    let batch = dir.join("script.plyrb");
    std::fs::write(&batch,"script: rectangle(#{corner_a:pt(1000,0,0),corner_b:pt(1100,100,0)}); push_pull(#{face:sel(\"face & last\"),distance:20});\n").unwrap();
    cli()
        .args(["batch", "-f"])
        .arg(&doc)
        .arg(&batch)
        .assert()
        .success();
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("planyrith"))
        .args(["serve", "--stdio"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    for (id, method, params) in [
        (1, "session.new", json!({})),
        (
            2,
            "script.eval",
            json!({"doc_id":"d1","source":"rectangle(#{corner_a:pt(0,0,0),corner_b:pt(100,100,0)});query(\"face\").len()"}),
        ),
        (3, "document.snapshot", json!({"doc_id":"d1"})),
    ] {
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert!(value.get("error").is_none(), "{value}");
        if id == 2 {
            assert_eq!(value["result"]["output"]["value"], 1);
        }
        if id == 3 {
            assert!(!value["result"]["bytes"].as_array().unwrap().is_empty());
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}
#[test]
fn archive_install_check_and_discovery_are_persistent() {
    let dir = scratch("install");
    let archive = dir.join("example.plyrx");
    let root = dir.join("installed");
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/org.planyrith.stairs");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
    for name in ["plugin.toml", "main.rhai", "params.json", "form.json"] {
        let mut text = std::fs::read_to_string(source.join(name)).unwrap();
        if name == "plugin.toml" {
            text = text.replace("org.planyrith.stairs", "org.example.test");
        }
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    cli()
        .args(["ext", "check"])
        .arg(&archive)
        .assert()
        .success();
    cli()
        .env("PLANYRITH_PLUGIN_DIR", &root)
        .args(["ext", "install"])
        .arg(&archive)
        .assert()
        .success();
    assert!(root.join("org.example.test.plyrx").exists());
    let result = cli()
        .env("PLANYRITH_PLUGIN_DIR", &root)
        .args(["tool", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let list: Value = serde_json::from_slice(&result).unwrap();
    assert!(
        list["output"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "ext:org.example.test/stairs" && c["source"] == "plugin")
    );
    cli()
        .env("PLANYRITH_PLUGIN_DIR", &root)
        .args([
            "tool",
            "ext:org.example.test/stairs",
            "--steps",
            "3",
            "--no-save",
        ])
        .assert()
        .success();
}
