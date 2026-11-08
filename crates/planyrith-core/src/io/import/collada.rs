//! Implements docs/09-file-formats.md §5 COLLADA import (1.4/1.5): mesh
//! geometries (`triangles`, `polylist`, `polygons`), effects' diffuse colors
//! and textures (read beside the file), transparency, and the visual
//! scene's node tree (`matrix`, `translate`, `rotate`, `scale`,
//! `instance_geometry`, `instance_node`), in the asset's unit and up axis,
//! through the shared scene builder (docs/09 Decisions, COLLADA).

use std::collections::BTreeMap;
use std::io::Read;

use glam::{DAffine3, DMat3, DQuat, DVec2, DVec3};
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::diagnostics::{Warning, WarningCode};
use crate::document::Rgba8;
use crate::io::import::build::{
    ImportedMaterial, ImportedMesh, ImportedNode, ImportedScene, build_scene,
};
use crate::io::import::{
    Confidence, ImportFormat, ImportOptions, ImportReport, Importer, sibling, source_stem,
};
use crate::io::{IoError, Progress, StorageKey};
use crate::units::LengthUnit;

/// The COLLADA importer.
#[derive(Debug, Default, Clone, Copy)]
pub struct ColladaImporter;

/// Deepest node nesting followed.
const MAX_DEPTH: usize = 64;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "dae".into(),
        reason: reason.into(),
    }
}

/// A light element tree: local names, attributes, text.
#[derive(Debug, Default, Clone)]
struct Element {
    name: String,
    attrs: BTreeMap<String, String>,
    text: String,
    children: Vec<Element>,
}

impl Element {
    fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).map(String::as_str)
    }

    /// The first descendant named `name`, depth first.
    fn find(&self, name: &str) -> Option<&Element> {
        for c in &self.children {
            if c.name == name {
                return Some(c);
            }
            if let Some(found) = c.find(name) {
                return Some(found);
            }
        }
        None
    }

    fn numbers(&self) -> Vec<f64> {
        self.text
            .split_ascii_whitespace()
            .filter_map(|t| t.parse().ok())
            .collect()
    }
}

