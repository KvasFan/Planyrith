//! Implements docs/09-file-formats.md §5 FBX export (binary 7.4; docs/09
//! Decisions, FBX): one geometry per container (and material override),
//! shared by every occurrence whose transform is a rotation and scale; one
//! model per occurrence with its translation, rotation and scaling; lambert
//! materials with the color, opacity and an embedded texture; Y up (or Z up
//! with `z_up`) and the unit declared in the global settings.

use std::collections::BTreeMap;
use std::io::Write;

use glam::{DAffine3, DMat3, DVec3};

use crate::document::selection::occurrence_name;
use crate::document::{ContainerRef, Document, FrontOrBack, ImageFormat};
use crate::ids::MaterialId;
use crate::io::export::scene::{ExportedMesh, collect};
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter};
use crate::io::fbx::{CREATION_TIME, FILE_ID, Node, Prop, WRITE_VERSION, write_binary};
use crate::io::{IoError, Progress};

/// The FBX exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct FbxExporter;

fn s(text: &str) -> Prop {
    Prop::Str(text.into())
}

/// `Name\0\x01Class`, the binary encoding's object name.
fn named(name: &str, class: &str) -> Prop {
    Prop::Str(format!("{}\0\u{1}{class}", name.replace('\0', "")))
}

fn leaf(name: &str, value: Prop) -> Node {
    Node::new(name, vec![value])
}

/// A `Properties70` entry.
fn p(name: &str, kind: &str, label: &str, flags: &str, values: Vec<Prop>) -> Node {
    let mut props = vec![s(name), s(kind), s(label), s(flags)];
    props.extend(values);
    Node::new("P", props)
}

fn vector(name: &str, v: DVec3) -> Node {
    p(
        name,
        name,
        "",
        "A",
        vec![Prop::Real(v.x), Prop::Real(v.y), Prop::Real(v.z)],
    )
}

fn round(v: f64) -> f64 {
    let r = (v * 1e9).round() / 1e9;
    r + 0.0
}

/// `L = R · S` with `R` a rotation (Euler XYZ, degrees) and `S` a scale,
/// when `L` has no shear.
fn rotation_scale(l: DMat3) -> Option<(DVec3, DVec3)> {
    let mut scale = DVec3::new(l.x_axis.length(), l.y_axis.length(), l.z_axis.length());
    if scale.min_element() < 1e-12 {
        return None;
    }
    if l.determinant() < 0.0 {
        scale.x = -scale.x;
    }
    let r = DMat3::from_cols(l.x_axis / scale.x, l.y_axis / scale.y, l.z_axis / scale.z);
    let rt_r = r.transpose() * r;
    if !rt_r.abs_diff_eq(DMat3::IDENTITY, 1e-9) {
        return None;
    }
    // R = Rz · Ry · Rx.
    let sy = (-r.x_axis.z).clamp(-1.0, 1.0);
    let y = libm::asin(sy);
    let (x, z) = if sy.abs() < 1.0 - 1e-12 {
        (
            libm::atan2(r.y_axis.z, r.z_axis.z),
            libm::atan2(r.x_axis.y, r.x_axis.x),
        )
    } else {
        // Gimbal lock: X and Z turn about one axis; put it all in Z.
        (0.0, libm::atan2(-r.y_axis.x, r.y_axis.y))
    };
    Some((
        DVec3::new(x, y, z).map(|a| round(a.to_degrees())),
        scale.map(round),
    ))
}

/// One written geometry: its node and the materials of its polygons.
struct Geometry {
    node: Node,
    materials: Vec<MaterialId>,
    triangles: usize,
}

