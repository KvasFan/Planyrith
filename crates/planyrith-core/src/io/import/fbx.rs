//! Implements docs/09-file-formats.md §5 FBX import (docs/09 Decisions,
//! FBX): mesh geometries (polygons of any size, triangulated in their plane;
//! texture coordinates and normals of the first layer; per-polygon
//! materials), materials' diffuse colors, opacity and diffuse textures
//! (embedded or beside the file), and the model hierarchy with the full
//! transform chain (pivots, offsets, pre- and post-rotation, rotation order
//! and geometric transforms), in the file's unit and axes, through the
//! shared scene builder. Animation, skinning, blend shapes, cameras and
//! lights are not read.

use std::collections::BTreeMap;
use std::io::Read;

use glam::{DAffine3, DMat3, DVec2, DVec3};

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::document::Rgba8;
use crate::io::fbx::{self, Node, Prop};
use crate::io::import::build::{
    ImportedMaterial, ImportedMesh, ImportedNode, ImportedScene, build_scene,
};
use crate::io::import::gltf::base64_decode;
use crate::io::import::{
    Confidence, ImportFormat, ImportOptions, ImportReport, Importer, sibling, source_stem,
};
use crate::io::{IoError, Progress, StorageKey};
use crate::units::LengthUnit;

/// The FBX importer.
#[derive(Debug, Default, Clone, Copy)]
pub struct FbxImporter;

/// Deepest model nesting followed.
const MAX_DEPTH: usize = 64;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "fbx".into(),
        reason: reason.into(),
    }
}

/// An object's name without its class (`Name\0\x01Class` or `Class::Name`).
fn object_name(node: &Node) -> String {
    let raw = node.props.get(1).and_then(Prop::as_str).unwrap_or("");
    if let Some((name, _)) = raw.split_once("\0\u{1}") {
        name.to_string()
    } else if let Some((_, name)) = raw.split_once("::") {
        name.to_string()
    } else {
        raw.to_string()
    }
}

/// An object's subclass (`Mesh`, `Null`, `Camera`…).
fn object_kind(node: &Node) -> &str {
    node.props.get(2).and_then(Prop::as_str).unwrap_or("")
}

fn numbers(props: &[Prop]) -> Vec<f64> {
    props.iter().filter_map(Prop::as_f64).collect()
}

fn vec3(node: &Node, name: &str, default: DVec3) -> DVec3 {
    match node.property(name).map(numbers).as_deref() {
        Some([x, y, z, ..]) => DVec3::new(*x, *y, *z),
        _ => default,
    }
}

fn scalar(node: &Node, name: &str) -> Option<f64> {
    node.property(name)
        .and_then(|p| p.first())
        .and_then(Prop::as_f64)
}

/// A rotation of `deg` (degrees about X, Y, Z) applied in `order`
/// (0 XYZ, 1 XZY, 2 YZX, 3 YXZ, 4 ZXY, 5 ZYX: the first letter turns first).
fn euler(deg: DVec3, order: i64) -> DMat3 {
    let r = deg * (std::f64::consts::PI / 180.0);
    let (x, y, z) = (
        DMat3::from_rotation_x(r.x),
        DMat3::from_rotation_y(r.y),
        DMat3::from_rotation_z(r.z),
    );
    match order {
        1 => y * z * x,
        2 => x * z * y,
        3 => z * x * y,
        4 => y * x * z,
        5 => x * y * z,
        _ => z * y * x,
    }
}