fn parse_tree(xml: &str) -> Result<Element, IoError> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Element> = vec![Element::default()];
    loop {
        match reader
            .read_event()
            .map_err(|e| corrupt(format!("XML: {e}")))?
        {
            Event::Start(e) => {
                let mut el = Element {
                    name: String::from_utf8_lossy(e.local_name().as_ref()).into_owned(),
                    ..Element::default()
                };
                for a in e.attributes() {
                    let a = a.map_err(|err| corrupt(err.to_string()))?;
                    let v = a
                        .normalized_value(XmlVersion::Implicit1_0)
                        .map_err(|err| corrupt(err.to_string()))?;
                    el.attrs.insert(
                        String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned(),
                        v.into_owned(),
                    );
                }
                stack.push(el);
            }
            Event::Empty(e) => {
                let mut el = Element {
                    name: String::from_utf8_lossy(e.local_name().as_ref()).into_owned(),
                    ..Element::default()
                };
                for a in e.attributes() {
                    let a = a.map_err(|err| corrupt(err.to_string()))?;
                    let v = a
                        .normalized_value(XmlVersion::Implicit1_0)
                        .map_err(|err| corrupt(err.to_string()))?;
                    el.attrs.insert(
                        String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned(),
                        v.into_owned(),
                    );
                }
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(el);
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    let text = t
                        .xml_content(XmlVersion::Implicit1_0)
                        .map_err(|err| corrupt(err.to_string()))?;
                    top.text.push_str(&text);
                }
            }
            Event::End(_) => {
                let el = stack.pop().ok_or_else(|| corrupt("unbalanced XML"))?;
                stack
                    .last_mut()
                    .ok_or_else(|| corrupt("unbalanced XML"))?
                    .children
                    .push(el);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut root = stack.pop().ok_or_else(|| corrupt("empty file"))?;
    root.children
        .pop()
        .filter(|r| r.name == "COLLADA")
        .ok_or_else(|| corrupt("not a COLLADA file"))
}

/// `#id` → `id`.
fn target(url: &str) -> &str {
    url.trim_start_matches('#')
}

/// A source's values and stride by id.
fn sources(mesh: &Element) -> BTreeMap<String, (Vec<f64>, usize)> {
    let mut out = BTreeMap::new();
    for s in mesh.all("source") {
        let Some(id) = s.attr("id") else { continue };
        let values = s
            .find("float_array")
            .map(Element::numbers)
            .unwrap_or_default();
        let stride = s
            .find("accessor")
            .and_then(|a| a.attr("stride"))
            .and_then(|v| v.parse().ok())
            .unwrap_or(1usize)
            .max(1);
        out.insert(id.to_string(), (values, stride));
    }
    // <vertices> maps its id to the POSITION source.
    if let Some(v) = mesh.child("vertices")
        && let (Some(id), Some(pos)) = (
            v.attr("id"),
            v.all("input")
                .find(|i| i.attr("semantic") == Some("POSITION"))
                .and_then(|i| i.attr("source")),
        )
        && let Some(src) = out.get(target(pos)).cloned()
    {
        out.insert(id.to_string(), src);
    }
    out
}

/// A geometry's triangles (fanned polygons) with per-corner positions,
/// texture coordinates, and the material symbol of each.
fn read_geometry(
    geometry: &Element,
    symbols: &mut Vec<String>,
) -> Result<Option<ImportedMesh>, IoError> {
    let Some(mesh) = geometry.child("mesh") else {
        return Ok(None);
    };
    let sources = sources(mesh);
    let mut out = ImportedMesh {
        name: geometry
            .attr("name")
            .or(geometry.attr("id"))
            .unwrap_or("Geometry")
            .to_string(),
        ..ImportedMesh::default()
    };
    let mut any_uv = false;
    for primitive in &mesh.children {
        let kind = primitive.name.as_str();
        if !matches!(kind, "triangles" | "polylist" | "polygons") {
            continue;
        }
        let inputs: Vec<(&str, usize, &str)> = primitive
            .all("input")
            .filter_map(|i| {
                Some((
                    i.attr("semantic")?,
                    i.attr("offset")?.parse().ok()?,
                    target(i.attr("source")?),
                ))
            })
            .collect();
        let stride = inputs.iter().map(|i| i.1).max().map_or(1, |m| m + 1);
        let find = |semantic: &str| inputs.iter().find(|i| i.0 == semantic);
        let Some(&(_, vertex_offset, vertex_source)) = find("VERTEX") else {
            continue;
        };
        let Some((positions, pstride)) = sources.get(vertex_source) else {
            return Err(corrupt(format!("missing source `{vertex_source}`")));
        };
        let uv = find("TEXCOORD").and_then(|i| sources.get(i.2).map(|s| (i.1, s)));
        let symbol = primitive.attr("material").unwrap_or("").to_string();
        let material = match symbols.iter().position(|s| *s == symbol) {
            Some(i) => i,
            None => {
                symbols.push(symbol);
                symbols.len() - 1
            }
        };
        // Polygon sizes: 3 each, `vcount`, or one `<p>` per polygon.
        let (counts, indices): (Vec<usize>, Vec<f64>) = match kind {
            "triangles" => {
                let p = primitive
                    .child("p")
                    .map(Element::numbers)
                    .unwrap_or_default();
                (vec![3; p.len() / (3 * stride)], p)
            }
            "polylist" => (
                primitive
                    .child("vcount")
                    .map(Element::numbers)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|v| v as usize)
                    .collect(),
                primitive
                    .child("p")
                    .map(Element::numbers)
                    .unwrap_or_default(),
            ),
            _ => {
                let mut counts = Vec::new();
                let mut all = Vec::new();
                for p in primitive.all("p") {
                    let n = p.numbers();
                    counts.push(n.len() / stride);
                    all.extend(n);
                }
                // Polygons with holes: triangulated on their own.
                for ph in primitive.all("ph") {
                    let mut rings: Vec<Vec<f64>> = ph.all("p").map(Element::numbers).collect();
                    rings.extend(ph.all("h").map(Element::numbers));
                    holed(
                        &mut out,
                        &rings,
                        stride,
                        vertex_offset,
                        positions,
                        *pstride,
                        uv,
                        material,
                    )?;
                }
                (counts, all)
            }
        };
        let mut at = 0usize;
        for count in counts {
            let mut corners = Vec::with_capacity(count);
            for _ in 0..count {
                let read = |offset: usize| indices.get(at + offset).map(|v| *v as usize);
                let Some(vi) = read(vertex_offset) else {
                    return Err(corrupt("an index list ends early"));
                };
                let p = positions
                    .get(vi * pstride..vi * pstride + 3)
                    .ok_or_else(|| corrupt("an index names a missing position"))?;
                out.positions.push(DVec3::new(p[0], p[1], p[2]));
                let t = uv.and_then(|(offset, (values, s))| {
                    let ti = read(offset)?;
                    let st = values.get(ti * s..ti * s + 2)?;
                    Some(DVec2::new(st[0], st[1]))
                });
                any_uv |= t.is_some();
                out.uvs.push(t.unwrap_or_default());
                corners.push((out.positions.len() - 1) as u32);
                at += stride;
            }
            for k in 1..corners.len().saturating_sub(1) {
                out.triangles.push([corners[0], corners[k], corners[k + 1]]);
                out.materials.push(Some(material));
            }
        }
    }
    if !any_uv {
        out.uvs.clear();
    }
    Ok((!out.triangles.is_empty()).then_some(out))
}

