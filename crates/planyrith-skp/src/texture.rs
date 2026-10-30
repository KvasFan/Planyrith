//! Converts face texture placements without changing physical repeat sizes.
use super::Result;
use glam::{DMat3, DVec2, DVec3};

pub(super) fn frame(n: DVec3) -> (DVec3, DVec3) {
    let v = if n.z.abs() > 1.0 - 1e-9 {
        DVec3::Y
    } else {
        (DVec3::Z - n * n.z).normalize_or_zero()
    };
    (v.cross(n), v)
}

pub(super) fn from_source(
    face: &openskp::MeshFace,
    side: openskp::Side,
    plane: planyrith_core::math::Plane,
    applied_in: (f64, f64),
    tile_mm: DVec2,
) -> Result<DMat3> {
    let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let values = match (&face.texture, side) {
        (Some(t), openskp::Side::Front) => t.front,
        (Some(t), openskp::Side::Back) => t.back,
        (None, _) => identity,
    };
    let k = DMat3::from_cols_array(&values);
    if !k.is_finite()
        || k.determinant().abs() <= f64::MIN_POSITIVE
        || applied_in.0.abs() <= f64::MIN_POSITIVE
        || applied_in.1.abs() <= f64::MIN_POSITIVE
    {
        return Err("invalid SKP texture placement".into());
    }
    let (u, v) = frame(DVec3::from_array(face.normal));
    let core = plane.frame();
    let to_source = DMat3::from_cols(
        DVec3::new(u.dot(core.u) / 25.4, v.dot(core.u) / 25.4, 0.0),
        DVec3::new(u.dot(core.v) / 25.4, v.dot(core.v) / 25.4, 0.0),
        DVec3::new(u.dot(core.origin) / 25.4, v.dot(core.origin) / 25.4, 1.0),
    );
    let physical = DMat3::from_diagonal(DVec3::new(
        tile_mm.x / applied_in.0,
        tile_mm.y / applied_in.1,
        1.0,
    ));
    let map = physical * k.inverse() * to_source;
    if !map.is_finite() {
        return Err("non-finite SKP texture placement".into());
    }
    Ok(map)
}
fn solve(mut rows: Vec<Vec<f64>>) -> Option<Vec<f64>> {
    let n = rows.len();
    for i in 0..n {
        let pivot = (i..n).max_by(|&a, &b| rows[a][i].abs().total_cmp(&rows[b][i].abs()))?;
        rows.swap(i, pivot);
        let divisor = rows[i][i];
        if divisor.abs() < 1e-12 {
            return None;
        }
        for value in &mut rows[i][i..] {
            *value /= divisor;
        }
        let pivot_row = rows[i].clone();
        for (k, row) in rows.iter_mut().enumerate() {
            if k == i {
                continue;
            }
            let factor = row[i];
            for (value, pivot) in row[i..].iter_mut().zip(&pivot_row[i..]) {
                *value -= factor * pivot;
            }
        }
    }
    Some(rows.into_iter().map(|r| r[n]).collect())
}
fn fit(samples: &[(DVec2, DVec2)], projective: bool) -> Option<DMat3> {
    // Normalize both frames before elimination. Real architectural models can
    // have a large offset and a tiny face; solving in absolute inches loses UV
    // precision even when one exact projective placement exists.
    let count = samples.len() as f64;
    let source = samples.iter().map(|(p, _)| *p).sum::<DVec2>() / count;
    let target = samples.iter().map(|(_, p)| *p).sum::<DVec2>() / count;
    let source_scale = samples
        .iter()
        .map(|(p, _)| (*p - source).abs().max_element())
        .fold(0.0, f64::max);
    let target_scale = samples
        .iter()
        .map(|(_, p)| (*p - target).abs().max_element())
        .fold(0.0, f64::max);
    if source_scale < 1e-15 || target_scale < 1e-15 {
        return None;
    }
    let input = DMat3::from_cols(
        DVec3::new(1.0 / source_scale, 0.0, 0.0),
        DVec3::new(0.0, 1.0 / source_scale, 0.0),
        (-source / source_scale).extend(1.0),
    );
    let output = DMat3::from_cols(
        DVec3::new(target_scale, 0.0, 0.0),
        DVec3::new(0.0, target_scale, 0.0),
        target.extend(1.0),
    );
    let mut rows = Vec::new();
    for &(p, q) in samples {
        let p = (p - source) / source_scale;
        let q = (q - target) / target_scale;
        let (x, y, u, v) = (p.x, p.y, q.x, q.y);
        if projective {
            rows.push(vec![x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u]);
            rows.push(vec![0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v]);
        } else {
            rows.push(vec![x, y, 1.0, 0.0, 0.0, 0.0, u]);
            rows.push(vec![0.0, 0.0, 0.0, x, y, 1.0, v]);
        }
    }
    let h = solve(rows)?;
    Some(
        output
            * DMat3::from_cols(
                DVec3::new(h[0], h[3], if projective { h[6] } else { 0.0 }),
                DVec3::new(h[1], h[4], if projective { h[7] } else { 0.0 }),
                DVec3::new(h[2], h[5], 1.0),
            )
            * input,
    )
}
pub(super) fn mapping(
    samples: &[(DVec3, DVec2)],
    normal: DVec3,
    tolerance_mm: f64,
) -> Result<[f64; 9]> {
    let (u, v) = frame(normal);
    let pairs = samples
        .iter()
        .map(|(p, t)| (*t / 25.4, DVec2::new(u.dot(*p), v.dot(*p)) / 25.4))
        .collect::<Vec<_>>();
    if pairs.len() < 3 {
        return Err("not enough UV points".into());
    }
    // A non-collinear triple also works when a polygon starts with straight
    // boundary segments. Try a fourth sample for genuinely projective pins.
    for b in 1..pairs.len() {
        for c in b + 1..pairs.len() {
            let triple = [pairs[0], pairs[b], pairs[c]];
            let Some(affine) = fit(&triple, false) else {
                continue;
            };
            let matches = |m: DMat3| {
                pairs.iter().all(|(a, b)| {
                    let p = m * a.extend(1.0);
                    p.z.abs() > 1e-12 && (p.truncate() / p.z - *b).length() <= tolerance_mm / 25.4
                })
            };
            if matches(affine) {
                return Ok(affine.to_cols_array());
            }
            for d in 1..pairs.len() {
                if d == b || d == c {
                    continue;
                }
                if let Some(m) = fit(&[pairs[0], pairs[b], pairs[c], pairs[d]], true)
                    && matches(m)
                {
                    return Ok(m.to_cols_array());
                }
            }
            return Err("face UV pins cannot be represented by one SKP texture placement; triangulate the face first".into());
        }
    }
    Err("degenerate face texture placement".into())
}
