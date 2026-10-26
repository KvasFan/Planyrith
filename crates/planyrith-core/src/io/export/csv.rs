//! Implements docs/09-file-formats.md §5 CSV export: the component report.
//! One row per component the export scope shows, in definition order:
//! name, how many times it occurs, its size (bounding box in its own axes)
//! in `--units`, its volume when it is a solid (cubic units), and its
//! description.

use std::collections::BTreeMap;
use std::io::Write;

use crate::document::{ContainerRef, Document};
use crate::io::export::obj::num;
use crate::io::export::scene::collect;
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter};
use crate::io::{IoError, Progress};

/// The component report exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct CsvExporter;

/// A CSV field, quoted when it holds a separator, quote or line break.
fn field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

impl Exporter for CsvExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Csv
    }

    fn export(
        &self,
        doc: &Document,
        scope: &ExportScope,
        opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<ExportReport, IoError> {
        let scene = collect(doc, scope);
        let mut counts: BTreeMap<crate::ids::DefinitionId, usize> = BTreeMap::new();
        for occ in &scene.occurrences {
            if let ContainerRef::Definition(d) = occ.container
                && doc.definition(d).is_some_and(|def| !def.is_internal_group)
            {
                *counts.entry(d).or_default() += 1;
            }
        }
        progress.report(0.5, "measuring components");
        let unit = opts.units.symbol();
        let to_unit = 1.0 / opts.units.millimeters();
        let mut text = format!(
            "name,count,width_{unit},depth_{unit},height_{unit},volume_{unit}3,description\n"
        );
        let mut rows = 0;
        for &d in &doc.definition_order {
            let (Some(&count), Some(def)) = (counts.get(&d), doc.definition(d)) else {
                continue;
            };
            let bounds = def.entities.local_aabb(&doc.definitions, false);
            let size = if bounds.is_empty() {
                glam::DVec3::ZERO
            } else {
                (bounds.max - bounds.min) * to_unit
            };
            let mesh = def.entities.mesh();
            let volume = if mesh.is_solid() {
                mesh.volume()
                    .map(|v| num(v.abs() * to_unit * to_unit * to_unit))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            text.push_str(&format!(
                "{},{count},{},{},{},{volume},{}\n",
                field(&def.name),
                num(size.x),
                num(size.y),
                num(size.z),
                field(&def.description)
            ));
            rows += 1;
        }
        dst.write_all(text.as_bytes())
            .map_err(|e| IoError::Storage {
                key: "csv".into(),
                reason: e.to_string(),
            })?;
        Ok(ExportReport {
            bytes_written: text.len() as u64,
            entities: rows,
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}
