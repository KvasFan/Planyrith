//! Implements docs/05-modeling-tools.md §5.18 (standard parts, milestone
//! 1.17): size tables of common metric fasteners, pins and ball bearings,
//! and their simplified solids.
//!
//! Dimensions are the nominal values of the standards named (threads are
//! not modeled: a bolt's shank is a plain cylinder of the nominal
//! diameter). Each part's axis is +Z: a screw's head sits on z = 0 with the
//! shank going down; nuts, washers, pins and bearings stand on z = 0.

use glam::{DAffine3, DVec2, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geom::boolean::{BooleanOp, boolean};
use crate::geom::fabrication::{ProfileFrame, prism};
use crate::geom::{GeometryError, HalfEdgeMesh};
use crate::tolerance::Tolerances;

/// What kind of standard part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PartKind {
    /// Hexagon head bolt, ISO 4017.
    HexBolt,
    /// Hexagon socket head cap screw, ISO 4762.
    SocketScrew,
    /// Hexagon nut, ISO 4032.
    HexNut,
    /// Plain washer, ISO 7089.
    Washer,
    /// Parallel pin, ISO 8734.
    DowelPin,
    /// Deep groove ball bearing, ISO 15 (series 60 and 62).
    Bearing,
}

impl PartKind {
    /// Every kind, in order.
    pub const ALL: [PartKind; 6] = [
        PartKind::HexBolt,
        PartKind::SocketScrew,
        PartKind::HexNut,
        PartKind::Washer,
        PartKind::DowelPin,
        PartKind::Bearing,
    ];

    /// The sizes offered.
    pub fn sizes(self) -> Vec<&'static str> {
        match self {
            PartKind::Bearing => BEARINGS.iter().map(|b| b.0).collect(),
            PartKind::DowelPin => PINS.to_vec(),
            _ => METRIC.iter().map(|m| m.0).collect(),
        }
    }

    /// Whether a length is given.
    pub fn has_length(self) -> bool {
        matches!(
            self,
            PartKind::HexBolt | PartKind::SocketScrew | PartKind::DowelPin
        )
    }
}

/// Metric sizes: name, d, across flats s (bolt and nut), bolt head k,
/// nut height m, socket head diameter dk, socket key, washer d1, d2, h.
#[allow(clippy::type_complexity)]
const METRIC: [(&str, f64, f64, f64, f64, f64, f64, f64, f64, f64); 10] = [
    ("M3", 3.0, 5.5, 2.0, 2.4, 5.5, 2.5, 3.2, 7.0, 0.5),
    ("M4", 4.0, 7.0, 2.8, 3.2, 7.0, 3.0, 4.3, 9.0, 0.8),
    ("M5", 5.0, 8.0, 3.5, 4.7, 8.5, 4.0, 5.3, 10.0, 1.0),
    ("M6", 6.0, 10.0, 4.0, 5.2, 10.0, 5.0, 6.4, 12.0, 1.6),
    ("M8", 8.0, 13.0, 5.3, 6.8, 13.0, 6.0, 8.4, 16.0, 1.6),
    ("M10", 10.0, 16.0, 6.4, 8.4, 16.0, 8.0, 10.5, 20.0, 2.0),
    ("M12", 12.0, 18.0, 7.5, 10.8, 18.0, 10.0, 13.0, 24.0, 2.5),
    ("M16", 16.0, 24.0, 10.0, 14.8, 24.0, 14.0, 17.0, 30.0, 3.0),
    ("M20", 20.0, 30.0, 12.5, 18.0, 30.0, 17.0, 21.0, 37.0, 3.0),
    ("M24", 24.0, 36.0, 15.0, 21.5, 36.0, 19.0, 25.0, 44.0, 4.0),
];

/// Pin diameters, mm.
const PINS: [&str; 9] = ["2", "3", "4", "5", "6", "8", "10", "12", "16"];

/// Bearings: designation, bore d, outside D, width B.
const BEARINGS: [(&str, f64, f64, f64); 22] = [
    ("6000", 10.0, 26.0, 8.0),
    ("6001", 12.0, 28.0, 8.0),
    ("6002", 15.0, 32.0, 9.0),
    ("6003", 17.0, 35.0, 10.0),
    ("6004", 20.0, 42.0, 12.0),
    ("6005", 25.0, 47.0, 12.0),
    ("6006", 30.0, 55.0, 13.0),
    ("6007", 35.0, 62.0, 14.0),
    ("6008", 40.0, 68.0, 15.0),
    ("6009", 45.0, 75.0, 16.0),
    ("6010", 50.0, 80.0, 16.0),
    ("6200", 10.0, 30.0, 9.0),
    ("6201", 12.0, 32.0, 10.0),
    ("6202", 15.0, 35.0, 11.0),
    ("6203", 17.0, 40.0, 12.0),
    ("6204", 20.0, 47.0, 14.0),
    ("6205", 25.0, 52.0, 15.0),
    ("6206", 30.0, 62.0, 16.0),
    ("6207", 35.0, 72.0, 17.0),
    ("6208", 40.0, 80.0, 18.0),
    ("6209", 45.0, 85.0, 19.0),
    ("6210", 50.0, 90.0, 20.0),
];

