//! Implements docs/09-file-formats.md §6 for every mesh importer: the
//! format-neutral scene an importer reads (materials, triangle meshes, nodes
//! placing them) and how it enters the document. Meshes go through the
//! fast path `HalfEdgeMesh::from_triangles_keyed` (coplanar triangles of the
//! same material and texture mapping merge into faces); a mesh placed once
//! becomes a group, a mesh placed several times a component; everything sits
//! in one top-level group (or component, `--as component`) at `at`.

use std::collections::BTreeMap;

use glam::{DAffine2, DAffine3, DMat2, DMat3, DVec2, DVec3};

use crate::commands::params::Target;
use crate::commands::{CommandError, Transaction};
use crate::diagnostics::{Warning, WarningCode};
use crate::document::{
    ComponentDefinition, ComponentInstance, ContainerRef, Document, Entities, EntityRecord,
    FrontOrBack, Group, Material, Rgba8, Texture, UvMap,
};
use crate::geom::mesh::TriangleMergeOptions;
use crate::geom::{FaceSides, HalfEdgeMesh};
use crate::ids::{DefinitionId, MaterialId, Pid};
use crate::io::IoError;
use crate::io::import::{ImportAs, ImportOptions, ImportReport};
use crate::tolerance::Tolerances;
use crate::units::LengthUnit;

/// A material as a file describes it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImportedMaterial {
    /// Name in the file.
    pub name: String,
    /// Base color.
    pub color: Rgba8,
    /// Opacity 0–1.
    pub opacity: f32,
    /// Encoded texture image (PNG, JPEG, WebP), if any.
    pub texture: Option<Vec<u8>>,
}

/// A triangle mesh in source units.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImportedMesh {
    /// Name in the file.
    pub name: String,
    /// Vertex positions.
    pub positions: Vec<DVec3>,
    /// Triangles (indices into `positions`).
    pub triangles: Vec<[u32; 3]>,
    /// Material of each triangle (index into the scene's materials).
    pub materials: Vec<Option<usize>>,
    /// Texture coordinates per position (empty when the file has none).
    pub uvs: Vec<DVec2>,
}

/// A placement of a mesh (world transform in source units).
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedNode {
    /// Name in the file.
    pub name: String,
    /// Mesh-to-world transform.
    pub transform: DAffine3,
    /// Index into the scene's meshes.
    pub mesh: usize,
}

/// What a mesh importer read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImportedScene {
    /// Name of the top-level group (the file stem).
    pub name: String,
    /// Materials.
    pub materials: Vec<ImportedMaterial>,
    /// Meshes.
    pub meshes: Vec<ImportedMesh>,
    /// Placements; when empty, every mesh is placed once as it is.
    pub nodes: Vec<ImportedNode>,
    /// The source is Y-up (`swap_yz` decides when the options do not).
    pub y_up: bool,
    /// The file's own length unit, if it declares one.
    pub units: Option<LengthUnit>,
}

/// Size of one texture repeat for imported texture coordinates, mm. Any
/// value works (the face maps scale with it); this one makes faces without
/// coordinates tile every meter.
const IMPORT_TILE_MM: f64 = 1000.0;

fn io(e: CommandError) -> IoError {
    match e {
        CommandError::Io(io) => io,
        other => IoError::Corrupt {
            part: "import".into(),
            reason: other.to_string(),
        },
    }
}

fn unique_material_name(doc: &Document, name: &str) -> String {
    let base = if name.trim().is_empty() {
        "Material"
    } else {
        name.trim()
    };
    if doc.materials.by_name(base).is_none() {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} #{n}"))
        .find(|c| doc.materials.by_name(c).is_none())
        .unwrap_or_else(|| base.to_string())
}

fn unique_component_name(doc: &Document, name: &str) -> String {
    let base = if name.trim().is_empty() {
        "Component"
    } else {
        name.trim()
    };
    if doc.definition_by_name(base).is_none() {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} #{n}"))
        .find(|c| doc.definition_by_name(c).is_none())
        .unwrap_or_else(|| base.to_string())
}

