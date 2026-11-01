//! Imports placed image entities and converts source image encodings (docs/09 §5).
use super::{Result, from_transform};
use glam::{DAffine3, DVec3};
use openskp::Model;
use planyrith_core::{document::ImageEntity, ids::MaterialId, units::Length};

fn psd_u16(bytes: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?).into())
}
fn psd_u32(bytes: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize)
}
/// One PackBits-compressed row.
fn packbits(input: &[u8], output: &mut Vec<u8>, width: usize) -> Option<()> {
    let mut at = 0;
    let end = output.len() + width;
    while output.len() < end {
        let n = *input.get(at)? as i8;
        at += 1;
        if n >= 0 {
            let count = n as usize + 1;
            output.extend_from_slice(input.get(at..at + count)?);
            at += count;
        } else if n != -128 {
            let value = *input.get(at)?;
            at += 1;
            output.resize(output.len() + (1 - n as isize) as usize, value);
        }
    }
    (output.len() == end).then_some(())
}
/// The merged image of an 8-bit RGB or grayscale Photoshop file, which models
/// can embed as a texture and the image library does not read.
fn psd(bytes: &[u8]) -> Option<image::RgbaImage> {
    let side = planyrith_core::document::ImageStore::MAX_SIDE_PX as usize;
    let channels = psd_u16(bytes, 12)?;
    let (height, width) = (psd_u32(bytes, 14)?, psd_u32(bytes, 18)?);
    let (depth, mode) = (psd_u16(bytes, 22)?, psd_u16(bytes, 24)?);
    if !bytes.starts_with(b"8BPS\0\x01")
        || depth != 8
        || !matches!(mode, 1 | 3)
        || channels == 0
        || width == 0
        || height == 0
        || width > side
        || height > side
    {
        return None;
    }
    // Color data, image resources and layers precede the merged image.
    let mut at = 26usize;
    for _ in 0..3 {
        at = at.checked_add(4)?.checked_add(psd_u32(bytes, at)?)?;
    }
    let compression = psd_u16(bytes, at)?;
    at += 2;
    let mut planes = Vec::with_capacity(width * height * channels);
    match compression {
        0 => planes.extend_from_slice(bytes.get(at..at + width * height * channels)?),
        1 => {
            let lengths = at;
            at += height * channels * 2;
            for row in 0..height * channels {
                let length = psd_u16(bytes, lengths + row * 2)?;
                packbits(bytes.get(at..at + length)?, &mut planes, width)?;
                at += length;
            }
        }
        _ => return None,
    }
    let color = if mode == 3 { 3 } else { 1 };
    if channels < color {
        return None;
    }
    let plane = |c: usize, i: usize| planes[c * width * height + i];
    let pixels = (0..width * height)
        .flat_map(|i| {
            let (r, g, b) = if color == 3 {
                (plane(0, i), plane(1, i), plane(2, i))
            } else {
                (plane(0, i), plane(0, i), plane(0, i))
            };
            let a = if channels > color {
                plane(color, i)
            } else {
                255
            };
            [r, g, b, a]
        })
        .collect();
    image::RgbaImage::from_raw(width as u32, height as u32, pixels)
}
/// Converts legacy bitmaps losslessly to the native asset encodings.
pub(super) fn asset(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.starts_with(b"8BPS") {
        let decoded = psd(bytes).ok_or("unsupported Photoshop image layout")?;
        let mut png = std::io::Cursor::new(Vec::new());
        decoded.write_to(&mut png, image::ImageFormat::Png)?;
        return Ok(png.into_inner());
    }
    use image::ImageFormat as F;
    // Targa is the one texture format with no signature to detect.
    let format = image::guess_format(bytes).unwrap_or(F::Tga);
    if matches!(format, F::Png | F::Jpeg | F::WebP) {
        return Ok(bytes.to_vec());
    }
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(planyrith_core::document::ImageStore::MAX_SIDE_PX);
    limits.max_image_height = Some(planyrith_core::document::ImageStore::MAX_SIDE_PX);
    reader.limits(limits);
    let decoded = reader.decode()?;
    let mut png = std::io::Cursor::new(Vec::new());
    match decoded {
        // PNG has no 32-bit float channels; such images narrow to 16 bits.
        image::DynamicImage::ImageRgb32F(_) => {
            image::DynamicImage::ImageRgb16(decoded.to_rgb16()).write_to(&mut png, F::Png)?;
        }
        image::DynamicImage::ImageRgba32F(_) => {
            image::DynamicImage::ImageRgba16(decoded.to_rgba16()).write_to(&mut png, F::Png)?;
        }
        _ => decoded.write_to(&mut png, F::Png)?,
    }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn header(channels: u16, height: u32, width: u32, mode: u16) -> Vec<u8> {
        let mut b = b"8BPS\0\x01\0\0\0\0\0\0".to_vec();
        b.extend(channels.to_be_bytes());
        b.extend(height.to_be_bytes());
        b.extend(width.to_be_bytes());
        b.extend(8u16.to_be_bytes());
        b.extend(mode.to_be_bytes());
        // Empty color data, resources and layers; resources may hold bytes.
        b.extend(0u32.to_be_bytes());
        b.extend(3u32.to_be_bytes());
        b.extend([1, 2, 3]);
        b.extend(0u32.to_be_bytes());
        b
    }

    #[test]
    fn raw_and_packed_photoshop_images_keep_their_pixels_and_alpha() {
        let mut raw = header(4, 1, 2, 3);
        raw.extend(0u16.to_be_bytes());
        raw.extend([10, 20, 30, 40, 50, 60, 70, 80]);
        let image = psd(&raw).unwrap();
        assert_eq!(image.get_pixel(0, 0).0, [10, 30, 50, 70]);
        assert_eq!(image.get_pixel(1, 0).0, [20, 40, 60, 80]);

        let mut packed = header(3, 2, 3, 3);
        packed.extend(1u16.to_be_bytes());
        let rows: [&[u8]; 6] = [
            &[0xfe, 9],
            &[2, 1, 2, 3],
            &[0, 7, 0xff, 8],
            &[0xfe, 4],
            &[0xfe, 5],
            &[0xfe, 6],
        ];
        for row in rows {
            packed.extend((row.len() as u16).to_be_bytes());
        }
        for row in rows {
            packed.extend_from_slice(row);
        }
        let image = psd(&packed).unwrap();
        assert_eq!(image.dimensions(), (3, 2));
        assert_eq!(image.get_pixel(1, 0).0, [9, 8, 5, 255]);
        assert_eq!(image.get_pixel(2, 1).0, [3, 4, 6, 255]);
        let png = asset(&packed).unwrap();
        assert_eq!(image::guess_format(&png).unwrap(), image::ImageFormat::Png);
    }

    #[test]
    fn other_texture_encodings_become_lossless_assets() {
        let source = image::RgbaImage::from_fn(3, 2, |x, y| {
            image::Rgba([x as u8 * 40, y as u8 * 90, 7, 255])
        });
        for format in [
            image::ImageFormat::Tiff,
            image::ImageFormat::Gif,
            image::ImageFormat::Tga,
            image::ImageFormat::Bmp,
        ] {
            let mut encoded = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(source.clone())
                .write_to(&mut encoded, format)
                .unwrap();
            let png = asset(encoded.get_ref()).unwrap();
            assert_eq!(image::guess_format(&png).unwrap(), image::ImageFormat::Png);
            let back = image::load_from_memory(&png).unwrap().to_rgba8();
            assert_eq!(back.dimensions(), (3, 2), "{format:?}");
            if format != image::ImageFormat::Gif {
                assert_eq!(back, source, "{format:?}");
            }
        }
    }

    #[test]
    fn damaged_or_unsupported_photoshop_images_are_errors() {
        let mut raw = header(3, 2, 2, 3);
        raw.extend(0u16.to_be_bytes());
        assert!(asset(&raw).is_err());
        raw.extend([0; 12]);
        assert!(asset(&raw).is_ok());
        assert!(asset(&raw[..20]).is_err());
        let cmyk = header(4, 1, 1, 4);
        assert!(asset(&cmyk).is_err());
    }
}