/// A polygon with holes (`<ph>`: the outer ring, then `<h>` rings), as
/// triangles from ear clipping in the outer ring's plane.
#[allow(clippy::too_many_arguments)]
fn holed(
    out: &mut ImportedMesh,
    rings: &[Vec<f64>],
    stride: usize,
    vertex_offset: usize,
    positions: &[f64],
    pstride: usize,
    uv: Option<(usize, &(Vec<f64>, usize))>,
    material: usize,
) -> Result<(), IoError> {
    let mut points: Vec<DVec3> = Vec::new();
    let mut uvs: Vec<DVec2> = Vec::new();
    let mut hole_starts: Vec<usize> = Vec::new();
    for (r, ring) in rings.iter().enumerate() {
        if r > 0 {
            hole_starts.push(points.len());
        }
        for corner in ring.chunks(stride) {
            let vi = *corner
                .get(vertex_offset)
                .ok_or_else(|| corrupt("an index list ends early"))? as usize;
            let p = positions
                .get(vi * pstride..vi * pstride + 3)
                .ok_or_else(|| corrupt("an index names a missing position"))?;
            points.push(DVec3::new(p[0], p[1], p[2]));
            let t = uv.and_then(|(offset, (values, s))| {
                let ti = *corner.get(offset)? as usize;
                let st = values.get(ti * s..ti * s + 2)?;
                Some(DVec2::new(st[0], st[1]))
            });
            uvs.push(t.unwrap_or_default());
        }
    }
    let outer = hole_starts.first().copied().unwrap_or(points.len());
    if outer < 3 {
        return Ok(());
    }
    let mut normal = DVec3::ZERO;
    for i in 0..outer {
        normal += points[i].cross(points[(i + 1) % outer]);
    }
    let Some(normal) = normal.try_normalize() else {
        return Ok(());
    };
    let u = normal.any_orthonormal_vector();
    let v = normal.cross(u);
    let flat: Vec<f64> = points.iter().flat_map(|p| [p.dot(u), p.dot(v)]).collect();
    let triangles = earcutr::earcut(&flat, &hole_starts, 2)
        .map_err(|e| corrupt(format!("a polygon with holes: {e:?}")))?;
    let base = out.positions.len() as u32;
    out.positions.extend(points);
    out.uvs.extend(uvs);
    for t in triangles.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| base + i as u32);
        // Ear clipping may turn triangles over; keep the ring's side.
        let pa = out.positions[a as usize];
        let n = (out.positions[b as usize] - pa).cross(out.positions[c as usize] - pa);
        out.triangles.push(if n.dot(normal) < 0.0 {
            [a, c, b]
        } else {
            [a, b, c]
        });
        out.materials.push(Some(material));
    }
    Ok(())
}

