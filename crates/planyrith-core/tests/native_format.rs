//! Implements docs/09-file-formats.md §4 and §9: the frozen native format.
//!
//! `tests/fixtures/native/v<N>/` holds the `.plyr` files that the release
//! introducing `format_version` N wrote (`tools/native-fixtures.sh`); they are
//! never regenerated. Every one must load without warnings, and saving it again
//! must keep every stored key and value: a field may be added, but never
//! removed, renamed, retyped or reinterpreted, because readers of that version
//! require what it wrote. Such a change needs a new `format_version` and a
//! migration, after which the comparison runs on the migrated tree.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Read;
use std::path::{Path, PathBuf};

use planyrith_core::document::Document;
use planyrith_core::io::NoProgress;
use planyrith_core::io::native::{
    WriteOptions, migrate, read_document, read_document_part, read_manifest, write_document,
};
use serde_json::Value;

fn corpus() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native");
    let mut files = Vec::new();
    for version in std::fs::read_dir(&root).expect("tests/fixtures/native") {
        for file in std::fs::read_dir(version.unwrap().path()).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_some_and(|e| e == "plyr") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// A msgpack part of a `.plyr` as a tree, or `None` when absent.
fn part(bytes: &[u8], name: &str) -> Option<Value> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut entry = archive.by_name(name).ok()?;
    let mut data = Vec::new();
    entry.read_to_end(&mut data).unwrap();
    Some(migrate::decode_tree(&data).unwrap())
}

/// Paths where `old` has something `new` lacks or holds differently.
fn missing(old: &Value, new: &Value, path: &str, out: &mut Vec<String>) {
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => {
            for (key, value) in old {
                match new.get(key) {
                    Some(other) => missing(value, other, &format!("{path}.{key}"), out),
                    None => out.push(format!("{path}.{key} removed")),
                }
            }
        }
        (Value::Array(old), Value::Array(new)) if old.len() == new.len() => {
            for (i, (a, b)) in old.iter().zip(new).enumerate() {
                missing(a, b, &format!("{path}[{i}]"), out);
            }
        }
        _ if old == new => {}
        _ => out.push(format!("{path}: {old} became {new}")),
    }
}

fn options() -> WriteOptions {
    WriteOptions {
        deterministic: true,
        compress: true,
        thumbnail: None,
        include_history: true,
        generator: "planyrith test".into(),
        created: String::new(),
        modified: String::new(),
        is_template: false,
        extra_parts: Default::default(),
    }
}

#[test]
fn the_corpus_covers_every_registered_command() {
    let files = corpus();
    assert!(files.len() >= 100, "only {} frozen files", files.len());
}

#[test]
fn frozen_files_load_and_save_without_losing_anything() {
    let mut problems = Vec::new();
    for file in corpus() {
        let name = file.display();
        let bytes = std::fs::read(&file).unwrap();
        let version = read_manifest(&bytes).unwrap().format_version;
        let loaded = match read_document(&bytes, &NoProgress) {
            Ok(loaded) => loaded,
            Err(e) => {
                problems.push(format!("{name}: {e}"));
                continue;
            }
        };
        if !loaded.warnings.is_empty() {
            problems.push(format!("{name}: warnings {:?}", loaded.warnings));
        }
        let saved =
            write_document(&loaded.document, &loaded.history, &options(), &NoProgress).unwrap();
        for name_of_part in ["document.msgpack", "sheets.msgpack"] {
            let Some(old) = part(&bytes, name_of_part) else {
                continue;
            };
            let old = if name_of_part == "document.msgpack" {
                migrate::migrate(old, version).unwrap()
            } else {
                old
            };
            let Some(new) = part(&saved, name_of_part) else {
                problems.push(format!("{name}: {name_of_part} no longer written"));
                continue;
            };
            let mut out = Vec::new();
            missing(&old, &new, "", &mut out);
            if name_of_part == "document.msgpack" {
                // The migration path re-encodes the tree; it must decode to
                // the same document as the direct path.
                let migrated: Document =
                    rmp_serde::from_slice(&migrate::encode_tree(&old).unwrap()).unwrap();
                if serde_json::to_value(&migrated).unwrap()
                    != serde_json::to_value(&loaded.document).unwrap()
                {
                    out.push(" decodes differently through the migration tree".into());
                }
            }
            problems.extend(out.into_iter().map(|p| format!("{name} {name_of_part}{p}")));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Forward compatibility (docs/09 §4): a later writer may add fields, which
/// this reader ignores.
#[test]
fn unknown_fields_are_ignored() {
    let file = corpus()
        .into_iter()
        .find(|f| f.ends_with("v1/rectangle.plyr"))
        .expect("v1/rectangle.plyr");
    let bytes = std::fs::read(file).unwrap();
    let mut tree = part(&bytes, "document.msgpack").unwrap();
    let future = |object: &mut Value| {
        object
            .as_object_mut()
            .unwrap()
            .insert("added_by_a_later_writer".into(), Value::Bool(true));
    };
    future(&mut tree);
    future(&mut tree["info"]);
    let document = read_document_part(&rmp_serde::to_vec_named(&tree).unwrap()).unwrap();
    let reference = read_document(&bytes, &NoProgress).unwrap().document;
    assert_eq!(
        serde_json::to_value(&document).unwrap(),
        serde_json::to_value(&reference).unwrap()
    );
}
