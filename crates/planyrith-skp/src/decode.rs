//! Maps decoded SKP objects into validated, editable core entities (docs/09 §5).
use super::{Result, from_transform};
use glam::{DAffine3, DMat3, DVec2, DVec3};
use openskp::{Model, Owner};
use planyrith_core::commands::{Transaction, transaction::AttrTarget};
use planyrith_core::document::*;
use planyrith_core::geom::{
    FaceSides,
    mesh::{DirectedEdgeIndex, EdgeFlags, IndexedEdge, IndexedFace, IndexedMesh},
};
use planyrith_core::ids::{DefinitionId, EntityId, MaterialId, Pid, TagId};
use planyrith_core::math::Plane;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn camera(c: &openskp::Camera) -> Result<Camera> {
    let camera = Camera {
        eye: DVec3::from_array(c.eye_m) * 1000.0,
        target: DVec3::from_array(c.target_m) * 1000.0,
        up: DVec3::from_array(c.up).normalize_or_zero(),
        aspect: if c.aspect_ratio > 0.0 {
            c.aspect_ratio
        } else {
            1.5
        },
        projection: if !c.perspective {
            Projection::Parallel {
                height_mm: c.parallel_height_m * 1000.0,
            }
        } else if c.two_point_perspective {
            Projection::TwoPoint {
                fov_y_deg: c.fov_deg,
            }
        } else {
            Projection::Perspective {
                fov_y_deg: c.fov_deg,
            }
        },
    };
    if !camera.eye.is_finite()
        || !camera.target.is_finite()
        || !camera.up.is_finite()
        || camera.up.length_squared() < 0.5
        || camera.distance() < 1e-3
    {
        return Err("invalid SKP camera".into());
    }
    let projection_valid = match camera.projection {
        Projection::Parallel { height_mm } => height_mm.is_finite() && height_mm > 0.0,
        Projection::Perspective { fov_y_deg } | Projection::TwoPoint { fov_y_deg } => {
            fov_y_deg.is_finite() && (1.0..179.0).contains(&fov_y_deg)
        }
    };
    if !projection_valid || !camera.aspect.is_finite() {
        return Err("invalid SKP camera projection".into());
    }
    Ok(camera)
}
fn value(v: &openskp::Value) -> Value {
    use openskp::Value as V;
    match v {
        V::Nil => Value::Array(Vec::new()),
        V::Int(n) => Value::Int(i64::from(*n)),
        V::Float(n) => Value::Float(f64::from(*n)),
        V::Double(n) => Value::Float(*n),
        V::Bool(b) => Value::Bool(*b),
        V::Time(t) => Value::Int(i64::from(*t)),
        V::Text(s) => Value::Str(s.clone()),
        V::Array(a) => Value::Array(a.iter().map(value).collect()),
        V::Bytes8(b) => Value::Bytes(b.to_vec()),
        V::Vector(v) | V::Direction(v) => Value::Vec3(DVec3::from_array(*v)),
    }
}
pub(super) fn attributes(model: &Model, owner: Owner) -> AttributeDictionaries {
    let mut result = AttributeDictionaries::new();
    for attrs in model.dictionaries.iter().filter(|a| a.owner == owner) {
        for dict in &attrs.dictionaries {
            for (key, v) in &dict.entries {
                result.set(&dict.name, key, value(v));
            }
        }
    }
    result
}
pub(super) fn entity_attributes(
    tx: &mut Transaction<'_>,
    c: ContainerRef,
    id: EntityId,
    attrs: AttributeDictionaries,
) -> Result<()> {
    for (dict, entries) in attrs.0 {
        for (key, v) in entries {
            tx.set_attribute(
                AttrTarget::Entity { container: c, id },
                &dict,
                &key,
                Some(v),
            )?;
        }
    }
    Ok(())
}
pub(super) fn tag(model: &Model, tags: &[TagId], slot: u16) -> Option<TagId> {
    model
        .layer_links
        .iter()
        .find(|(s, _)| *s == slot)
        .and_then(|(_, i)| tags.get(*i))
        .copied()
        .or_else(|| (slot == 0).then(|| tags.first().copied()).flatten())
}
pub(super) fn material(
    model: &Model,
    materials: &[MaterialId],
    slot: Option<u16>,
) -> Option<MaterialId> {
    let slot = slot?;
    model
        .material_links
        .iter()
        .find(|(s, _)| *s == slot)
        .and_then(|(_, i)| materials.get(*i))
        .copied()
}
pub(super) fn metadata(
    tx: &mut Transaction<'_>,
    c: ContainerRef,
    id: EntityId,
    hidden: bool,
    tag: Option<TagId>,
) -> Result<()> {
    if hidden {
        tx.set_hidden(c, id, true)?;
    }
    if let Some(tag) = tag {
        tx.set_tag(c, id, Some(tag))?;
    }
    Ok(())
}

