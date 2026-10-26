//! Bundled materials (docs/07 §6 Materials "bundled" library): textures
//! generated procedurally and deterministically (hash-based value noise, no
//! clock or randomness), so the application ships no third-party images.
//! Using one writes its PNG once to a cache folder and creates the material
//! with `material-create`, like any texture file (docs/01 §3.3).

use std::path::PathBuf;

use image::{Rgb, RgbImage};

/// A bundled material.
pub struct Bundled {
    /// Material name.
    pub name: &'static str,
    /// Base color (`#RRGGBB`), the texture's average.
    pub color: &'static str,
    /// Real-world tile size, mm.
    pub size_mm: (f64, f64),
    /// Draws the tile.
    pub generate: fn() -> RgbImage,
}

/// The bundled materials, in panel order.
pub fn catalogue() -> Vec<Bundled> {
    vec![
        Bundled {
            name: "Oak",
            color: "#B08455",
            size_mm: (1200.0, 600.0),
            generate: oak,
        },
        Bundled {
            name: "Walnut",
            color: "#5E4030",
            size_mm: (1200.0, 600.0),
            generate: walnut,
        },
        Bundled {
            name: "Brick",
            color: "#9A4A36",
            size_mm: (860.0, 520.0),
            generate: brick,
        },
        Bundled {
            name: "Ceramic Tile",
            color: "#E6E6E2",
            size_mm: (600.0, 600.0),
            generate: tiles,
        },
        Bundled {
            name: "Concrete",
            color: "#9C9A96",
            size_mm: (2000.0, 2000.0),
            generate: concrete,
        },
        Bundled {
            name: "Marble",
            color: "#E8E6E1",
            size_mm: (1200.0, 1200.0),
            generate: marble,
        },
        Bundled {
            name: "Grass",
            color: "#5E7F3A",
            size_mm: (1500.0, 1500.0),
            generate: grass,
        },
        Bundled {
            name: "Stone Paving",
            color: "#8E8A82",
            size_mm: (1200.0, 1200.0),
            generate: paving,
        },
        Bundled {
            name: "Brushed Steel",
            color: "#A9ADB1",
            size_mm: (500.0, 500.0),
            generate: steel,
        },
        Bundled {
            name: "Fabric",
            color: "#5C6E8A",
            size_mm: (300.0, 300.0),
            generate: fabric,
        },
    ]
}

/// The tile's PNG in the cache folder, written when missing.
pub fn texture_path(b: &Bundled) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("planyrith-bundled-1");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(format!("{}.png", b.name.to_lowercase().replace(' ', "-")));
    if !file.exists() {
        (b.generate)().save(&file).map_err(|e| e.to_string())?;
    }
    Ok(file)
}

const SIZE: u32 = 256;

// ----- noise --------------------------------------------------------------------------------

fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343)
        ^ (y as u32).wrapping_mul(0xd816_3841)
        ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0x00ff_ffff) as f32 / 16_777_215.0
}

/// Smooth value noise in 0..1, tiling with `period` cells.
fn value(x: f32, y: f32, period: i32, seed: u32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let at = |i: i32, j: i32| {
        hash(
            (x0 as i32 + i).rem_euclid(period),
            (y0 as i32 + j).rem_euclid(period),
            seed,
        )
    };
    let a = at(0, 0) + (at(1, 0) - at(0, 0)) * s(fx);
    let b = at(0, 1) + (at(1, 1) - at(0, 1)) * s(fx);
    a + (b - a) * s(fy)
}

/// Fractal noise of `octaves`, tiling over the image (u, v in 0..1).
fn fbm(u: f32, v: f32, cells: i32, octaves: u32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut norm, mut freq) = (0.0, 0.5, 0.0, cells);
    for o in 0..octaves {
        sum += amp * value(u * freq as f32, v * freq as f32, freq, seed + o);
        norm += amp;
        amp *= 0.5;
        freq *= 2;
    }
    sum / norm
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> Rgb<u8> {
    let t = t.clamp(0.0, 1.0);
    Rgb([0, 1, 2].map(|i| (a[i] + (b[i] - a[i]) * t).round().clamp(0.0, 255.0) as u8))
}

fn image(f: impl Fn(f32, f32) -> Rgb<u8>) -> RgbImage {
    RgbImage::from_fn(SIZE, SIZE, |x, y| {
        f(
            (x as f32 + 0.5) / SIZE as f32,
            (y as f32 + 0.5) / SIZE as f32,
        )
    })
}

// ----- materials ----------------------------------------------------------------------------

/// Wood planks: grain along u, four boards across v.
fn wood(light: [f32; 3], dark: [f32; 3], seed: u32) -> RgbImage {
    image(|u, v| {
        let board = (v * 4.0).floor();
        let warp = fbm(u, v, 4, 3, seed + board as u32) * 2.0;
        let grain = ((v * 4.0 + warp) * 9.0 * std::f32::consts::TAU).sin() * 0.5 + 0.5;
        let fine = fbm(u * 0.25, v * 8.0, 16, 2, seed + 7);
        let mut t = grain * 0.6 + fine * 0.4;
        // A dark seam between boards.
        if (v * 4.0).fract() < 0.012 {
            t = 1.3;
        }
        mix(light, dark, t)
    })
}

fn oak() -> RgbImage {
    wood([196.0, 152.0, 104.0], [138.0, 96.0, 58.0], 11)
}

