//! Implements docs/09-file-formats.md §5 3MF export: one object per visible
//! occurrence with faces, its triangles welded into a shared-vertex mesh in
//! world coordinates (Z up, `--file-units`), the faces' colors as base
//! materials, in a zip package with deterministic timestamps
//! (docs/09 Decisions, 3MF).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{Cursor, Write};

use glam::DVec3;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, System, ZipWriter};

use crate::diagnostics::{Warning, WarningCode};
use crate::document::selection::occurrence_name;
use crate::document::{ContainerRef, Document, FrontOrBack};
use crate::geom::solid_check::diagnose;
use crate::ids::MaterialId;
use crate::io::export::scene::collect;
use crate::io::export::{
    ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter, output_transform,
};
use crate::io::{IoError, Progress};
use crate::units::LengthUnit;

/// Namespace of the 3MF core specification's model part.
pub(crate) const CORE_NAMESPACE: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";
/// Relationship type of the 3MF model part.
pub(crate) const MODEL_RELATIONSHIP: &str =
    "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";
/// Where the model part is stored.
pub(crate) const MODEL_PART: &str = "3D/3dmodel.model";

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
 <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
 <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
</Types>
"#;

/// The 3MF exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThreeMfExporter;

/// The `unit` attribute for `unit`, and the unit coordinates are written in
/// (3MF has no feet-and-inches: those write inches).
fn model_unit(unit: LengthUnit) -> (&'static str, LengthUnit) {
    match unit {
        LengthUnit::Centimeter => ("centimeter", unit),
        LengthUnit::Meter => ("meter", unit),
        LengthUnit::Inch | LengthUnit::FeetInches | LengthUnit::ArchitecturalFraction => {
            ("inch", LengthUnit::Inch)
        }
        LengthUnit::Foot => ("foot", unit),
        LengthUnit::Millimeter => ("millimeter", unit),
    }
}

