//! Imports placed image entities, retaining their physical placement (docs/09 §5).
use super::{Result, from_transform};
use glam::{DAffine3, DVec3};
use openskp::Model;
use planyrith_core::{document::ImageEntity, ids::MaterialId, units::Length};

/// Converts legacy bitmaps losslessly to the native asset encodings.
pub(super) fn asset(bytes: &[u8]) -> Result<Vec<u8>> {
    if image::guess_format(bytes)? != image::ImageFormat::Bmp {
        return Ok(bytes.to_vec());
    }
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Bmp);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(planyrith_core::document::ImageStore::MAX_SIDE_PX);
    limits.max_image_height = Some(planyrith_core::document::ImageStore::MAX_SIDE_PX);
    reader.limits(limits);
    let decoded = reader.decode()?;
    let mut png = std::io::Cursor::new(Vec::new());
    decoded.write_to(&mut png, image::ImageFormat::Png)?;
    Ok(png.into_inner())
}

pub(super) fn placed(
    model: &Model,
    reference: u32,
    transform: &[f64; 13],
    materials: &[MaterialId],
) -> Result<ImageEntity> {
    let run = model
        .geometry
        .iter()
        .find(|r| r.def_index == Some(reference as usize))
        .ok_or("image definition geometry missing")?;
    let face = run
        .mesh
        .faces
        .first()
        .ok_or("image definition has no rectangle")?;
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    for v in &run.mesh.vertices {
        let p = DVec3::from_array(*v) * 1000.0;
        min = min.min(p);
        max = max.max(p);
    }
    if !min.is_finite()
        || !max.is_finite()
        || (max.z - min.z).abs() > 0.01
        || max.x - min.x <= 0.001
        || max.y - min.y <= 0.001
    {
        return Err("invalid image definition rectangle".into());
    }
    let material = face
        .front_material
        .or(face.back_material)
        .and_then(|slot| super::decode::material(model, materials, Some(slot)))
        .ok_or("image material missing")?;
    Ok(ImageEntity {
        width: Length::mm(max.x - min.x),
        height: Length::mm(max.y - min.y),
        material,
        transform: from_transform(transform)? * DAffine3::from_translation(min),
    })
}
