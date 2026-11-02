//! Implements the pin placements of docs/05-modeling-tools.md §5.9: the map
//! from points of a face's frame to points of its texture that puts each
//! pin's texture point where the pin is.
//!
//! Every solver works on coordinates moved to their centroid and scaled to
//! unit spread first, so that millimeter-sized faces far from the origin are
//! as well conditioned as small ones near it.

use glam::{DAffine2, DMat2, DMat3, DVec2, DVec3};

/// Centroid and spread of `points`, and the map taking them to unit spread
/// about the origin; `None` when they coincide.
fn normalizer(points: &[DVec2]) -> Option<DAffine2> {
    let n = points.len() as f64;
    let center = points.iter().copied().sum::<DVec2>() / n;
    let spread = (points
        .iter()
        .map(|p| p.distance_squared(center))
        .sum::<f64>()
        / n)
        .sqrt();
    (spread.is_finite() && spread > 1e-12).then(|| {
        DAffine2::from_mat2_translation(
            DMat2::from_diagonal(DVec2::splat(1.0 / spread)),
            -center / spread,
        )
    })
}

/// The map with rotation and uniform scale, no skew, taking `from[i]` to
/// `to[i]`; `None` when the source points coincide.
pub fn similarity(from: [DVec2; 2], to: [DVec2; 2]) -> Option<DAffine2> {
    let (a, b) = (from[1] - from[0], to[1] - to[0]);
    let len = a.length_squared();
    if !(len.is_finite() && len > 1e-18) {
        return None;
    }
    // b = z·a with z = (c, s) a complex factor: rotation and scale.
    let c = a.dot(b) / len;
    let s = a.perp_dot(b) / len;
    let m = DMat2::from_cols(DVec2::new(c, s), DVec2::new(-s, c));
    Some(DAffine2::from_mat2_translation(m, to[0] - m * from[0]))
}

/// The affine map closest, in least squares, to taking each `from` point to
/// its `to` point: exact for three. `None` for fewer than three pairs or
/// source points on one line.
pub fn affine_fit(pairs: &[(DVec2, DVec2)]) -> Option<DAffine2> {
    if pairs.len() < 3 {
        return None;
    }
    let from: Vec<DVec2> = pairs.iter().map(|p| p.0).collect();
    let to: Vec<DVec2> = pairs.iter().map(|p| p.1).collect();
    let (nf, nt) = (normalizer(&from)?, normalizer(&to)?);
    // Normal equations: Σ (x, y, 1)ᵀ(x, y, 1) · [row] = Σ (x, y, 1) · target.
    let mut ata = DMat3::ZERO;
    let (mut atx, mut aty) = (DVec3::ZERO, DVec3::ZERO);
    for (f, t) in from.iter().zip(&to) {
        let r = nf.transform_point2(*f).extend(1.0);
        let t = nt.transform_point2(*t);
        ata += DMat3::from_cols(r * r.x, r * r.y, r * r.z);
        atx += r * t.x;
        aty += r * t.y;
    }
    // Normalized points span about unit area; collinear ones none.
    if ata.determinant().abs() < 1e-9 * (pairs.len() as f64).powi(3) {
        return None;
    }
    let inverse = ata.inverse();
    let (rx, ry) = (inverse * atx, inverse * aty);
    let m = DAffine2::from_mat2_translation(
        DMat2::from_cols(DVec2::new(rx.x, ry.x), DVec2::new(rx.y, ry.y)),
        DVec2::new(rx.z, ry.z),
    );
    Some(nt.inverse() * m * nf)
}

/// The projective map (homogeneous, last entry 1) taking each of four `from`
/// points to its `to` point. `None` when three points of either set are on
/// one line, or the map would send a pin to infinity.
pub fn homography(pairs: &[(DVec2, DVec2); 4]) -> Option<DMat3> {
    let from: Vec<DVec2> = pairs.iter().map(|p| p.0).collect();
    let to: Vec<DVec2> = pairs.iter().map(|p| p.1).collect();
    for set in [&from, &to] {
        for skip in 0..4 {
            let [a, b, c] = [0, 1, 2, 3]
                .into_iter()
                .filter(|&i| i != skip)
                .map(|i| set[i])
                .collect::<Vec<_>>()[..]
            else {
                return None;
            };
            let spread = (b - a).length().max((c - a).length());
            if (b - a).perp_dot(c - a).abs() <= 1e-9 * spread * spread {
                return None;
            }
        }
    }
    let (nf, nt) = (normalizer(&from)?, normalizer(&to)?);
    // h = (h11 h12 h13 h21 h22 h23 h31 h32), h33 = 1:
    // u = (h11 x + h12 y + h13) / (h31 x + h32 y + 1), likewise v.
    let mut a = [[0.0f64; 9]; 8];
    for (i, (f, t)) in from.iter().zip(&to).enumerate() {
        let (p, q) = (nf.transform_point2(*f), nt.transform_point2(*t));
        a[2 * i] = [p.x, p.y, 1.0, 0.0, 0.0, 0.0, -q.x * p.x, -q.x * p.y, q.x];
        a[2 * i + 1] = [0.0, 0.0, 0.0, p.x, p.y, 1.0, -q.y * p.x, -q.y * p.y, q.y];
    }
    let h = solve8(a)?;
    let m = DMat3::from_cols(
        DVec3::new(h[0], h[3], h[6]),
        DVec3::new(h[1], h[4], h[7]),
        DVec3::new(h[2], h[5], 1.0),
    );
    let to_affine = |n: DAffine2| DMat3::from(n);
    let full = to_affine(nt.inverse()) * m * to_affine(nf);
    let scale = full.z_axis.z;
    if !(scale.is_finite() && scale.abs() > 1e-300) {
        return None;
    }
    let full = full * (1.0 / scale);
    // Every pin stays at a finite point, on the same side of infinity.
    let w: Vec<f64> = from.iter().map(|f| (full * f.extend(1.0)).z).collect();
    (w.iter().all(|w| *w > 1e-12) || w.iter().all(|w| *w < -1e-12)).then_some(full)
}

