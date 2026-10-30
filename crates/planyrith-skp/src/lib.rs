//! Implements docs/09 §5: optional GPL-3.0-only SKP conversion.
//! This crate is linked only into the separate converter executable.

mod decode;
mod encode;
mod source;
mod surfaces;
mod texture;

use glam::{DAffine3, DMat3, DVec3};
use planyrith_core::io::{IoError, NoProgress, adapter::Converted, native};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// Display/snap units, not the file's intrinsic inch geometry coordinates.
fn unit_mm(unit: u32) -> f64 {
    match unit {
        0 => 25.4,
        1 => 304.8,
        2 => 1.0,
        3 => 10.0,
        4 => 1000.0,
        _ => 1.0,
    }
}

/// Converts SKP to native model bytes or native bytes to a real SKP container.
pub fn convert(operation: &str, bytes: &[u8]) -> Result<Converted> {
    if bytes.len() > planyrith_core::io::adapter::MAX_BYTES {
        return Err("converter input exceeds 512 MiB".into());
    }
    match operation {
        "decode" => {
            let model = openskp::Model::parse(bytes)?;
            let (doc, warnings) = decode::document(&model, blake3::hash(bytes).as_bytes())?;
            let bytes =
                native::write_document(&doc, &[], &source::options(&doc, bytes)?, &NoProgress)?;
            Ok(Converted { bytes, warnings })
        }
        "encode-preserve" | "encode-2017" | "encode-2026" => {
            let loaded = native::read_document(bytes, &NoProgress)?;
            let source_version = source::version(&loaded)?;
            let target_version = match operation {
                "encode-2017" => 2017,
                "encode-2026" => 2026,
                // Do not silently write a newer file than the source's own
                // application can open. Intermediate releases can read 2017.
                _ => {
                    if source_version.is_some_and(|v| v >= 2026) {
                        2026
                    } else {
                        2017
                    }
                }
            };
            if (operation == "encode-preserve"
                || (source_version == Some(target_version)
                    && source::matches_container(&loaded, target_version)))
                && let Some(bytes) = source::unchanged(&loaded)?
            {
                return Ok(Converted {
                    bytes,
                    warnings: Vec::new(),
                });
            }
            let (model, mut warnings) = encode::model(&loaded.document)?;
            if operation == "encode-preserve"
                && let Some(version) = source_version
                && version != target_version
            {
                warnings.push(format!("Edited SKP {version} is saved in the {target_version} container; the complete source remains in the native working copy"));
            }
            let env = openskp::WriteEnv {
                unix_time: 0,
                seed: 0x504c_414e_5952_4954,
            };
            let bytes = if target_version == 2017 {
                let doc = openskp::Document17::from_model_with(&model, env)?;
                doc.verify()?;
                warnings.extend(doc.notes().iter().cloned());
                doc.write()
            } else {
                let doc = openskp::Document::from_model_with(&model, env)?;
                warnings.extend(doc.notes().iter().cloned());
                doc.write()?
            };
            Ok(Converted { bytes, warnings })
        }
        _ => Err(IoError::UnsupportedFormat(
            "converter expects decode, encode-preserve, encode-2017 or encode-2026".into(),
        )
        .into()),
    }
}

fn from_transform(t: &[f64; 13]) -> Result<DAffine3> {
    let matrix = DMat3::from_cols_array(&[t[0], t[3], t[6], t[1], t[4], t[7], t[2], t[5], t[8]]);
    let transform = DAffine3::from_mat3_translation(matrix, DVec3::new(t[9], t[10], t[11]) * 25.4);
    if !transform.is_finite() || matrix.determinant().abs() < 1e-15 {
        return Err("SKP contains a non-finite or singular transform".into());
    }
    Ok(transform)
}
fn to_transform(t: DAffine3) -> [f64; 13] {
    let c = t.matrix3.to_cols_array();
    let p = t.translation / 25.4;
    [
        c[0], c[3], c[6], c[1], c[4], c[7], c[2], c[5], c[8], p.x, p.y, p.z, 1.0,
    ]
}

#[cfg(test)]
mod tests;
