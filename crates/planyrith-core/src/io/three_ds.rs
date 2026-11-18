//! Implements docs/09-file-formats.md §5 3DS (milestone 1.49): the chunked
//! binary mesh format of older modelers and renderers.
//!
//! Import reads every triangle object of the editor chunk — vertices (in
//! world coordinates), faces, face material groups, texture coordinates and
//! smoothing groups (faces sharing a group meet along soft edges) — and the
//! materials' diffuse color, transparency and diffuse texture (a file beside
//! the model). The keyframer's animation and hierarchy are not read; the
//! vertices are already placed.
//!
//! Export writes one object per occurrence and material set (split at 65 535
//! vertices or faces, the format's limit), each face's corners with their
//! texture coordinates, face material groups, and materials with their
//! color, transparency and texture (copied beside the model as
//! `<name>_textures/…`), Z up in `--units`.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use glam::{DVec2, DVec3};

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::diagnostics::{Warning, WarningCode};
use crate::document::{ContainerRef, Document, FrontOrBack, Rgba8};
use crate::ids::MaterialId;
use crate::io::export::scene::collect;
use crate::io::export::{
    ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter, output_transform,
};
use crate::io::import::build::{ImportedMaterial, ImportedMesh, ImportedScene, build_scene};
use crate::io::import::{
    Confidence, ImportFormat, ImportOptions, ImportReport, Importer, sibling, source_stem,
};
use crate::io::{IoError, Progress, StorageKey};

// Chunk identifiers.
const MAIN: u16 = 0x4D4D;
const VERSION: u16 = 0x0002;
const EDITOR: u16 = 0x3D3D;
const MESH_VERSION: u16 = 0x3D3E;
const MASTER_SCALE: u16 = 0x0100;
const OBJECT: u16 = 0x4000;
const TRIANGLES: u16 = 0x4100;
const VERTICES: u16 = 0x4110;
const FACES: u16 = 0x4120;
const FACE_MATERIAL: u16 = 0x4130;
const MAPPING: u16 = 0x4140;
const SMOOTHING: u16 = 0x4150;
const LOCAL_FRAME: u16 = 0x4160;
const MATERIAL: u16 = 0xAFFF;
const MATERIAL_NAME: u16 = 0xA000;
const AMBIENT: u16 = 0xA010;
const DIFFUSE: u16 = 0xA020;
const SPECULAR: u16 = 0xA030;
const TRANSPARENCY: u16 = 0xA050;
const TEXTURE_MAP: u16 = 0xA200;
const MAP_FILE: u16 = 0xA300;
const COLOR_F: u16 = 0x0010;
const COLOR_24: u16 = 0x0011;
const LIN_COLOR_24: u16 = 0x0012;
const LIN_COLOR_F: u16 = 0x0013;
const PERCENT_I: u16 = 0x0030;
const PERCENT_F: u16 = 0x0031;

/// The most vertices or faces one object may hold (a 16-bit count).
const MAX_PER_OBJECT: usize = 65_535;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "3ds".into(),
        reason: reason.into(),
    }
}

// ----- reading --------------------------------------------------------------------------

/// A little-endian reader over one chunk's data.
struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Cursor { data, at: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], IoError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|e| *e <= self.data.len())
            .ok_or_else(|| corrupt("a chunk ends early"))?;
        let s = &self.data[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, IoError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, IoError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, IoError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn f32(&mut self) -> Result<f32, IoError> {
        Ok(f32::from_bits(self.u32()?))
    }
    /// A zero-terminated string (Latin-1).
    fn cstring(&mut self) -> Result<String, IoError> {
        let rest = &self.data[self.at..];
        let end = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| corrupt("a name is not terminated"))?;
        let s: String = rest[..end].iter().map(|b| char::from(*b)).collect();
        self.at += end + 1;
        Ok(s)
    }
    fn rest(&self) -> &'a [u8] {
        &self.data[self.at..]
    }
}