fn walnut() -> RgbImage {
    wood([118.0, 82.0, 60.0], [62.0, 40.0, 28.0], 23)
}

fn brick() -> RgbImage {
    image(|u, v| {
        let rows = 4.0;
        let row = (v * rows).floor();
        let shift = if row as i32 % 2 == 0 { 0.0 } else { 0.25 };
        let x = (u * 2.0 + shift).fract();
        let y = (v * rows).fract();
        let mortar = x < 0.03 || y < 0.07;
        let id = ((u * 2.0 + shift).floor() as i32, row as i32);
        let tone = hash(id.0, id.1, 5) * 0.35;
        let n = fbm(u, v, 16, 3, 9) * 0.3;
        if mortar {
            mix([196.0, 190.0, 180.0], [160.0, 154.0, 146.0], n)
        } else {
            mix([170.0, 78.0, 56.0], [112.0, 50.0, 38.0], tone + n)
        }
    })
}

fn tiles() -> RgbImage {
    image(|u, v| {
        let x = (u * 2.0).fract();
        let y = (v * 2.0).fract();
        let grout = x < 0.02 || y < 0.02 || x > 0.98 || y > 0.98;
        let n = fbm(u, v, 8, 3, 31) * 0.15;
        if grout {
            mix([150.0, 150.0, 146.0], [120.0, 120.0, 116.0], n)
        } else {
            mix([236.0, 236.0, 232.0], [214.0, 214.0, 208.0], n)
        }
    })
}

fn concrete() -> RgbImage {
    image(|u, v| {
        let n = fbm(u, v, 8, 5, 41);
        let speck = hash((u * 256.0) as i32, (v * 256.0) as i32, 43);
        let t = n * 0.8 + if speck > 0.97 { 0.4 } else { 0.0 };
        mix([176.0, 174.0, 170.0], [124.0, 122.0, 118.0], t)
    })
}

fn marble() -> RgbImage {
    image(|u, v| {
        let turbulence = fbm(u, v, 4, 5, 53) * 6.0;
        let vein = ((u * 3.0 + v * 2.0 + turbulence) * std::f32::consts::PI)
            .sin()
            .abs();
        let t = (1.0 - vein).powf(6.0);
        mix([238.0, 236.0, 231.0], [120.0, 118.0, 116.0], t * 0.8)
    })
}

fn grass() -> RgbImage {
    image(|u, v| {
        let n = fbm(u, v, 16, 4, 61);
        let blades = hash((u * 256.0) as i32, (v * 128.0) as i32, 67) * 0.3;
        mix([118.0, 156.0, 72.0], [58.0, 92.0, 34.0], n * 0.8 + blades)
    })
}

fn paving() -> RgbImage {
    image(|u, v| {
        // Irregular stones: the nearest of jittered cell centers.
        let cells = 4;
        let (cx, cy) = (
            (u * cells as f32).floor() as i32,
            (v * cells as f32).floor() as i32,
        );
        let mut best = (f32::MAX, f32::MAX, 0);
        for j in -1..=1 {
            for i in -1..=1 {
                let (gx, gy) = (cx + i, cy + j);
                let (wx, wy) = (gx.rem_euclid(cells), gy.rem_euclid(cells));
                let px = (gx as f32 + 0.2 + 0.6 * hash(wx, wy, 71)) / cells as f32;
                let py = (gy as f32 + 0.2 + 0.6 * hash(wx, wy, 73)) / cells as f32;
                let d = ((u - px).powi(2) + (v - py).powi(2)).sqrt();
                if d < best.0 {
                    best = (d, best.0, wx * 7 + wy);
                } else if d < best.1 {
                    best.1 = d;
                }
            }
        }
        let edge = best.1 - best.0;
        let n = fbm(u, v, 16, 3, 79) * 0.3;
        if edge < 0.012 {
            mix([96.0, 92.0, 86.0], [70.0, 66.0, 60.0], n)
        } else {
            let tone = hash(best.2, 3, 83) * 0.4;
            mix([168.0, 162.0, 152.0], [116.0, 110.0, 100.0], tone + n)
        }
    })
}

fn steel() -> RgbImage {
    image(|u, v| {
        let streak = fbm(u * 0.05, v, 64, 3, 89);
        let fine = hash((u * 256.0) as i32 / 8, (v * 256.0) as i32, 97) * 0.15;
        mix([190.0, 194.0, 198.0], [140.0, 144.0, 148.0], streak + fine)
    })
}

fn fabric() -> RgbImage {
    image(|u, v| {
        let weave = (((u * 64.0).floor() + (v * 64.0).floor()) as i32 % 2) as f32;
        let thread = ((u * 64.0).fract() - 0.5)
            .abs()
            .max(((v * 64.0).fract() - 0.5).abs());
        let n = fbm(u, v, 8, 3, 101) * 0.3;
        mix(
            [104.0, 124.0, 156.0],
            [64.0, 80.0, 108.0],
            weave * 0.25 + thread * 0.4 + n,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_textures_are_deterministic_and_tile() {
        for b in catalogue() {
            let a = (b.generate)();
            let again = (b.generate)();
            assert_eq!(a, again, "{} is deterministic", b.name);
            assert_eq!(a.dimensions(), (SIZE, SIZE));
        }
        // Value noise tiles across the image edge.
        let left = value(0.0, 3.3, 8, 1);
        let right = value(8.0, 3.3, 8, 1);
        assert!((left - right).abs() < 1e-6);
    }
}
