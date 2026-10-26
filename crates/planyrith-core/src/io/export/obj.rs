//! Implements docs/09-file-formats.md §5 OBJ export: one object (`o`) per
//! occurrence with its faces as triangles in world coordinates, texture
//! coordinates and face normals, materials in an MTL file next to it and
//! their textures beside it (`<name>_textures/`). In `--units`, Y-up unless
//! `--z-up` (docs/09 Decisions).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write;

use crate::document::{ContainerRef, Document, FrontOrBack};
use crate::ids::MaterialId;
use crate::io::export::scene::collect;
use crate::io::export::{
    ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter, output_transform,
};
use crate::io::import::source_stem;
use crate::io::{IoError, Progress};

/// The OBJ exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct ObjExporter;

/// A number with at most six decimals and no trailing zeros.
pub(crate) fn num(x: f64) -> String {
    let mut s = format!("{:.6}", x + 0.0);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" { "0".into() } else { s }
}

/// A name usable as one OBJ/MTL token.
fn token(name: &str) -> String {
    let t: String = name
        .chars()
        .map(|c| {
            if c.is_whitespace() || c == '#' {
                '_'
            } else {
                c
            }
        })
        .collect();
    if t.is_empty() { "unnamed".into() } else { t }
}

fn write_err(e: std::io::Error) -> IoError {
    IoError::Storage {
        key: "obj".into(),
        reason: e.to_string(),
    }
}

impl Exporter for ObjExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Obj
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
        let out = output_transform(opts, true);
        let stem = source_stem(&opts.destination, "model");
        let mut report = ExportReport::default();

        // Materials in order of first use, with unique tokens.
        let mut used: Vec<MaterialId> = Vec::new();
        let mut names: BTreeMap<MaterialId, String> = BTreeMap::new();
        let mut taken: BTreeMap<String, ()> = BTreeMap::new();
        let mut material_name = |id: MaterialId, used: &mut Vec<MaterialId>| -> String {
            if let Some(n) = names.get(&id) {
                return n.clone();
            }
            let base = token(
                doc.materials
                    .get(id)
                    .map_or("Material", |m| m.name.as_str()),
            );
            let mut name = base.clone();
            let mut k = 2;
            while taken.contains_key(&name) {
                name = format!("{base}_{k}");
                k += 1;
            }
            taken.insert(name.clone(), ());
            names.insert(id, name.clone());
            used.push(id);
            name
        };

        let mut text = String::new();
        let _ = writeln!(text, "# Planyrith OBJ export");
        let _ = writeln!(text, "mtllib {}", token(&format!("{stem}.mtl")));
        let (mut v_base, mut vt_base, mut vn_base) = (1usize, 1usize, 1usize);
        let mut objects: BTreeMap<String, usize> = BTreeMap::new();
        let mut written = 0u64;
        let mut flush = |text: &mut String, dst: &mut dyn Write| -> Result<(), IoError> {
            dst.write_all(text.as_bytes()).map_err(write_err)?;
            written += text.len() as u64;
            text.clear();
            Ok(())
        };
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
                    .map_or_else(|| "Object".into(), |def| token(&def.name)),
            };
            let count = objects.entry(base.clone()).or_insert(0);
            *count += 1;
            let object = if *count == 1 {
                base
            } else {
                format!("{base}_{count}")
            };
            let _ = writeln!(text, "o {object}");
            let to = out * occ.world;
            let normal_matrix = to.matrix3.inverse().transpose();
            let flip = to.matrix3.determinant() < 0.0;
            let mut current: Option<MaterialId> = None;
            for face in &mesh.faces {
                let id = doc
                    .materials
                    .resolve(face.material.or(occ.material_override), FrontOrBack::Front);
                if current != Some(id) {
                    let _ = writeln!(text, "usemtl {}", material_name(id, &mut used));
                    current = Some(id);
                }
                for p in &face.positions {
                    let p = to.transform_point3(*p);
                    let _ = writeln!(text, "v {} {} {}", num(p.x), num(p.y), num(p.z));
                }
                for uv in &face.uvs {
                    let _ = writeln!(text, "vt {} {}", num(uv.x), num(uv.y));
                }
                let n = (normal_matrix * face.normal).normalize_or_zero();
                let _ = writeln!(text, "vn {} {} {}", num(n.x), num(n.y), num(n.z));
                for t in &face.triangles {
                    let t = if flip { [t[0], t[2], t[1]] } else { *t };
                    let corner = |i: u32| {
                        let i = i as usize;
                        format!("{}/{}/{}", v_base + i, vt_base + i, vn_base)
                    };
                    let _ = writeln!(text, "f {} {} {}", corner(t[0]), corner(t[1]), corner(t[2]));
                    report.entities += 1;
                }
                v_base += face.positions.len();
                vt_base += face.uvs.len();
                vn_base += 1;
                if text.len() > 1 << 16 {
                    flush(&mut text, dst)?;
                }
            }
        }
        flush(&mut text, dst)?;
        progress.report(0.8, "writing materials");

        // The MTL file and the textures.
        let mut mtl = String::from("# Planyrith MTL export\n");
        let textures = format!("{stem}_textures");
        for id in &used {
            let Some(m) = doc.materials.get(*id) else {
                continue;
            };
            let name = names.get(id).cloned().unwrap_or_default();
            let c = m.color;
            let _ = writeln!(mtl, "\nnewmtl {name}");
            let _ = writeln!(
                mtl,
                "Kd {} {} {}",
                num(f64::from(c.r) / 255.0),
                num(f64::from(c.g) / 255.0),
                num(f64::from(c.b) / 255.0)
            );
            let _ = writeln!(mtl, "d {}", num(f64::from(m.opacity)));
            if let Some(t) = &m.texture
                && let Some(image) = doc.images.get(t.image)
            {
                let file = format!("{textures}/{name}.{}", image.format.extension());
                let _ = writeln!(mtl, "map_Kd {}", token(&file));
                report.side_files.push((token(&file), image.bytes.clone()));
            }
        }
        report
            .side_files
            .insert(0, (token(&format!("{stem}.mtl")), mtl.into_bytes()));
        report.bytes_written = written;
        Ok(report)
    }
}
