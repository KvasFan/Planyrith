//! Implements docs/09-file-formats.md §5 PLY import: polygon meshes in the
//! ASCII and both binary encodings, `vertex` (`x y z`, optional `red green
//! blue`) and `face` (`vertex_indices` or `vertex_index`, optional colors)
//! elements; other elements are skipped with `W2004`. Polygons are fanned
//! into triangles; colors become materials (docs/09 Decisions, PLY).

use std::collections::BTreeMap;
use std::io::Read;

use glam::DVec3;

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::diagnostics::{Warning, WarningCode};
use crate::document::Rgba8;
use crate::io::import::build::{ImportedMaterial, ImportedMesh, ImportedScene, build_scene};
use crate::io::import::{
    Confidence, ImportFormat, ImportOptions, ImportReport, Importer, source_stem,
};
use crate::io::{IoError, Progress};

/// The PLY importer.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlyImporter;

/// Most vertices or faces read (a guard against corrupt counts).
const MAX_COUNT: usize = 50_000_000;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "ply".into(),
        reason: reason.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Ascii,
    Little,
    Big,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scalar {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Scalar {
    fn parse(name: &str) -> Result<Scalar, IoError> {
        Ok(match name {
            "char" | "int8" => Scalar::I8,
            "uchar" | "uint8" => Scalar::U8,
            "short" | "int16" => Scalar::I16,
            "ushort" | "uint16" => Scalar::U16,
            "int" | "int32" => Scalar::I32,
            "uint" | "uint32" => Scalar::U32,
            "float" | "float32" => Scalar::F32,
            "double" | "float64" => Scalar::F64,
            other => return Err(corrupt(format!("unknown property type `{other}`"))),
        })
    }

    fn size(self) -> usize {
        match self {
            Scalar::I8 | Scalar::U8 => 1,
            Scalar::I16 | Scalar::U16 => 2,
            Scalar::I32 | Scalar::U32 | Scalar::F32 => 4,
            Scalar::F64 => 8,
        }
    }

    /// Color channels: integers are 0–255, floats 0–1.
    fn is_float(self) -> bool {
        matches!(self, Scalar::F32 | Scalar::F64)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Property {
    Scalar(String, Scalar),
    List(String, Scalar, Scalar),
}

#[derive(Debug, Clone, PartialEq)]
struct Element {
    name: String,
    count: usize,
    properties: Vec<Property>,
}

/// Values of one element instance: scalars by property name, and the
/// (first) list.
#[derive(Debug, Default)]
struct Row {
    values: BTreeMap<String, (f64, Scalar)>,
    list: Vec<f64>,
}

/// Reads values one at a time from the body.
struct Body<'a> {
    bytes: &'a [u8],
    at: usize,
    encoding: Encoding,
    /// ASCII tokens.
    tokens: std::str::SplitAsciiWhitespace<'a>,
}

impl Body<'_> {
    fn next(&mut self, kind: Scalar) -> Result<f64, IoError> {
        if self.encoding == Encoding::Ascii {
            let token = self
                .tokens
                .next()
                .ok_or_else(|| corrupt("the file ends early"))?;
            return token
                .parse::<f64>()
                .map_err(|_| corrupt(format!("`{token}` is not a number")));
        }
        let size = kind.size();
        let raw = self
            .bytes
            .get(self.at..self.at + size)
            .ok_or_else(|| corrupt("the file ends early"))?;
        self.at += size;
        let mut b = [0u8; 8];
        b[..size].copy_from_slice(raw);
        if self.encoding == Encoding::Big {
            b[..size].reverse();
        }
        Ok(match kind {
            Scalar::I8 => f64::from(b[0] as i8),
            Scalar::U8 => f64::from(b[0]),
            Scalar::I16 => f64::from(i16::from_le_bytes([b[0], b[1]])),
            Scalar::U16 => f64::from(u16::from_le_bytes([b[0], b[1]])),
            Scalar::I32 => f64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            Scalar::U32 => f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            Scalar::F32 => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            Scalar::F64 => f64::from_le_bytes(b),
        })
    }

    fn row(&mut self, element: &Element) -> Result<Row, IoError> {
        let mut row = Row::default();
        for p in &element.properties {
            match p {
                Property::Scalar(name, kind) => {
                    let v = self.next(*kind)?;
                    row.values.insert(name.clone(), (v, *kind));
                }
                Property::List(_, count, item) => {
                    let n = self.next(*count)?;
                    if !(0.0..=1_000_000.0).contains(&n) {
                        return Err(corrupt("a list is too long"));
                    }
                    let items = (0..n as usize)
                        .map(|_| self.next(*item))
                        .collect::<Result<Vec<_>, _>>()?;
                    if row.list.is_empty() {
                        row.list = items;
                    }
                }
            }
        }
        Ok(row)
    }
}

/// A color from a row's `red green blue`, if it has them.
fn color(row: &Row) -> Option<Rgba8> {
    let channel = |name: &str| {
        row.values.get(name).map(|&(v, kind)| {
            let v = if kind.is_float() { v * 255.0 } else { v };
            v.round().clamp(0.0, 255.0) as u8
        })
    };
    Some(Rgba8::rgb(
        channel("red")?,
        channel("green")?,
        channel("blue")?,
    ))
}

/// The header (elements) and where the body starts.
fn header(bytes: &[u8]) -> Result<(Encoding, Vec<Element>, usize), IoError> {
    const END: &[u8] = b"end_header";
    let end = bytes
        .windows(END.len())
        .position(|w| w == END)
        .ok_or_else(|| corrupt("no end_header"))?;
    let mut body = end + END.len();
    // The line break after end_header (\n or \r\n).
    if bytes.get(body) == Some(&b'\r') {
        body += 1;
    }
    if bytes.get(body) == Some(&b'\n') {
        body += 1;
    }
    let text = std::str::from_utf8(&bytes[..end]).map_err(|_| corrupt("the header is not text"))?;
    let mut lines = text.lines().map(str::trim);
    if lines.next() != Some("ply") {
        return Err(corrupt("not a PLY file"));
    }
    let mut encoding = None;
    let mut elements: Vec<Element> = Vec::new();
    for line in lines {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["format", kind, _version] => {
                encoding = Some(match *kind {
                    "ascii" => Encoding::Ascii,
                    "binary_little_endian" => Encoding::Little,
                    "binary_big_endian" => Encoding::Big,
                    other => return Err(corrupt(format!("unknown format `{other}`"))),
                });
            }
            ["element", name, count] => elements.push(Element {
                name: (*name).to_string(),
                count: count
                    .parse()
                    .ok()
                    .filter(|&c: &usize| c <= MAX_COUNT)
                    .ok_or_else(|| corrupt(format!("bad count `{count}`")))?,
                properties: Vec::new(),
            }),
            ["property", "list", count, item, name] => elements
                .last_mut()
                .ok_or_else(|| corrupt("a property before any element"))?
                .properties
                .push(Property::List(
                    (*name).to_string(),
                    Scalar::parse(count)?,
                    Scalar::parse(item)?,
                )),
            ["property", kind, name] => elements
                .last_mut()
                .ok_or_else(|| corrupt("a property before any element"))?
                .properties
                .push(Property::Scalar((*name).to_string(), Scalar::parse(kind)?)),
            [] | ["comment", ..] | ["obj_info", ..] => {}
            _ => return Err(corrupt(format!("unreadable header line `{line}`"))),
        }
    }
    let encoding = encoding.ok_or_else(|| corrupt("no format line"))?;
    Ok((encoding, elements, body))
}

