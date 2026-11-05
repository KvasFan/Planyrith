//! Implements docs/09-file-formats.md §5 3MF import: the package's model
//! part (found through its root relationship), mesh objects with their base
//! materials or color groups, objects built from components, and the build
//! items, placed through the shared scene builder (`build::build_scene`).
//! Parts in other model files, textures and extensions beyond colors are
//! skipped with `W2004` (docs/09 Decisions, 3MF).

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use glam::{DAffine3, DMat3, DVec3};
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use zip::ZipArchive;

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::diagnostics::{Warning, WarningCode};
use crate::document::Rgba8;
use crate::io::export::three_mf::{MODEL_PART, MODEL_RELATIONSHIP};
use crate::io::import::build::{
    ImportedMaterial, ImportedMesh, ImportedNode, ImportedScene, build_scene,
};
use crate::io::import::{
    Confidence, ImportFormat, ImportOptions, ImportReport, Importer, source_stem,
};
use crate::io::{IoError, Progress};
use crate::units::LengthUnit;

/// The 3MF importer.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThreeMfImporter;

/// Largest model part read (bytes): a guard against packed bombs.
const MAX_PART: u64 = 2 << 30;

/// Deepest chain of components followed.
const MAX_DEPTH: usize = 32;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "3mf".into(),
        reason: reason.into(),
    }
}

/// What an object holds.
#[derive(Debug, Clone, PartialEq)]
enum Content {
    /// Vertices, triangles and each triangle's property (group, index).
    Mesh {
        positions: Vec<DVec3>,
        triangles: Vec<[u32; 3]>,
        properties: Vec<Option<(u32, u32)>>,
    },
    /// Other objects placed by transforms.
    Components(Vec<(u32, DAffine3)>),
}

#[derive(Debug, Clone, PartialEq)]
struct Object {
    name: String,
    content: Content,
}

/// The model part as read.
#[derive(Debug, Default, Clone, PartialEq)]
struct Model {
    /// Millimeters per file unit.
    unit: Option<LengthUnit>,
    /// Extra factor for units the document has no name for (microns).
    factor: f64,
    materials: Vec<ImportedMaterial>,
    /// Property groups: group id → scene material per index.
    groups: BTreeMap<u32, Vec<usize>>,
    objects: BTreeMap<u32, Object>,
    items: Vec<(u32, DAffine3)>,
    /// Elements not read (`W2004`), by name.
    skipped: BTreeMap<String, usize>,
}

fn attributes(e: &BytesStart<'_>) -> Result<BTreeMap<String, String>, IoError> {
    let mut out = BTreeMap::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|err| corrupt(err.to_string()))?;
        let key = String::from_utf8_lossy(attr.key.local_name().as_ref()).into_owned();
        let value = attr
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|err| corrupt(err.to_string()))?;
        out.insert(key, value.into_owned());
    }
    Ok(out)
}

fn number<T: std::str::FromStr>(
    attrs: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<T>, IoError> {
    attrs
        .get(key)
        .map(|v| {
            v.trim()
                .parse()
                .map_err(|_| corrupt(format!("`{key}` is not a number: `{v}`")))
        })
        .transpose()
}

fn required<T: std::str::FromStr>(
    attrs: &BTreeMap<String, String>,
    key: &str,
) -> Result<T, IoError> {
    number(attrs, key)?.ok_or_else(|| corrupt(format!("missing `{key}`")))
}

/// `#RRGGBB` or `#RRGGBBAA`.
fn color(text: &str) -> Option<(Rgba8, f32)> {
    let hex = text.trim().strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    let (r, g, b) = (byte(0)?, byte(2)?, byte(4)?);
    let a = match hex.len() {
        6 => 255,
        8 => byte(6)?,
        _ => return None,
    };
    Some((Rgba8::rgb(r, g, b), f32::from(a) / 255.0))
}

/// `m00 m01 m02 m10 m11 m12 m20 m21 m22 m30 m31 m32`: rows of a 4×3
/// matrix applied to row vectors, so its first three rows are the columns
/// of the usual matrix and the last row the translation.
fn transform(text: Option<&String>) -> Result<DAffine3, IoError> {
    let Some(text) = text else {
        return Ok(DAffine3::IDENTITY);
    };
    let m: Vec<f64> = text
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| corrupt(format!("bad transform `{text}`")))?;
    if m.len() != 12 || m.iter().any(|v| !v.is_finite()) {
        return Err(corrupt(format!("bad transform `{text}`")));
    }
    Ok(DAffine3 {
        matrix3: DMat3::from_cols(
            DVec3::new(m[0], m[1], m[2]),
            DVec3::new(m[3], m[4], m[5]),
            DVec3::new(m[6], m[7], m[8]),
        ),
        translation: DVec3::new(m[9], m[10], m[11]),
    })
}