/// A generated standard part.
#[derive(Debug, Clone)]
pub struct StandardPart {
    /// Its designation, also its component name (`ISO 4017 M8x30`).
    pub designation: String,
    /// What it is (`Hexagon head bolt M8 × 30`).
    pub description: String,
    /// The standard.
    pub standard: &'static str,
    /// Its solid.
    pub mesh: HalfEdgeMesh,
}

const SEGMENTS: usize = 24;

fn circle(r: f64) -> Vec<DVec2> {
    (0..SEGMENTS)
        .map(|k| {
            let t = std::f64::consts::TAU * k as f64 / SEGMENTS as f64;
            DVec2::new(r * libm::cos(t), r * libm::sin(t))
        })
        .collect()
}

fn hexagon(across_flats: f64) -> Vec<DVec2> {
    let r = across_flats / 3f64.sqrt();
    (0..6)
        .map(|k| {
            let t = std::f64::consts::TAU * k as f64 / 6.0;
            DVec2::new(r * libm::cos(t), r * libm::sin(t))
        })
        .collect()
}

/// A prism of `loops` from z = `z0` to z = `z1`.
fn slab(
    loops: &[Vec<DVec2>],
    z0: f64,
    z1: f64,
    tol: &Tolerances,
) -> Result<HalfEdgeMesh, GeometryError> {
    prism(
        ProfileFrame {
            origin: DVec3::ZERO,
            u: DVec3::X,
            v: DVec3::Y,
        },
        loops,
        DVec3::Z,
        (DVec3::new(0.0, 0.0, z0), DVec3::Z),
        (DVec3::new(0.0, 0.0, z1), DVec3::Z),
        tol,
    )
}

fn join(
    op: BooleanOp,
    a: &HalfEdgeMesh,
    b: &HalfEdgeMesh,
    tol: &Tolerances,
) -> Result<HalfEdgeMesh, GeometryError> {
    let r = boolean(op, a, &DAffine3::IDENTITY, b, &DAffine3::IDENTITY, tol)?;
    r.solids
        .into_iter()
        .next()
        .ok_or_else(|| GeometryError::NotApplicable("a standard part came out empty".into()))
}

/// Softens the edges between faces turning by less than 20°, so round
/// surfaces shade smoothly while corners stay drawn.
fn soften_round(mut mesh: HalfEdgeMesh) -> HalfEdgeMesh {
    let limit = libm::cos(20f64.to_radians());
    let edges: Vec<_> = mesh.edges().map(|(e, _)| e).collect();
    for e in edges {
        let Ok([Some(a), Some(b)]) = mesh.edge_faces(e) else {
            continue;
        };
        let (Some(fa), Some(fb)) = (mesh.face(a), mesh.face(b)) else {
            continue;
        };
        if fa.plane.normal.dot(fb.plane.normal) > limit {
            let _ = mesh.set_edge_flags(
                e,
                crate::geom::EdgeFlags::SOFT | crate::geom::EdgeFlags::SMOOTH,
                &mut crate::geom::NoDeltas,
            );
        }
    }
    mesh
}

/// The part `kind` of `size` (and `length`, mm, for screws and pins).
pub fn standard_part(
    kind: PartKind,
    size: &str,
    length: Option<f64>,
    tol: &Tolerances,
) -> Result<StandardPart, GeometryError> {
    let mut part = standard_part_raw(kind, size, length, tol)?;
    part.mesh = soften_round(part.mesh);
    Ok(part)
}

