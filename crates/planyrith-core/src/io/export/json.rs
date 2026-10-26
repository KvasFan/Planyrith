//! Implements the `json` exporter of docs/09-file-formats.md §5 and the
//! canonical form of docs/08 §10: a full document dump (`--format json
//! --pretty` for debugging, `--canonical` for version control).
//!
//! Draft decisions (docs/09 Decisions, 2026-10-20):
//! - The dump has the same shape as `document.msgpack` (the `Document` type's
//!   derived `serde` form) plus an `images` array listing each stored image's
//!   hash, format, pixel size and byte count; image bytes are excluded.
//!   Containers appear in their file form (`EntitiesSer`: indexed mesh in
//!   canonical order, mesh entities by index).
//! - The canonical form sorts object keys recursively, prints floats with the
//!   shortest round-trip representation (`serde_json`'s fixed formatting),
//!   folds `-0.0` to `0.0`, and never contains NaN or infinities (they become
//!   `null` when the value is built). Output ends with one newline.
//! - The exporter always dumps the whole document; `ExportScope` filters are
//!   applied by the geometry exporters only.
//!
//! Owner key: `io-native`.

#![warn(unused_variables, dead_code)]

use std::io::Write;

use serde_json::Value;

use crate::document::Document;
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter};
use crate::io::{IoError, Progress};

/// The JSON document exporter.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonExporter;

/// Serializes the document to a JSON value in the file layout of docs/09 §2
/// (image bytes excluded; an `images` array lists hash, format and size).
pub fn document_to_json(doc: &Document) -> Result<Value, IoError> {
    let mut value = serde_json::to_value(doc)
        .map_err(|e| IoError::corrupt("document", format!("cannot convert to JSON: {e}")))?;
    let images: Vec<Value> = doc
        .images
        .iter()
        .map(|(hash, image)| {
            serde_json::json!({
                "hash": hash.to_hex(),
                "format": image.format.extension(),
                "width": image.width,
                "height": image.height,
                "bytes": image.bytes.len(),
            })
        })
        .collect();
    match &mut value {
        Value::Object(map) => {
            map.insert("images".to_string(), Value::Array(images));
        }
        other => {
            return Err(IoError::corrupt(
                "document",
                format!("document did not serialize to a JSON object: {other}"),
            ));
        }
    }
    Ok(value)
}