/// The transform a node's own children (`matrix`, `translate`, `rotate`,
/// `scale`, in order) make.
fn node_transform(node: &Element) -> DAffine3 {
    let mut t = DAffine3::IDENTITY;
    for c in &node.children {
        let n = c.numbers();
        let step = match (c.name.as_str(), n.as_slice()) {
            ("matrix", m) if m.len() == 16 => DAffine3 {
                matrix3: DMat3::from_cols(
                    DVec3::new(m[0], m[4], m[8]),
                    DVec3::new(m[1], m[5], m[9]),
                    DVec3::new(m[2], m[6], m[10]),
                ),
                translation: DVec3::new(m[3], m[7], m[11]),
            },
            ("translate", [x, y, z]) => DAffine3::from_translation(DVec3::new(*x, *y, *z)),
            ("scale", [x, y, z]) => DAffine3::from_scale(DVec3::new(*x, *y, *z)),
            ("rotate", [x, y, z, deg]) => {
                let axis = DVec3::new(*x, *y, *z).normalize_or_zero();
                if axis == DVec3::ZERO {
                    continue;
                }
                DAffine3::from_quat(DQuat::from_axis_angle(axis, deg.to_radians()))
            }
            _ => continue,
        };
        t *= step;
    }
    t
}

/// Everything a file holds, ready for the scene builder.
fn read(
    root: &Element,
    texture: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
    name: String,
) -> Result<(ImportedScene, Vec<Warning>), IoError> {
    let mut warnings = Vec::new();
    let asset = root.child("asset");
    let meters = asset
        .and_then(|a| a.child("unit"))
        .and_then(|u| u.attr("meter"))
        .and_then(|m| m.parse::<f64>().ok())
        .filter(|m| *m > 0.0 && m.is_finite())
        .unwrap_or(1.0);
    let up = asset
        .and_then(|a| a.child("up_axis"))
        .map_or("Y_UP", |u| u.text.trim());
    // Images by id → file name; effects by id → color, opacity, image.
    let mut images: BTreeMap<String, String> = BTreeMap::new();
    for lib in root.all("library_images") {
        for image in lib.all("image") {
            if let (Some(id), Some(file)) = (image.attr("id"), image.find("init_from")) {
                images.insert(id.to_string(), file.text.trim().to_string());
            }
        }
    }
    let mut effects: BTreeMap<String, (Rgba8, f32, Option<String>)> = BTreeMap::new();
    for lib in root.all("library_effects") {
        for effect in lib.all("effect") {
            let Some(id) = effect.attr("id") else {
                continue;
            };
            let diffuse = effect.find("diffuse");
            let color = diffuse
                .and_then(|d| d.child("color"))
                .map(Element::numbers)
                .filter(|c| c.len() >= 3)
                .map_or(Rgba8::rgb(200, 200, 200), |c| {
                    let s = |v: f64| {
                        let v = v.clamp(0.0, 1.0);
                        let e = if v <= 0.003_130_8 {
                            v * 12.92
                        } else {
                            1.055 * libm::pow(v, 1.0 / 2.4) - 0.055
                        };
                        (e * 255.0).round() as u8
                    };
                    Rgba8::rgb(s(c[0]), s(c[1]), s(c[2]))
                });
            // The texture names a sampler, the sampler a surface, the
            // surface an image (1.4), or the sampler the image (1.5).
            let image = diffuse.and_then(|d| d.child("texture")).and_then(|t| {
                let mut key = t.attr("texture")?.to_string();
                for _ in 0..3 {
                    if images.contains_key(&key) {
                        return Some(key);
                    }
                    let param = effect
                        .find("profile_COMMON")?
                        .all("newparam")
                        .find(|p| p.attr("sid") == Some(key.as_str()))?;
                    key = param
                        .find("source")
                        .or_else(|| param.find("init_from"))
                        .or_else(|| param.find("instance_image").map(|_| param))?
                        .text
                        .trim()
                        .to_string();
                }
                images.contains_key(&key).then_some(key)
            });
            let opacity = effect
                .find("transparency")
                .and_then(|t| t.child("float"))
                .and_then(|f| f.text.trim().parse::<f32>().ok())
                .filter(|o| (0.0..=1.0).contains(o) && *o > 0.0)
                .unwrap_or(1.0);
            effects.insert(id.to_string(), (color, opacity, image));
        }
    }
    let mut materials_by_id: BTreeMap<String, (String, String)> = BTreeMap::new();
    for lib in root.all("library_materials") {
        for m in lib.all("material") {
            if let (Some(id), Some(effect)) = (
                m.attr("id"),
                m.child("instance_effect").and_then(|e| e.attr("url")),
            ) {
                materials_by_id.insert(
                    id.to_string(),
                    (
                        m.attr("name").unwrap_or(id).to_string(),
                        target(effect).to_string(),
                    ),
                );
            }
        }
    }
    // Lines (edges without faces) are not read.
    let lines: usize = root
        .all("library_geometries")
        .flat_map(|l| l.all("geometry"))
        .filter_map(|g| g.child("mesh"))
        .map(|m| m.all("lines").count() + m.all("linestrips").count())
        .sum();
    if lines > 0 {
        warnings.push(Warning::new(
            WarningCode::W2004,
            format!("{lines} line sets (edges without faces) not read"),
        ));
    }
    // Geometries; their material symbols are bound per instance.
    let mut meshes: Vec<(ImportedMesh, Vec<String>)> = Vec::new();
    let mut mesh_of: BTreeMap<String, usize> = BTreeMap::new();
    for lib in root.all("library_geometries") {
        for g in lib.all("geometry") {
            let mut symbols = Vec::new();
            if let (Some(id), Some(mesh)) = (g.attr("id"), read_geometry(g, &mut symbols)?) {
                mesh_of.insert(id.to_string(), meshes.len());
                meshes.push((mesh, symbols));
            }
        }
    }
    let library_nodes: BTreeMap<String, &Element> = root
        .all("library_nodes")
        .flat_map(|l| l.all("node"))
        .filter_map(|n| Some((n.attr("id")?.to_string(), n)))
        .collect();
    // Scene materials, one per (material id); bound placements per mesh.
    let mut scene_materials: Vec<ImportedMaterial> = Vec::new();
    let mut material_index: BTreeMap<String, usize> = BTreeMap::new();
    let mut texture_cache: BTreeMap<String, Option<Vec<u8>>> = BTreeMap::new();
    let mut placed: Vec<(usize, DAffine3, Vec<Option<usize>>, String)> = Vec::new();
    let scene_url = root
        .child("scene")
        .and_then(|s| s.child("instance_visual_scene"))
        .and_then(|i| i.attr("url"))
        .map(target);
    let visual = root
        .all("library_visual_scenes")
        .flat_map(|l| l.all("visual_scene"))
        .find(|v| scene_url.is_none_or(|u| v.attr("id") == Some(u)));
    let mut stack: Vec<(&Element, DAffine3, usize)> = visual
        .map(|v| v.all("node").map(|n| (n, DAffine3::IDENTITY, 0)).collect())
        .unwrap_or_default();
    stack.reverse();
    while let Some((node, parent, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            warnings.push(Warning::new(WarningCode::W2004, "nodes nest too deep"));
            continue;
        }
        let world = parent * node_transform(node);
        for ig in node.all("instance_geometry") {
            let Some(&mesh) = ig.attr("url").and_then(|u| mesh_of.get(target(u))) else {
                continue;
            };
            let binds: BTreeMap<&str, &str> = ig
                .find("technique_common")
                .map(|t| {
                    t.all("instance_material")
                        .filter_map(|m| Some((m.attr("symbol")?, target(m.attr("target")?))))
                        .collect()
                })
                .unwrap_or_default();
            let materials: Vec<Option<usize>> = meshes[mesh]
                .1
                .iter()
                .map(|symbol| {
                    let id = binds
                        .get(symbol.as_str())
                        .copied()
                        .unwrap_or(symbol.as_str());
                    if let Some(&i) = material_index.get(id) {
                        return Some(i);
                    }
                    let (name, effect) = materials_by_id.get(id)?;
                    let (color, opacity, image) = effects.get(effect).cloned()?;
                    let bytes = image.and_then(|img| {
                        let file = images.get(&img)?.clone();
                        texture_cache
                            .entry(file.clone())
                            .or_insert_with(|| texture(&file))
                            .clone()
                    });
                    scene_materials.push(ImportedMaterial {
                        name: name.clone(),
                        color,
                        opacity,
                        texture: bytes,
                        ..ImportedMaterial::default()
                    });
                    material_index.insert(id.to_string(), scene_materials.len() - 1);
                    Some(scene_materials.len() - 1)
                })
                .collect();
            let label = node
                .attr("name")
                .or(node.attr("id"))
                .unwrap_or("Node")
                .to_string();
            placed.push((mesh, world, materials, label));
        }
        let mut children: Vec<(&Element, DAffine3, usize)> =
            node.all("node").map(|n| (n, world, depth + 1)).collect();
        for inst in node.all("instance_node") {
            if let Some(n) = inst.attr("url").and_then(|u| library_nodes.get(target(u))) {
                children.push((n, world, depth + 1));
            }
        }
        children.reverse();
        stack.extend(children);
    }
    // A mesh placed with different material bindings is copied per binding.
    let mut out_meshes: Vec<ImportedMesh> = Vec::new();
    let mut variant: BTreeMap<(usize, Vec<Option<usize>>), usize> = BTreeMap::new();
    let mut nodes = Vec::new();
    for (mesh, world, materials, label) in placed {
        let index = *variant.entry((mesh, materials.clone())).or_insert_with(|| {
            let mut m = meshes[mesh].0.clone();
            m.materials = m
                .materials
                .iter()
                .map(|s| s.and_then(|i| materials.get(i).copied().flatten()))
                .collect();
            out_meshes.push(m);
            out_meshes.len() - 1
        });
        nodes.push(ImportedNode {
            name: label,
            transform: world,
            mesh: index,
        });
    }
    // The unit: a named one when it matches, else millimeters scaled.
    let unit = [
        LengthUnit::Millimeter,
        LengthUnit::Centimeter,
        LengthUnit::Meter,
        LengthUnit::Inch,
        LengthUnit::Foot,
    ]
    .into_iter()
    .find(|u| (u.millimeters() / 1000.0 - meters).abs() < 1e-9 * meters.max(1e-9));
    let (unit, factor) = match unit {
        Some(u) => (u, 1.0),
        None => (LengthUnit::Millimeter, meters * 1000.0),
    };
    if (factor - 1.0).abs() > 1e-12 {
        for m in &mut out_meshes {
            for p in &mut m.positions {
                *p *= factor;
            }
        }
        for n in &mut nodes {
            n.transform.translation *= factor;
        }
    }
    if up == "X_UP" {
        warnings.push(Warning::new(
            WarningCode::W2004,
            "X-up axes are read as Z-up; turn the import if it lies on its side",
        ));
    }
    let scene = ImportedScene {
        name,
        materials: scene_materials,
        meshes: out_meshes,
        nodes,
        y_up: up == "Y_UP",
        units: Some(unit),
        cameras: Vec::new(),
    };
    Ok((scene, warnings))
}