fn geometry(
    doc: &Document,
    id: i64,
    name: &str,
    mesh: &ExportedMesh,
    override_material: Option<MaterialId>,
    bake: DAffine3,
) -> Geometry {
    let normal_matrix = bake.matrix3.inverse().transpose();
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut polygon_materials = Vec::new();
    let mut materials: Vec<MaterialId> = Vec::new();
    let mut triangles = 0;
    for face in &mesh.faces {
        let material = doc
            .materials
            .resolve(face.material.or(override_material), FrontOrBack::Front);
        let slot = match materials.iter().position(|m| *m == material) {
            Some(i) => i,
            None => {
                materials.push(material);
                materials.len() - 1
            }
        };
        let base = vertices.len() / 3;
        for (k, p) in face.positions.iter().enumerate() {
            let q = bake.transform_point3(*p);
            vertices.extend([round(q.x), round(q.y), round(q.z)]);
            let uv = face.uvs.get(k).copied().unwrap_or_default();
            uvs.extend([round(uv.x), round(uv.y)]);
        }
        let n = (normal_matrix * face.normal).normalize_or_zero();
        for t in &face.triangles {
            let [a, b, c] = t.map(|i| (base + i as usize) as i64);
            indices.extend([a, b, !c]);
            for _ in 0..3 {
                normals.extend([round(n.x), round(n.y), round(n.z)]);
            }
            polygon_materials.push(slot as i64);
            triangles += 1;
        }
    }
    let uv_index: Vec<i64> = indices
        .iter()
        .map(|&i| if i < 0 { !i } else { i })
        .collect();
    let layer = |kind: &str| {
        Node::new("LayerElement", vec![])
            .with(leaf("Type", s(kind)))
            .with(leaf("TypedIndex", Prop::Int(0)))
    };
    let node = Node::new(
        "Geometry",
        vec![Prop::Int(id), named(name, "Geometry"), s("Mesh")],
    )
    .with(Node::new("Properties70", vec![]))
    .with(leaf("GeometryVersion", Prop::Int(124)))
    .with(leaf("Vertices", Prop::Reals(vertices)))
    .with(leaf("PolygonVertexIndex", Prop::Ints(indices)))
    .with(
        Node::new("LayerElementNormal", vec![Prop::Int(0)])
            .with(leaf("Version", Prop::Int(101)))
            .with(leaf("Name", s("")))
            .with(leaf("MappingInformationType", s("ByPolygonVertex")))
            .with(leaf("ReferenceInformationType", s("Direct")))
            .with(leaf("Normals", Prop::Reals(normals))),
    )
    .with(
        Node::new("LayerElementUV", vec![Prop::Int(0)])
            .with(leaf("Version", Prop::Int(101)))
            .with(leaf("Name", s("UVMap")))
            .with(leaf("MappingInformationType", s("ByPolygonVertex")))
            .with(leaf("ReferenceInformationType", s("IndexToDirect")))
            .with(leaf("UV", Prop::Reals(uvs)))
            .with(leaf("UVIndex", Prop::Ints(uv_index))),
    )
    .with(
        Node::new("LayerElementMaterial", vec![Prop::Int(0)])
            .with(leaf("Version", Prop::Int(101)))
            .with(leaf("Name", s("")))
            .with(leaf("MappingInformationType", s("ByPolygon")))
            .with(leaf("ReferenceInformationType", s("IndexToDirect")))
            .with(leaf("Materials", Prop::Ints(polygon_materials))),
    )
    .with(
        Node::new("Layer", vec![Prop::Int(0)])
            .with(leaf("Version", Prop::Int(100)))
            .with(layer("LayerElementNormal"))
            .with(layer("LayerElementMaterial"))
            .with(layer("LayerElementUV")),
    );
    Geometry {
        node,
        materials,
        triangles,
    }
}

