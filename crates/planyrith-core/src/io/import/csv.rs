//! Implements docs/09-file-formats.md §5 CSV import: `x,y,z[,name]` rows
//! (`,`, `;` or tab separated; `z` optional) become guide points in a new
//! group or component. Rows that are not numbers (a header) are skipped and
//! counted. Unitless (`--units`, millimeters by default), Z up.

use std::io::Read;

use glam::DVec3;

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::diagnostics::{Warning, WarningCode};
use crate::document::{ComponentDefinition, Entities, EntityRecord, Guide};
use crate::io::import::build::{io, place, top_name};
use crate::io::import::{
    Confidence, ImportFormat, ImportOptions, ImportReport, Importer, source_stem,
};
use crate::io::{IoError, Progress};
use crate::units::LengthUnit;

/// The CSV points importer.
#[derive(Debug, Default, Clone, Copy)]
pub struct CsvImporter;

/// The points of a CSV file and the number of rows skipped.
pub fn read_points(text: &str) -> (Vec<DVec3>, usize) {
    let mut points = Vec::new();
    let mut skipped = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split([',', ';', '\t']).map(str::trim).collect();
        let numbers: Vec<f64> = fields
            .iter()
            .take(3)
            .map_while(|f| f.parse::<f64>().ok().filter(|x| x.is_finite()))
            .collect();
        match numbers.as_slice() {
            [x, y, z] => points.push(DVec3::new(*x, *y, *z)),
            [x, y] if fields.len() == 2 || fields.get(2).is_some_and(|f| f.is_empty()) => {
                points.push(DVec3::new(*x, *y, 0.0));
            }
            _ => skipped += 1,
        }
    }
    (points, skipped)
}

impl Importer for CsvImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Csv
    }

    fn probe(&self, _bytes: &[u8], name: &str) -> Confidence {
        if name.to_ascii_lowercase().ends_with(".csv") {
            Confidence::EXTENSION
        } else {
            Confidence::NONE
        }
    }

    fn import(
        &self,
        src: &mut dyn Read,
        opts: &ImportOptions,
        tx: &mut Transaction<'_>,
        target: &Target,
        progress: &dyn Progress,
    ) -> Result<ImportReport, IoError> {
        let mut text = String::new();
        src.read_to_string(&mut text)
            .map_err(|e| IoError::Corrupt {
                part: "csv".into(),
                reason: e.to_string(),
            })?;
        progress.report(0.3, "reading points");
        let (points, skipped) = read_points(&text);
        let factor = opts.units.unwrap_or(LengthUnit::Millimeter).millimeters() * opts.scale;
        let mut report = ImportReport {
            skipped,
            ..ImportReport::default()
        };
        if skipped > 0 {
            report.warnings.push(Warning::new(
                WarningCode::W2004,
                format!("{skipped} row(s) without coordinates skipped"),
            ));
        }
        let mut entities = Entities::new();
        for p in points {
            let pid = tx.allocate_pid();
            entities
                .insert(EntityRecord::Guide(Guide::Point { pos: p * factor }), pid)
                .map_err(|e| IoError::Corrupt {
                    part: "csv".into(),
                    reason: e.to_string(),
                })?;
        }
        let name = top_name(tx.document(), opts);
        let uuid =
            crate::commands::common::definition_uuid_of(tx.document(), &name, &[text.as_bytes()]);
        let mut def = ComponentDefinition::new(uuid, name);
        def.is_internal_group =
            !matches!(opts.import_as, crate::io::import::ImportAs::Component(_));
        def.entities = entities;
        let top = tx.insert_definition(def).map_err(io)?;
        let pid = place(tx, target, top, opts, &source_stem(&opts.source, "Points"))?;
        report.created.push(pid);
        Ok(report)
    }
}
