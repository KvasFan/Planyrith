//! Exports image entities as image definitions and placements (docs/09 §5).
use super::{
    Result,
    encode::{self, Pids},
    to_transform,
};
use glam::DVec3;
use openskp::Model;
use planyrith_core::{
    document::*,
    ids::{EntityId, MaterialId, TagId},
};
use std::collections::BTreeMap;

pub(super) fn append(
    doc: &Document,
    model: &mut Model,
    materials: &BTreeMap<MaterialId, u16>,
    tags: &BTreeMap<TagId, u16>,
    pids: &mut Pids,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let containers = doc
        .definition_order
        .iter()
        .map(|d| ContainerRef::Definition(*d))
        .chain(std::iter::once(ContainerRef::Model));
    let mut definitions = Vec::new();
    for (ri, c) in containers.enumerate() {
        let entities = doc.container(c).ok_or("image container missing")?;
        for (id, image) in entities.images() {
            let entity = EntityId::Image(id);
            let pid = encode::pid(entities.pid_of(entity))?;
            let reference = u32::try_from(model.definitions.len() + 1)?;
            let material = *materials
                .get(&image.material)
                .ok_or("image material unavailable to SKP")?;
            let index = model.definitions.len();
            model.definition_links.push((reference, index));
            model.definitions.push(openskp::Definition {
                name: format!("Image {pid}"),
                guid: blake3::hash(format!("image:{pid}").as_bytes()).to_hex()[..32].to_owned(),
                kind: openskp::DefinitionKind::Image,
                map_index: Some(reference as usize),
                ..Default::default()
            });
            let width = image.width.as_mm() / 1000.0;
            let height = image.height.as_mm() / 1000.0;
            let mut run = openskp::GeometryRun {
                def_index: Some(reference as usize),
                ..Default::default()
            };
            run.mesh.vertices = vec![
                [0.0, 0.0, 0.0],
                [width, 0.0, 0.0],
                [width, height, 0.0],
                [0.0, height, 0.0],
            ];
            for _ in 0..4 {
                run.mesh.vertex_pids.push(pids.next()?);
            }
            for v in 0..4 {
                run.mesh.edges.push(openskp::MeshEdge {
                    pid: pids.next()?,
                    v0: v,
                    v1: (v + 1) % 4,
                    ..Default::default()
                });
            }
            let tile = doc
                .materials
                .get(image.material)
                .and_then(|m| m.texture.as_ref())
                .map(|t| t.size_mm)
                .ok_or("image entity has no texture")?;
            let points = [
                DVec3::ZERO,
                DVec3::new(width * 1000.0, 0.0, 0.0),
                DVec3::new(width * 1000.0, height * 1000.0, 0.0),
                DVec3::new(0.0, height * 1000.0, 0.0),
            ];
            let coords = [
                glam::DVec2::ZERO,
                glam::DVec2::new(tile.x, 0.0),
                tile,
                glam::DVec2::new(0.0, tile.y),
            ];
            let mapping = super::texture::mapping(
                &points.into_iter().zip(coords).collect::<Vec<_>>(),
                DVec3::Z,
                doc.tolerances().length,
            )?;
            run.mesh.faces.push(openskp::MeshFace {
                pid: pids.next()?,
                outer: vec![0, 1, 2, 3],
                normal: [0.0, 0.0, 1.0],
                front_material: Some(material),
                back_material: Some(material),
                texture: Some(Box::new(openskp::FaceTexture {
                    front: mapping,
                    back: mapping,
                    front_extra: [0.0; 3],
                    back_extra: [0.0; 3],
                    front_pins: Vec::new(),
                    back_pins: Vec::new(),
                    flags: [1, 0],
                })),
                ..Default::default()
            });
            run.topology.vertices = 4;
            run.topology.edges = 4;
            run.topology.faces = 1;
            definitions.push(run);
            if let Some(a) = entities.attrs(entity) {
                encode::attributes(
                    model,
                    openskp::Owner::Image(ri, model.geometry[ri].images.len()),
                    a,
                    warnings,
                );
            }
            model.geometry[ri].images.push(openskp::ImagePlacement {
                pid,
                defref: reference,
                transform: to_transform(image.transform),
                hidden: entities.is_hidden(entity),
                layer: encode::layer(entities, entity, tags),
                casts_shadows: false,
                receives_shadows: true,
            });
            model.geometry[ri].topology.images_placed += 1;
        }
    }
    model.geometry.extend(definitions);
    Ok(())
}
