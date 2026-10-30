//! Maps editable core data into real SKP model objects (docs/09 §5).
use super::{Result, to_transform};
use glam::{DVec2, DVec3};
use openskp::{GeometryRun, Model, Owner};
use planyrith_core::document::*;
use planyrith_core::ids::{DefinitionId, EntityId, MaterialId, Pid, TagId, VertexId};
use std::collections::BTreeMap;

fn pid(p: Option<Pid>) -> Result<u32> {
    match p {
        Some(p) => Ok(u32::try_from(p.raw())?),
        None => Ok(0),
    }
}
pub(super) fn camera(c: &Camera) -> openskp::Camera {
    let (perspective, two_point, fov, height) = match c.projection {
        Projection::Parallel { height_mm } => (false, false, 35.0, height_mm / 1000.0),
        Projection::Perspective { fov_y_deg } => (true, false, fov_y_deg, 1.0),
        Projection::TwoPoint { fov_y_deg } => (true, true, fov_y_deg, 1.0),
    };
    openskp::Camera {
        eye_m: (c.eye / 1000.0).to_array(),
        target_m: (c.target / 1000.0).to_array(),
        up: c.up.to_array(),
        perspective,
        fov_deg: fov,
        parallel_height_m: height,
        two_point_perspective: two_point,
        description: String::new(),
        undecoded: [1.0, 1000.0],
        aspect_ratio: c.aspect,
        scale_2d: 1.0,
        offset_2d: [0.0; 2],
    }
}
fn value(v: &Value) -> Result<openskp::Value> {
    use openskp::Value as V;
    Ok(match v {
        Value::Bool(v)=>V::Bool(*v),Value::Int(v)=>V::Int(i32::try_from(*v).map_err(|_|"attribute integer exceeds SKP signed 32-bit range")?),
        Value::Float(v)=>V::Double(*v),Value::Str(v)=>V::Text(v.clone()),
        Value::Vec3(v)=>V::Vector(v.to_array()),Value::Array(v)=>V::Array(v.iter().map(value).collect::<Result<_>>()?),
        Value::Bytes(v) if v.len()==8 => {let mut b=[0;8];b.copy_from_slice(v);V::Bytes8(b)},
        _=>return Err("attribute type cannot be represented in SKP (map, color, entity reference or arbitrary byte array)".into()),
    })
}
fn attributes(
    model: &mut Model,
    owner: Owner,
    attrs: &AttributeDictionaries,
    warnings: &mut Vec<String>,
) {
    let dictionaries = attrs
        .0
        .iter()
        .map(|(name, entries)| openskp::Dictionary {
            name: name.clone(),
            entries: entries
                .iter()
                .filter_map(|(k, v)| match value(v) {
                    Ok(v) => Some((k.clone(), v)),
                    Err(e) => {
                        warnings.push(format!("Attribute {name}.{k}: {e}"));
                        None
                    }
                })
                .collect(),
            flag: 0,
        })
        .collect::<Vec<_>>();
    if !dictionaries.is_empty() {
        model.dictionaries.push(openskp::Attributes {
            owner,
            dictionaries,
        });
    }
}
fn uv(
    map: Option<&UvMap>,
    p: DVec3,
    v: VertexId,
    plane: planyrith_core::math::Plane,
    size: DVec2,
) -> DVec2 {
    let q = plane.frame().to_2d(p);
    match map {
        Some(UvMap::Fixed(pins)) => pins
            .iter()
            .find(|(id, _)| *id == v)
            .map_or(q, |(_, t)| *t * size),
        Some(UvMap::Affine(a)) => a.transform_point2(q),
        Some(UvMap::Projected { plane, affine }) => affine.transform_point2(plane.frame().to_2d(p)),
        Some(UvMap::Projective(m)) => {
            let h = *m * q.extend(1.0);
            h.truncate() / h.z
        }
        None => q,
    }
}
fn layer(entities: &Entities, id: EntityId, tags: &BTreeMap<TagId, u16>) -> u16 {
    entities
        .tag_of(id)
        .and_then(|t| tags.get(&t).copied())
        .unwrap_or(0)
}