/// A model's local transform:
/// `T · Roff · Rp · Rpre · R · Rpost⁻¹ · Rp⁻¹ · Soff · Sp · S · Sp⁻¹`.
fn local_transform(model: &Node) -> DAffine3 {
    let order = scalar(model, "RotationOrder").unwrap_or(0.0) as i64;
    let t = |v: DVec3| DAffine3::from_translation(v);
    let rot =
        |name: &str, order: i64| DAffine3::from_mat3(euler(vec3(model, name, DVec3::ZERO), order));
    let rp = vec3(model, "RotationPivot", DVec3::ZERO);
    let sp = vec3(model, "ScalingPivot", DVec3::ZERO);
    let s = vec3(model, "Lcl Scaling", DVec3::ONE);
    t(vec3(model, "Lcl Translation", DVec3::ZERO))
        * t(vec3(model, "RotationOffset", DVec3::ZERO))
        * t(rp)
        * rot("PreRotation", 0)
        * rot("Lcl Rotation", order)
        * rot("PostRotation", 0).inverse()
        * t(-rp)
        * t(vec3(model, "ScalingOffset", DVec3::ZERO))
        * t(sp)
        * DAffine3::from_scale(s)
        * t(-sp)
}

/// The geometric transform: placed on the geometry only, not inherited.
fn geometric_transform(model: &Node) -> DAffine3 {
    DAffine3::from_translation(vec3(model, "GeometricTranslation", DVec3::ZERO))
        * DAffine3::from_mat3(euler(vec3(model, "GeometricRotation", DVec3::ZERO), 0))
        * DAffine3::from_scale(vec3(model, "GeometricScaling", DVec3::ONE))
}

/// A layer element's values for polygon corner `corner` of polygon
/// `polygon` at control point `vertex`.
struct Layer<'a> {
    values: &'a [f64],
    index: Option<&'a [i64]>,
    mapping: &'a str,
    stride: usize,
}

impl<'a> Layer<'a> {
    fn read(
        geometry: &'a Node,
        element: &str,
        data: &str,
        index: &str,
        stride: usize,
    ) -> Option<Self> {
        let layer = geometry.child(element)?;
        let values = match layer.value(data)? {
            Prop::Reals(v) => v.as_slice(),
            _ => return None,
        };
        let reference = layer
            .value("ReferenceInformationType")
            .and_then(Prop::as_str)
            .unwrap_or("Direct");
        let index = match (reference, layer.value(index)) {
            ("IndexToDirect" | "Index", Some(Prop::Ints(i))) => Some(i.as_slice()),
            _ => None,
        };
        let mapping = layer
            .value("MappingInformationType")
            .and_then(Prop::as_str)
            .unwrap_or("ByPolygonVertex");
        Some(Layer {
            values,
            index,
            mapping,
            stride,
        })
    }

    fn at(&self, corner: usize, polygon: usize, vertex: usize) -> Option<&'a [f64]> {
        let i = match self.mapping {
            "ByPolygonVertex" => corner,
            "ByVertice" | "ByVertex" | "ByControlPoint" => vertex,
            "ByPolygon" => polygon,
            "AllSame" => 0,
            _ => return None,
        };
        let i = match self.index {
            Some(index) => usize::try_from(*index.get(i)?).ok()?,
            None => i,
        };
        self.values
            .get(i * self.stride..i * self.stride + self.stride)
    }
}

/// The triangles of a polygon of `corners` (indices into `positions`), in
/// its plane; ear clipping for more than four corners or a concave quad.
fn triangulate(positions: &[DVec3], corners: &[u32]) -> Vec<[u32; 3]> {
    let n = corners.len();
    if n < 3 {
        return Vec::new();
    }
    if n == 3 {
        return vec![[corners[0], corners[1], corners[2]]];
    }
    let p = |i: usize| positions[corners[i] as usize];
    let mut normal = DVec3::ZERO;
    for i in 0..n {
        normal += p(i).cross(p((i + 1) % n));
    }
    let Some(normal) = normal.try_normalize() else {
        return Vec::new();
    };
    let u = normal.any_orthonormal_vector();
    let v = normal.cross(u);
    let flat: Vec<f64> = (0..n).flat_map(|i| [p(i).dot(u), p(i).dot(v)]).collect();
    let Ok(triangles) = earcutr::earcut(&flat, &[], 2) else {
        return Vec::new();
    };
    triangles
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| corners[i]);
            let (pa, pb, pc) = (
                positions[a as usize],
                positions[b as usize],
                positions[c as usize],
            );
            if (pb - pa).cross(pc - pa).dot(normal) < 0.0 {
                [a, c, b]
            } else {
                [a, b, c]
            }
        })
        .collect()
}