/// Gaussian elimination with partial pivoting on an 8 × 8 system whose
/// right-hand side is the ninth column.
fn solve8(mut a: [[f64; 9]; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let pivot = (col..8).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        let pivot_row = a[col];
        for (row, values) in a.iter_mut().enumerate() {
            if row != col {
                let f = values[col] / pivot_row[col];
                for (v, p) in values.iter_mut().zip(&pivot_row).skip(col) {
                    *v -= f * p;
                }
            }
        }
    }
    let mut x = [0.0; 8];
    for (i, row) in a.iter().enumerate() {
        x[i] = row[8] / row[i];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn apply(m: &DMat3, p: DVec2) -> DVec2 {
        let h = *m * p.extend(1.0);
        h.truncate() / h.z
    }

    fn arb_point() -> impl Strategy<Value = DVec2> {
        (-5000.0f64..5000.0, -5000.0f64..5000.0).prop_map(|(x, y)| DVec2::new(x, y))
    }

    #[test]
    fn degenerate_pins_have_no_map() {
        let p = DVec2::new(10.0, 20.0);
        assert!(similarity([p, p], [p, DVec2::ZERO]).is_none());
        let line = [
            (DVec2::ZERO, DVec2::ZERO),
            (DVec2::X, DVec2::Y),
            (DVec2::X * 2.0, DVec2::ONE),
        ];
        assert!(affine_fit(&line).is_none());
        let square = [DVec2::ZERO, DVec2::X, DVec2::ONE, DVec2::Y];
        let mut pairs = square.map(|s| (s, s));
        pairs[3].0 = DVec2::new(2.0, 0.0);
        assert!(homography(&pairs).is_none(), "three source pins on a line");
    }

    proptest! {
        // About one case in twenty is degenerate and skipped.
        #![proptest_config(ProptestConfig { max_global_rejects: 1 << 20, ..ProptestConfig::default() })]

        /// Two pins fix position, rotation and scale.
        #[test]
        fn similarities_are_recovered(a in arb_point(), b in arb_point(), angle in -3.0f64..3.0, scale in 0.01f64..100.0, shift in arb_point()) {
            prop_assume!(a.distance(b) > 1.0);
            let truth = DAffine2::from_scale_angle_translation(DVec2::splat(scale), angle, shift);
            let m = similarity([a, b], [truth.transform_point2(a), truth.transform_point2(b)]).unwrap();
            let probe = DVec2::new(123.0, -45.0);
            let error = m.transform_point2(probe).distance(truth.transform_point2(probe));
            prop_assert!(error < 1e-6 * scale * (1.0 + probe.length()), "{error}");
        }

        /// Three pins fix an affine map exactly; a fourth consistent one changes nothing.
        #[test]
        fn affine_maps_are_recovered(pts in prop::collection::vec(arb_point(), 4), m in prop::array::uniform4(-3.0f64..3.0), shift in arb_point()) {
            let lin = DMat2::from_cols(DVec2::new(m[0], m[1]), DVec2::new(m[2], m[3]));
            prop_assume!(lin.determinant().abs() > 0.05);
            let (a, b, c) = (pts[0], pts[1], pts[2]);
            // Pins about a line amplify rounding without bound; real ones
            // spread at least 3° apart.
            prop_assume!((b - a).perp_dot(c - a).abs() > 0.05 * (b - a).length() * (c - a).length());
            let truth = DAffine2::from_mat2_translation(lin, shift);
            let pairs: Vec<_> = pts.iter().map(|p| (*p, truth.transform_point2(*p))).collect();
            for n in [3, 4] {
                let fit = affine_fit(&pairs[..n]).unwrap();
                for p in &pts {
                    let error = fit.transform_point2(*p).distance(truth.transform_point2(*p));
                    // A tenth of the kernel's length tolerance (1 µm).
                    prop_assert!(error < 1e-4, "{n} pins: {error}");
                }
            }
        }

        /// Four pins at the corners of a quad fix a perspective map.
        #[test]
        fn homographies_are_recovered(corners in prop::array::uniform4(0.0f64..1.0), h in prop::array::uniform2(-1e-4f64..1e-4), shift in arb_point()) {
            // A convex quad: jittered corners of a 2 m square.
            let quad = [
                DVec2::new(0.0, 0.0) + DVec2::splat(corners[0] * 300.0),
                DVec2::new(2000.0, 0.0) + DVec2::new(-corners[1] * 300.0, corners[1] * 300.0),
                DVec2::new(2000.0, 2000.0) - DVec2::splat(corners[2] * 300.0),
                DVec2::new(0.0, 2000.0) + DVec2::new(corners[3] * 300.0, -corners[3] * 300.0),
            ];
            let truth = DMat3::from_cols(
                DVec3::new(1.3, 0.2, h[0]),
                DVec3::new(-0.4, 0.9, h[1]),
                shift.extend(1.0),
            );
            let pairs = quad.map(|p| (p, apply(&truth, p)));
            let m = homography(&pairs).unwrap();
            for p in quad.iter().chain([DVec2::new(1000.0, 1000.0)].iter()) {
                let error = apply(&m, *p).distance(apply(&truth, *p));
                prop_assert!(error < 1e-4, "{error}");
            }
        }
    }
}