/// An object while its element is open: id, name, default property
/// (group, index), content.
type OpenObject = (u32, String, Option<(u32, u32)>, Content);

/// Reads the model part.
fn parse(xml: &str) -> Result<Model, IoError> {
    let mut reader = Reader::from_str(xml);
    let mut model = Model {
        factor: 1.0,
        ..Model::default()
    };
    // The object being read: id, name, default property, content.
    let mut object: Option<OpenObject> = None;
    // The property group being read.
    let mut group: Option<u32> = None;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| corrupt(format!("model XML: {e}")))?;
        let (e, empty) = match &event {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(e) => {
                match e.local_name().as_ref() {
                    b"object" => {
                        if let Some((id, name, _, content)) = object.take() {
                            model.objects.insert(id, Object { name, content });
                        }
                    }
                    b"basematerials" | b"colorgroup" => group = None,
                    _ => {}
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let name = e.local_name();
        let attrs = attributes(e)?;
        match name.as_ref() {
            b"model" => {
                let (unit, factor) = match attrs.get("unit").map(String::as_str) {
                    None | Some("millimeter") => (LengthUnit::Millimeter, 1.0),
                    Some("micron") => (LengthUnit::Millimeter, 0.001),
                    Some("centimeter") => (LengthUnit::Centimeter, 1.0),
                    Some("meter") => (LengthUnit::Meter, 1.0),
                    Some("inch") => (LengthUnit::Inch, 1.0),
                    Some("foot") => (LengthUnit::Foot, 1.0),
                    Some(other) => return Err(corrupt(format!("unknown unit `{other}`"))),
                };
                model.unit = Some(unit);
                model.factor = factor;
            }
            b"basematerials" | b"colorgroup" if !empty => {
                let id: u32 = required(&attrs, "id")?;
                model.groups.entry(id).or_default();
                group = Some(id);
            }
            b"base" | b"color" => {
                let Some(id) = group else { continue };
                let key = if name.as_ref() == b"base" {
                    "displaycolor"
                } else {
                    "color"
                };
                let (rgb, opacity) = attrs
                    .get(key)
                    .and_then(|c| color(c))
                    .unwrap_or((Rgba8::rgb(255, 255, 255), 1.0));
                let entries = model.groups.entry(id).or_default();
                let label = attrs
                    .get("name")
                    .cloned()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| format!("Color {id}.{}", entries.len()));
                entries.push(model.materials.len());
                model.materials.push(ImportedMaterial {
                    name: label,
                    color: rgb,
                    opacity,
                    texture: None,
                });
            }
            b"object" if !empty => {
                let id: u32 = required(&attrs, "id")?;
                let label = attrs
                    .get("name")
                    .cloned()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| format!("Object {id}"));
                let default = match (number(&attrs, "pid")?, number(&attrs, "pindex")?) {
                    (Some(pid), index) => Some((pid, index.unwrap_or(0))),
                    _ => None,
                };
                object = Some((id, label, default, Content::Components(Vec::new())));
            }
            b"mesh" => {
                if let Some((.., content)) = &mut object {
                    *content = Content::Mesh {
                        positions: Vec::new(),
                        triangles: Vec::new(),
                        properties: Vec::new(),
                    };
                }
            }
            b"vertex" => {
                if let Some((.., Content::Mesh { positions, .. })) = &mut object {
                    let p = DVec3::new(
                        required(&attrs, "x")?,
                        required(&attrs, "y")?,
                        required(&attrs, "z")?,
                    );
                    if !p.is_finite() {
                        return Err(corrupt("a vertex is not finite"));
                    }
                    positions.push(p * model.factor);
                }
            }
            b"triangle" => {
                if let Some((
                    _,
                    _,
                    default,
                    Content::Mesh {
                        positions,
                        triangles,
                        properties,
                    },
                )) = &mut object
                {
                    let t: [u32; 3] = [
                        required(&attrs, "v1")?,
                        required(&attrs, "v2")?,
                        required(&attrs, "v3")?,
                    ];
                    if t.iter().any(|&i| i as usize >= positions.len()) {
                        return Err(corrupt("a triangle names a missing vertex"));
                    }
                    let pid = number::<u32>(&attrs, "pid")?.or(default.map(|d| d.0));
                    let index = number::<u32>(&attrs, "p1")?.or(default.map(|d| d.1));
                    triangles.push(t);
                    properties.push(pid.zip(index.or(Some(0))));
                }
            }
            b"component" => {
                if attrs.contains_key("path") {
                    *model
                        .skipped
                        .entry("components in other model parts".into())
                        .or_default() += 1;
                    continue;
                }
                if let Some((.., Content::Components(list))) = &mut object {
                    list.push((
                        required(&attrs, "objectid")?,
                        transform(attrs.get("transform"))?,
                    ));
                }
            }
            b"item" => {
                if attrs.contains_key("path") {
                    *model
                        .skipped
                        .entry("build items in other model parts".into())
                        .or_default() += 1;
                    continue;
                }
                model.items.push((
                    required(&attrs, "objectid")?,
                    transform(attrs.get("transform"))?,
                ));
            }
            b"texture2dgroup"
            | b"compositematerials"
            | b"multiproperties"
            | b"beamlattice"
            | b"slicestack"
            | b"displacementmesh" => {
                *model
                    .skipped
                    .entry(String::from_utf8_lossy(name.as_ref()).into_owned())
                    .or_default() += 1;
            }
            _ => {}
        }
    }
    Ok(model)
}