/// The mesh, its materials and warnings.
fn read(bytes: &[u8], name: &str) -> Result<(ImportedScene, Vec<Warning>), IoError> {
    let (encoding, elements, start) = header(bytes)?;
    let body_text = if encoding == Encoding::Ascii {
        std::str::from_utf8(&bytes[start..]).map_err(|_| corrupt("the body is not text"))?
    } else {
        ""
    };
    let mut body = Body {
        bytes,
        at: start,
        encoding,
        tokens: body_text.split_ascii_whitespace(),
    };
    let mut positions = Vec::new();
    let mut vertex_colors: Vec<Option<Rgba8>> = Vec::new();
    let mut polygons: Vec<(Vec<u32>, Option<Rgba8>)> = Vec::new();
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    for element in &elements {
        for _ in 0..element.count {
            let row = body.row(element)?;
            match element.name.as_str() {
                "vertex" => {
                    let get = |k: &str| row.values.get(k).map(|v| v.0);
                    let (Some(x), Some(y), Some(z)) = (get("x"), get("y"), get("z")) else {
                        return Err(corrupt("a vertex lacks x, y or z"));
                    };
                    let p = DVec3::new(x, y, z);
                    if !p.is_finite() {
                        return Err(corrupt("a vertex is not finite"));
                    }
                    positions.push(p);
                    vertex_colors.push(color(&row));
                }
                "face" => {
                    let indices: Vec<u32> = row
                        .list
                        .iter()
                        .map(|&i| {
                            if i >= 0.0 && (i as usize) < positions.len() {
                                Ok(i as u32)
                            } else {
                                Err(corrupt("a face names a missing vertex"))
                            }
                        })
                        .collect::<Result<_, _>>()?;
                    polygons.push((indices, color(&row)));
                }
                other => *skipped.entry(other.to_string()).or_default() += 1,
            }
        }
    }
    // Colors → materials, one per distinct color.
    let mut materials: Vec<ImportedMaterial> = Vec::new();
    let mut by_color: BTreeMap<[u8; 3], usize> = BTreeMap::new();
    let mut mesh = ImportedMesh {
        name: name.to_string(),
        positions,
        ..ImportedMesh::default()
    };
    for (indices, face_color) in polygons {
        if indices.len() < 3 {
            continue;
        }
        let tint = face_color.or_else(|| vertex_colors.get(indices[0] as usize).copied().flatten());
        let material = tint.map(|c| {
            *by_color.entry([c.r, c.g, c.b]).or_insert_with(|| {
                materials.push(ImportedMaterial {
                    name: format!("Color {:02X}{:02X}{:02X}", c.r, c.g, c.b),
                    color: c,
                    opacity: 1.0,
                    ..ImportedMaterial::default()
                });
                materials.len() - 1
            })
        });
        for k in 1..indices.len() - 1 {
            mesh.triangles
                .push([indices[0], indices[k], indices[k + 1]]);
            mesh.materials.push(material);
        }
    }
    let warnings = skipped
        .iter()
        .map(|(what, n)| Warning::new(WarningCode::W2004, format!("{n} × {what} not read")))
        .collect();
    Ok((
        ImportedScene {
            name: name.to_string(),
            materials,
            meshes: vec![mesh],
            ..ImportedScene::default()
        },
        warnings,
    ))
}