/// A coordinate as text: the shortest form that reads back exactly, never
/// `-0`.
fn number(v: f64) -> String {
    format!("{}", v + 0.0)
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// One exported object: welded vertices and triangles with their material.
struct Object {
    name: String,
    vertices: Vec<DVec3>,
    triangles: Vec<([u32; 3], MaterialId)>,
}

fn objects(
    doc: &Document,
    scope: &ExportScope,
    opts: &ExportOptions,
) -> Vec<(Object, ContainerRef)> {
    let scene = collect(doc, scope);
    let (_, unit) = model_unit(opts.units);
    let out = output_transform(
        &ExportOptions {
            units: unit,
            z_up: true,
            ..opts.clone()
        },
        false,
    );
    let mut objects = Vec::new();
    for occ in &scene.occurrences {
        let Some(mesh) = scene.mesh(occ.container) else {
            continue;
        };
        if mesh.faces.is_empty() {
            continue;
        }
        let to = out * occ.world;
        let flip = to.matrix3.determinant() < 0.0;
        let mut object = Object {
            name: occurrence_name(doc, &occ.path),
            vertices: Vec::new(),
            triangles: Vec::new(),
        };
        let mut index: BTreeMap<[u64; 3], u32> = BTreeMap::new();
        for face in &mesh.faces {
            let material = doc
                .materials
                .resolve(face.material.or(occ.material_override), FrontOrBack::Front);
            let ids: Vec<u32> = face
                .positions
                .iter()
                .map(|p| {
                    let q = to.transform_point3(*p) + DVec3::ZERO;
                    let key = [q.x.to_bits(), q.y.to_bits(), q.z.to_bits()];
                    *index.entry(key).or_insert_with(|| {
                        object.vertices.push(q);
                        (object.vertices.len() - 1) as u32
                    })
                })
                .collect();
            for t in &face.triangles {
                let [a, b, c] = t.map(|i| ids[i as usize]);
                if a == b || b == c || a == c {
                    continue;
                }
                object
                    .triangles
                    .push((if flip { [a, c, b] } else { [a, b, c] }, material));
            }
        }
        if !object.triangles.is_empty() {
            objects.push((object, occ.container));
        }
    }
    objects
}

/// The model part's XML.
fn model_xml(doc: &Document, objects: &[(Object, ContainerRef)], unit: &str) -> String {
    // Materials in order of first use.
    let mut materials: Vec<MaterialId> = Vec::new();
    for (object, _) in objects {
        for (_, m) in &object.triangles {
            if !materials.contains(m) {
                materials.push(*m);
            }
        }
    }
    let slot = |m: MaterialId| materials.iter().position(|x| *x == m).unwrap_or(0);
    let mut xml = String::new();
    let _ = writeln!(xml, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        xml,
        r#"<model unit="{unit}" xml:lang="en-US" xmlns="{CORE_NAMESPACE}">"#
    );
    let _ = writeln!(
        xml,
        r#" <metadata name="Application">Planyrith {}</metadata>"#,
        env!("CARGO_PKG_VERSION")
    );
    xml.push_str(" <resources>\n");
    if !materials.is_empty() {
        xml.push_str("  <basematerials id=\"1\">\n");
        for m in &materials {
            let (name, color, opacity) = doc.materials.get(*m).map_or(
                ("Default".to_string(), [255, 255, 255], 1.0),
                |mat| {
                    (
                        mat.name.clone(),
                        [mat.color.r, mat.color.g, mat.color.b],
                        mat.opacity,
                    )
                },
            );
            let alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
            let _ = writeln!(
                xml,
                r##"   <base name="{}" displaycolor="#{:02X}{:02X}{:02X}{alpha:02X}"/>"##,
                escape(&name),
                color[0],
                color[1],
                color[2]
            );
        }
        xml.push_str("  </basematerials>\n");
    }
    for (i, (object, _)) in objects.iter().enumerate() {
        let id = i + 2;
        let default = object.triangles.first().map_or(0, |(_, m)| slot(*m));
        let _ = writeln!(
            xml,
            r#"  <object id="{id}" type="model" name="{}" pid="1" pindex="{default}">"#,
            escape(&object.name)
        );
        xml.push_str("   <mesh>\n    <vertices>\n");
        for v in &object.vertices {
            let _ = writeln!(
                xml,
                r#"     <vertex x="{}" y="{}" z="{}"/>"#,
                number(v.x),
                number(v.y),
                number(v.z)
            );
        }
        xml.push_str("    </vertices>\n    <triangles>\n");
        for ([a, b, c], m) in &object.triangles {
            let p = slot(*m);
            if p == default {
                let _ = writeln!(xml, r#"     <triangle v1="{a}" v2="{b}" v3="{c}"/>"#);
            } else {
                let _ = writeln!(
                    xml,
                    r#"     <triangle v1="{a}" v2="{b}" v3="{c}" pid="1" p1="{p}"/>"#
                );
            }
        }
        xml.push_str("    </triangles>\n   </mesh>\n  </object>\n");
    }
    xml.push_str(" </resources>\n <build>\n");
    for i in 0..objects.len() {
        let _ = writeln!(xml, r#"  <item objectid="{}"/>"#, i + 2);
    }
    xml.push_str(" </build>\n</model>\n");
    xml
}

/// Zips the package parts with fixed timestamps.
pub(crate) fn package(parts: &[(&str, &[u8])]) -> Result<Vec<u8>, IoError> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let err = |e: &dyn std::fmt::Display| IoError::Storage {
        key: "3mf".into(),
        reason: e.to_string(),
    };
    for (name, bytes) in parts {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(DateTime::default())
            .unix_permissions(0o644)
            .system(System::Unix);
        writer.start_file(*name, options).map_err(|e| err(&e))?;
        writer.write_all(bytes).map_err(|e| err(&e))?;
    }
    Ok(writer.finish().map_err(|e| err(&e))?.into_inner())
}

impl Exporter for ThreeMfExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::ThreeMf
    }

    fn export(
        &self,
        doc: &Document,
        scope: &ExportScope,
        opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<ExportReport, IoError> {
        let objects = objects(doc, scope, opts);
        progress.report(0.4, "checking solids");
        let mut warnings = Vec::new();
        let mut checked: BTreeMap<ContainerRef, bool> = BTreeMap::new();
        for (object, container) in &objects {
            let printable = *checked.entry(*container).or_insert_with(|| {
                doc.container(*container).is_some_and(|c| {
                    diagnose(c.mesh(), doc.tolerances()).is_ok_and(|d| d.is_printable())
                })
            });
            if !printable {
                warnings.push(Warning::new(
                    WarningCode::W2007,
                    format!(
                        "`{}` is not a closed solid; a slicer may refuse or misread it \
                         (run solid-check / solid-repair)",
                        object.name
                    ),
                ));
            }
        }
        progress.report(0.6, "writing the model");
        let (unit, _) = model_unit(opts.units);
        let model = model_xml(doc, &objects, unit);
        let rels = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n <Relationship Target=\"/{MODEL_PART}\" Id=\"rel0\" Type=\"{MODEL_RELATIONSHIP}\"/>\n</Relationships>\n"
        );
        let bytes = package(&[
            ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
            ("_rels/.rels", rels.as_bytes()),
            (MODEL_PART, model.as_bytes()),
        ])?;
        dst.write_all(&bytes).map_err(|e| IoError::Storage {
            key: "3mf".into(),
            reason: e.to_string(),
        })?;
        Ok(ExportReport {
            bytes_written: bytes.len() as u64,
            entities: objects.len(),
            warnings,
            side_files: Vec::new(),
        })
    }
}