/// The scene of `model`: one mesh per mesh object, one node per placed
/// mesh object, components expanded into their world transforms.
fn scene(model: Model, name: String) -> (ImportedScene, Vec<Warning>) {
    let mut warnings = Vec::new();
    let mut meshes = Vec::new();
    let mut mesh_of: BTreeMap<u32, usize> = BTreeMap::new();
    let mut names: BTreeMap<u32, String> = BTreeMap::new();
    let mut parts: BTreeMap<u32, Vec<(u32, DAffine3)>> = BTreeMap::new();
    for (id, object) in model.objects {
        names.insert(id, object.name.clone());
        match object.content {
            Content::Mesh {
                positions,
                triangles,
                properties,
            } => {
                let materials = properties
                    .iter()
                    .map(|p| {
                        p.and_then(|(group, index)| {
                            model.groups.get(&group)?.get(index as usize).copied()
                        })
                    })
                    .collect();
                mesh_of.insert(id, meshes.len());
                meshes.push(ImportedMesh {
                    name: object.name,
                    positions,
                    triangles,
                    materials,
                    uvs: Vec::new(),
                    normals: Vec::new(),
                });
            }
            Content::Components(list) => {
                parts.insert(id, list);
            }
        }
    }
    let mut nodes = Vec::new();
    let mut missing = 0usize;
    // Depth-first, so a component's meshes follow each other.
    let mut stack: Vec<(u32, DAffine3, usize)> = model
        .items
        .iter()
        .rev()
        .map(|&(id, t)| (id, t, 0))
        .collect();
    while let Some((id, t, depth)) = stack.pop() {
        if let Some(&mesh) = mesh_of.get(&id) {
            nodes.push(ImportedNode {
                name: names.get(&id).cloned().unwrap_or_default(),
                transform: t,
                mesh,
            });
        } else if let Some(list) = parts.get(&id) {
            if depth >= MAX_DEPTH {
                missing += 1;
                continue;
            }
            for &(child, ct) in list.iter().rev() {
                stack.push((child, t * ct, depth + 1));
            }
        } else {
            missing += 1;
        }
    }
    if missing > 0 {
        warnings.push(Warning::new(
            WarningCode::W2004,
            format!("{missing} placements name objects the file does not hold, or nest too deep"),
        ));
    }
    for (what, count) in &model.skipped {
        warnings.push(Warning::new(
            WarningCode::W2004,
            format!("{count} × {what} not read"),
        ));
    }
    let scene = ImportedScene {
        name,
        materials: model.materials,
        meshes,
        nodes,
        y_up: false,
        units: model.unit,
        cameras: Vec::new(),
    };
    (scene, warnings)
}

/// The model part's name from the package's root relationships.
fn model_part(archive: &mut ZipArchive<Cursor<&[u8]>>) -> String {
    let rels = archive.by_name("_rels/.rels").ok().and_then(|mut f| {
        let mut text = String::new();
        f.read_to_string(&mut text).ok().map(|_| text)
    });
    let found = rels.and_then(|text| {
        let mut reader = Reader::from_str(&text);
        loop {
            match reader.read_event() {
                Ok(Event::Empty(e) | Event::Start(e))
                    if e.local_name().as_ref() == b"Relationship" =>
                {
                    let attrs = attributes(&e).ok()?;
                    if attrs.get("Type").map(String::as_str) == Some(MODEL_RELATIONSHIP) {
                        return attrs
                            .get("Target")
                            .map(|t| t.trim_start_matches('/').to_string());
                    }
                }
                Ok(Event::Eof) | Err(_) => return None,
                _ => {}
            }
        }
    });
    found.unwrap_or_else(|| MODEL_PART.to_string())
}

/// Reads a 3MF package into a scene (exposed for tests).
fn read(bytes: &[u8], name: String) -> Result<(ImportedScene, Vec<Warning>), IoError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| corrupt(format!("not a 3MF package: {e}")))?;
    let part = model_part(&mut archive);
    let mut xml = String::new();
    archive
        .by_name(&part)
        .map_err(|_| corrupt(format!("the package has no model part `{part}`")))?
        .take(MAX_PART)
        .read_to_string(&mut xml)
        .map_err(|e| corrupt(format!("{part}: {e}")))?;
    Ok(scene(parse(&xml)?, name))
}