fn standard_part_raw(
    kind: PartKind,
    size: &str,
    length: Option<f64>,
    tol: &Tolerances,
) -> Result<StandardPart, GeometryError> {
    let unknown = || {
        GeometryError::NotApplicable(format!(
            "no size `{size}`; sizes: {}",
            kind.sizes().join(", ")
        ))
    };
    let need_length = || -> Result<f64, GeometryError> {
        match length {
            Some(l) if l > 0.0 && l <= 1000.0 => Ok(l),
            _ => Err(GeometryError::NotApplicable(
                "give a length from 0 to 1000 mm".into(),
            )),
        }
    };
    match kind {
        PartKind::Bearing => {
            let (name, d, big_d, b) = *BEARINGS.iter().find(|x| x.0 == size).ok_or_else(unknown)?;
            // Inner and outer rings; the balls' gap between them is open.
            let wall = (big_d - d) / 3.0;
            let outer = slab(
                &[circle(big_d / 2.0), circle(big_d / 2.0 - wall)],
                0.0,
                b,
                tol,
            )?;
            let inner = slab(&[circle(d / 2.0 + wall), circle(d / 2.0)], 0.0, b, tol)?;
            let (mut idx, _) = outer.to_indexed();
            let (inner_idx, _) = inner.to_indexed();
            let base = idx.vertices.len() as u32;
            idx.vertices.extend(inner_idx.vertices);
            let edge_base = idx.edges.len() as u32;
            for mut e in inner_idx.edges {
                e.a += base;
                e.b += base;
                idx.edges.push(e);
            }
            for mut f in inner_idx.faces {
                for l in &mut f.loops {
                    for de in l.iter_mut() {
                        *de = if de.is_reversed() {
                            crate::geom::mesh::DirectedEdgeIndex::reversed(de.index() + edge_base)
                        } else {
                            crate::geom::mesh::DirectedEdgeIndex::forward(de.index() + edge_base)
                        };
                    }
                }
                idx.faces.push(f);
            }
            Ok(StandardPart {
                designation: format!("Bearing {name}"),
                description: format!("Deep groove ball bearing {name}, {d} × {big_d} × {b}"),
                standard: "ISO 15",
                mesh: HalfEdgeMesh::from_indexed(&idx, tol)?,
            })
        }
        PartKind::DowelPin => {
            if !PINS.contains(&size) {
                return Err(unknown());
            }
            let d: f64 = size.parse().map_err(|_| unknown())?;
            let l = need_length()?;
            Ok(StandardPart {
                designation: format!("ISO 8734 {size}x{l}"),
                description: format!("Parallel pin ⌀{size} × {l}"),
                standard: "ISO 8734",
                mesh: slab(&[circle(d / 2.0)], 0.0, l, tol)?,
            })
        }
        _ => {
            let (name, d, s, k, m, dk, key, d1, d2, h) =
                *METRIC.iter().find(|x| x.0 == size).ok_or_else(unknown)?;
            match kind {
                PartKind::HexBolt => {
                    let l = need_length()?;
                    let head = slab(&[hexagon(s)], 0.0, k, tol)?;
                    let shank = slab(&[circle(d / 2.0)], -l, 0.0, tol)?;
                    Ok(StandardPart {
                        designation: format!("ISO 4017 {name}x{l}"),
                        description: format!("Hexagon head bolt {name} × {l}"),
                        standard: "ISO 4017",
                        mesh: join(BooleanOp::Union, &head, &shank, tol)?,
                    })
                }
                PartKind::SocketScrew => {
                    let l = need_length()?;
                    let head = slab(&[circle(dk / 2.0)], 0.0, d, tol)?;
                    let socket = slab(&[hexagon(key)], d * 0.4, d * 1.5, tol)?;
                    let head = join(BooleanOp::Subtract, &head, &socket, tol)?;
                    let shank = slab(&[circle(d / 2.0)], -l, 0.0, tol)?;
                    Ok(StandardPart {
                        designation: format!("ISO 4762 {name}x{l}"),
                        description: format!("Hexagon socket head cap screw {name} × {l}"),
                        standard: "ISO 4762",
                        mesh: join(BooleanOp::Union, &head, &shank, tol)?,
                    })
                }
                PartKind::HexNut => Ok(StandardPart {
                    designation: format!("ISO 4032 {name}"),
                    description: format!("Hexagon nut {name}"),
                    standard: "ISO 4032",
                    mesh: slab(&[hexagon(s), circle(d / 2.0)], 0.0, m, tol)?,
                }),
                PartKind::Washer => Ok(StandardPart {
                    designation: format!("ISO 7089 {name}"),
                    description: format!("Plain washer {name}, {d1} × {d2} × {h}"),
                    standard: "ISO 7089",
                    mesh: slab(&[circle(d2 / 2.0), circle(d1 / 2.0)], 0.0, h, tol)?,
                }),
                _ => Err(unknown()),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_size_of_every_part_is_a_solid() {
        let tol = Tolerances::default();
        for kind in PartKind::ALL {
            for size in kind.sizes() {
                let p = standard_part(kind, size, Some(20.0), &tol).unwrap();
                assert!(p.mesh.is_solid(), "{}", p.designation);
                assert!(p.mesh.volume().unwrap() > 0.0, "{}", p.designation);
            }
        }
        let bolt = standard_part(PartKind::HexBolt, "M8", Some(30.0), &tol).unwrap();
        assert_eq!(bolt.designation, "ISO 4017 M8x30");
        // Head prism plus a 24-sided shank.
        let head = 13.0 * 13.0 * 3f64.sqrt() / 2.0 * 5.3;
        let shank = 24.0 * 0.5 * 16.0 * libm::sin(std::f64::consts::TAU / 24.0) * 30.0;
        assert!((bolt.mesh.volume().unwrap() - head - shank).abs() < 1e-6);
        assert!(standard_part(PartKind::HexNut, "M7", None, &tol).is_err());
        assert!(standard_part(PartKind::HexBolt, "M8", None, &tol).is_err());
    }
}