/// A geometry's mesh; `materials` maps the file's material indices to the
/// scene's.
fn mesh(
    geometry: &Node,
    name: &str,
    materials: &[Option<usize>],
) -> Result<Option<ImportedMesh>, IoError> {
    let points = match geometry.value("Vertices") {
        Some(Prop::Reals(v)) => v,
        _ => return Ok(None),
    };
    let indices = match geometry.value("PolygonVertexIndex") {
        Some(Prop::Ints(v)) => v,
        _ => return Ok(None),
    };
    let uv = Layer::read(geometry, "LayerElementUV", "UV", "UVIndex", 2);
    let normals = Layer::read(geometry, "LayerElementNormal", "Normals", "NormalsIndex", 3);
    let material_layer = geometry.child("LayerElementMaterial");
    let material_of = |polygon: usize| -> Option<usize> {
        let layer = material_layer?;
        let Some(Prop::Ints(list)) = layer.value("Materials") else {
            return materials.first().copied().flatten();
        };
        let all_same =
            layer.value("MappingInformationType").and_then(Prop::as_str) == Some("AllSame");
        let i = *list.get(if all_same { 0 } else { polygon })?;
        materials.get(usize::try_from(i).ok()?).copied().flatten()
    };
    let mut out = ImportedMesh {
        name: name.to_string(),
        ..ImportedMesh::default()
    };
    let mut corners: Vec<u32> = Vec::new();
    let mut polygon = 0;
    for (corner, &raw) in indices.iter().enumerate() {
        let last = raw < 0;
        let vertex = usize::try_from(if last { !raw } else { raw })
            .map_err(|_| corrupt("a polygon names a missing point"))?;
        let p = points
            .get(vertex * 3..vertex * 3 + 3)
            .ok_or_else(|| corrupt("a polygon names a missing point"))?;
        out.positions.push(DVec3::new(p[0], p[1], p[2]));
        out.uvs.push(
            uv.as_ref()
                .and_then(|l| l.at(corner, polygon, vertex))
                .map_or(DVec2::ZERO, |t| DVec2::new(t[0], t[1])),
        );
        out.normals.push(
            normals
                .as_ref()
                .and_then(|l| l.at(corner, polygon, vertex))
                .map_or(DVec3::ZERO, |n| DVec3::new(n[0], n[1], n[2])),
        );
        corners.push((out.positions.len() - 1) as u32);
        if last {
            let material = material_of(polygon).or(materials.first().copied().flatten());
            for t in triangulate(&out.positions, &corners) {
                out.triangles.push(t);
                out.materials.push(material);
            }
            corners.clear();
            polygon += 1;
        }
    }
    if uv.is_none() {
        out.uvs.clear();
    }
    if normals.is_none() {
        out.normals.clear();
    }
    Ok((!out.triangles.is_empty()).then_some(out))
}

/// The axes of the file as a map to Planyrith's (right +X, up +Z, front −Y).
fn axes(settings: Option<&Node>) -> DMat3 {
    let get = |name: &str, default: i64| {
        settings
            .and_then(|s| scalar(s, name))
            .map_or(default, |v| v as i64)
    };
    let axis = |i: i64| match i {
        0 => DVec3::X,
        2 => DVec3::Z,
        _ => DVec3::Y,
    };
    let sign = |s: i64| if s < 0 { -1.0 } else { 1.0 };
    let up = axis(get("UpAxis", 1)) * sign(get("UpAxisSign", 1));
    let front = axis(get("FrontAxis", 2)) * sign(get("FrontAxisSign", 1));
    let right = axis(get("CoordAxis", 0)) * sign(get("CoordAxisSign", 1));
    // The file's basis in its own coordinates, mapped to ours.
    let file = DMat3::from_cols(right, up, front);
    let ours = DMat3::from_cols(DVec3::X, DVec3::Z, DVec3::NEG_Y);
    if file.determinant().abs() < 0.5 {
        // Inconsistent settings: the usual Y-up.
        return DMat3::from_cols(DVec3::X, DVec3::NEG_Z, DVec3::Y).transpose();
    }
    ours * file.inverse()
}