pub(super) fn model(doc: &Document) -> Result<(Model, Vec<String>)> {
    doc.nesting_depth()?;
    let report = planyrith_core::diagnostics::validate(doc);
    if report.has_errors() {
        return Err("cannot export an invalid document to SKP".into());
    }
    let mut warnings = Vec::new();
    if !doc.sheets.is_empty() {
        warnings.push(
            "Paper-space Sheets are not stored in SKP; save a .plyr copy to preserve them".into(),
        );
    }
    warnings.push("SKP conversion does not preserve edit history, rendering styles, shadow settings or watermarks; keep a .plyr copy".into());
    let mut model = Model {
        camera: Some(camera(&doc.camera)),
        ..Default::default()
    };
    use planyrith_core::units::LengthUnit;
    model.units.length_unit = match doc.info.display_units {
        LengthUnit::Inch | LengthUnit::ArchitecturalFraction | LengthUnit::FeetInches => 0,
        LengthUnit::Foot => 1,
        LengthUnit::Millimeter => 2,
        LengthUnit::Centimeter => 3,
        LengthUnit::Meter => 4,
    };
    model.units.length_format = match doc.info.display_units {
        LengthUnit::FeetInches => 1,
        LengthUnit::ArchitecturalFraction => 3,
        _ => 0,
    };
    model.units.length_precision = u32::from(doc.info.precision);
    model.units.angle_precision = u32::from(doc.info.angle_precision);
    model.units.length_snap = doc.info.length_snap.is_some();
    model.units.length_snap_length = doc
        .info
        .length_snap
        .map_or(1.0, |l| l.as_mm() / super::unit_mm(model.units.length_unit));
    model.units.angle_snap = doc.info.angle_snap.is_some();
    model.units.snap_angle = doc.info.angle_snap.map_or(15.0, |a| a.to_degrees());
    let axes = doc.info.axes;
    model.axes = Some(openskp::Axes {
        origin_m: (axes.translation / 1000.0).to_array(),
        x: axes.matrix3.x_axis.to_array(),
        y: axes.matrix3.y_axis.to_array(),
        z: axes.matrix3.z_axis.to_array(),
    });
    let mut materials = BTreeMap::<MaterialId, u16>::new();
    for (id, m) in doc
        .materials
        .iter()
        .filter(|(_, m)| m.is_default_for.is_none())
    {
        let slot =
            u16::try_from(model.materials.len() + 1).map_err(|_| "too many SKP materials")?;
        materials.insert(id, slot);
        model.material_links.push((slot, model.materials.len()));
        let c = [m.color.r, m.color.g, m.color.b, m.color.a];
        let material = if let Some(t) = &m.texture {
            let image = doc.images.get(t.image).ok_or("missing texture image")?;
            let (image_bytes, extension) = if image.format == ImageFormat::WebP {
                let decoded = image::load_from_memory(&image.bytes)?;
                let mut png = std::io::Cursor::new(Vec::new());
                decoded.write_to(&mut png, image::ImageFormat::Png)?;
                (png.into_inner(), "png")
            } else {
                (image.bytes.clone(), image.format.extension())
            };
            if t.colorize.is_some() {
                warnings.push(format!(
                    "Material {}: texture tint is not transferred",
                    m.name
                ));
            }
            openskp::Material::Textured {
                name: m.name.clone(),
                texture: Some(format!("{}.{}", t.image.to_hex(), extension)),
                applied_size_in: Some((t.size_mm.x / 25.4, t.size_mm.y / 25.4)),
                jpeg_quality: None,
                image_bytes: Some(image_bytes),
                avg_rgba: Some(c),
                colorized: false,
                texture_avg_rgba: Some(c),
                colorize_type: 0,
                opacity: f64::from(m.opacity),
            }
        } else {
            openskp::Material::Solid {
                name: m.name.clone(),
                rgba: c,
                opacity: f64::from(m.opacity),
                colorize_type: 0,
            }
        };
        attributes(
            &mut model,
            Owner::Material(usize::from(slot) - 1),
            &m.attrs,
            &mut warnings,
        );
        model.materials.push(material);
    }
    let mut tags = BTreeMap::<TagId, u16>::new();
    for (id, t) in doc.tags.iter() {
        let slot = u16::try_from(model.layers.len()).map_err(|_| "too many SKP tags")?;
        tags.insert(id, slot);
        model.layer_links.push((slot, usize::from(slot)));
        model.layers.push(openskp::Layer {
            name: t.name.clone(),
            visible: t.visible,
            rgba: [t.color.r, t.color.g, t.color.b, t.color.a],
        });
    }
    let mut definitions = BTreeMap::<DefinitionId, u32>::new();
    for (i, id) in doc.definition_order.iter().enumerate() {
        let d = doc.definition(*id).ok_or("missing definition")?;
        let reference = u32::try_from(i + 1)?;
        definitions.insert(*id, reference);
        model.definition_links.push((reference, i));
        model.definitions.push(openskp::Definition {
            name: d.name.clone(),
            guid: d.uuid.simple().to_string(),
            map_index: Some(reference as usize),
            kind: if d.is_internal_group {
                openskp::DefinitionKind::Group
            } else {
                openskp::DefinitionKind::Component
            },
            description: d.description.clone(),
            behaviour: openskp::Behaviour {
                glues_to_surface: d.behavior.glue_to != GlueTo::None,
                glue_plane: match d.behavior.glue_to {
                    GlueTo::Horizontal => 1,
                    GlueTo::Vertical => 2,
                    GlueTo::Sloped => 3,
                    _ => 0,
                },
                cuts_opening: d.behavior.cuts_opening,
                always_faces_camera: d.behavior.always_face_camera,
                shadows_face_sun: d.behavior.shadows_face_sun,
                extra: 0,
            },
            ..Default::default()
        });
        attributes(&mut model, Owner::Definition(i), &d.attrs, &mut warnings);
    }
    let mut containers = doc
        .definition_order
        .iter()
        .map(|d| ContainerRef::Definition(*d))
        .collect::<Vec<_>>();
    containers.push(ContainerRef::Model);
    for container in containers {
        let entities = doc.container(container).ok_or("missing container")?;
        let mesh = entities.mesh();
        if mesh.curves().count() > 0 {
            warnings.push(format!(
                "Container {container:?}: curve descriptors are retained as segmented edges"
            ));
        }
        let ri = model.geometry.len();
        let mut run = GeometryRun {
            def_index: match container {
                ContainerRef::Model => None,
                ContainerRef::Definition(id) => Some(definitions[&id] as usize),
            },
            ..Default::default()
        };
        let mut vertices = BTreeMap::new();
        // Keep mesh order from the canonical indexed representation.
        let (_, ids) = mesh.to_indexed();
        for v in &ids.vertices {
            let p = mesh.vertex(*v).ok_or("missing vertex")?.position;
            vertices.insert(*v, run.mesh.vertices.len() as u32);
            run.mesh.vertices.push((p / 1000.0).to_array());
        }
        for edge in &ids.edges {
            let id = EntityId::Edge(*edge);
            let (a, b) = mesh.edge_endpoints(*edge)?;
            let e = mesh.edge(*edge).ok_or("missing edge")?;
            let i = run.mesh.edges.len();
            run.mesh.edges.push(openskp::MeshEdge {
                pid: pid(entities.pid_of(id))?,
                v0: vertices[&a],
                v1: vertices[&b],
                soft: e.flags.contains(planyrith_core::geom::EdgeFlags::SOFT),
                smooth: e.flags.contains(planyrith_core::geom::EdgeFlags::SMOOTH),
                hidden: entities.is_hidden(id)
                    || e.flags.contains(planyrith_core::geom::EdgeFlags::HIDDEN),
                layer: layer(entities, id, &tags),
                ..Default::default()
            });
            if let Some(a) = entities.attrs(id) {
                attributes(&mut model, Owner::Edge(ri, i), a, &mut warnings);
            }
        }
        for face in &ids.faces {
            let id = EntityId::Face(*face);
            let f = mesh.face(*face).ok_or("missing face")?;
            let loops = std::iter::once(f.outer)
                .chain(f.holes.iter().copied())
                .collect::<Vec<_>>();
            let rings = loops
                .iter()
                .map(|l| mesh.loop_vertices(*l))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let i = run.mesh.faces.len();
            let mut face = openskp::MeshFace {
                pid: pid(entities.pid_of(id))?,
                outer: rings
                    .first()
                    .ok_or("face without boundary")?
                    .iter()
                    .map(|v| vertices[v])
                    .collect(),
                holes: rings
                    .iter()
                    .skip(1)
                    .map(|r| r.iter().map(|v| vertices[v]).collect())
                    .collect(),
                normal: f.plane.normal.to_array(),
                front_material: f
                    .sides
                    .material_front
                    .and_then(|m| materials.get(&m).copied()),
                back_material: f
                    .sides
                    .material_back
                    .and_then(|m| materials.get(&m).copied()),
                hidden: entities.is_hidden(id),
                layer: layer(entities, id, &tags),
                ..Default::default()
            };
            let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
            let mut placement = openskp::FaceTexture {
                front: identity,
                back: identity,
                front_extra: [0.0; 3],
                back_extra: [0.0; 3],
                front_pins: Vec::new(),
                back_pins: Vec::new(),
                flags: [1, 0],
            };
            let mut painted = false;
            for (material, map, out) in [
                (
                    f.sides.material_front,
                    f.sides.uv_front.as_ref(),
                    &mut placement.front,
                ),
                (
                    f.sides.material_back,
                    f.sides.uv_back.as_ref(),
                    &mut placement.back,
                ),
            ] {
                let tile = material
                    .and_then(|m| doc.materials.get(m))
                    .and_then(|m| m.texture.as_ref())
                    .map(|t| t.size_mm)
                    .or_else(|| map.is_some().then_some(DVec2::splat(25.4)));
                if let Some(tile) = tile {
                    let samples = rings
                        .iter()
                        .flatten()
                        .map(|v| {
                            let p = mesh.vertex(*v).ok_or("missing UV vertex")?.position;
                            Ok((p, uv(map, p, *v, f.plane, tile)))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    *out =
                        super::texture::mapping(&samples, f.plane.normal, doc.tolerances().length)
                            .map_err(|e| {
                                format!(
                                    "SKP face {} in {container:?}: {e}",
                                    pid(entities.pid_of(id)).unwrap_or(0)
                                )
                            })?;
                    painted = true;
                }
            }
            if painted {
                face.texture = Some(Box::new(placement));
            }
            run.mesh.faces.push(face);
            if let Some(a) = entities.attrs(id) {
                attributes(&mut model, Owner::Face(ri, i), a, &mut warnings);
            }
        }
        for id in entities.order().iter().copied() {
            if let Some(def) = entities.definition_of(id) {
                let reference = *definitions.get(&def).ok_or("missing placed definition")?;
                let transform = entities
                    .transform_of(id)
                    .ok_or("missing placement transform")?;
                let is_group = matches!(id, EntityId::Group(_));
                let (name, material) = match id {
                    EntityId::Group(g) => (entities.group(g).map(|g| g.name.clone()), None),
                    EntityId::Instance(i) => {
                        let i = entities.instance(i).ok_or("missing instance")?;
                        (i.name.clone(), i.material_override)
                    }
                    _ => continue,
                };
                let p = openskp::PlacedInstance {
                    pid: pid(entities.pid_of(id))?,
                    defref: reference,
                    transform: to_transform(transform),
                    is_group,
                    material: material
                        .and_then(|m| materials.get(&m).copied())
                        .unwrap_or(0),
                    hidden: entities.is_hidden(id),
                    layer: layer(entities, id, &tags),
                    locked: entities.is_locked(id),
                    ..Default::default()
                };
                model.instances.push(openskp::Instance {
                    pid: p.pid,
                    definition: Some(model.definitions[reference as usize - 1].name.clone()),
                    defref: reference,
                    offset: usize::MAX,
                    translation_m: (transform.translation / 1000.0).to_array(),
                    transform: p.transform,
                    material: Some(p.material),
                    name,
                    is_group: Some(is_group),
                    hidden: Some(p.hidden),
                    layer: Some(p.layer),
                    slot: None,
                    guid: String::new(),
                    casts_shadows: Some(true),
                    receives_shadows: Some(true),
                    locked: Some(p.locked),
                });
                if let Some(a) = entities.attrs(id) {
                    attributes(
                        &mut model,
                        Owner::Instance(ri, run.placed.len()),
                        a,
                        &mut warnings,
                    );
                }
                run.placed.push(p);
            } else {
                match id {
                EntityId::SectionPlane(s)=>{let s=entities.section_plane(s).ok_or("missing section plane")?;let p=pid(entities.pid_of(id))?;if s.active {run.active_section=Some(p);}
                    run.sections.push(openskp::SectionPlane {pid:p,plane:[s.plane.normal.x,s.plane.normal.y,s.plane.normal.z,s.plane.d/25.4],hidden:entities.is_hidden(id),name:s.name.clone(),symbol:s.symbol.clone(),layer:layer(entities,id,&tags),material:None,casts_shadows:true,receives_shadows:true});}
                EntityId::Guide(g)=>{let p=pid(entities.pid_of(id))?;match entities.guide(g).ok_or("missing guide")? {
                    Guide::Point {pos}=>run.guide_points.push(openskp::GuidePoint {pid:p,point_m:(*pos/1000.0).to_array(),anchor_m:(*pos/1000.0).to_array(),layer:layer(entities,id,&tags),hidden:entities.is_hidden(id),..Default::default()}),
                    Guide::Line {origin,dir,finite}=>model.guides.push(openskp::Guide {def_index:run.def_index,point_m:(*origin/1000.0).to_array(),direction:dir.to_array(),pid:p,bounds:finite.map_or([-1e30,1e30],|(a,b)|[a/25.4,b/25.4]),layer:layer(entities,id,&tags),material:None,hidden:entities.is_hidden(id),casts_shadows:true,receives_shadows:true})}}
                EntityId::Dimension(_)|EntityId::Text(_)|EntityId::Image(_)=>warnings.push(format!("Container {container:?}: {id:?} annotation/image not transferred to SKP")),
                _=>{}
            }
            }
        }
        run.topology = openskp::Topology {
            vertices: run.mesh.vertices.len(),
            edges: run.mesh.edges.len(),
            faces: run.mesh.faces.len(),
            ..Default::default()
        };
        model.geometry.push(run);
    }
    for (_, s) in doc.scenes.iter() {
        let mut saved = 0;
        let mut scene = openskp::Scene {
            name: s.name.clone(),
            description: s.description.clone(),
            in_animation: s.include_in_animation,
            ..Default::default()
        };
        if let Some(c) = s.camera {
            scene.camera = Some(camera(&c));
            saved |= openskp::SceneProperties::CAMERA;
        }
        if let Some(visibility) = &s.tag_visibility {
            scene.hidden_layers = visibility
                .iter()
                .filter_map(|(t, visible)| {
                    (!visible)
                        .then(|| tags.get(t).map(|i| usize::from(*i)))
                        .flatten()
                })
                .collect();
            saved |= openskp::SceneProperties::VISIBLE_LAYERS;
        }
        if let Some(hidden) = &s.hidden_entities {
            scene.hidden_entities = hidden
                .iter()
                .map(|p| pid(Some(*p)))
                .collect::<Result<_>>()?;
            saved |= openskp::SceneProperties::HIDDEN_GEOMETRY;
        }
        if let Some(sections) = &s.section_planes {
            scene.active_section_planes = sections
                .iter()
                .filter(|(_, active)| **active)
                .map(|(p, _)| pid(Some(*p)))
                .collect::<Result<_>>()?;
            saved |= openskp::SceneProperties::ACTIVE_SECTION_PLANES;
        }
        scene.saved = openskp::SceneProperties(saved);
        model.scenes.push(scene);
    }
    attributes(&mut model, Owner::Model, &doc.attrs, &mut warnings);
    Ok((model, warnings))
}
