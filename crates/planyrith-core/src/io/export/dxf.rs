//! Implements docs/09-file-formats.md §5 DXF 3D export with the core's own
//! writer (docs/02 Decisions): an ASCII DXF of version R12, the most widely
//! read, with every visible face as `3DFACE` triangles and every hard edge
//! as a `LINE`, in world coordinates and `--units` (`$INSUNITS`), Z up. Each
//! occurrence goes on the layer of its innermost tag (`0` when untagged);
//! the layer table lists them. Output is deterministic.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write;

use crate::document::{ContainerRef, Document, InstancePath};
use crate::ids::TagId;
use crate::io::export::obj::num;
use crate::io::export::scene::collect;
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter};
use crate::io::{IoError, Progress};
use crate::units::LengthUnit;

/// The DXF exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct DxfExporter;

/// The innermost tag along an occurrence's path.
pub(crate) fn occurrence_tag(doc: &Document, path: &InstancePath) -> Option<TagId> {
    let mut container = ContainerRef::Model;
    let mut tag = None;
    for step in &path.steps {
        let entities = doc.container(container)?;
        let id = entities.key_to_id(*step)?;
        if let Some(t) = entities.tag_of(id) {
            tag = Some(t);
        }
        container = ContainerRef::Definition(entities.definition_of(id)?);
    }
    tag
}

/// A tag name usable as a DXF layer name.
pub(crate) fn layer_name(name: &str) -> String {
    let n: String = name
        .chars()
        .map(|c| {
            if "<>/\\\":;?*|=`".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    if n.trim().is_empty() { "0".into() } else { n }
}

/// `$INSUNITS` of a unit.
pub(crate) fn insunits(unit: LengthUnit) -> i32 {
    match unit {
        LengthUnit::Inch | LengthUnit::ArchitecturalFraction => 1,
        LengthUnit::Foot | LengthUnit::FeetInches => 2,
        LengthUnit::Millimeter => 4,
        LengthUnit::Centimeter => 5,
        LengthUnit::Meter => 6,
    }
}

/// The header and layer table of an R12 DXF, then `entities`.
pub(crate) fn document(unit: LengthUnit, layers: &[String], entities: &str) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "0\nSECTION\n2\nHEADER\n9\n$ACADVER\n1\nAC1009\n9\n$INSUNITS\n70\n{}\n0\nENDSEC\n",
        insunits(unit)
    );
    let _ = write!(
        out,
        "0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nLAYER\n70\n{}\n",
        layers.len()
    );
    for layer in layers {
        let _ = write!(out, "0\nLAYER\n2\n{layer}\n70\n0\n62\n7\n6\nCONTINUOUS\n");
    }
    out.push_str("0\nENDTAB\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n");
    out.push_str(entities);
    out.push_str("0\nENDSEC\n0\nEOF\n");
    out
}

impl Exporter for DxfExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Dxf
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
        let to_unit = 1.0 / opts.units.millimeters();
        let mut layers: BTreeMap<String, ()> = BTreeMap::new();
        layers.insert("0".into(), ());
        let mut body = String::new();
        let mut count = 0usize;
        let p = |v: glam::DVec3| v * to_unit;
        for occ in &scene.occurrences {
            let Some(mesh) = scene.mesh(occ.container) else {
                continue;
            };
            let layer = occurrence_tag(doc, &occ.path)
                .and_then(|t| doc.tags.get(t))
                .map_or_else(|| "0".to_string(), |t| layer_name(&t.name));
            layers.insert(layer.clone(), ());
            for face in &mesh.faces {
                for t in &face.triangles {
                    let c = t.map(|i| p(occ.world.transform_point3(face.positions[i as usize])));
                    let _ = write!(body, "0\n3DFACE\n8\n{layer}\n");
                    for (k, v) in [c[0], c[1], c[2], c[2]].iter().enumerate() {
                        let _ = write!(
                            body,
                            "{}\n{}\n{}\n{}\n{}\n{}\n",
                            10 + k,
                            num(v.x),
                            20 + k,
                            num(v.y),
                            30 + k,
                            num(v.z)
                        );
                    }
                    count += 1;
                }
            }
            for [a, b] in &mesh.edges {
                let (a, b) = (
                    p(occ.world.transform_point3(*a)),
                    p(occ.world.transform_point3(*b)),
                );
                let _ = write!(
                    body,
                    "0\nLINE\n8\n{layer}\n10\n{}\n20\n{}\n30\n{}\n11\n{}\n21\n{}\n31\n{}\n",
                    num(a.x),
                    num(a.y),
                    num(a.z),
                    num(b.x),
                    num(b.y),
                    num(b.z)
                );
                count += 1;
            }
        }
        progress.report(0.8, "writing DXF");
        let layers: Vec<String> = layers.into_keys().collect();
        let text = document(opts.units, &layers, &body);
        dst.write_all(text.as_bytes())
            .map_err(|e| IoError::Storage {
                key: "dxf".into(),
                reason: e.to_string(),
            })?;
        Ok(ExportReport {
            bytes_written: text.len() as u64,
            entities: count,
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}