impl Importer for PlyImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Ply
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        if bytes.starts_with(b"ply\n") || bytes.starts_with(b"ply\r\n") {
            Confidence::CERTAIN
        } else if name.to_ascii_lowercase().ends_with(".ply") {
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
        progress.report(0.1, "reading the mesh");
        let (scene, warnings) = read(&bytes, &source_stem(&opts.source, "PLY"))?;
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

    const SQUARE: &str = "ply\nformat ascii 1.0\ncomment made by hand\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nelement face 1\nproperty list uchar int vertex_indices\nelement camera 1\nproperty float view_px\nend_header\n0 0 0 255 0 0\n10 0 0 255 0 0\n10 10 0 255 0 0\n0 10 0 255 0 0\n4 0 1 2 3\n1.5\n";

    #[test]
    fn ascii_quads_fan_into_colored_triangles() {
        let (scene, warnings) = read(SQUARE.as_bytes(), "sq").unwrap();
        let mesh = &scene.meshes[0];
        assert_eq!(mesh.triangles, vec![[0, 1, 2], [0, 2, 3]]);
        assert_eq!(mesh.materials, vec![Some(0), Some(0)]);
        assert_eq!(scene.materials[0].color, Rgba8::rgb(255, 0, 0));
        assert_eq!(warnings.len(), 1, "the camera element");
    }

    #[test]
    fn both_binary_encodings_read_the_same_triangle() {
        for big in [false, true] {
            let mut bytes = format!(
                "ply\nformat {} 1.0\nelement vertex 3\nproperty double x\nproperty double y\nproperty double z\nelement face 1\nproperty list uchar uint vertex_index\nproperty float red\nproperty float green\nproperty float blue\nend_header\n",
                if big { "binary_big_endian" } else { "binary_little_endian" }
            )
            .into_bytes();
            let f64s = |v: f64| {
                if big {
                    v.to_be_bytes()
                } else {
                    v.to_le_bytes()
                }
            };
            for v in [0.0, 0.0, 0.0, 5.0, 0.0, 0.0, 0.0, 5.0, 1.0] {
                bytes.extend(f64s(v));
            }
            bytes.push(3);
            for i in [0u32, 1, 2] {
                bytes.extend(if big {
                    i.to_be_bytes()
                } else {
                    i.to_le_bytes()
                });
            }
            for c in [0.0f32, 0.5, 1.0] {
                bytes.extend(if big {
                    c.to_be_bytes()
                } else {
                    c.to_le_bytes()
                });
            }
            let (scene, _) = read(&bytes, "t").unwrap();
            assert_eq!(scene.meshes[0].positions[2], DVec3::new(0.0, 5.0, 1.0));
            assert_eq!(scene.meshes[0].triangles, vec![[0, 1, 2]]);
            assert_eq!(scene.materials[0].color, Rgba8::rgb(0, 128, 255));
        }
    }

    #[test]
    fn broken_files_are_corrupt() {
        for text in [
            "ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nend_header\n1\n",
            "ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nend_header\n1 2\n",
            "ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0\n3 0 1 2\n",
            "ply\nformat sideways 1.0\nend_header\n",
            "not ply",
        ] {
            assert!(read(text.as_bytes(), "x").is_err(), "{text}");
        }
    }
}