impl Importer for ColladaImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Dae
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]);
        if head.contains("<COLLADA") {
            Confidence::CERTAIN
        } else if name.to_ascii_lowercase().ends_with(".dae") {
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
            .map_err(|e| corrupt(e.to_string()))?;
        progress.report(0.1, "reading the model");
        let root = parse_tree(&text)?;
        let storage = tx.storage();
        let source = opts.source.clone();
        let mut texture = |file: &str| -> Option<Vec<u8>> {
            let file = file.trim_start_matches("file://");
            let key = sibling(&source, &percent_decode(file));
            storage.and_then(|s| s.read(&StorageKey::new(&key)).ok())
        };
        let (scene, warnings) = read(&root, &mut texture, source_stem(&opts.source, "COLLADA"))?;
        progress.report(0.5, "building faces");
        let mut report = build_scene(tx, target, scene, opts)?;
        report.skipped += warnings.len();
        report.warnings.extend(warnings);
        Ok(report)
    }
}

/// `%20`-style escapes in a file reference.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const QUAD: &str = r##"<?xml version="1.0"?>
<COLLADA xmlns="http://www.collada.org/2005/11/COLLADASchema" version="1.4.1">
 <asset><unit meter="0.0254" name="inch"/><up_axis>Z_UP</up_axis></asset>
 <library_effects><effect id="fx"><profile_COMMON><technique sid="c"><lambert>
  <diffuse><color>1 0 0 1</color></diffuse></lambert></technique></profile_COMMON></effect></library_effects>
 <library_materials><material id="red" name="Red"><instance_effect url="#fx"/></material></library_materials>
 <library_geometries><geometry id="g" name="Quad"><mesh>
  <source id="g-p"><float_array id="g-pa" count="12">0 0 0 10 0 0 10 10 0 0 10 0</float_array>
   <technique_common><accessor source="#g-pa" count="4" stride="3"/></technique_common></source>
  <vertices id="g-v"><input semantic="POSITION" source="#g-p"/></vertices>
  <polylist material="m" count="1"><input semantic="VERTEX" source="#g-v" offset="0"/>
   <vcount>4</vcount><p>0 1 2 3</p></polylist>
 </mesh></geometry></library_geometries>
 <library_nodes><node id="lib"><instance_geometry url="#g"><bind_material><technique_common>
  <instance_material symbol="m" target="#red"/></technique_common></bind_material></instance_geometry></node></library_nodes>
 <library_visual_scenes><visual_scene id="s">
  <node name="A"><translate>100 0 0</translate><instance_node url="#lib"/></node>
  <node name="B"><rotate>0 0 1 90</rotate><node name="C"><instance_node url="#lib"/></node></node>
 </visual_scene></library_visual_scenes>
 <scene><instance_visual_scene url="#s"/></scene>