/// The face-frame → texture millimeter map of one triangle, `None` when its
/// texture coordinates are degenerate.
fn triangle_uv_map(
    points: [DVec3; 3],
    uvs: [DVec2; 3],
    frame: &crate::math::PlaneFrame,
) -> Option<DAffine2> {
    let q = points.map(|p| frame.to_2d(p));
    let geometry = DMat2::from_cols(q[1] - q[0], q[2] - q[0]);
    if geometry.determinant().abs() <= 1e-12 {
        return None;
    }
    let texture = DMat2::from_cols(uvs[1] - uvs[0], uvs[2] - uvs[0]);
    let m = DMat2::from_diagonal(DVec2::splat(IMPORT_TILE_MM)) * texture * geometry.inverse();
    let t = DVec2::splat(IMPORT_TILE_MM) * uvs[0] - m * q[0];
    m.is_finite().then(|| DAffine2::from_mat2_translation(m, t))
}

/// Triangles grouped into connected shells (sharing a corner within
/// tolerance), each in triangle order, shells by their first triangle.
fn shells(positions: &[DVec3], triangles: &[[u32; 3]], tol: &Tolerances) -> Vec<Vec<usize>> {
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut parent: Vec<usize> = (0..triangles.len()).collect();
    let mut owner: BTreeMap<[i64; 3], usize> = BTreeMap::new();
    let cell = tol.length.max(f64::MIN_POSITIVE);
    for (t, tri) in triangles.iter().enumerate() {
        for &i in tri {
            let Some(p) = positions.get(i as usize) else {
                continue;
            };
            let key = [p.x, p.y, p.z].map(|c| (c / cell).round() as i64);
            let other = *owner.entry(key).or_insert(t);
            let (a, b) = (find(&mut parent, t), find(&mut parent, other));
            if a != b {
                parent[a.max(b)] = a.min(b);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for t in 0..triangles.len() {
        let root = find(&mut parent, t);
        groups.entry(root).or_default().push(t);
    }
    groups.into_values().collect()
}

/// Merges the `subset` triangles of `mesh` into faces and paints them with
/// their materials and texture mappings; also returns how many triangles
/// were skipped (degenerate or non-manifold).
#[allow(clippy::too_many_arguments)]
fn build_part(
    positions: &[DVec3],
    mesh: &ImportedMesh,
    subset: &[usize],
    keys: &[u32],
    maps: &[Option<DAffine2>],
    materials: &[Option<MaterialId>],
    opts: &ImportOptions,
    tol: &Tolerances,
) -> Result<(HalfEdgeMesh, usize), IoError> {
    let triangles: Vec<[u32; 3]> = subset.iter().map(|&t| mesh.triangles[t]).collect();
    let part_keys: Vec<u32> = subset.iter().map(|&t| keys[t]).collect();
    let merged = HalfEdgeMesh::from_triangles_keyed(
        positions,
        &triangles,
        &part_keys,
        TriangleMergeOptions {
            merge_coplanar: opts.merge_coplanar,
            soften_angle: opts.soften_angle,
        },
        tol,
    )
    .map_err(|e| IoError::Corrupt {
        part: mesh.name.clone(),
        reason: e.to_string(),
    })?;
    let skipped = merged.degenerate_triangles + merged.non_manifold_edges;
    let mut built = merged.mesh;
    // Materials and texture mappings, from each face's first triangle.
    let mut done = std::collections::BTreeSet::new();
    for (local, face) in merged.triangle_faces.iter().enumerate() {
        let Some(f) = face else { continue };
        if !done.insert(*f) {
            continue;
        }
        let t = subset[local];
        let Some(material) = mesh.materials.get(t).copied().flatten() else {
            continue;
        };
        let Some(&Some(id)) = materials.get(material) else {
            continue;
        };
        let Some(current) = built.face(*f) else {
            continue;
        };
        // A face's map is in its own frame; recompute for its plane.
        let uv = maps[t].and_then(|_| {
            let tri = mesh.triangles[t];
            let p = tri.map(|i| positions[i as usize]);
            let uv = tri.map(|i| mesh.uvs[i as usize]);
            triangle_uv_map(p, uv, &current.plane.frame())
        });
        let sides = FaceSides {
            material_front: Some(id),
            material_back: Some(id),
            uv_front: uv.map(UvMap::Affine),
            uv_back: uv.map(UvMap::Affine),
        };
        let _ = built.set_face_sides(*f, sides, &mut Vec::new());
    }
    Ok((built, skipped))
}

/// Brings `scene` into the target container; returns the report.
pub fn build_scene(
    tx: &mut Transaction<'_>,
    target: &Target,
    scene: ImportedScene,
    opts: &ImportOptions,
) -> Result<ImportReport, IoError> {
    let mut report = ImportReport::default();
    let tol = *tx.tolerances();
    // Source → document: units, scale and, for Y-up sources, Y-up → Z-up.
    let unit = opts.units.or(scene.units).unwrap_or(LengthUnit::Millimeter);
    let factor = unit.millimeters() * opts.scale;
    let swap = opts.swap_yz.unwrap_or(scene.y_up);
    let axes = if swap {
        DMat3::from_cols(DVec3::X, DVec3::Z, -DVec3::Y)
    } else {
        DMat3::IDENTITY
    };
    let convert = DAffine3::from_mat3(axes * factor);
    let to_document = |t: DAffine3| convert * t * convert.inverse();

    // Materials; one named like the document's default material (as
    // exporters write unpainted faces) stays the default.
    let default_name = {
        let doc = tx.document();
        doc.materials
            .get(doc.materials.default_for(FrontOrBack::Front))
            .map(|m| m.name.clone())
    };
    let mut materials: Vec<Option<MaterialId>> = Vec::with_capacity(scene.materials.len());
    let mut textured: Vec<bool> = Vec::with_capacity(scene.materials.len());
    for m in &scene.materials {
        if m.texture.is_none() && default_name.as_deref() == Some(m.name.trim()) {
            materials.push(None);
            textured.push(false);
            continue;
        }
        let mut material = Material::solid(m.name.trim().to_string(), m.color);
        material.opacity = m.opacity.clamp(0.0, 1.0);
        material.color.a = (material.opacity * 255.0).round() as u8;
        if let Some(bytes) = &m.texture {
            match tx.insert_image(bytes.clone()) {
                Ok(image) => {
                    material.texture = Some(Texture {
                        image,
                        size_mm: DVec2::splat(IMPORT_TILE_MM),
                        colorize: None,
                    });
                }
                Err(e) => report.warnings.push(Warning::new(
                    WarningCode::W2004,
                    format!("texture of `{}` skipped: {e}", m.name),
                )),
            }
        }
        textured.push(material.texture.is_some());
        // The same material imported before is reused.
        let doc = tx.document();
        let same = std::iter::once(material.name.clone())
            .chain((2..).map(|n| format!("{} #{n}", material.name)))
            .map_while(|name| doc.materials.by_name(&name))
            .find(|&id| {
                doc.materials.get(id).is_some_and(|old| {
                    old.color == material.color
                        && (old.opacity - material.opacity).abs() <= f32::EPSILON
                        && old.texture == material.texture
                        && old.is_default_for.is_none()
                })
            });
        if let Some(id) = same {
            materials.push(Some(id));
            continue;
        }
        material.name = unique_material_name(doc, &material.name);
        materials.push(Some(tx.insert_material(material).map_err(io)?));
    }

    // Placements: given nodes, or each mesh once.
    let nodes: Vec<ImportedNode> = if scene.nodes.is_empty() {
        scene
            .meshes
            .iter()
            .enumerate()
            .map(|(i, m)| ImportedNode {
                name: m.name.clone(),
                transform: DAffine3::IDENTITY,
                mesh: i,
            })
            .collect()
    } else {
        scene.nodes.clone()
    };
    let mut uses: BTreeMap<usize, usize> = BTreeMap::new();
    for n in &nodes {
        *uses.entry(n.mesh).or_default() += 1;
    }
    // The top-level group or component; one mesh placed as is becomes it.
    let as_component = matches!(opts.import_as, ImportAs::Component(_));
    let top_name = match &opts.import_as {
        ImportAs::Component(name) => unique_component_name(tx.document(), name),
        _ => format!(
            "Group {}",
            crate::commands::common::group_number(tx.document())
        ),
    };
    let single = nodes.len() == 1
        && nodes[0].transform == DAffine3::IDENTITY
        && uses.get(&nodes[0].mesh) == Some(&1);

    // One definition per used mesh.
    let mut definitions: BTreeMap<usize, DefinitionId> = BTreeMap::new();
    for (&mesh_index, &count) in &uses {
        let Some(mesh) = scene.meshes.get(mesh_index) else {
            continue;
        };
        let positions: Vec<DVec3> = mesh
            .positions
            .iter()
            .map(|p| convert.transform_point3(*p))
            .collect();
        // Triangles merge only with the same material and texture mapping.
        let mut maps: Vec<Option<DAffine2>> = vec![None; mesh.triangles.len()];
        let mut keys: Vec<u32> = Vec::with_capacity(mesh.triangles.len());
        let mut key_ids: BTreeMap<(Option<usize>, [i64; 6]), u32> = BTreeMap::new();
        for (t, tri) in mesh.triangles.iter().enumerate() {
            let material = mesh.materials.get(t).copied().flatten();
            let mut quantized = [0i64; 6];
            let wants_uv = material.is_some_and(|m| textured.get(m).copied().unwrap_or(false));
            if wants_uv && mesh.uvs.len() == mesh.positions.len() {
                let p = tri.map(|i| positions.get(i as usize).copied().unwrap_or_default());
                if let Some(plane) = crate::math::Plane::from_points(p[0], p[1], p[2], &tol) {
                    let uv = tri.map(|i| mesh.uvs[i as usize]);
                    if let Some(map) = triangle_uv_map(p, uv, &plane.frame()) {
                        let c = map.to_cols_array();
                        for k in 0..6 {
                            quantized[k] = (c[k] * 1e4).round() as i64;
                        }
                        maps[t] = Some(map);
                    }
                }
            }
            let next = key_ids.len() as u32;
            keys.push(*key_ids.entry((material, quantized)).or_insert(next));
        }
        let ids: Vec<Option<MaterialId>> = materials.clone();
        let part =
            |subset: &[usize]| build_part(&positions, mesh, subset, &keys, &maps, &ids, opts, &tol);
        let all: Vec<usize> = (0..mesh.triangles.len()).collect();
        let (whole, mut skipped) = part(&all)?;
        // Overlapping shells in one mesh cross each other: then each
        // connected shell becomes a group, and a shell that crosses itself
        // is left out.
        let pieces: Vec<HalfEdgeMesh> = if whole.edge_crossings(&tol).is_empty() {
            vec![whole]
        } else {
            skipped = 0;
            let mut pieces = Vec::new();
            let mut crossing = 0usize;
            for shell in shells(&positions, &mesh.triangles, &tol) {
                let (piece, bad) = part(&shell)?;
                if piece.edge_crossings(&tol).is_empty() {
                    skipped += bad;
                    if piece.face_count() > 0 {
                        pieces.push(piece);
                    }
                } else {
                    crossing += shell.len();
                }
            }
            if crossing > 0 {
                report.warnings.push(Warning::new(
                    WarningCode::W2004,
                    format!(
                        "`{}`: {crossing} self-intersecting triangles skipped",
                        mesh.name
                    ),
                ));
                report.skipped += crossing;
            }
            pieces
        };
        if skipped > 0 {
            report.warnings.push(Warning::new(
                WarningCode::W2004,
                format!(
                    "`{}`: {skipped} degenerate or non-manifold triangles skipped",
                    mesh.name
                ),
            ));
        }
        for piece in &pieces {
            report.faces += piece.face_count();
            report.edges += piece.edge_count();
        }
        let shared = count > 1;
        let (name, internal) = if single {
            (top_name.clone(), !as_component)
        } else if shared {
            (unique_component_name(tx.document(), &mesh.name), false)
        } else {
            (
                format!(
                    "Group {}",
                    crate::commands::common::group_number(tx.document())
                ),
                true,
            )
        };
        let mut pieces = pieces.into_iter();
        let entities = match (pieces.len(), pieces.next()) {
            (1, Some(only)) => Entities::from_mesh(only, || tx.allocate_pid()),
            (_, first) => {
                // One group per shell.
                let mut groups = Vec::new();
                for piece in first.into_iter().chain(pieces) {
                    let group_name = format!(
                        "Group {}",
                        crate::commands::common::group_number(tx.document())
                    );
                    let entities = Entities::from_mesh(piece, || tx.allocate_pid());
                    let uuid = crate::commands::common::definition_uuid_of(
                        tx.document(),
                        &group_name,
                        &[
                            bytemuck::cast_slice(&positions),
                            bytemuck::cast_slice(&mesh.triangles),
                            &entities.order().len().to_le_bytes(),
                        ],
                    );
                    let mut def = ComponentDefinition::new(uuid, group_name.clone());
                    def.is_internal_group = true;
                    def.entities = entities;
                    groups.push((tx.insert_definition(def).map_err(io)?, group_name));
                }
                let mut entities = Entities::new();
                for (definition, name) in groups {
                    let pid = tx.allocate_pid();
                    entities
                        .insert(
                            EntityRecord::Group(Group {
                                definition,
                                transform: DAffine3::IDENTITY,
                                name,
                            }),
                            pid,
                        )
                        .map_err(|e| IoError::Corrupt {
                            part: mesh.name.clone(),
                            reason: e.to_string(),
                        })?;
                }
                entities
            }
        };
        // The source data names the contents (cheaper than encoding them).
        let uuid = crate::commands::common::definition_uuid_of(
            tx.document(),
            &name,
            &[
                bytemuck::cast_slice(&positions),
                bytemuck::cast_slice(&mesh.triangles),
                &entities.order().len().to_le_bytes(),
            ],
        );
        let mut def = ComponentDefinition::new(uuid, name);
        def.is_internal_group = internal;
        def.entities = entities;
        definitions.insert(mesh_index, tx.insert_definition(def).map_err(io)?);
    }

    // The top-level definition holding every placement.
    let top = if single {
        definitions[&nodes[0].mesh]
    } else {
        let mut def = ComponentDefinition::new(
            crate::commands::common::definition_uuid(tx.document(), &top_name, &Entities::new()),
            top_name.clone(),
        );
        def.is_internal_group = !as_component;
        tx.allocate_pid();
        let top = tx.insert_definition(def).map_err(io)?;
        for node in &nodes {
            let Some(&d) = definitions.get(&node.mesh) else {
                continue;
            };
            let transform = to_document(node.transform);
            let shared = uses.get(&node.mesh).copied().unwrap_or(0) > 1;
            let record = if shared {
                EntityRecord::Instance(ComponentInstance {
                    definition: d,
                    transform,
                    name: (!node.name.is_empty()).then(|| node.name.clone()),
                    material_override: None,
                    glued_to: None,
                    dynamic_attrs: Default::default(),
                })
            } else {
                EntityRecord::Group(Group {
                    definition: d,
                    transform,
                    name: node.name.clone(),
                })
            };
            tx.insert_entity(ContainerRef::Definition(top), record)
                .map_err(io)?;
        }
        top
    };
    let placement = target.local * DAffine3::from_translation(opts.at);
    let record = if as_component {
        EntityRecord::Instance(ComponentInstance {
            definition: top,
            transform: placement,
            name: None,
            material_override: None,
            glued_to: None,
            dynamic_attrs: Default::default(),
        })
    } else {
        EntityRecord::Group(Group {
            definition: top,
            transform: placement,
            name: scene.name.clone(),
        })
    };
    let (_, pid): (_, Pid) = tx.insert_entity(target.container, record).map_err(io)?;
    report.created.push(pid);
    Ok(report)
}