impl Exporter for FbxExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Fbx
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
        let unit = 1.0 / opts.units.millimeters();
        let axes = if opts.z_up {
            DMat3::IDENTITY
        } else {
            // (x, y, z) → (x, z, −y): Z-up to Y-up.
            DMat3::from_cols(DVec3::X, -DVec3::Z, DVec3::Y)
        };
        let offset = DVec3::from_array(opts.offset_mm);
        let mut next_id: i64 = 1_000_000;
        let mut id = || {
            next_id += 1;
            next_id
        };
        let mut objects = Node::new("Objects", vec![]);
        let mut connections = Node::new("Connections", vec![]);
        let mut connect = |kind: &str, child: i64, parent: i64, property: Option<&str>| {
            let mut props = vec![s(kind), Prop::Int(child), Prop::Int(parent)];
            props.extend(property.map(s));
            connections.children.push(Node::new("C", props));
        };
        let mut shared: BTreeMap<(ContainerRef, Option<MaterialId>), (i64, Vec<MaterialId>)> =
            BTreeMap::new();
        let mut material_ids: BTreeMap<MaterialId, i64> = BTreeMap::new();
        let (mut models, mut geometries, mut triangles) = (0, 0, 0);
        for occ in &scene.occurrences {
            let Some(mesh) = scene.mesh(occ.container) else {
                continue;
            };
            if mesh.faces.is_empty() {
                continue;
            }
            let name = occurrence_name(doc, &occ.path);
            let linear = axes * occ.world.matrix3;
            let translation = axes * ((occ.world.translation + offset) * unit);
            let parts = rotation_scale(linear);
            // A transform with shear cannot be a model's: its geometry is
            // written placed, with the model at the origin.
            let (geometry_id, materials, rotation, scale) = match parts {
                Some((rotation, scale)) => {
                    let key = (occ.container, occ.material_override);
                    let entry = match shared.get(&key) {
                        Some(e) => e.clone(),
                        None => {
                            let g = geometry(
                                doc,
                                id(),
                                &name,
                                mesh,
                                occ.material_override,
                                DAffine3::from_scale(DVec3::splat(unit)),
                            );
                            triangles += g.triangles;
                            geometries += 1;
                            let gid = g.node.props[0].as_i64().unwrap_or(0);
                            objects.children.push(g.node);
                            shared.insert(key, (gid, g.materials.clone()));
                            (gid, g.materials)
                        }
                    };
                    (entry.0, entry.1, rotation, scale)
                }
                None => {
                    let bake =
                        DAffine3::from_mat3(linear) * DAffine3::from_scale(DVec3::splat(unit));
                    let g = geometry(doc, id(), &name, mesh, occ.material_override, bake);
                    triangles += g.triangles;
                    geometries += 1;
                    let gid = g.node.props[0].as_i64().unwrap_or(0);
                    objects.children.push(g.node);
                    (gid, g.materials, DVec3::ZERO, DVec3::ONE)
                }
            };
            let model_id = id();
            objects.children.push(
                Node::new(
                    "Model",
                    vec![Prop::Int(model_id), named(&name, "Model"), s("Mesh")],
                )
                .with(leaf("Version", Prop::Int(232)))
                .with(
                    Node::new("Properties70", vec![])
                        .with(vector("Lcl Translation", translation.map(round)))
                        .with(vector("Lcl Rotation", rotation))
                        .with(vector("Lcl Scaling", scale)),
                )
                .with(leaf("Shading", Prop::Bool(true)))
                .with(leaf("Culling", s("CullingOff"))),
            );
            models += 1;
            connect("OO", model_id, 0, None);
            connect("OO", geometry_id, model_id, None);
            for m in materials {
                let next = material_ids.len();
                let mid = *material_ids
                    .entry(m)
                    .or_insert_with(|| 2_000_000_000 + next as i64);
                connect("OO", mid, model_id, None);
            }
        }
        progress.report(0.6, "writing materials");
        let mut textures = 0;
        for (&material, &mid) in &material_ids {
            let m = doc.materials.get(material);
            let name = m.map_or("Material".to_string(), |m| m.name.clone());
            let c = m.map_or([255, 255, 255], |m| [m.color.r, m.color.g, m.color.b]);
            let color = c.map(|v| Prop::Real(round(f64::from(v) / 255.0)));
            let opacity = m.map_or(1.0, |m| f64::from(m.opacity.clamp(0.0, 1.0)));
            objects.children.push(
                Node::new(
                    "Material",
                    vec![Prop::Int(mid), named(&name, "Material"), s("")],
                )
                .with(leaf("Version", Prop::Int(102)))
                .with(leaf("ShadingModel", s("lambert")))
                .with(leaf("MultiLayer", Prop::Int(0)))
                .with(
                    Node::new("Properties70", vec![])
                        .with(p("DiffuseColor", "Color", "", "A", color.to_vec()))
                        .with(p("DiffuseFactor", "Number", "", "A", vec![Prop::Real(1.0)]))
                        .with(p(
                            "TransparentColor",
                            "Color",
                            "",
                            "A",
                            vec![Prop::Real(1.0), Prop::Real(1.0), Prop::Real(1.0)],
                        ))
                        .with(p(
                            "TransparencyFactor",
                            "Number",
                            "",
                            "A",
                            vec![Prop::Real(round(1.0 - opacity))],
                        ))
                        .with(p(
                            "Opacity",
                            "double",
                            "Number",
                            "",
                            vec![Prop::Real(round(opacity))],
                        )),
                ),
            );
            let Some((image, _)) = m
                .and_then(|m| m.texture.as_ref())
                .and_then(|t| doc.images.get(t.image).map(|img| (img, t)))
            else {
                continue;
            };
            let ext = match image.format {
                ImageFormat::Png => "png",
                ImageFormat::Jpeg => "jpg",
                ImageFormat::WebP => "webp",
            };
            let file: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let file = format!("{file}.{ext}");
            let (tid, vid) = (id(), id());
            objects.children.push(
                Node::new(
                    "Video",
                    vec![Prop::Int(vid), named(&name, "Video"), s("Clip")],
                )
                .with(leaf("Type", s("Clip")))
                .with(Node::new("Properties70", vec![]).with(p(
                    "Path",
                    "KString",
                    "XRefUrl",
                    "",
                    vec![s(&file)],
                )))
                .with(leaf("UseMipMap", Prop::Int(0)))
                .with(leaf("Filename", s(&file)))
                .with(leaf("RelativeFilename", s(&file)))
                .with(leaf("Content", Prop::Raw(image.bytes.clone()))),
            );
            objects.children.push(
                Node::new(
                    "Texture",
                    vec![Prop::Int(tid), named(&name, "Texture"), s("")],
                )
                .with(leaf("Type", s("TextureVideoClip")))
                .with(leaf("Version", Prop::Int(202)))
                .with(leaf("TextureName", named(&name, "Texture")))
                .with(Node::new("Properties70", vec![]).with(p(
                    "UseMaterial",
                    "bool",
                    "",
                    "",
                    vec![Prop::Int(1)],
                )))
                .with(leaf("Media", named(&name, "Video")))
                .with(leaf("FileName", s(&file)))
                .with(leaf("RelativeFilename", s(&file))),
            );
            connect("OO", vid, tid, None);
            connect("OP", tid, mid, Some("DiffuseColor"));
            textures += 1;
        }

        let (up, front, front_sign) = if opts.z_up { (2, 1, -1) } else { (1, 2, 1) };
        let int = |name: &str, v: i64| p(name, "int", "Integer", "", vec![Prop::Int(v)]);
        let cm = opts.units.millimeters() / 10.0;
        let settings = Node::new("GlobalSettings", vec![])
            .with(leaf("Version", Prop::Int(1000)))
            .with(
                Node::new("Properties70", vec![])
                    .with(int("UpAxis", up))
                    .with(int("UpAxisSign", 1))
                    .with(int("FrontAxis", front))
                    .with(int("FrontAxisSign", front_sign))
                    .with(int("CoordAxis", 0))
                    .with(int("CoordAxisSign", 1))
                    .with(int("OriginalUpAxis", up))
                    .with(int("OriginalUpAxisSign", 1))
                    .with(p(
                        "UnitScaleFactor",
                        "double",
                        "Number",
                        "",
                        vec![Prop::Real(cm)],
                    ))
                    .with(p(
                        "OriginalUnitScaleFactor",
                        "double",
                        "Number",
                        "",
                        vec![Prop::Real(cm)],
                    )),
            );
        let count = |kind: &str, n: usize| {
            Node::new("ObjectType", vec![s(kind)]).with(leaf("Count", Prop::Int(n as i64)))
        };
        let materials = material_ids.len();
        let definitions = Node::new("Definitions", vec![])
            .with(leaf("Version", Prop::Int(100)))
            .with(leaf(
                "Count",
                Prop::Int((1 + models + geometries + materials + 2 * textures) as i64),
            ))
            .with(count("GlobalSettings", 1))
            .with(count("Model", models))
            .with(count("Geometry", geometries))
            .with(count("Material", materials))
            .with(count("Texture", textures))
            .with(count("Video", textures));
        let creator = format!("Planyrith {}", env!("CARGO_PKG_VERSION"));
        let stamp = Node::new("CreationTimeStamp", vec![])
            .with(leaf("Version", Prop::Int(1000)))
            .with(leaf("Year", Prop::Int(1970)))
            .with(leaf("Month", Prop::Int(1)))
            .with(leaf("Day", Prop::Int(1)))
            .with(leaf("Hour", Prop::Int(10)))
            .with(leaf("Minute", Prop::Int(0)))
            .with(leaf("Second", Prop::Int(0)))
            .with(leaf("Millisecond", Prop::Int(0)));
        let header = Node::new("FBXHeaderExtension", vec![])
            .with(leaf("FBXHeaderVersion", Prop::Int(1003)))
            .with(leaf("FBXVersion", Prop::Int(i64::from(WRITE_VERSION))))
            .with(leaf("EncryptionType", Prop::Int(0)))
            .with(stamp)
            .with(leaf("Creator", s(&creator)));
        let documents = Node::new("Documents", vec![])
            .with(leaf("Count", Prop::Int(1)))
            .with(
                Node::new(
                    "Document",
                    vec![Prop::Int(1_000_000), s("Scene"), s("Scene")],
                )
                .with(Node::new("Properties70", vec![]))
                .with(leaf("RootNode", Prop::Int(0))),
            );
        let nodes = vec![
            header,
            leaf("FileId", Prop::Raw(FILE_ID.to_vec())),
            leaf("CreationTime", s(CREATION_TIME)),
            leaf("Creator", s(&creator)),
            settings,
            documents,
            Node::new("References", vec![]),
            definitions,
            objects,
            connections,
            Node::new("Takes", vec![]).with(leaf("Current", s(""))),
        ];
        let bytes = write_binary(&nodes);
        dst.write_all(&bytes).map_err(|e| IoError::Storage {
            key: "fbx".into(),
            reason: e.to_string(),
        })?;
        Ok(ExportReport {
            bytes_written: bytes.len() as u64,
            entities: triangles,
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn transforms_split_into_rotation_and_scale() {
        let r = DMat3::from_rotation_z(0.3)
            * DMat3::from_rotation_y(-1.1)
            * DMat3::from_rotation_x(2.0);
        let l = r * DMat3::from_diagonal(DVec3::new(2.0, 3.0, 0.5));
        let (deg, scale) = rotation_scale(l).unwrap();
        assert!(scale.distance(DVec3::new(2.0, 3.0, 0.5)) < 1e-8);
        let rad = deg * (std::f64::consts::PI / 180.0);
        let back = DMat3::from_rotation_z(rad.z)
            * DMat3::from_rotation_y(rad.y)
            * DMat3::from_rotation_x(rad.x);
        assert!(back.abs_diff_eq(r, 1e-8));
        // A mirror is a negative scale; a shear has no split.
        let (_, scale) = rotation_scale(DMat3::from_diagonal(DVec3::new(-1.0, 1.0, 1.0))).unwrap();
        assert!(scale.x < 0.0);
        let shear = DMat3::from_cols(DVec3::X, DVec3::new(0.5, 1.0, 0.0), DVec3::Z);
        assert!(rotation_scale(shear).is_none());
    }
}
