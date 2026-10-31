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
/// Samples spread as far as possible: `fit` degrades on nearly collinear
/// points, which long straight runs of a polygon's boundary produce.
fn spread(pairs: &[(DVec2, DVec2)]) -> Option<[usize; 3]> {
    let furthest = |value: &dyn Fn(usize) -> f64| {
        (0..pairs.len()).max_by(|a, b| value(*a).total_cmp(&value(*b)))
    };
    let a = 0;
    let b = furthest(&|i| (pairs[i].0 - pairs[a].0).length_squared())?;
    let line = pairs[b].0 - pairs[a].0;
    let c = furthest(&|i| line.perp_dot(pairs[i].0 - pairs[a].0).abs())?;
    (line.perp_dot(pairs[c].0 - pairs[a].0).abs() > 0.0).then_some([a, b, c])
}

/// The placement of `samples`, and whether it reproduces them. Sources can
/// hold pins or degenerate placements one matrix cannot express: those take
/// the closest fit (else the identity) so one face does not stop a save.
pub(super) fn mapping(
    samples: &[(DVec3, DVec2)],
    normal: DVec3,
    tolerance_mm: f64,
) -> Result<([f64; 9], bool)> {
    let (u, v) = frame(normal);
    let pairs = samples
        .iter()
        .map(|(p, t)| (*t / 25.4, DVec2::new(u.dot(*p), v.dot(*p)) / 25.4))
        .collect::<Vec<_>>();
    if pairs.len() < 3 {
        return Err("not enough UV points".into());
    }
    let error = |m: DMat3| {
        pairs
            .iter()
            .map(|(a, b)| {
                let p = m * a.extend(1.0);
                if p.z.abs() > 1e-12 {
                    (p.truncate() / p.z - *b).length()
                } else {
                    f64::INFINITY
                }
            })
            .fold(0.0, f64::max)
    };
    let mut best: Option<(DMat3, f64)> = None;
    let mut consider = |m: Option<DMat3>| {
        if let Some(m) = m {
            let e = error(m);
            if e.is_finite() && best.is_none_or(|(_, old)| e < old) {
                best = Some((m, e));
            }
        }
    };
    if let Some([a, b, c]) = spread(&pairs) {
        let triple = [pairs[a], pairs[b], pairs[c]];
        consider(fit(&triple, false));
        for d in (0..pairs.len()).filter(|d| ![a, b, c].contains(d)) {
            consider(fit(&[triple[0], triple[1], triple[2], pairs[d]], true));
        }
    }
    Ok(match best {
        Some((m, e)) => (m.to_cols_array(), e <= tolerance_mm / 25.4),
        None => ([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], false),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn samples(map: impl Fn(DVec2) -> DVec2) -> Vec<(DVec3, DVec2)> {
        // A long straight run first: the opening samples are collinear.
        [
            (0.0, 0.0),
            (100.0, 0.0),
            (200.0, 0.0),
            (300.0, 1e-7),
            (1000.0, 0.0),
            (1000.0, 1000.0),
            (0.0, 1000.0),
        ]
        .into_iter()
        .map(|(x, y)| (DVec3::new(x, y, 0.0), map(DVec2::new(x, y))))
        .collect()
    }

    #[test]
    fn affine_and_projective_placements_fit_past_collinear_boundary_runs() {
        let affine = samples(|p| p * 2.0 + DVec2::new(5.0, 7.0));
        assert!(mapping(&affine, DVec3::Z, 1e-3).unwrap().1);
        let projective = samples(|p| {
            let w = 1.0 + p.x * 2e-4 + p.y * 1e-4;
            DVec2::new(p.x / w, (p.y + 3.0) / w)
        });
        assert!(mapping(&projective, DVec3::Z, 1e-3).unwrap().1);
    }

    /// A real 2017 face pinned into a projective placement (its back side).
    fn pinned_face() -> openskp::MeshFace {
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        openskp::MeshFace {
            normal: [0.0, 0.0, -1.0],
            texture: Some(Box::new(openskp::FaceTexture {
                front: identity,
                back: [
                    0.8359580052493438,
                    -0.0,
                    0.0,
                    0.41010498687664043,
                    0.8359580052493438,
                    -0.003417541557305337,
                    -19.68503937007874,
                    0.0,
                    1.0,
                ],
                front_extra: [0.0; 3],
                back_extra: [0.0; 3],
                front_pins: Vec::new(),
                back_pins: Vec::new(),
                flags: [0, 1],
            })),
            ..Default::default()
        }
    }

    #[test]
    fn projective_placements_reproduce_their_pins_through_import_and_export() {
        let face = pinned_face();
        let tile = DVec2::new(3048.0, 1219.2);
        let normal = DVec3::from_array(face.normal);
        let plane = planyrith_core::math::Plane::from_point_normal(DVec3::ZERO, normal);
        let map = from_source(&face, openskp::Side::Back, plane, (120.0, 48.0), tile).unwrap();
        let (u, v) = frame(normal);
        let core = plane.frame();
        // (anchor_u, anchor_v, face_x, face_y), all inches, as stored.
        let pins = [
            [0.0, 0.0, -19.68503937007874, 0.0],
            [-120.0, 0.0, -120.0, 0.0],
            [0.0, 48.0, 0.0, 48.0],
            [-120.0, 48.0, -120.0, 48.0],
        ];
        let mut samples = Vec::new();
        for [a, b, x, y] in pins {
            let p = (u * x + v * y) * 25.4;
            let h = map * core.to_2d(p).extend(1.0);
            let texture = h.truncate() / h.z;
            let expected = DVec2::new(a, b) * 25.4;
            assert!((texture - expected).length() < 1e-6, "{texture} {expected}");
            samples.push((p, texture));
        }
        // Export finds the same placement again.
        let (fitted, exact) = mapping(&samples, normal, 1e-3).unwrap();
        assert!(exact);
        let k = DMat3::from_cols_array(&face.texture.as_ref().unwrap().back);
        let again = DMat3::from_cols_array(&fitted);
        for [a, b, ..] in pins {
            let t = DVec3::new(a, b, 1.0);
            let (want, got) = (k * t, again * t);
            assert!((want.truncate() / want.z - got.truncate() / got.z).length() < 1e-6);
        }
    }

    #[test]
    fn unrepresentable_placements_are_reported_instead_of_stopping_a_save() {
        let mut pins = samples(|p| p);
        pins[5].1 += DVec2::new(40.0, -60.0);
        pins[1].1 += DVec2::new(-30.0, 25.0);
        assert!(!mapping(&pins, DVec3::Z, 1e-3).unwrap().1);
        let single = samples(|_| DVec2::ZERO);
        let (identity, exact) = mapping(&single, DVec3::Z, 1e-3).unwrap();
        assert!(!exact);
        assert_eq!(identity, DMat3::IDENTITY.to_cols_array());
    }
}