impl Importer for ThreeMfImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::ThreeMf
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        let named = name.to_ascii_lowercase().ends_with(".3mf");
        if bytes.starts_with(b"PK\x03\x04") && named {
            Confidence::CERTAIN
        } else if named {
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
        let mut bytes = Vec::new();
        src.read_to_end(&mut bytes)
            .map_err(|e| corrupt(e.to_string()))?;
        progress.report(0.1, "reading the model");
        let (scene, warnings) = read(&bytes, source_stem(&opts.source, "3MF"))?;
        progress.report(0.5, "building faces");
        let mut report = build_scene(tx, target, scene, opts)?;
        report.skipped += warnings.len();
        report.warnings.extend(warnings);
        Ok(report)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::io::export::three_mf::package;

    const CUBE: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<model unit="centimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"
  xmlns:m="http://schemas.microsoft.com/3dmanufacturing/material/2015/02">
 <resources>
  <basematerials id="1"><base name="Red &amp; shiny" displaycolor="#FF000080"/><base name="" displaycolor="#00FF00"/></basematerials>
  <m:colorgroup id="5"><m:color color="#0000FFFF"/></m:colorgroup>
  <object id="2" name="Tetra" pid="1" pindex="1"><mesh>
   <vertices>
    <vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/>
    <vertex x="0" y="1" z="0"/><vertex x="0" y="0" z="1"/>
   </vertices>
   <triangles>
    <triangle v1="0" v2="2" v3="1"/><triangle v1="0" v2="1" v3="3" p1="0"/>
    <triangle v1="0" v2="3" v3="2" pid="5" p1="0"/><triangle v1="1" v2="2" v3="3"/>
   </triangles>
  </mesh></object>
  <object id="3" name="Pair"><components>
   <component objectid="2"/>
   <component objectid="2" transform="1 0 0 0 1 0 0 0 1 5 0 0"/>
  </components></object>
 </resources>
 <build><item objectid="3" transform="1 0 0 0 1 0 0 0 1 0 0 2"/><item objectid="9"/></build>
</model>"##;

    #[test]
    fn components_expand_into_placed_meshes_with_their_colors() {
        let model = parse(CUBE).unwrap();
        assert_eq!(model.unit, Some(LengthUnit::Centimeter));
        assert_eq!(model.materials.len(), 3);
        assert_eq!(model.materials[0].name, "Red & shiny");
        assert!((model.materials[0].opacity - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(model.materials[1].name, "Color 1.1");
        let (scene, warnings) = scene(model, "cube".into());
        assert_eq!(scene.meshes.len(), 1);
        assert_eq!(
            scene.meshes[0].materials,
            vec![Some(1), Some(0), Some(2), Some(1)],
            "object default, own index, other group, default"
        );
        assert_eq!(scene.nodes.len(), 2);
        assert_eq!(
            scene.nodes[0].transform.translation,
            DVec3::new(0.0, 0.0, 2.0)
        );
        assert_eq!(
            scene.nodes[1].transform.translation,
            DVec3::new(5.0, 0.0, 2.0)
        );
        assert_eq!(warnings.len(), 1, "the item naming object 9");
    }

    #[test]
    fn the_model_part_is_found_through_the_relationships() {
        let rels = format!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/3D/other.model" Id="r" Type="{MODEL_RELATIONSHIP}"/></Relationships>"#
        );
        let bytes = package(&[
            ("_rels/.rels", rels.as_bytes()),
            ("3D/other.model", CUBE.as_bytes()),
        ])
        .unwrap();
        let (scene, _) = read(&bytes, "x".into()).unwrap();
        assert_eq!(scene.nodes.len(), 2);
        assert!(read(b"not a zip", "x".into()).is_err());
        let empty = package(&[("_rels/.rels", rels.as_bytes())]).unwrap();
        assert!(read(&empty, "x".into()).is_err());
    }

    #[test]
    fn bad_input_is_corrupt_not_a_panic() {
        for xml in [
            r#"<model unit="parsec"/>"#,
            r#"<model><resources><object id="1"><mesh><vertices><vertex x="a" y="0" z="0"/></vertices></mesh></object></resources></model>"#,
            r#"<model><resources><object id="1"><mesh><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object></resources></model>"#,
            r#"<model><build><item objectid="1" transform="1 2 3"/></build></model>"#,
            r#"<model><resources><object id="1"></resources>"#,
        ] {
            assert!(parse(xml).is_err(), "{xml}");
        }
        assert_eq!(color("#0a0B0c"), Some((Rgba8::rgb(10, 11, 12), 1.0)));
        assert_eq!(color("red"), None);
    }
}