/// The length unit of `cm` centimeters per file unit, when it is one of
/// Planyrith's.
fn unit_of(cm: f64) -> Option<LengthUnit> {
    [
        LengthUnit::Millimeter,
        LengthUnit::Centimeter,
        LengthUnit::Meter,
        LengthUnit::Inch,
        LengthUnit::Foot,
    ]
    .into_iter()
    .find(|u| (u.millimeters() / 10.0 - cm).abs() < 1e-9 * cm.max(1.0))
}

/// Reads the scene of `doc`; `file` reads a file beside the source.
fn read(
    doc: &fbx::Document,
    file: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
    name: String,
    swap: Option<bool>,
) -> Result<ImportedScene, IoError> {
    if doc.version < 7000 {
        return Err(corrupt(format!(
            "FBX {}.{} files are not supported (7.0 and later are)",
            doc.version / 1000,
            doc.version % 1000 / 100
        )));
    }
    let empty = Node::default();
    let objects = doc.node("Objects").unwrap_or(&empty);
    let by_id: BTreeMap<i64, &Node> = objects
        .children
        .iter()
        .filter_map(|n| Some((n.props.first()?.as_i64()?, n)))
        .collect();
    // Connections: child → parents, parent → children (in file order).
    let mut children: BTreeMap<i64, Vec<(i64, Option<String>)>> = BTreeMap::new();
    if let Some(connections) = doc.node("Connections") {
        for c in connections.all("C") {
            let (Some(child), Some(parent)) = (
                c.props.get(1).and_then(Prop::as_i64),
                c.props.get(2).and_then(Prop::as_i64),
            ) else {
                continue;
            };
            let property = c.props.get(3).and_then(Prop::as_str).map(str::to_string);
            children.entry(parent).or_default().push((child, property));
        }
    }
    let kind_of = |id: i64| by_id.get(&id).map_or("", |n| n.name.as_str());
    let children_of = |id: i64, kind: &'static str| -> Vec<(i64, Option<String>)> {
        children
            .get(&id)
            .map(|list| {
                list.iter()
                    .filter(|(c, _)| kind_of(*c) == kind)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };

    let mut scene = ImportedScene {
        name,
        ..ImportedScene::default()
    };
    // Materials.
    let mut material_index: BTreeMap<i64, usize> = BTreeMap::new();
    for (&id, node) in by_id.iter().filter(|(_, n)| n.name == "Material") {
        let color = node
            .property("DiffuseColor")
            .or_else(|| node.property("Diffuse"))
            .map(numbers)
            .filter(|c| c.len() >= 3)
            .unwrap_or_else(|| vec![0.8, 0.8, 0.8]);
        let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let opacity = scalar(node, "Opacity").unwrap_or_else(|| {
            let factor = scalar(node, "TransparencyFactor").unwrap_or(0.0);
            let tint = node
                .property("TransparentColor")
                .map(numbers)
                .unwrap_or_default();
            let tint = if tint.len() >= 3 {
                (tint[0] + tint[1] + tint[2]) / 3.0
            } else {
                0.0
            };
            1.0 - factor * tint
        });
        // A texture on the diffuse color, and its picture.
        let texture = children_of(id, "Texture")
            .into_iter()
            .find(|(_, p)| {
                p.as_deref().is_none_or(|p| {
                    let p = p.to_ascii_lowercase();
                    p.contains("diffuse") || p.contains("base") || p == "color"
                })
            })
            .and_then(|(t, _)| {
                let texture = by_id.get(&t)?;
                let video = children_of(t, "Video")
                    .first()
                    .and_then(|(v, _)| by_id.get(v).copied());
                let embedded = video.and_then(|v| match v.value("Content") {
                    Some(Prop::Raw(b)) if !b.is_empty() => Some(b.clone()),
                    Some(Prop::Str(s)) if !s.is_empty() => base64_decode(s),
                    _ => None,
                });
                embedded.or_else(|| {
                    let names = [
                        texture.value("RelativeFilename"),
                        video.and_then(|v| v.value("RelativeFilename")),
                        texture.value("FileName"),
                        video.and_then(|v| v.value("Filename")),
                    ];
                    names
                        .into_iter()
                        .flatten()
                        .filter_map(Prop::as_str)
                        .find_map(|f| {
                            let base = f.rsplit(['/', '\\']).next().unwrap_or(f);
                            file(f).or_else(|| file(base))
                        })
                })
            });
        material_index.insert(id, scene.materials.len());
        scene.materials.push(ImportedMaterial {
            name: object_name(node),
            color: Rgba8 {
                r: byte(color[0]),
                g: byte(color[1]),
                b: byte(color[2]),
                a: 255,
            },
            opacity: opacity.clamp(0.0, 1.0) as f32,
            texture,
            roughness: None,
            metallic: None,
        });
    }

    // Models from the root down.
    let mut meshes: BTreeMap<(i64, Vec<Option<usize>>), usize> = BTreeMap::new();
    let mut stack: Vec<(i64, DAffine3, usize)> = children_of(0, "Model")
        .into_iter()
        .rev()
        .map(|(id, _)| (id, DAffine3::IDENTITY, 0))
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    while let Some((id, parent, depth)) = stack.pop() {
        if depth > MAX_DEPTH || !seen.insert(id) {
            continue;
        }
        let Some(model) = by_id.get(&id) else {
            continue;
        };
        let world = parent * local_transform(model);
        for (child, _) in children_of(id, "Model").into_iter().rev() {
            stack.push((child, world, depth + 1));
        }
        if object_kind(model) != "Mesh" {
            continue;
        }
        let materials: Vec<Option<usize>> = children_of(id, "Material")
            .into_iter()
            .map(|(m, _)| material_index.get(&m).copied())
            .collect();
        for (g, _) in children_of(id, "Geometry") {
            let Some(geometry) = by_id.get(&g).filter(|n| object_kind(n) == "Mesh") else {
                continue;
            };
            let key = (g, materials.clone());
            let index = match meshes.get(&key) {
                Some(i) => *i,
                None => {
                    let Some(m) = mesh(geometry, &object_name(model), &materials)? else {
                        continue;
                    };
                    scene.meshes.push(m);
                    meshes.insert(key, scene.meshes.len() - 1);
                    scene.meshes.len() - 1
                }
            };
            scene.nodes.push(ImportedNode {
                name: object_name(model),
                transform: world * geometric_transform(model),
                mesh: index,
            });
        }
    }

    // Units and axes.
    let settings = doc.node("GlobalSettings");
    let cm = settings
        .and_then(|s| scalar(s, "UnitScaleFactor"))
        .filter(|v| *v > 0.0 && v.is_finite())
        .unwrap_or(1.0);
    let mut fix = DAffine3::IDENTITY;
    match unit_of(cm) {
        Some(u) => scene.units = Some(u),
        None => {
            scene.units = Some(LengthUnit::Millimeter);
            fix = DAffine3::from_scale(DVec3::splat(cm * 10.0));
        }
    }
    match swap {
        None => fix = DAffine3::from_mat3(axes(settings)) * fix,
        Some(_) => {
            scene.y_up = settings
                .and_then(|s| scalar(s, "UpAxis"))
                .is_none_or(|v| v as i64 == 1);
        }
    }
    for node in &mut scene.nodes {
        node.transform = fix * node.transform;
    }
    Ok(scene)
}

impl Importer for FbxImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Fbx
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
        if bytes.starts_with(fbx::BINARY_MAGIC) || head.contains("FBXHeaderExtension") {
            Confidence::CERTAIN
        } else if name.to_ascii_lowercase().ends_with(".fbx") {
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
        let doc = fbx::parse(&bytes)?;
        let storage = tx.storage();
        let source = opts.source.clone();
        let mut file = |name: &str| -> Option<Vec<u8>> {
            let key = sibling(&source, name);
            storage.and_then(|s| s.read(&StorageKey::new(&key)).ok())
        };
        let scene = read(
            &doc,
            &mut file,
            source_stem(&opts.source, "FBX"),
            opts.swap_yz,
        )?;
        progress.report(0.5, "building faces");
        build_scene(tx, target, scene, opts)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn rotation_orders_and_pivots_follow_the_transform_chain() {
        // XYZ: X turns first.
        let m = euler(DVec3::new(90.0, 0.0, 90.0), 0);
        let v = m * DVec3::Y;
        assert!(v.distance(DVec3::new(0.0, 0.0, 1.0)) < 1e-12, "{v}");
        // ZYX: Z turns first (+Y to −X), then X leaves −X as it is.
        let v = euler(DVec3::new(90.0, 0.0, 90.0), 5) * DVec3::Y;
        assert!(v.distance(DVec3::NEG_X) < 1e-12, "{v}");
        // A turn about a pivot keeps the pivot in place.
        let p70 = |name: &str, v: [f64; 3]| {
            Node::new(
                "P",
                vec![
                    Prop::Str(name.into()),
                    Prop::Str("Vector3D".into()),
                    Prop::Str(String::new()),
                    Prop::Str("A".into()),
                    Prop::Real(v[0]),
                    Prop::Real(v[1]),
                    Prop::Real(v[2]),
                ],
            )
        };
        let model = Node::new("Model", vec![]).with(
            Node::new("Properties70", vec![])
                .with(p70("Lcl Rotation", [0.0, 0.0, 90.0]))
                .with(p70("RotationPivot", [1.0, 0.0, 0.0])),
        );
        let t = local_transform(&model);
        assert!(t.transform_point3(DVec3::X).distance(DVec3::X) < 1e-12);
        assert!(
            t.transform_point3(DVec3::new(2.0, 0.0, 0.0))
                .distance(DVec3::new(1.0, 1.0, 0.0))
                < 1e-12
        );
    }

    #[test]
    fn y_up_files_turn_z_up() {
        let a = axes(None);
        assert!((a * DVec3::Y).distance(DVec3::Z) < 1e-12);
        assert!((a * DVec3::Z).distance(DVec3::NEG_Y) < 1e-12);
        assert!((a * DVec3::X).distance(DVec3::X) < 1e-12);
        assert_eq!(unit_of(2.54), Some(LengthUnit::Inch));
        assert_eq!(unit_of(0.5), None);
    }

    #[test]
    fn concave_polygons_triangulate_in_their_plane() {
        let p: Vec<DVec3> = [[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [2.0, 1.0], [0.0, 4.0]]
            .iter()
            .map(|[x, y]| DVec3::new(*x, 0.0, *y))
            .collect();
        let t = triangulate(&p, &[0, 1, 2, 3, 4]);
        assert_eq!(t.len(), 3);
        let area: f64 = t
            .iter()
            .map(|[a, b, c]| {
                let (a, b, c) = (p[*a as usize], p[*b as usize], p[*c as usize]);
                (b - a).cross(c - a).length() / 2.0
            })
            .sum();
        assert!((area - 10.0).abs() < 1e-9, "{area}");
    }
}