/// Canonicalizes in place: object keys sorted, floats printed with the shortest
/// round-trip representation, `-0.0` normalized to `0.0`, no NaN/∞
/// (docs/08 §10).
pub fn canonicalize(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = std::mem::take(map).into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (key, mut child) in entries {
                canonicalize(&mut child);
                map.insert(key, child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(canonicalize),
        Value::Number(number) => {
            if number.is_f64()
                && let Some(f) = number.as_f64()
                && f.to_bits() == (-0.0_f64).to_bits()
                && let Some(zero) = serde_json::Number::from_f64(0.0)
            {
                *number = zero;
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
}

/// Serializes `value` as the exporter writes it: compact or pretty, with one
/// trailing newline.
fn render(value: &Value, pretty: bool) -> Result<Vec<u8>, IoError> {
    let mut bytes = if pretty {
        serde_json::to_vec_pretty(value)
    } else {
        serde_json::to_vec(value)
    }
    .map_err(|e| IoError::corrupt("json", format!("cannot render JSON: {e}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

impl Exporter for JsonExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Json
    }

    fn export(
        &self,
        doc: &Document,
        _scope: &ExportScope,
        opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<ExportReport, IoError> {
        progress.check_cancelled()?;
        progress.report(0.0, "converting document");
        let mut value = document_to_json(doc)?;
        if opts.canonical {
            canonicalize(&mut value);
        }
        progress.check_cancelled()?;
        progress.report(0.5, "writing json");
        let bytes = render(&value, opts.pretty)?;
        dst.write_all(&bytes)
            .map_err(|e| IoError::storage("json", format!("write failed: {e}")))?;
        let entities = doc
            .containers()
            .iter()
            .filter_map(|c| doc.container(*c))
            .map(|entities| entities.len())
            .sum();
        progress.report(1.0, "done");
        Ok(ExportReport {
            bytes_written: bytes.len() as u64,
            entities,
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{ComponentDefinition, ImageData, ImageFormat, Value as AttrValue};
    use crate::io::NoProgress;

    fn fixture() -> Document {
        let mut doc = Document::new();
        doc.info.title = "JSON fixture".to_string();
        doc.attrs.set("z", "last", AttrValue::Int(1));
        doc.attrs.set("a", "first", AttrValue::Float(-0.0));
        doc.attrs.set("a", "nan", AttrValue::Float(f64::NAN));
        doc.add_definition(ComponentDefinition::new(
            uuid::Uuid::from_u128(0x1234),
            "Door",
        ));
        doc.images.insert(ImageData {
            bytes: vec![1, 2, 3, 4],
            format: ImageFormat::Png,
            width: 1,
            height: 1,
        });
        doc
    }

    fn export(doc: &Document, canonical: bool, pretty: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let opts = ExportOptions {
            canonical,
            pretty,
            ..ExportOptions::default()
        };
        let report = JsonExporter
            .export(doc, &ExportScope::default(), &opts, &mut out, &NoProgress)
            .unwrap();
        assert_eq!(report.bytes_written, out.len() as u64);
        assert_eq!(report.entities, 0);
        assert!(report.warnings.is_empty());
        out
    }

    #[test]
    fn canonicalize_sorts_keys_recursively_and_folds_negative_zero() {
        let mut value = serde_json::json!({
            "b": 1,
            "a": { "z": -0.0, "y": [ { "d": 1.5, "c": 2 } ], "x": 1e21 }
        });
        canonicalize(&mut value);
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            r#"{"a":{"x":1e+21,"y":[{"c":2,"d":1.5}],"z":0.0},"b":1}"#
        );
        let mut scalar = serde_json::json!(-0.0);
        canonicalize(&mut scalar);
        assert_eq!(scalar.to_string(), "0.0");
        let mut int = serde_json::json!(-5);
        canonicalize(&mut int);
        assert_eq!(int.to_string(), "-5");
    }

    #[test]
    fn document_dump_has_the_file_layout_and_lists_images() {
        let doc = fixture();
        let value = document_to_json(&doc).unwrap();
        for key in [
            "info",
            "model",
            "definitions",
            "definition_order",
            "materials",
            "tags",
            "scenes",
            "styles",
            "rendering",
            "camera",
            "attrs",
            "images",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        assert_eq!(value["info"]["title"], "JSON fixture");
        let images = value["images"].as_array().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0]["format"], "png");
        assert_eq!(images[0]["bytes"], 4);
        assert_eq!(images[0]["hash"].as_str().unwrap().len(), 64);
        // NaN cannot be represented: it becomes null (docs/08 §10).
        assert_eq!(
            value["attrs"]["a"]["nan"],
            serde_json::json!({ "Float": null })
        );
    }

    #[test]
    fn canonical_export_is_stable_sorted_and_parseable() {
        let doc = fixture();
        let a = export(&doc, true, false);
        let b = export(&doc, true, false);
        assert_eq!(a, b);
        assert!(a.ends_with(b"\n"));
        assert_eq!(a.iter().filter(|&&c| c == b'\n').count(), 1);
        let text = std::str::from_utf8(&a).unwrap();
        assert!(
            text.starts_with(r#"{"attrs":{"a":{"first":{"Float":0.0}"#),
            "{text}"
        );
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["info"]["title"], "JSON fixture");

        let pretty = export(&doc, true, true);
        assert!(pretty.starts_with(b"{\n  \"attrs\""));
        assert_ne!(pretty, a);
        let compact: Value = serde_json::from_slice(&a).unwrap();
        let spaced: Value = serde_json::from_slice(&pretty).unwrap();
        assert_eq!(compact, spaced);
    }

    #[test]
    fn export_reports_cancellation() {
        let token = crate::io::CancelToken::new();
        token.cancel();
        let mut out = Vec::new();
        let err = JsonExporter
            .export(
                &fixture(),
                &ExportScope::default(),
                &ExportOptions::default(),
                &mut out,
                &token,
            )
            .unwrap_err();
        assert_eq!(err.code(), "E1701");
        assert!(out.is_empty());
    }
}
