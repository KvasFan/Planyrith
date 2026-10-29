//! End-to-end docs/13 0.9 acceptance: paper scale, a three-page PDF, and RPC.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use assert_cmd::Command;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
fn cli() -> Command {
    Command::cargo_bin("planyrith").unwrap()
}
fn scratch(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("planyrith09-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}
#[test]
fn sheets_are_reachable_from_cli_scripts_and_export_scaled_pages() {
    let dir = scratch("sheets");
    let file = dir.join("house.plyr");
    let batch = dir.join("sheets.plyrb");
    let mut source = include_str!("batches/house-section.plyrb").to_string();
    for (name, eye, target, up) in [
        ("A101", [3000, 2000, 10000], [3000, 2000, 0], [0, 1, 0]),
        ("A201", [3000, -10000, 1350], [3000, 2000, 1350], [0, 0, 1]),
        ("A301", [-10000, 2000, 1350], [3000, 2000, 1350], [0, 0, 1]),
    ] {
        source.push_str(&format!("sheet-add-page --page {name}\n"));
        let p = json!({"page":name,"id":"view","camera":{"eye":eye,"target":target,"up":up,"projection":{"parallel":{"height_mm":6000}},"aspect":1.5},"scale":"1:50","frame":{"x":20,"y":35,"width":180,"height":140}});
        source.push_str(&format!("sheet-add-view --params-json '{}'\n", p));
        source.push_str(&format!("sheet-add-text --page {name} --text '{name} — House' --params-json '{{\"frame\":{{\"x\":20,\"y\":180,\"width\":120,\"height\":15}}}}'\n"));
        source.push_str(&format!("sheet-add-title-block --page {name} --params-json '{{\"frame\":{{\"x\":210,\"y\":20,\"width\":70,\"height\":60}},\"fields\":{{\"Scale\":\"1:50\",\"Revision\":\"A\"}}}}'\n"));
    }
    std::fs::write(&batch, source).unwrap();
    cli()
        .args(["batch", "--deterministic", "-f"])
        .arg(&file)
        .arg(&batch)
        .assert()
        .success();
    let pdf = dir.join("house.pdf");
    cli()
        .arg("-f")
        .arg(&file)
        .args(["sheet", "export", "--pages", "all", "--vector", "--out"])
        .arg(&pdf)
        .assert()
        .success();
    let bytes = std::fs::read(&pdf).unwrap();
    assert!(bytes.starts_with(b"%PDF"));
    assert!(String::from_utf8_lossy(&bytes).contains("/Count 3"));
    let output = cli()
        .arg("-f")
        .arg(&file)
        .args(["sheet", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["output"]["pages"].as_array().unwrap().len(), 3);
    cli()
        .arg("-f")
        .arg(&file)
        .arg("validate")
        .assert()
        .success();
    for ext in ["svg", "dxf"] {
        let path = dir.join(format!("house.{ext}"));
        cli()
            .arg("-f")
            .arg(&file)
            .args(["sheet", "export", "--page", "A101", "--out"])
            .arg(&path)
            .assert()
            .success();
        assert!(path.exists());
    }
    cli()
        .args(["sheet", "add-view", "--help"])
        .assert()
        .success();
}
#[test]
fn sheets_are_available_from_rpc_and_generated_rhai_bindings() {
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
            json!({"doc_id":"d1","source":"sheet_add_page(#{page:\"A101\"}); sheet_add_text(#{page:\"A101\",text:\"Hello\"});"}),
        ),
        (
            3,
            "command.execute",
            json!({"doc_id":"d1","name":"sheet-list","params":{}}),
        ),
        (4, "history.undo", json!({"doc_id":"d1"})),
        (
            5,
            "command.execute",
            json!({"doc_id":"d1","name":"sheet-list","params":{}}),
        ),
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
        let v: Value = serde_json::from_str(&line).unwrap();
        assert!(v.get("error").is_none(), "{v}");
        if id == 3 {
            assert_eq!(v["result"]["output"]["pages"].as_array().unwrap().len(), 1);
        }
        if id == 5 {
            assert!(
                v["result"]["output"]["pages"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn paper_lengths_do_not_inherit_model_display_units() {
    use planyrith_core::document::Document;
    use planyrith_core::io::NoProgress;
    use planyrith_core::io::native::{WriteOptions, write_document};
    let dir = scratch("paper-units");
    let file = dir.join("meters.plyr");
    let mut doc = Document::new();
    doc.info.display_units = planyrith_core::units::LengthUnit::Meter;
    std::fs::write(
        &file,
        write_document(&doc, &[], &WriteOptions::default(), &NoProgress).unwrap(),
    )
    .unwrap();
    cli()
        .arg("-f")
        .arg(&file)
        .args(["sheet", "add-page", "--page", "A101", "--no-landscape"])
        .assert()
        .success();
    cli()
        .arg("-f")
        .arg(&file)
        .args([
            "sheet", "add-text", "--page", "A101", "--text", "Note", "--height", "3.5",
        ])
        .assert()
        .success();
    let out = cli()
        .arg("-f")
        .arg(&file)
        .args(["sheet", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert!(
        (v["output"]["pages"][0]["items"][0]["item"]["runs"][0]["height"]
            .as_f64()
            .unwrap()
            - 3.5)
            .abs()
            < 1e-9
    );
}