/// The sub-chunks of `data`: `(id, body)`.
fn chunks(data: &[u8]) -> Result<Vec<(u16, &[u8])>, IoError> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 6 <= data.len() {
        let id = u16::from_le_bytes([data[at], data[at + 1]]);
        let len =
            u32::from_le_bytes([data[at + 2], data[at + 3], data[at + 4], data[at + 5]]) as usize;
        if len < 6 || at + len > data.len() {
            return Err(corrupt(format!("chunk {id:#06x} overruns its parent")));
        }
        out.push((id, &data[at + 6..at + len]));
        at += len;
    }
    Ok(out)
}

/// A color sub-chunk (the first one: 24-bit or float, gamma or linear).
fn color(data: &[u8]) -> Result<Option<[u8; 3]>, IoError> {
    for (id, body) in chunks(data)? {
        let mut c = Cursor::new(body);
        match id {
            COLOR_24 | LIN_COLOR_24 => return Ok(Some([c.u8()?, c.u8()?, c.u8()?])),
            COLOR_F | LIN_COLOR_F => {
                let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                return Ok(Some([
                    channel(c.f32()?),
                    channel(c.f32()?),
                    channel(c.f32()?),
                ]));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// A percentage sub-chunk, 0–100.
fn percent(data: &[u8]) -> Result<Option<f32>, IoError> {
    for (id, body) in chunks(data)? {
        let mut c = Cursor::new(body);
        match id {
            PERCENT_I => return Ok(Some(f32::from(c.u16()?))),
            PERCENT_F => return Ok(Some(c.f32()?)),
            _ => {}
        }
    }
    Ok(None)
}

/// A material read from the file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReadMaterial {
    /// Its name.
    pub name: String,
    /// Diffuse color (else ambient).
    pub color: Option<[u8; 3]>,
    /// Transparency, 0–100.
    pub transparency: f32,
    /// The diffuse texture's file name.
    pub texture: Option<String>,
}

/// A triangle object read from the file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReadObject {
    /// Its name.
    pub name: String,
    /// Vertices.
    pub vertices: Vec<DVec3>,
    /// Faces.
    pub faces: Vec<[u16; 3]>,
    /// Texture coordinates per vertex (empty when absent).
    pub uvs: Vec<DVec2>,
    /// Smoothing groups per face (empty when absent).
    pub smoothing: Vec<u32>,
    /// Material groups: name and faces.
    pub groups: Vec<(String, Vec<u16>)>,
}

/// What a 3DS file holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReadModel {
    /// Materials.
    pub materials: Vec<ReadMaterial>,
    /// Triangle objects.
    pub objects: Vec<ReadObject>,
    /// The master scale, file units per 3DS unit.
    pub scale: f32,
}

fn read_material(body: &[u8]) -> Result<ReadMaterial, IoError> {
    let mut m = ReadMaterial::default();
    let mut ambient = None;
    for (id, data) in chunks(body)? {
        match id {
            MATERIAL_NAME => m.name = Cursor::new(data).cstring()?,
            AMBIENT => ambient = color(data)?,
            DIFFUSE => m.color = color(data)?,
            TRANSPARENCY => m.transparency = percent(data)?.unwrap_or(0.0),
            TEXTURE_MAP => {
                for (sub, d) in chunks(data)? {
                    if sub == MAP_FILE {
                        m.texture = Some(Cursor::new(d).cstring()?);
                    }
                }
            }
            _ => {}
        }
    }
    if m.color.is_none() {
        m.color = ambient;
    }
    Ok(m)
}

fn read_triangles(name: String, body: &[u8]) -> Result<ReadObject, IoError> {
    let mut o = ReadObject {
        name,
        ..Default::default()
    };
    for (id, data) in chunks(body)? {
        let mut c = Cursor::new(data);
        match id {
            VERTICES => {
                let n = usize::from(c.u16()?);
                for _ in 0..n {
                    let (x, y, z) = (c.f32()?, c.f32()?, c.f32()?);
                    o.vertices
                        .push(DVec3::new(f64::from(x), f64::from(y), f64::from(z)));
                }
            }
            FACES => {
                let n = usize::from(c.u16()?);
                for _ in 0..n {
                    let f = [c.u16()?, c.u16()?, c.u16()?];
                    c.u16()?; // edge visibility flags
                    o.faces.push(f);
                }
                // Material groups and smoothing follow inside the faces.
                for (sub, d) in chunks(c.rest())? {
                    let mut s = Cursor::new(d);
                    match sub {
                        FACE_MATERIAL => {
                            let name = s.cstring()?;
                            let k = usize::from(s.u16()?);
                            let faces = (0..k).map(|_| s.u16()).collect::<Result<Vec<_>, _>>()?;
                            o.groups.push((name, faces));
                        }
                        SMOOTHING => {
                            o.smoothing = (0..n).map(|_| s.u32()).collect::<Result<Vec<_>, _>>()?;
                        }
                        _ => {}
                    }
                }
            }
            MAPPING => {
                let n = usize::from(c.u16()?);
                for _ in 0..n {
                    let (u, v) = (c.f32()?, c.f32()?);
                    o.uvs.push(DVec2::new(f64::from(u), f64::from(v)));
                }
            }
            _ => {}
        }
    }
    Ok(o)
}

/// Reads a 3DS file's materials and triangle objects.
pub fn read(bytes: &[u8]) -> Result<ReadModel, IoError> {
    let top = chunks(bytes)?;
    let Some((MAIN, main)) = top.first().copied() else {
        return Err(IoError::UnsupportedFormat("not a 3DS file".into()));
    };
    let mut model = ReadModel {
        scale: 1.0,
        ..Default::default()
    };
    for (id, body) in chunks(main)? {
        if id != EDITOR {
            continue;
        }
        for (sub, data) in chunks(body)? {
            match sub {
                MATERIAL => model.materials.push(read_material(data)?),
                MASTER_SCALE => model.scale = Cursor::new(data).f32()?,
                OBJECT => {
                    let mut c = Cursor::new(data);
                    let name = c.cstring()?;
                    for (kind, d) in chunks(c.rest())? {
                        if kind == TRIANGLES {
                            model.objects.push(read_triangles(name.clone(), d)?);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(model)
}

/// The scene of a 3DS model: one mesh per object, materials by name, soft
/// edges between faces sharing a smoothing group.
pub fn scene_of(model: &ReadModel, name: &str) -> (ImportedScene, usize) {
    let mut scene = ImportedScene {
        name: name.to_string(),
        ..Default::default()
    };
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for m in &model.materials {
        let [r, g, b] = m.color.unwrap_or([204, 204, 204]);
        index.insert(m.name.clone(), scene.materials.len());
        scene.materials.push(ImportedMaterial {
            name: m.name.clone(),
            color: Rgba8::rgb(r, g, b),
            opacity: (1.0 - m.transparency / 100.0).clamp(0.0, 1.0),
            texture: None,
            roughness: None,
            metallic: None,
        });
    }
    let mut skipped = 0;
    for o in &model.objects {
        let n = o.vertices.len();
        let mut face_material = vec![None; o.faces.len()];
        for (material, faces) in &o.groups {
            for &f in faces {
                if let Some(slot) = face_material.get_mut(usize::from(f)) {
                    *slot = index.get(material).copied();
                }
            }
        }
        // Corners of faces that share a smoothing group share a vertex
        // and its averaged normal; other corners get their own.
        let smooth = o.smoothing.len() == o.faces.len();
        let mut mesh = ImportedMesh {
            name: o.name.clone(),
            ..Default::default()
        };
        let mut corner: BTreeMap<(u16, u32, usize), u32> = BTreeMap::new();
        let mut sums: Vec<DVec3> = Vec::new();
        for (k, f) in o.faces.iter().enumerate() {
            if f.iter().any(|&i| usize::from(i) >= n)
                || f[0] == f[1]
                || f[1] == f[2]
                || f[0] == f[2]
            {
                skipped += 1;
                continue;
            }
            let p = f.map(|i| o.vertices[usize::from(i)]);
            let normal = (p[1] - p[0]).cross(p[2] - p[0]);
            let group = if smooth { o.smoothing[k] } else { 0 };
            let t = f.map(|i| {
                // Faces without a group keep their corners to themselves.
                let key = (i, group, if group == 0 { k } else { 0 });
                *corner.entry(key).or_insert_with(|| {
                    mesh.positions.push(o.vertices[usize::from(i)]);
                    if let Some(uv) = o.uvs.get(usize::from(i)) {
                        mesh.uvs.push(*uv);
                    }
                    sums.push(DVec3::ZERO);
                    (mesh.positions.len() - 1) as u32
                })
            });
            for &i in &t {
                sums[i as usize] += normal;
            }
            mesh.triangles.push(t);
            mesh.materials.push(face_material[k]);
        }
        if mesh.uvs.len() != mesh.positions.len() {
            mesh.uvs.clear();
        }
        if smooth {
            mesh.normals = sums.iter().map(|s| s.normalize_or_zero()).collect();
        }
        if !mesh.triangles.is_empty() {
            scene.meshes.push(mesh);
        }
    }
    (scene, skipped)
}

/// 3DS import.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThreeDsImporter;

impl Importer for ThreeDsImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::ThreeDs
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        if bytes.len() >= 16
            && bytes[..2] == MAIN.to_le_bytes()
            && bytes[6..8] == VERSION.to_le_bytes()
        {
            Confidence::CERTAIN
        } else if name.to_ascii_lowercase().ends_with(".3ds") {
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
        src.take(256 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| corrupt(e.to_string()))?;
        if bytes.len() > 256 * 1024 * 1024 {
            return Err(IoError::LimitExceeded("3DS input exceeds 256 MiB".into()));
        }
        progress.report(0.1, "reading 3DS");
        let model = read(&bytes)?;
        let (mut scene, skipped) = scene_of(&model, &source_stem(&opts.source, "3DS"));
        let mut warnings = Vec::new();
        let storage = tx.storage();
        for (m, read) in scene.materials.iter_mut().zip(&model.materials) {
            let Some(file) = &read.texture else { continue };
            let key = sibling(&opts.source, file);
            match storage.map(|s| s.read(&StorageKey::new(&key))) {
                Some(Ok(bytes)) => m.texture = Some(bytes),
                _ => warnings.push(Warning::new(
                    WarningCode::W2004,
                    format!("texture `{key}` of `{}` not found", m.name),
                )),
            }
        }
        if skipped > 0 {
            warnings.push(Warning::new(
                WarningCode::W2004,
                format!("{skipped} degenerate or broken 3DS faces were skipped"),
            ));
        }
        if scene.meshes.is_empty() {
            return Err(IoError::UnsupportedFormat(
                "the 3DS file holds no triangle objects".into(),
            ));
        }
        let mut report = build_scene(tx, target, scene, opts)?;
        report.warnings.extend(warnings);
        Ok(report)
    }
}

// ----- writing --------------------------------------------------------------------------

/// A chunk under construction.
struct Chunk {
    id: u16,
    data: Vec<u8>,
}

impl Chunk {
    fn new(id: u16) -> Self {
        Chunk {
            id,
            data: Vec::new(),
        }
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.data.extend(v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.data.extend(v.to_le_bytes());
        self
    }
    fn f32(&mut self, v: f32) -> &mut Self {
        self.data.extend(v.to_le_bytes());
        self
    }
    fn cstring(&mut self, s: &str) -> &mut Self {
        self.data.extend(s.chars().map(|c| {
            u8::try_from(u32::from(c))
                .ok()
                .filter(|b| *b != 0)
                .unwrap_or(b'_')
        }));
        self.data.push(0);
        self
    }
    fn child(&mut self, c: Chunk) -> &mut Self {
        self.data.extend(c.bytes());
        self
    }
    fn bytes(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() + 6);
        out.extend(self.id.to_le_bytes());
        out.extend(((self.data.len() + 6) as u32).to_le_bytes());
        out.extend(self.data);
        out
    }
}

fn color_chunk(id: u16, rgb: [u8; 3]) -> Chunk {
    let mut c = Chunk::new(id);
    let mut inner = Chunk::new(COLOR_24);
    inner.data.extend(rgb);
    c.child(inner);
    c
}

fn percent_chunk(id: u16, value: u16) -> Chunk {
    let mut c = Chunk::new(id);
    let mut inner = Chunk::new(PERCENT_I);
    inner.u16(value);
    c.child(inner);
    c
}

/// A name of at most ten characters (the format's limit), unique among
/// `taken`.
fn short_name(base: &str, taken: &mut std::collections::BTreeSet<String>) -> String {
    let clean: String = base
        .chars()
        .filter(|c| c.is_ascii_graphic())
        .take(10)
        .collect();
    let clean = if clean.is_empty() {
        "Object".to_string()
    } else {
        clean
    };
    let mut name = clean.clone();
    let mut k = 2;
    while taken.contains(&name) {
        let suffix = k.to_string();
        let keep: String = clean.chars().take(10 - suffix.len().min(9)).collect();
        name = format!("{keep}{suffix}");
        k += 1;
    }
    taken.insert(name.clone());
    name
}

/// One object being filled: vertices with their texture coordinates, faces
/// with their materials.
#[derive(Default)]
struct Part {
    vertices: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    faces: Vec<[u16; 3]>,
    materials: Vec<MaterialId>,
}

fn write_err(e: std::io::Error) -> IoError {
    IoError::Storage {
        key: "3ds".into(),
        reason: e.to_string(),
    }
}

/// 3DS export.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThreeDsExporter;

impl Exporter for ThreeDsExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::ThreeDs
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
        let out = output_transform(
            &ExportOptions {
                z_up: true,
                ..opts.clone()
            },
            false,
        );
        let stem = source_stem(&opts.destination, "model");
        let mut report = ExportReport::default();
        let mut taken = std::collections::BTreeSet::new();
        let mut objects: Vec<(String, Part)> = Vec::new();
        let mut used: Vec<MaterialId> = Vec::new();
        for occ in &scene.occurrences {
            let Some(mesh) = scene.mesh(occ.container) else {
                continue;
            };
            if mesh.faces.is_empty() {
                continue;
            }
            let base = match occ.container {
                ContainerRef::Model => "Model".to_string(),
                ContainerRef::Definition(d) => doc
                    .definitions
                    .get(d)
                    .map_or_else(|| "Object".into(), |def| def.name.clone()),
            };
            let to = out * occ.world;
            let flip = to.matrix3.determinant() < 0.0;
            let mut part = Part::default();
            for face in &mesh.faces {
                let id = doc
                    .materials
                    .resolve(face.material.or(occ.material_override), FrontOrBack::Front);
                if !used.contains(&id) {
                    used.push(id);
                }
                if part.vertices.len() + face.positions.len() > MAX_PER_OBJECT
                    || part.faces.len() + face.triangles.len() > MAX_PER_OBJECT
                {
                    let name = short_name(&base, &mut taken);
                    objects.push((name, std::mem::take(&mut part)));
                }
                let first = part.vertices.len();
                for (k, p) in face.positions.iter().enumerate() {
                    let q = to.transform_point3(*p);
                    part.vertices.push([q.x as f32, q.y as f32, q.z as f32]);
                    let uv = face.uvs.get(k).copied().unwrap_or_default();
                    part.uvs.push([uv.x as f32, uv.y as f32]);
                }
                for t in &face.triangles {
                    let t = if flip { [t[0], t[2], t[1]] } else { *t };
                    part.faces.push(t.map(|i| (first + i as usize) as u16));
                    part.materials.push(id);
                    report.entities += 1;
                }
            }
            let name = short_name(&base, &mut taken);
            objects.push((name, part));
        }
        if objects.is_empty() {
            return Err(IoError::UnsupportedFormat(
                "nothing visible to export".into(),
            ));
        }
        progress.report(0.6, "writing 3DS");
        // Materials, with unique short names and their textures beside.
        let mut names: BTreeMap<MaterialId, String> = BTreeMap::new();
        let mut material_taken = std::collections::BTreeSet::new();
        let mut editor = Chunk::new(EDITOR);
        let mut v = Chunk::new(MESH_VERSION);
        v.u32(3);
        editor.child(v);
        let mut scale = Chunk::new(MASTER_SCALE);
        scale.f32(1.0);
        editor.child(scale);
        let textures = format!("{stem}_textures");
        for id in &used {
            let Some(m) = doc.materials.get(*id) else {
                continue;
            };
            let name = short_name(&m.name, &mut material_taken);
            let c = [m.color.r, m.color.g, m.color.b];
            let mut chunk = Chunk::new(MATERIAL);
            let mut n = Chunk::new(MATERIAL_NAME);
            n.cstring(&name);
            chunk.child(n);
            chunk.child(color_chunk(AMBIENT, c));
            chunk.child(color_chunk(DIFFUSE, c));
            chunk.child(color_chunk(SPECULAR, [229, 229, 229]));
            let transparency = ((1.0 - m.opacity.clamp(0.0, 1.0)) * 100.0).round() as u16;
            chunk.child(percent_chunk(TRANSPARENCY, transparency));
            if let Some(t) = &m.texture
                && let Some(image) = doc.images.get(t.image)
            {
                let file = format!("{textures}/{name}.{}", image.format.extension());
                let mut map = percent_chunk(TEXTURE_MAP, 100);
                let mut f = Chunk::new(MAP_FILE);
                f.cstring(&file);
                map.child(f);
                chunk.child(map);
                report.side_files.push((file, image.bytes.clone()));
            }
            editor.child(chunk);
            names.insert(*id, name);
        }
        for (name, part) in objects {
            let mut object = Chunk::new(OBJECT);
            object.cstring(&name);
            let mut tri = Chunk::new(TRIANGLES);
            let mut vertices = Chunk::new(VERTICES);
            vertices.u16(part.vertices.len() as u16);
            for p in &part.vertices {
                for c in p {
                    vertices.f32(*c);
                }
            }
            tri.child(vertices);
            let mut mapping = Chunk::new(MAPPING);
            mapping.u16(part.uvs.len() as u16);
            for uv in &part.uvs {
                mapping.f32(uv[0]).f32(uv[1]);
            }
            tri.child(mapping);
            let mut frame = Chunk::new(LOCAL_FRAME);
            for x in [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0] {
                frame.f32(x);
            }
            tri.child(frame);
            let mut faces = Chunk::new(FACES);
            faces.u16(part.faces.len() as u16);
            for f in &part.faces {
                faces.u16(f[0]).u16(f[1]).u16(f[2]).u16(0x0007);
            }
            // Face material groups, in material order.
            for id in &used {
                let list: Vec<u16> = part
                    .materials
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| *m == id)
                    .map(|(k, _)| k as u16)
                    .collect();
                if list.is_empty() {
                    continue;
                }
                let mut group = Chunk::new(FACE_MATERIAL);
                group.cstring(names.get(id).map_or("Material", String::as_str));
                group.u16(list.len() as u16);
                for k in list {
                    group.u16(k);
                }
                faces.child(group);
            }
            tri.child(faces);
            object.child(tri);
            editor.child(object);
        }
        let mut main = Chunk::new(MAIN);
        let mut version = Chunk::new(VERSION);
        version.u32(3);
        main.child(version);
        main.child(editor);
        let bytes = main.bytes();
        dst.write_all(&bytes).map_err(write_err)?;
        report.bytes_written = bytes.len() as u64;
        Ok(report)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::commands::{CommandRegistry, Engine};
    use serde_json::json;
    use std::sync::Arc;

    fn engine() -> Engine {
        Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
    }

    #[test]
    fn chunks_nest_and_refuse_overruns() {
        let mut main = Chunk::new(MAIN);
        let mut v = Chunk::new(VERSION);
        v.u32(3);
        main.child(v);
        let bytes = main.bytes();
        assert_eq!(bytes.len(), 16);
        assert_eq!(chunks(&bytes).unwrap()[0].0, MAIN);
        let mut broken = bytes.clone();
        broken[2] = 200;
        assert!(chunks(&broken).is_err());
        assert!(read(b"not a model").is_err());
        let mut taken = Default::default();
        assert_eq!(short_name("Kitchen cabinet", &mut taken), "Kitchencab");
        assert_eq!(short_name("Kitchen cabinet", &mut taken), "Kitchenca2");
    }

    /// A box with a red top goes out and comes back: twelve triangles, the
    /// material, the volume.
    #[test]
    fn a_painted_box_goes_out_and_comes_back() {
        let mut e = engine();
        e.execute_erased(
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [100, 50, 0] }),
        )
        .unwrap();
        e.execute_erased(
            "push-pull",
            json!({ "face": { "Query": "face" }, "distance": 30 }),
        )
        .unwrap();
        e.execute_erased(
            "material-create",
            json!({ "name": "Red paint", "color": "#C81414" }),
        )
        .unwrap();
        e.execute_erased(
            "material-apply",
            json!({ "targets": "face & normal:z", "material": "Red paint" }),
        )
        .unwrap();
        let mut bytes = Vec::new();
        let report = ThreeDsExporter
            .export(
                e.document(),
                &ExportScope::default(),
                &ExportOptions::default(),
                &mut bytes,
                &crate::io::NoProgress,
            )
            .unwrap();
        assert_eq!(report.entities, 12);
        // The golden file: the same bytes every time.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/3ds/painted-box.3ds");
        if std::env::var_os("PLANYRITH_UPDATE_GOLDEN").is_some() || !path.exists() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
        }
        assert!(
            std::fs::read(&path).unwrap() == bytes,
            "{} differs (set PLANYRITH_UPDATE_GOLDEN=1 to accept)",
            path.display()
        );
        let model = read(&bytes).unwrap();
        assert_eq!(model.objects.len(), 1);
        let o = &model.objects[0];
        assert_eq!((o.vertices.len(), o.faces.len()), (24, 12));
        let red = model
            .materials
            .iter()
            .find(|m| m.name == "Redpaint")
            .unwrap();
        assert_eq!(red.color, Some([200, 20, 20]));
        let group = o.groups.iter().find(|(n, _)| *n == red.name).unwrap();
        assert_eq!(group.1.len(), 2, "the top's two triangles");
        // Back in: a closed box of the same size with the red top.
        let mut back = engine();
        let storage = Arc::new(crate::io::MemoryStorage::new());
        back.set_storage(storage.clone());
        crate::io::Storage::write(storage.as_ref(), &StorageKey::new("box.3ds"), &bytes).unwrap();
        let out = back
            .execute_erased("import", json!({ "path": "box.3ds" }))
            .unwrap();
        assert_eq!(out["faces"], 6, "{out}");
        let doc = back.document();
        let (_, def) = doc
            .definitions
            .iter()
            .find(|(_, d)| d.entities.mesh().face_count() > 0)
            .unwrap();
        let mesh = def.entities.mesh();
        assert!(mesh.is_solid());
        assert!((mesh.volume().unwrap() - 150_000.0).abs() < 1e-3);
        assert!(
            doc.materials
                .iter()
                .any(|(_, m)| m.color == Rgba8::rgb(200, 20, 20))
        );
    }

    /// Smoothing groups: a fan of triangles in one group comes in with soft
    /// edges; without a group the edges stay hard.
    #[test]
    fn smoothing_groups_make_soft_edges() {
        let object = ReadObject {
            name: "Fan".into(),
            vertices: vec![
                DVec3::new(0.0, 0.0, 0.0),
                DVec3::new(10.0, 0.0, 0.0),
                DVec3::new(10.0, 10.0, 2.0),
                DVec3::new(0.0, 10.0, 0.0),
            ],
            faces: vec![[0, 1, 2], [0, 2, 3]],
            smoothing: vec![1, 1],
            ..Default::default()
        };
        let model = ReadModel {
            objects: vec![object.clone()],
            scale: 1.0,
            ..Default::default()
        };
        let (scene, skipped) = scene_of(&model, "fan");
        assert_eq!(skipped, 0);
        let mesh = &scene.meshes[0];
        assert_eq!(mesh.positions.len(), 4, "shared corners");
        assert_eq!(mesh.normals.len(), 4);
        let model = ReadModel {
            objects: vec![ReadObject {
                smoothing: vec![0, 0],
                ..object
            }],
            scale: 1.0,
            ..Default::default()
        };
        let (scene, _) = scene_of(&model, "fan");
        assert_eq!(scene.meshes[0].positions.len(), 6, "corners kept apart");
    }
}