</COLLADA>"##;

    #[test]
    fn library_nodes_transforms_and_materials() {
        let root = parse_tree(QUAD).unwrap();
        let (scene, warnings) = read(&root, &mut |_| None, "q".into()).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(scene.units, Some(LengthUnit::Inch));
        assert!(!scene.y_up);
        assert_eq!(scene.meshes.len(), 1, "both placements share the mesh");
        assert_eq!(scene.meshes[0].triangles.len(), 2);
        assert_eq!(scene.meshes[0].materials, vec![Some(0), Some(0)]);
        assert_eq!(scene.materials[0].name, "Red");
        assert_eq!(scene.materials[0].color, Rgba8::rgb(255, 0, 0));
        assert_eq!(scene.nodes.len(), 2);
        assert_eq!(
            scene.nodes[0].transform.translation,
            DVec3::new(100.0, 0.0, 0.0)
        );
        let turned = scene.nodes[1].transform.transform_point3(DVec3::X);
        assert!(turned.distance(DVec3::Y) < 1e-12, "{turned}");
    }

    #[test]
    fn broken_files_are_corrupt_and_escapes_decode() {
        assert!(parse_tree("<notcollada/>").is_err());
        assert!(parse_tree("<COLLADA><asset></COLLADA>").is_err());
        assert_eq!(percent_decode("my%20oak.png"), "my oak.png");
    }
}