pub(super) fn document(model: &Model, source_hash: &[u8; 32]) -> Result<(Document, Vec<String>)> {
    if model
        .geometry
        .iter()
        .all(|run| run.mesh.vertices.is_empty())
        && model.diagnostics.iter().any(|d| {
            matches!(
                d,
                openskp::Diagnostic::ContinuousFallback { .. }
                    | openskp::Diagnostic::Resync { .. }
                    | openskp::Diagnostic::Truncated { .. }
                    | openskp::Diagnostic::Skipped { .. }
            )
        })
    {
        return Err(format!(
            "SKP {}: geometry could not be decoded reliably; refusing to open an empty replacement",
            model.version
        )
        .into());
    }
    let mut doc = Document::new();
    use planyrith_core::units::{Angle, Length, LengthUnit};
    doc.info.display_units = match model.units.length_unit {
        0 if model.units.length_format == 1 => LengthUnit::FeetInches,
        0 if model.units.length_format == 3 => LengthUnit::ArchitecturalFraction,
        0 => LengthUnit::Inch,
        1 if model.units.length_format == 1 => LengthUnit::FeetInches,
        1 => LengthUnit::Foot,
        2 => LengthUnit::Millimeter,
        3 => LengthUnit::Centimeter,
        4 => LengthUnit::Meter,
        _ => LengthUnit::Millimeter,
    };
    doc.info.precision = model.units.length_precision.min(8) as u8;
    doc.info.angle_precision = model.units.angle_precision.min(8) as u8;
    doc.info.length_snap = (model.units.length_snap
        && model.units.length_snap_length.is_finite()
        && model.units.length_snap_length > 0.0)
        .then_some(Length::mm(
            model.units.length_snap_length * super::unit_mm(model.units.length_unit),
        ));
    doc.info.angle_snap = (model.units.angle_snap
        && model.units.snap_angle.is_finite()
        && model.units.snap_angle > 0.0)
        .then_some(Angle::degrees(model.units.snap_angle));
    if let Some(a) = &model.axes {
        let axes = DAffine3::from_mat3_translation(
            DMat3::from_cols(
                DVec3::from_array(a.x),
                DVec3::from_array(a.y),
                DVec3::from_array(a.z),
            ),
            DVec3::from_array(a.origin_m) * 1000.0,
        );
        if !axes.is_finite() || axes.matrix3.determinant().abs() < 1e-9 {
            return Err("invalid SKP drawing axes".into());
        }
        doc.info.axes = axes;
    }
    doc.attrs = attributes(model, Owner::Model);
    if model.geo_located
        && let Some(s) = &model.shadows
    {
        let settings = super::appearance::shadows(s)?;
        if let SunModel::Geo {
            lat,
            lon,
            north_angle,
            ..
        } = settings.sun
        {
            doc.info.geo = Some(GeoLocation {
                lat,
                lon,
                north_angle,
            });
        }
    }
    let mut warnings = Vec::new();
    if model
        .fonts
        .iter()
        .any(|f| f.family != "Inter" || f.bold || f.italic)
        && (!model.texts.is_empty()
            || !model.dimensions.is_empty()
            || !model.radial_dimensions.is_empty())
    {
        warnings.push("Annotation font family and weight are retained; native display substitutes the available font".into());
    }
    if model.units.length_unit > 4 {
        warnings.push(
            "Unsupported SKP display unit; using millimeters without rescaling geometry".into(),
        );
    }
    if model
        .camera
        .iter()
        .chain(model.scenes.iter().filter_map(|s| s.camera.as_ref()))
        .any(|c| {
            c.two_point_perspective
                && ((c.scale_2d - 1.0).abs() > 1e-9 || c.offset_2d.iter().any(|v| v.abs() > 1e-9))
        })
    {
        warnings.push("Two-point camera screen pan/zoom is not transferred".into());
    }
    for diagnostic in &model.diagnostics {
        if !matches!(
            diagnostic,
            openskp::Diagnostic::WholeFile
                | openskp::Diagnostic::ContinuousWalk { .. }
                | openskp::Diagnostic::PadSlotBound { .. }
                | openskp::Diagnostic::RunFiltered { .. }
        ) {
            warnings.push(format!("SKP reader: {diagnostic:?}"));
        }
    }
    let mut tx = Transaction::new(&mut doc, "Decode SKP");
    let mut materials = Vec::new();
    let mut names = BTreeSet::new();
    for (index, m) in model.materials.iter().enumerate() {
        let (name, rgba, opacity, texture, size, colorized) = match m {
            openskp::Material::Solid {
                name,
                rgba,
                opacity,
                ..
            } => (name, *rgba, *opacity, None, None, false),
            openskp::Material::Textured {
                name,
                avg_rgba,
                opacity,
                image_bytes,
                applied_size_in,
                colorized,
                ..
            } => (
                name,
                avg_rgba.unwrap_or([255; 4]),
                *opacity,
                image_bytes.as_ref(),
                *applied_size_in,
                *colorized,
            ),
        };
        let mut name = if name.is_empty() {
            format!("Material {}", index + 1)
        } else {
            name.clone()
        };
        while !names.insert(name.clone()) || tx.document().materials.by_name(&name).is_some() {
            name.push_str(" (imported)");
        }
        let mut m = Material::solid(
            name,
            Rgba8 {
                r: rgba[0],
                g: rgba[1],
                b: rgba[2],
                a: (opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
            },
        );
        m.opacity = opacity.clamp(0.0, 1.0) as f32;
        m.attrs = attributes(model, Owner::Material(index));
        if let Some(bytes) = texture {
            let image = tx.insert_image(bytes.clone())?;
            let (w, h) = size.unwrap_or((1.0, 1.0));
            m.texture = Some(Texture {
                image,
                size_mm: DVec2::new(w, h).abs().max(DVec2::splat(1e-6)) * 25.4,
                colorize: colorized.then_some(ColorizeMode::Tint),
            });
        }
        materials.push(tx.insert_material(m)?);
    }
    let mut tags = Vec::new();
    for (index, layer) in model.layers.iter().enumerate() {
        if index == 0 {
            tags.push(tx.document().tags.untagged());
            continue;
        }
        let mut name = layer.name.clone();
        if name.is_empty() {
            name = format!("Tag {index}");
        }
        while tx.document().tags.by_name(&name).is_some() {
            name.push_str(" (imported)");
        }
        let c = layer.rgba;
        let mut t = Tag::new(
            name,
            Rgba8 {
                r: c[0],
                g: c[1],
                b: c[2],
                a: c[3],
            },
        );
        t.visible = layer.visible;
        tags.push(tx.insert_tag(t)?);
    }
    let mut definitions = BTreeMap::<usize, DefinitionId>::new();
    for (index, d) in model.definitions.iter().enumerate() {
        let Some(reference) = d.map_index.or_else(|| {
            model
                .definition_links
                .iter()
                .find(|(_, i)| *i == index)
                .map(|(r, _)| *r as usize)
        }) else {
            return Err(format!("definition {} has no geometry linkage", d.name).into());
        };
        let mut name = if d.name.is_empty() {
            format!("Component {}", index + 1)
        } else {
            d.name.clone()
        };
        while tx.document().definition_by_name(&name).is_some() {
            name.push_str(" (imported)");
        }
        let hash =
            blake3::hash(format!("SKP:{source_hash:?}:{}:{index}:{reference}", d.guid).as_bytes());
        let mut uid = [0; 16];
        uid.copy_from_slice(&hash.as_bytes()[..16]);
        let mut def = ComponentDefinition::new(uuid::Uuid::from_bytes(uid), name);
        def.is_internal_group = d.kind != openskp::DefinitionKind::Component;
        def.description = d.description.clone();
        if let Some(bytes) = &d.thumbnail {
            def.thumbnail = Some(tx.insert_image(bytes.clone())?);
        }
        def.attrs = attributes(model, Owner::Definition(index));
        def.behavior.cuts_opening = d.behaviour.cuts_opening;
        def.behavior.always_face_camera = d.behaviour.always_faces_camera;
        def.behavior.shadows_face_sun = d.behaviour.shadows_face_sun;
        def.behavior.glue_to = if !d.behaviour.glues_to_surface {
            GlueTo::None
        } else {
            match d.behaviour.glue_plane {
                1 => GlueTo::Horizontal,
                2 => GlueTo::Vertical,
                3 => GlueTo::Sloped,
                _ => GlueTo::Any,
            }
        };
        definitions.insert(reference, tx.insert_definition(def)?);
    }
    let mut pids = BTreeMap::<u32, Vec<Pid>>::new();
    let mut seen_root = false;
    for (ri, run) in model.geometry.iter().enumerate() {
        let container = match run.def_index {
            Some(reference) => ContainerRef::Definition(
                *definitions
                    .get(&reference)
                    .ok_or_else(|| format!("missing definition {reference}"))?,
            ),
            None => {
                if seen_root {
                    return Err("multiple unresolved root geometry runs".into());
                }
                seen_root = true;
                ContainerRef::Model
            }
        };
        let mesh = &run.mesh;
        let mut indexed = IndexedMesh {
            vertices: mesh
                .vertices
                .iter()
                .map(|p| DVec3::from_array(*p) * 1000.0)
                .collect(),
            ..Default::default()
        };
        let mut curve_indices = BTreeMap::new();
        for i in 0..run.curves.len() {
            if let Some(curve) = super::curves::descriptor(run, i) {
                curve_indices.insert(i as u32, indexed.curves.len() as u32);
                indexed.curves.push(curve);
            }
        }
        if indexed
            .vertices
            .iter()
            .any(|p| !p.is_finite() || p.abs().max_element() > 1e9)
        {
            return Err("SKP vertices exceed the model coordinate range".into());
        }
        let mut edge_map = BTreeMap::new();
        let mut edge_indices = Vec::new();
        for edge in &mesh.edges {
            let key = (edge.v0.min(edge.v1), edge.v0.max(edge.v1));
            let i = *edge_map.entry(key).or_insert_with(|| {
                let i = indexed.edges.len() as u32;
                indexed.edges.push(IndexedEdge {
                    a: edge.v0,
                    b: edge.v1,
                    flags: EdgeFlags::NONE
                        .with(EdgeFlags::SOFT, edge.soft)
                        .with(EdgeFlags::SMOOTH, edge.smooth)
                        .with(EdgeFlags::HIDDEN, edge.hidden),
                    curve: edge.curve.and_then(|c| curve_indices.get(&c).copied()),
                });
                i
            });
            edge_indices.push(i as usize);
        }
        for face in &mesh.faces {
            let mut loops = Vec::new();
            for (ring_index, source_ring) in
                std::iter::once(&face.outer).chain(&face.holes).enumerate()
            {
                let mut ring = source_ring.clone();
                if ring.len() < 3 {
                    return Err("SKP face has fewer than three vertices".into());
                }
                let mut winding = DVec3::ZERO;
                let anchor = *indexed
                    .vertices
                    .get(ring[0] as usize)
                    .ok_or("SKP vertex index out of range")?;
                for i in 0..ring.len() {
                    let a = *indexed
                        .vertices
                        .get(ring[i] as usize)
                        .ok_or("SKP vertex index out of range")?;
                    let b = *indexed
                        .vertices
                        .get(ring[(i + 1) % ring.len()] as usize)
                        .ok_or("SKP vertex index out of range")?;
                    winding += (a - anchor).cross(b - anchor);
                }
                if (winding.dot(DVec3::from_array(face.normal)) > 0.0) != (ring_index == 0) {
                    ring.reverse();
                }
                let mut directed = Vec::new();
                for i in 0..ring.len() {
                    let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                    let key = (a.min(b), a.max(b));
                    let index = *edge_map.entry(key).or_insert_with(|| {
                        let i = indexed.edges.len() as u32;
                        indexed.edges.push(IndexedEdge {
                            a,
                            b,
                            flags: EdgeFlags::NONE,
                            curve: None,
                        });
                        i
                    });
                    directed.push(if indexed.edges[index as usize].a != a {
                        DirectedEdgeIndex::reversed(index)
                    } else {
                        DirectedEdgeIndex::forward(index)
                    });
                }
                loops.push(directed);
            }
            let p = *indexed
                .vertices
                .get(face.outer[0] as usize)
                .ok_or("SKP vertex index out of range")?;
            indexed.faces.push(IndexedFace {
                loops,
                plane: Some(Plane::from_point_normal(p, DVec3::from_array(face.normal))),
                sides: FaceSides {
                    material_front: material(model, &materials, face.front_material),
                    material_back: material(model, &materials, face.back_material),
                    ..Default::default()
                },
            });
        }
        let placements = super::surfaces::insert(&mut tx, container, &indexed, source_hash)?;
        if placements.len() > 1 {
            warnings.push(format!(
                "Container {ri}: conflicting face sides retained in {} private surface groups",
                placements.len()
            ));
        }
        for placement in placements {
            let container = placement.container;
            for (i, edge) in mesh.edges.iter().enumerate() {
                let Some(&edge_id) = placement.edges.get(&edge_indices[i]) else {
                    continue;
                };
                let id = EntityId::Edge(edge_id);
                metadata(
                    &mut tx,
                    container,
                    id,
                    edge.hidden,
                    tag(model, &tags, edge.layer),
                )?;
                entity_attributes(
                    &mut tx,
                    container,
                    id,
                    attributes(model, Owner::Edge(ri, i)),
                )?;
                if edge.pid != 0
                    && let Some(pid) = tx
                        .document()
                        .container(container)
                        .and_then(|c| c.pid_of(id))
                {
                    pids.entry(edge.pid).or_default().push(pid);
                }
            }
            for (i, face) in mesh.faces.iter().enumerate() {
                let Some(&face_id) = placement.faces.get(&i) else {
                    continue;
                };
                let id = EntityId::Face(face_id);
                let mut sides = indexed.faces[i].sides.clone();
                for (slot, side, uv) in [
                    (
                        face.front_material,
                        openskp::Side::Front,
                        &mut sides.uv_front,
                    ),
                    (face.back_material, openskp::Side::Back, &mut sides.uv_back),
                ] {
                    let size = slot
                        .and_then(|s| model.applied_size_of(s))
                        .unwrap_or((1.0, 1.0));
                    let tile = material(model, &materials, slot)
                        .and_then(|id| tx.document().materials.get(id))
                        .and_then(|m| m.texture.as_ref())
                        .map_or(DVec2::new(size.0.abs(), size.1.abs()) * 25.4, |t| t.size_mm);
                    let plane = tx
                        .document()
                        .container(container)
                        .and_then(|c| c.mesh().face(face_id))
                        .ok_or("missing imported face")?
                        .plane;
                    *uv = Some(UvMap::Projective(super::texture::from_source(
                        face, side, plane, size, tile,
                    )?));
                }
                tx.set_face_sides(container, face_id, sides)?;
                metadata(
                    &mut tx,
                    container,
                    id,
                    face.hidden,
                    tag(model, &tags, face.layer),
                )?;
                entity_attributes(
                    &mut tx,
                    container,
                    id,
                    attributes(model, Owner::Face(ri, i)),
                )?;
                if face.pid != 0
                    && let Some(pid) = tx
                        .document()
                        .container(container)
                        .and_then(|c| c.pid_of(id))
                {
                    pids.entry(face.pid).or_default().push(pid);
                }
            }
        }
        for (i, p) in run.placed.iter().enumerate() {
            if model.definitions.iter().any(|d| {
                d.map_index == Some(p.defref as usize) && d.kind == openskp::DefinitionKind::Image
            }) {
                let image = super::images::placed(model, p.defref, &p.transform, &materials)?;
                let (id, pid) = tx.insert_entity(container, EntityRecord::Image(image))?;
                metadata(&mut tx, container, id, p.hidden, tag(model, &tags, p.layer))?;
                entity_attributes(
                    &mut tx,
                    container,
                    id,
                    attributes(model, Owner::Instance(ri, i)),
                )?;
                if p.pid != 0 {
                    pids.entry(p.pid).or_default().push(pid);
                }
                continue;
            }
            let def = *definitions
                .get(&(p.defref as usize))
                .ok_or_else(|| format!("missing placed definition {}", p.defref))?;
            let transform = from_transform(&p.transform)?;
            let name = model
                .instances
                .iter()
                .find(|v| v.pid != 0 && v.pid == p.pid)
                .and_then(|v| v.name.clone())
                .unwrap_or_default();
            let painted_group =
                p.is_group && material(model, &materials, Some(p.material)).is_some();
            if painted_group {
                warnings.push("A painted group is represented as a private component instance to retain its inherited material".into());
            }
            let record = if p.is_group && !painted_group {
                EntityRecord::Group(Group {
                    definition: def,
                    transform,
                    name,
                })
            } else {
                EntityRecord::Instance(ComponentInstance {
                    definition: def,
                    transform,
                    name: (!name.is_empty()).then_some(name),
                    material_override: material(model, &materials, Some(p.material)),
                    glued_to: None,
                    dynamic_attrs: AttributeDictionaries::new(),
                })
            };
            let (id, pid) = tx.insert_entity(container, record)?;
            metadata(&mut tx, container, id, p.hidden, tag(model, &tags, p.layer))?;
            if p.locked {
                tx.set_locked(container, id, true)?;
            }
            entity_attributes(
                &mut tx,
                container,
                id,
                attributes(model, Owner::Instance(ri, i)),
            )?;
            if p.pid != 0 {
                pids.entry(p.pid).or_default().push(pid);
            }
        }
        for (i, s) in run.sections.iter().enumerate() {
            let normal = DVec3::new(s.plane[0], s.plane[1], s.plane[2]);
            if !normal.is_finite() || normal.length_squared() < 1e-12 {
                return Err("invalid section plane".into());
            }
            let (id, pid) = tx.insert_entity(
                container,
                EntityRecord::SectionPlane(SectionPlane {
                    plane: Plane::new(normal.normalize(), s.plane[3] * 25.4 / normal.length()),
                    name: s.name.clone(),
                    symbol: s.symbol.clone(),
                    active: run.active_section == Some(s.pid),
                    fill_cap: true,
                    cuts_hidden: false,
                }),
            )?;
            metadata(&mut tx, container, id, s.hidden, tag(model, &tags, s.layer))?;
            entity_attributes(
                &mut tx,
                container,
                id,
                attributes(model, Owner::SectionPlane(ri, i)),
            )?;
            if s.pid != 0 {
                pids.entry(s.pid).or_default().push(pid);
            }
        }
        for (i, p) in run.guide_points.iter().enumerate() {
            let (id, pid) = tx.insert_entity(
                container,
                EntityRecord::Guide(Guide::Point {
                    pos: DVec3::from_array(p.point_m) * 1000.0,
                }),
            )?;
            metadata(&mut tx, container, id, p.hidden, tag(model, &tags, p.layer))?;
            entity_attributes(
                &mut tx,
                container,
                id,
                attributes(model, Owner::GuidePoint(ri, i)),
            )?;
            if p.pid != 0 {
                pids.entry(p.pid).or_default().push(pid);
            }
        }
        for (i, p) in run.images.iter().enumerate() {
            let image = super::images::placed(model, p.defref, &p.transform, &materials)?;
            let (id, pid) = tx.insert_entity(container, EntityRecord::Image(image))?;
            metadata(&mut tx, container, id, p.hidden, tag(model, &tags, p.layer))?;
            entity_attributes(
                &mut tx,
                container,
                id,
                attributes(model, Owner::Image(ri, i)),
            )?;
            if p.pid != 0 {
                pids.entry(p.pid).or_default().push(pid);
            }
        }
        let curves = run.topology.curves.saturating_sub(run.curves.len());
        if curves > 0 {
            warnings.push(format!(
                "Container {ri}: {} curves retained as segmented edges",
                curves
            ));
        }
        let images = run.topology.images_placed.saturating_sub(run.images.len());
        for (i, polyline) in run.polylines.iter().enumerate() {
            let points = polyline
                .points_m
                .iter()
                .map(|p| DVec3::from_array(*p) * 1000.0)
                .collect::<Vec<_>>();
            if points.len() < 2 {
                continue;
            }
            let mesh = IndexedMesh {
                vertices: points.clone(),
                edges: (0..points.len() - 1)
                    .map(|i| IndexedEdge {
                        a: i as u32,
                        b: i as u32 + 1,
                        flags: EdgeFlags::NONE,
                        curve: Some(0),
                    })
                    .collect(),
                curves: vec![planyrith_core::geom::CurveDescriptor::Freehand { points }],
                ..Default::default()
            };
            let placed = super::surfaces::insert(&mut tx, container, &mesh, source_hash)?;
            for part in placed {
                for edge in part.edges.values() {
                    metadata(
                        &mut tx,
                        part.container,
                        EntityId::Edge(*edge),
                        polyline.hidden,
                        tag(model, &tags, polyline.layer),
                    )?;
                    entity_attributes(
                        &mut tx,
                        part.container,
                        EntityId::Edge(*edge),
                        attributes(model, Owner::Polyline(ri, i)),
                    )?;
                }
            }
        }
        let polylines = run.topology.polylines.saturating_sub(run.polylines.len());
        if images > 0 || polylines > 0 {
            warnings.push(format!(
                "Container {ri}: {} placed images and {} 3D polylines are not transferred",
                images, polylines
            ));
        }
    }
    for (i, g) in model.guides.iter().enumerate() {
        let container = match g.def_index {
            Some(r) => {
                ContainerRef::Definition(*definitions.get(&r).ok_or("guide definition missing")?)
            }
            None => ContainerRef::Model,
        };
        let (id, pid) = tx.insert_entity(
            container,
            EntityRecord::Guide(Guide::Line {
                origin: DVec3::from_array(g.point_m) * 1000.0,
                dir: DVec3::from_array(g.direction).normalize_or_zero(),
                finite: (g.bounds[0].abs() < 1e29 && g.bounds[1].abs() < 1e29)
                    .then_some((g.bounds[0] * 25.4, g.bounds[1] * 25.4)),
            }),
        )?;
        metadata(&mut tx, container, id, g.hidden, tag(model, &tags, g.layer))?;
        entity_attributes(&mut tx, container, id, attributes(model, Owner::Guide(i)))?;
        if g.pid != 0 {
            pids.entry(g.pid).or_default().push(pid);
        }
    }
    super::annotations::import(&mut tx, model, &definitions, &tags, &materials, &mut pids)?;
    super::metadata::import(&mut tx, model)?;
    let styles = super::appearance::import(&mut tx, model, &mut warnings)?;
    for s in &model.scenes {
        let mut scene = Scene::named(&s.name);
        scene.description = s.description.clone();
        scene.include_in_animation = s.in_animation;
        scene.transition_s = if model.animation.transitions {
            model.animation.transition_s.max(0.0) as f32
        } else {
            0.0
        };
        scene.camera = s.camera.as_ref().map(camera).transpose()?;
        scene.style = super::appearance::scene_style(&mut tx, s, &styles)?;
        scene.shadows = s
            .shadows
            .as_ref()
            .map(super::appearance::shadows)
            .transpose()?;
        scene.axes_visible = s.rendering.as_ref().map(|r| r.model_axes);
        if s.saved.has(
            openskp::SceneProperties::HIDDEN_GEOMETRY
                | openskp::SceneProperties::HIDDEN_GEOMETRY_2
                | openskp::SceneProperties::HIDDEN_OBJECTS,
        ) {
            scene.hidden_entities = Some(
                s.hidden_entities
                    .iter()
                    .flat_map(|p| pids.get(p).into_iter().flatten().copied())
                    .collect(),
            );
        }
        if s.saved.has(openskp::SceneProperties::VISIBLE_LAYERS) {
            scene.tag_visibility = Some(
                tags.iter()
                    .enumerate()
                    .map(|(i, t)| (*t, i == 0 || !s.hidden_layers.contains(&i)))
                    .collect(),
            );
        }
        if s.saved.has(openskp::SceneProperties::ACTIVE_SECTION_PLANES) {
            scene.section_planes = Some(
                pids.iter()
                    .filter(|(old, _)| {
                        model
                            .geometry
                            .iter()
                            .any(|r| r.sections.iter().any(|s| s.pid == **old))
                    })
                    .flat_map(|(old, ps)| {
                        ps.iter()
                            .map(move |p| (*p, s.active_section_planes.contains(old)))
                    })
                    .collect(),
            );
        }
        tx.insert_scene(scene)?;
    }
    if let Some(c) = &model.camera {
        tx.set_camera(camera(c)?);
    }
    tx.commit();
    doc.refresh_usage_counts();
    doc.nesting_depth()?;
    let texts = model
        .texts
        .len()
        .max(model.geometry.iter().map(|r| r.topology.texts).sum());
    let dimensions = model
        .dimensions
        .len()
        .max(model.geometry.iter().map(|r| r.topology.dimensions).sum());
    let radial_dimensions = model.radial_dimensions.len().max(
        model
            .geometry
            .iter()
            .map(|r| r.topology.radial_dimensions)
            .sum(),
    );
    let texts = texts.saturating_sub(model.texts.len());
    let dimensions = dimensions.saturating_sub(model.dimensions.len());
    let radial_dimensions = radial_dimensions.saturating_sub(model.radial_dimensions.len());
    if texts > 0 || dimensions > 0 || radial_dimensions > 0 {
        warnings.push(format!(
            "SKP annotations not transferred: {} texts, {} dimensions, {} radial dimensions",
            texts, dimensions, radial_dimensions
        ));
    }
    let report = planyrith_core::diagnostics::validate(&doc);
    if let Some(problem) = report
        .problems
        .iter()
        .find(|p| p.severity == planyrith_core::diagnostics::Severity::Error)
    {
        return Err(format!("converted SKP is invalid: {}", problem.message).into());
    }
    Ok((doc, warnings))
}
