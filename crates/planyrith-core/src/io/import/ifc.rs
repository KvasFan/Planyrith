//! Implements docs/09-file-formats.md §5 IFC import (IFC2X3 and IFC4): every
//! product with a body representation becomes a group named after it,
//! tagged with its class and carrying `ifc`/`class` and `ifc`/`storey`
//! attributes (so exporting it again keeps them). Geometry: triangulated
//! and polygonal face sets, faceted B-reps, extruded profiles (rectangle,
//! circle, closed polylines and indexed poly curves, with voids), mapped
//! items, and the first operand of boolean results; placements, the length
//! unit and surface colors (docs/09 Decisions, IFC import).

use std::collections::BTreeMap;
use std::io::Read;

use glam::{DAffine3, DMat3, DVec3};

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::commands::transaction::AttrTarget;
use crate::diagnostics::{Warning, WarningCode};
use crate::document::{ContainerRef, EntityView, Rgba8, Tag, Value as AttrValue};
use crate::io::import::build::{
    ImportedMaterial, ImportedMesh, ImportedNode, ImportedScene, build_scene,
};
use crate::io::import::step21::{Instance, Value, parse};
use crate::io::import::{
    Confidence, ImportFormat, ImportOptions, ImportReport, Importer, source_stem,
};
use crate::io::{IoError, Progress};
use crate::units::LengthUnit;

/// The IFC importer.
#[derive(Debug, Default, Clone, Copy)]
pub struct IfcImporter;

/// Segments of a circle profile.
const CIRCLE_SEGMENTS: usize = 24;
/// Deepest chain of placements or mapped items followed.
const MAX_DEPTH: usize = 64;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "ifc".into(),
        reason: reason.into(),
    }
}

/// Product classes that are not built things (they are not imported).
const SKIPPED: [&str; 8] = [
    "IFCSITE",
    "IFCBUILDING",
    "IFCBUILDINGSTOREY",
    "IFCSPACE",
    "IFCOPENINGELEMENT",
    "IFCPROJECT",
    "IFCANNOTATION",
    "IFCGRID",
];

/// Readable names of common classes (the file has them upper case).
const NAMES: [&str; 40] = [
    "IfcWall",
    "IfcWallStandardCase",
    "IfcSlab",
    "IfcRoof",
    "IfcColumn",
    "IfcBeam",
    "IfcStair",
    "IfcStairFlight",
    "IfcRailing",
    "IfcCovering",
    "IfcPlate",
    "IfcMember",
    "IfcFurniture",
    "IfcFurnishingElement",
    "IfcDoor",
    "IfcWindow",
    "IfcCurtainWall",
    "IfcFooting",
    "IfcPile",
    "IfcRamp",
    "IfcRampFlight",
    "IfcBuildingElementProxy",
    "IfcFlowTerminal",
    "IfcFlowSegment",
    "IfcFlowFitting",
    "IfcDistributionElement",
    "IfcSanitaryTerminal",
    "IfcLightFixture",
    "IfcChimney",
    "IfcShadingDevice",
    "IfcBuildingElementPart",
    "IfcReinforcingBar",
    "IfcTransportElement",
    "IfcVirtualElement",
    "IfcElementAssembly",
    "IfcDiscreteAccessory",
    "IfcMechanicalFastener",
    "IfcFastener",
    "IfcGeographicElement",
    "IfcCivilElement",
];

fn class_name(kind: &str) -> String {
    NAMES
        .iter()
        .find(|n| n.eq_ignore_ascii_case(kind))
        .map_or_else(
            || {
                let rest = kind.strip_prefix("IFC").unwrap_or(kind).to_lowercase();
                let mut c = rest.chars();
                format!(
                    "Ifc{}{}",
                    c.next().map(|f| f.to_ascii_uppercase()).unwrap_or_default(),
                    c.as_str()
                )
            },
            |n| (*n).to_string(),
        )
}

/// A product's geometry: triangles per color.
#[derive(Default)]
struct Shape {
    positions: Vec<DVec3>,
    triangles: Vec<[u32; 3]>,
    colors: Vec<Option<Rgba8>>,
}

impl Shape {
    /// The shape split into edge-connected parts (triangles sharing a
    /// corner position), so coinciding faces of separate parts never
    /// merge.
    fn parts(&self) -> Vec<Shape> {
        let n = self.triangles.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn root(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        let mut first_at: BTreeMap<[u64; 3], usize> = BTreeMap::new();
        for (t, tri) in self.triangles.iter().enumerate() {
            for &i in tri {
                let p = self.positions[i as usize];
                let key = [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
                let other = *first_at.entry(key).or_insert(t);
                let (a, b) = (root(&mut parent, t), root(&mut parent, other));
                parent[a] = b;
            }
        }
        let mut index: BTreeMap<usize, usize> = BTreeMap::new();
        let mut out: Vec<Shape> = Vec::new();
        for (t, tri) in self.triangles.iter().enumerate() {
            let r = root(&mut parent, t);
            let k = *index.entry(r).or_insert_with(|| {
                out.push(Shape::default());
                out.len() - 1
            });
            let part = &mut out[k];
            let base = part.positions.len() as u32;
            part.positions
                .extend(tri.iter().map(|&i| self.positions[i as usize]));
            part.triangles.push([base, base + 1, base + 2]);
            part.colors.push(self.colors[t]);
        }
        out
    }

    /// A planar polygon (outer ring, holes) as triangles, keeping the
    /// outer ring's side.
    fn polygon(&mut self, rings: &[Vec<DVec3>], color: Option<Rgba8>) {
        let Some(outer) = rings.first().filter(|o| o.len() >= 3) else {
            return;
        };
        let mut normal = DVec3::ZERO;
        for (i, a) in outer.iter().enumerate() {
            normal += a.cross(outer[(i + 1) % outer.len()]);
        }
        let Some(normal) = normal.try_normalize() else {
            return;
        };
        let u = normal.any_orthonormal_vector();
        let v = normal.cross(u);
        let mut points = Vec::new();
        let mut holes = Vec::new();
        for (r, ring) in rings.iter().enumerate() {
            if ring.len() < 3 {
                continue;
            }
            if r > 0 {
                holes.push(points.len());
            }
            points.extend(ring.iter().copied());
        }
        let flat: Vec<f64> = points.iter().flat_map(|p| [p.dot(u), p.dot(v)]).collect();
        let Ok(tris) = earcutr::earcut(&flat, &holes, 2) else {
            return;
        };
        let base = self.positions.len() as u32;
        self.positions.extend(points);
        for t in tris.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| base + i as u32);
            let pa = self.positions[a as usize];
            let n = (self.positions[b as usize] - pa).cross(self.positions[c as usize] - pa);
            self.triangles.push(if n.dot(normal) < 0.0 {
                [a, c, b]
            } else {
                [a, b, c]
            });
            self.colors.push(color);
        }
    }
}

/// The model being read.
struct Model {
    data: BTreeMap<u64, Instance>,
    /// Millimeters per file length unit.
    unit: f64,
    /// Surface colors by representation item.
    colors: BTreeMap<u64, Rgba8>,
    skipped: BTreeMap<String, usize>,
}

impl Model {
    fn get(&self, v: &Value) -> Option<&Instance> {
        self.data.get(&v.as_ref_id()?)
    }

    fn point(&self, v: &Value) -> Option<DVec3> {
        let p = self.get(v)?;
        let c = p.args.first()?.as_list()?;
        let n = |i: usize| c.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        Some(DVec3::new(n(0), n(1), n(2)) * self.unit)
    }

    fn direction(&self, v: &Value) -> Option<DVec3> {
        let d = self.get(v)?;
        let c = d.args.first()?.as_list()?;
        let n = |i: usize| c.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        DVec3::new(n(0), n(1), n(2)).try_normalize()
    }

    /// `IfcAxis2Placement3D` / `2D` as a transform.
    fn axes(&self, v: &Value) -> DAffine3 {
        let Some(a) = self.get(v) else {
            return DAffine3::IDENTITY;
        };
        let origin = a
            .args
            .first()
            .and_then(|p| self.point(p))
            .unwrap_or_default();
        if a.kind == "IFCAXIS2PLACEMENT2D" {
            let x = a
                .args
                .get(1)
                .and_then(|d| self.direction(d))
                .unwrap_or(DVec3::X);
            let x = DVec3::new(x.x, x.y, 0.0)
                .try_normalize()
                .unwrap_or(DVec3::X);
            return DAffine3 {
                matrix3: DMat3::from_cols(x, DVec3::Z.cross(x), DVec3::Z),
                translation: origin,
            };
        }
        let z = a
            .args
            .get(1)
            .and_then(|d| self.direction(d))
            .unwrap_or(DVec3::Z);
        let x0 = a
            .args
            .get(2)
            .and_then(|d| self.direction(d))
            .unwrap_or(DVec3::X);
        let x = (x0 - z * x0.dot(z))
            .try_normalize()
            .unwrap_or_else(|| z.any_orthonormal_vector());
        DAffine3 {
            matrix3: DMat3::from_cols(x, z.cross(x), z),
            translation: origin,
        }
    }

    /// `IfcLocalPlacement` chains.
    fn placement(&self, v: &Value, depth: usize) -> DAffine3 {
        let Some(p) = self.get(v) else {
            return DAffine3::IDENTITY;
        };
        if p.kind != "IFCLOCALPLACEMENT" || depth > MAX_DEPTH {
            return DAffine3::IDENTITY;
        }
        let parent = p
            .args
            .first()
            .map_or(DAffine3::IDENTITY, |r| self.placement(r, depth + 1));
        parent * p.args.get(1).map_or(DAffine3::IDENTITY, |a| self.axes(a))
    }

    fn skip(&mut self, what: &str) {
        *self.skipped.entry(what.to_string()).or_default() += 1;
    }

    /// A 2D profile's rings (outer first) in its plane, file units → mm.
    fn profile(&self, v: &Value) -> Option<Vec<Vec<DVec3>>> {
        let p = self.get(v)?;
        let placed = |args_at: usize, rings: Vec<Vec<DVec3>>| -> Vec<Vec<DVec3>> {
            let t = p
                .args
                .get(args_at)
                .map_or(DAffine3::IDENTITY, |a| self.axes(a));
            rings
                .into_iter()
                .map(|r| r.into_iter().map(|q| t.transform_point3(q)).collect())
                .collect()
        };
        match p.kind.as_str() {
            "IFCRECTANGLEPROFILEDEF" => {
                let x = p.args.get(3)?.as_f64()? * self.unit / 2.0;
                let y = p.args.get(4)?.as_f64()? * self.unit / 2.0;
                Some(placed(
                    2,
                    vec![vec![
                        DVec3::new(-x, -y, 0.0),
                        DVec3::new(x, -y, 0.0),
                        DVec3::new(x, y, 0.0),
                        DVec3::new(-x, y, 0.0),
                    ]],
                ))
            }
            "IFCCIRCLEPROFILEDEF" => {
                let r = p.args.get(3)?.as_f64()? * self.unit;
                let ring = (0..CIRCLE_SEGMENTS)
                    .map(|i| {
                        let a = std::f64::consts::TAU * i as f64 / CIRCLE_SEGMENTS as f64;
                        DVec3::new(r * libm::cos(a), r * libm::sin(a), 0.0)
                    })
                    .collect();
                Some(placed(2, vec![ring]))
            }
            "IFCARBITRARYCLOSEDPROFILEDEF" | "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
                let mut rings = vec![self.curve(p.args.get(2)?)?];
                if let Some(inner) = p.args.get(3).and_then(Value::as_list) {
                    rings.extend(inner.iter().filter_map(|c| self.curve(c)));
                }
                Some(rings)
            }
            _ => None,
        }
    }

    /// A closed curve's points (the last one dropped when it repeats the
    /// first).
    fn curve(&self, v: &Value) -> Option<Vec<DVec3>> {
        let c = self.get(v)?;
        let mut points: Vec<DVec3> = match c.kind.as_str() {
            "IFCPOLYLINE" => c
                .args
                .first()?
                .as_list()?
                .iter()
                .filter_map(|p| self.point(p))
                .collect(),
            "IFCINDEXEDPOLYCURVE" => {
                let list = self.get(c.args.first()?)?;
                list.args
                    .first()?
                    .as_list()?
                    .iter()
                    .filter_map(|xy| {
                        let xy = xy.as_list()?;
                        let n = |i: usize| xy.get(i).and_then(Value::as_f64).unwrap_or(0.0);
                        Some(DVec3::new(n(0), n(1), n(2)) * self.unit)
                    })
                    .collect()
            }
            _ => return None,
        };
        if points.len() > 1 && points[0].distance(points[points.len() - 1]) < 1e-9 {
            points.pop();
        }
        (points.len() >= 3).then_some(points)
    }

    /// Adds representation item `v` (with transform `t`) to `shape`.
    fn item(
        &mut self,
        v: &Value,
        t: DAffine3,
        color: Option<Rgba8>,
        shape: &mut Shape,
        depth: usize,
    ) {
        let Some(id) = v.as_ref_id() else { return };
        let Some(item) = self.data.get(&id).cloned() else {
            return;
        };
        let color = self.colors.get(&id).copied().or(color);
        if depth > MAX_DEPTH {
            return;
        }
        match item.kind.as_str() {
            "IFCTRIANGULATEDFACESET" | "IFCTRIANGULATEDIRREGULARNETWORK" => {
                let Some(coords) = self.coordinates(&item.args[0]) else {
                    return;
                };
                let index = item.args.get(3).and_then(Value::as_list).unwrap_or(&[]);
                let remap = item.args.get(4).and_then(Value::as_list);
                for tri in index {
                    let Some(tri) = tri.as_list() else { continue };
                    let corner = |k: usize| -> Option<DVec3> {
                        let mut i = tri.get(k)?.as_f64()? as usize;
                        if let Some(r) = remap {
                            i = r.get(i.checked_sub(1)?)?.as_f64()? as usize;
                        }
                        coords
                            .get(i.checked_sub(1)?)
                            .map(|p| t.transform_point3(*p))
                    };
                    if let (Some(a), Some(b), Some(c)) = (corner(0), corner(1), corner(2)) {
                        shape.polygon(&[vec![a, b, c]], color);
                    }
                }
            }
            "IFCPOLYGONALFACESET" => {
                let Some(coords) = self.coordinates(&item.args[0]) else {
                    return;
                };
                let remap = item.args.get(3).and_then(Value::as_list);
                let at = |i: f64| -> Option<DVec3> {
                    let mut i = i as usize;
                    if let Some(r) = remap {
                        i = r.get(i.checked_sub(1)?)?.as_f64()? as usize;
                    }
                    coords
                        .get(i.checked_sub(1)?)
                        .map(|p| t.transform_point3(*p))
                };
                for face in item.args.get(2).and_then(Value::as_list).unwrap_or(&[]) {
                    let Some(f) = self.get(face) else { continue };
                    let ring = |v: &Value| -> Vec<DVec3> {
                        v.as_list()
                            .unwrap_or(&[])
                            .iter()
                            .filter_map(|i| at(i.as_f64()?))
                            .collect()
                    };
                    let mut rings = vec![ring(&f.args[0])];
                    if let Some(inner) = f.args.get(1).and_then(Value::as_list) {
                        rings.extend(inner.iter().map(ring));
                    }
                    shape.polygon(&rings, color);
                }
            }
            "IFCFACETEDBREP" | "IFCFACETEDBREPWITHVOIDS" => {
                let shells = std::iter::once(item.args[0].clone())
                    .chain(
                        item.args
                            .get(1)
                            .and_then(Value::as_list)
                            .unwrap_or(&[])
                            .iter()
                            .cloned(),
                    )
                    .collect::<Vec<_>>();
                for s in shells {
                    self.shell(&s, t, color, shape);
                }
            }
            "IFCSHELLBASEDSURFACEMODEL" => {
                for s in item.args[0].as_list().unwrap_or(&[]).to_vec() {
                    self.shell(&s, t, color, shape);
                }
            }
            "IFCEXTRUDEDAREASOLID" => {
                let Some(rings) = self.profile(&item.args[0]) else {
                    self.skip("profiles");
                    return;
                };
                let position = item
                    .args
                    .get(1)
                    .map_or(DAffine3::IDENTITY, |a| self.axes(a));
                let dir = item
                    .args
                    .get(2)
                    .and_then(|d| self.direction(d))
                    .unwrap_or(DVec3::Z);
                let depth_mm = item.args.get(3).and_then(Value::as_f64).unwrap_or(0.0) * self.unit;
                let to = t * position;
                let offset = dir * depth_mm;
                let bottom: Vec<Vec<DVec3>> = rings
                    .iter()
                    .map(|r| r.iter().map(|p| to.transform_point3(*p)).collect())
                    .collect();
                let top: Vec<Vec<DVec3>> = rings
                    .iter()
                    .map(|r| r.iter().map(|p| to.transform_point3(*p + offset)).collect())
                    .collect();
                // Caps facing out: the profile's own side faces the
                // extrusion, so the bottom is turned over.
                let reversed: Vec<Vec<DVec3>> = bottom
                    .iter()
                    .map(|r| r.iter().rev().copied().collect())
                    .collect();
                shape.polygon(&reversed, color);
                shape.polygon(&top, color);
                for (b, u) in bottom.iter().zip(&top) {
                    for i in 0..b.len() {
                        let j = (i + 1) % b.len();
                        shape.polygon(&[vec![b[i], b[j], u[j], u[i]]], color);
                    }
                }
            }
            "IFCMAPPEDITEM" => {
                let Some(map) = self.get(&item.args[0]).cloned() else {
                    return;
                };
                let origin = map
                    .args
                    .first()
                    .map_or(DAffine3::IDENTITY, |a| self.axes(a));
                let target = item
                    .args
                    .get(1)
                    .map_or(DAffine3::IDENTITY, |o| self.operator(o));
                let Some(rep) = map.args.get(1).and_then(|r| self.get(r)).cloned() else {
                    return;
                };
                for sub in rep
                    .args
                    .get(3)
                    .and_then(Value::as_list)
                    .unwrap_or(&[])
                    .to_vec()
                {
                    self.item(&sub, t * target * origin.inverse(), color, shape, depth + 1);
                }
            }
            "IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT" => {
                self.skip("openings and cuts (kept uncut)");
                let first = item.args[1].clone();
                self.item(&first, t, color, shape, depth + 1);
            }
            other => self.skip(&class_name(other)),
        }
    }

    /// A shell's faces (`IfcFace` with `IfcPolyLoop` bounds).
    fn shell(&self, v: &Value, t: DAffine3, color: Option<Rgba8>, shape: &mut Shape) {
        let Some(s) = self.get(v) else { return };
        for f in s.args.first().and_then(Value::as_list).unwrap_or(&[]) {
            let Some(face) = self.get(f) else { continue };
            let mut outer: Option<Vec<DVec3>> = None;
            let mut holes = Vec::new();
            for b in face.args.first().and_then(Value::as_list).unwrap_or(&[]) {
                let Some(bound) = self.get(b) else { continue };
                let Some(lp) = bound.args.first().and_then(|l| self.get(l)) else {
                    continue;
                };
                let mut ring: Vec<DVec3> = lp
                    .args
                    .first()
                    .and_then(Value::as_list)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|p| self.point(p).map(|q| t.transform_point3(q)))
                    .collect();
                if bound.args.get(1).and_then(Value::as_enum) == Some("F") {
                    ring.reverse();
                }
                if bound.kind == "IFCFACEOUTERBOUND" && outer.is_none() {
                    outer = Some(ring);
                } else {
                    holes.push(ring);
                }
            }
            let rings: Vec<Vec<DVec3>> = match outer {
                Some(o) => std::iter::once(o).chain(holes).collect(),
                None => holes,
            };
            shape.polygon(&rings, color);
        }
    }

    /// `IfcCartesianPointList3D` → mm points.
    fn coordinates(&self, v: &Value) -> Option<Vec<DVec3>> {
        let list = self.get(v)?;
        Some(
            list.args
                .first()?
                .as_list()?
                .iter()
                .filter_map(|xyz| {
                    let xyz = xyz.as_list()?;
                    let n = |i: usize| xyz.get(i).and_then(Value::as_f64).unwrap_or(0.0);
                    Some(DVec3::new(n(0), n(1), n(2)) * self.unit)
                })
                .collect(),
        )
    }

    /// `IfcCartesianTransformationOperator3D` (uniform or not).
    fn operator(&self, v: &Value) -> DAffine3 {
        let Some(o) = self.get(v) else {
            return DAffine3::IDENTITY;
        };
        let x = o
            .args
            .first()
            .and_then(|d| self.direction(d))
            .unwrap_or(DVec3::X);
        let y = o
            .args
            .get(1)
            .and_then(|d| self.direction(d))
            .unwrap_or(DVec3::Y);
        let origin = o
            .args
            .get(2)
            .and_then(|p| self.point(p))
            .unwrap_or_default();
        let scale = o.args.get(3).and_then(Value::as_f64).unwrap_or(1.0);
        let z = o
            .args
            .get(4)
            .and_then(|d| self.direction(d))
            .unwrap_or_else(|| x.cross(y).normalize_or(DVec3::Z));
        let (sy, sz) = if o.kind == "IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM" {
            (
                o.args.get(5).and_then(Value::as_f64).unwrap_or(scale),
                o.args.get(6).and_then(Value::as_f64).unwrap_or(scale),
            )
        } else {
            (scale, scale)
        };
        DAffine3 {
            matrix3: DMat3::from_cols(x * scale, y * sy, z * sz),
            translation: origin,
        }
    }
}

/// Millimeters per length unit of the file's unit assignment.
fn length_unit(data: &BTreeMap<u64, Instance>) -> f64 {
    let prefix = |p: Option<&str>| match p {
        Some("MILLI") => 1.0,
        Some("CENTI") => 10.0,
        Some("DECI") => 100.0,
        Some("KILO") => 1_000_000.0,
        _ => 1000.0,
    };
    for inst in data.values().filter(|i| i.kind == "IFCUNITASSIGNMENT") {
        for u in inst.args.first().and_then(Value::as_list).unwrap_or(&[]) {
            let Some(unit) = u.as_ref_id().and_then(|id| data.get(&id)) else {
                continue;
            };
            match unit.kind.as_str() {
                "IFCSIUNIT" if unit.args.get(1).and_then(Value::as_enum) == Some("LENGTHUNIT") => {
                    return prefix(unit.args.get(2).and_then(Value::as_enum));
                }
                "IFCCONVERSIONBASEDUNIT"
                    if unit.args.get(1).and_then(Value::as_enum) == Some("LENGTHUNIT") =>
                {
                    let name = unit.args.get(2).and_then(Value::as_str).unwrap_or("");
                    return match name.to_ascii_uppercase().as_str() {
                        "FOOT" => 304.8,
                        "INCH" => 25.4,
                        _ => 1000.0,
                    };
                }
                _ => {}
            }
        }
    }
    1000.0
}

/// Colors of styled items (IFC4 styles and IFC2X3 style assignments).
fn styles(data: &BTreeMap<u64, Instance>) -> BTreeMap<u64, Rgba8> {
    let rgb = |id: u64| -> Option<Rgba8> {
        let c = data.get(&id)?;
        let n = |i: usize| c.args.get(i).and_then(Value::as_f64).unwrap_or(0.8);
        let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Some(Rgba8::rgb(b(n(1)), b(n(2)), b(n(3))))
    };
    let surface = |id: u64| -> Option<Rgba8> {
        let s = data.get(&id)?;
        if s.kind != "IFCSURFACESTYLE" {
            return None;
        }
        s.args.get(2)?.as_list()?.iter().find_map(|x| {
            let shading = data.get(&x.as_ref_id()?)?;
            if !shading.kind.starts_with("IFCSURFACESTYLE") {
                return None;
            }
            rgb(shading.args.first()?.as_ref_id()?)
        })
    };
    let mut out = BTreeMap::new();
    for inst in data.values().filter(|i| i.kind == "IFCSTYLEDITEM") {
        let Some(item) = inst.args.first().and_then(Value::as_ref_id) else {
            continue;
        };
        let color = inst
            .args
            .get(1)
            .and_then(Value::as_list)
            .unwrap_or(&[])
            .iter()
            .find_map(|s| {
                let id = s.as_ref_id()?;
                surface(id).or_else(|| {
                    // IFC2X3: a presentation style assignment.
                    let assignment = data.get(&id)?;
                    assignment
                        .args
                        .first()?
                        .as_list()?
                        .iter()
                        .find_map(|x| surface(x.as_ref_id()?))
                })
            });
        if let Some(c) = color {
            out.insert(item, c);
        }
    }
    out
}

/// One imported element.
struct Product {
    name: String,
    class: String,
    storey: Option<String>,
    /// One per body item.
    shapes: Vec<Shape>,
}

fn read(text: &str) -> Result<(Vec<Product>, Vec<Warning>), IoError> {
    let data = parse(text)?;
    let mut model = Model {
        unit: length_unit(&data),
        colors: styles(&data),
        data,
        skipped: BTreeMap::new(),
    };
    // Storeys (and other spatial elements) of contained products.
    let mut container_of: BTreeMap<u64, String> = BTreeMap::new();
    for rel in model
        .data
        .values()
        .filter(|i| i.kind == "IFCRELCONTAINEDINSPATIALSTRUCTURE")
    {
        let Some(structure) = rel.args.get(5).and_then(|s| model.get(s)) else {
            continue;
        };
        let name = structure
            .args
            .get(2)
            .and_then(Value::as_str)
            .unwrap_or("Storey")
            .to_string();
        for e in rel.args.get(4).and_then(Value::as_list).unwrap_or(&[]) {
            if let Some(id) = e.as_ref_id() {
                container_of.insert(id, name.clone());
            }
        }
    }
    let ids: Vec<u64> = model
        .data
        .iter()
        .filter(|(_, i)| {
            i.kind.starts_with("IFC")
                && !SKIPPED.contains(&i.kind.as_str())
                && i.args.len() >= 7
                && i.args[6].as_ref_id().is_some_and(|p| {
                    model
                        .data
                        .get(&p)
                        .is_some_and(|x| x.kind == "IFCPRODUCTDEFINITIONSHAPE")
                })
        })
        .map(|(id, _)| *id)
        .collect();
    let mut products = Vec::new();
    for id in ids {
        let Some(product) = model.data.get(&id).cloned() else {
            continue;
        };
        // A product without a placement stands at the origin.
        let placement = model.placement(&product.args[5], 0);
        let Some(definition) = model.get(&product.args[6]).cloned() else {
            continue;
        };
        let reps: Vec<Instance> = definition
            .args
            .get(2)
            .and_then(Value::as_list)
            .unwrap_or(&[])
            .iter()
            .filter_map(|r| model.get(r).cloned())
            .collect();
        // The body: identified as such, else the first with items.
        let body = reps
            .iter()
            .find(|r| r.args.get(1).and_then(Value::as_str) == Some("Body"))
            .or_else(|| {
                reps.iter().find(|r| {
                    r.args.get(1).and_then(Value::as_str) != Some("Axis")
                        && r.args
                            .get(3)
                            .and_then(Value::as_list)
                            .is_some_and(|l| !l.is_empty())
                })
            })
            .cloned();
        let Some(body) = body else { continue };
        // One shape per body item: separate items stay separate parts.
        let mut shapes = Vec::new();
        for item in body
            .args
            .get(3)
            .and_then(Value::as_list)
            .unwrap_or(&[])
            .to_vec()
        {
            let mut shape = Shape::default();
            model.item(&item, placement, None, &mut shape, 0);
            if !shape.triangles.is_empty() {
                shapes.push(shape);
            }
        }
        if shapes.is_empty() {
            continue;
        }
        let class = class_name(&product.kind);
        products.push(Product {
            name: product
                .args
                .get(2)
                .and_then(Value::as_str)
                .filter(|n| !n.trim().is_empty())
                .map_or_else(
                    || class.trim_start_matches("Ifc").to_string(),
                    str::to_owned,
                ),
            class,
            storey: container_of.get(&id).cloned(),
            shapes,
        });
    }
    let warnings = model
        .skipped
        .iter()
        .map(|(what, n)| Warning::new(WarningCode::W2004, format!("{n} × {what} not read")))
        .collect();
    Ok((products, warnings))
}

impl Importer for IfcImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Ifc
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
        if head.starts_with("ISO-10303-21") && head.contains("IFC") {
            Confidence::CERTAIN
        } else if name.to_ascii_lowercase().ends_with(".ifc") {
            Confidence::EXTENSION
        } else {
            Confidence::NONE
        }
    }

    fn import(
        &self,
        src: &mut dyn Read,
        opts: &ImportOptions,
        tx: &mut Transaction<'_>,
        target: &Target,
        progress: &dyn Progress,
    ) -> Result<ImportReport, IoError> {
        let mut text = String::new();
        src.read_to_string(&mut text)
            .map_err(|e| corrupt(e.to_string()))?;
        progress.report(0.1, "reading the model");
        let (products, warnings) = read(&text)?;
        if products.is_empty() {
            return Err(corrupt("the file holds no element with a body"));
        }
        // Materials by color; one mesh and node per product.
        let mut materials: Vec<ImportedMaterial> = Vec::new();
        let mut by_color: BTreeMap<[u8; 3], usize> = BTreeMap::new();
        let mut meshes = Vec::new();
        let mut nodes = Vec::new();
        let mut owner: Vec<usize> = Vec::new();
        for (i, p) in products.iter().enumerate() {
            for part in p.shapes.iter().flat_map(Shape::parts) {
                let mats = part
                    .colors
                    .iter()
                    .map(|c| {
                        c.map(|c| {
                            *by_color.entry([c.r, c.g, c.b]).or_insert_with(|| {
                                materials.push(ImportedMaterial {
                                    name: format!("Color {:02X}{:02X}{:02X}", c.r, c.g, c.b),
                                    color: c,
                                    opacity: 1.0,
                                    ..ImportedMaterial::default()
                                });
                                materials.len() - 1
                            })
                        })
                    })
                    .collect();
                nodes.push(ImportedNode {
                    name: p.name.clone(),
                    transform: DAffine3::IDENTITY,
                    mesh: meshes.len(),
                });
                meshes.push(ImportedMesh {
                    name: p.name.clone(),
                    positions: part.positions,
                    triangles: part.triangles,
                    materials: mats,
                    uvs: Vec::new(),
                    normals: Vec::new(),
                });
                owner.push(i);
            }
        }
        progress.report(0.5, "building faces");
        let scene = ImportedScene {
            name: source_stem(&opts.source, "IFC"),
            materials,
            meshes,
            nodes,
            y_up: false,
            units: Some(LengthUnit::Millimeter),
            cameras: Vec::new(),
        };
        let mut report = build_scene(tx, target, scene, opts)?;
        tag_products(tx, target, &report, &products, &owner)?;
        report.skipped += warnings.len();
        report.warnings.extend(warnings);
        Ok(report)
    }
}

/// Gathers each product's parts into one group named after it (a product
/// of one part is that part's group), tags it with its class (without the
/// prefix) and writes `ifc`/`class` and `ifc`/`storey`.
fn tag_products(
    tx: &mut Transaction<'_>,
    target: &Target,
    report: &ImportReport,
    products: &[Product],
    owner: &[usize],
) -> Result<(), IoError> {
    use crate::commands::Command;
    use crate::commands::organize::{MakeGroup, MakeGroupParams};
    let io = crate::io::import::build::io;
    let Some(&top) = report.created.first() else {
        return Ok(());
    };
    let Some((container, id)) = tx.document().locate_pid(top) else {
        return Ok(());
    };
    let Some(top_def) = tx
        .document()
        .container(container)
        .and_then(|e| e.definition_of(id))
    else {
        return Ok(());
    };
    let inner = ContainerRef::Definition(top_def);
    // One node: the top group is the product; several: its groups in order.
    let parts: Vec<(ContainerRef, crate::ids::EntityId)> = if owner.len() == 1 {
        vec![(container, id)]
    } else {
        tx.document()
            .container(inner)
            .map(|e| {
                e.order()
                    .iter()
                    .filter(|&&x| {
                        matches!(
                            e.view(x),
                            Some(EntityView::Group(_) | EntityView::Instance(_))
                        )
                    })
                    .map(|&x| (inner, x))
                    .collect()
            })
            .unwrap_or_default()
    };
    let inside = crate::document::EntityPath::new(
        target.context.path.clone(),
        crate::document::EntityKey::Pid(top),
    );
    for (index, product) in products.iter().enumerate() {
        let mine: Vec<(ContainerRef, crate::ids::EntityId)> = parts
            .iter()
            .zip(owner)
            .filter(|(_, o)| **o == index)
            .map(|(p, _)| *p)
            .collect();
        let element = match mine.as_slice() {
            [] => continue,
            [one] => *one,
            many => {
                let pids: Vec<String> = many
                    .iter()
                    .filter_map(|(c, e)| tx.document().container(*c)?.pid_of(*e))
                    .map(|pid| format!("@{}", pid.0))
                    .collect();
                let out = MakeGroup
                    .apply(
                        tx,
                        MakeGroupParams {
                            common: crate::commands::CommonParams {
                                context: Some(inside.clone()),
                                dry_run: false,
                            },
                            targets: crate::document::Query::new(pids.join(" | ")),
                            name: Some(product.name.clone()),
                        },
                    )
                    .map_err(io)?;
                match tx.document().locate_pid(out.group) {
                    Some(found) => found,
                    None => continue,
                }
            }
        };
        let (c, e) = element;
        let label = product.class.trim_start_matches("Ifc").to_string();
        let tag = match tx.document().tags.by_name(&label) {
            Some(t) => t,
            None => tx
                .insert_tag(Tag::new(label.clone(), Rgba8::rgb(0x80, 0x80, 0x80)))
                .map_err(io)?,
        };
        tx.set_tag(c, e, Some(tag)).map_err(io)?;
        let attrs = AttrTarget::Entity {
            container: c,
            id: e,
        };
        tx.set_attribute(
            attrs,
            "ifc",
            "class",
            Some(AttrValue::Str(product.class.clone())),
        )
        .map_err(io)?;
        if let Some(storey) = &product.storey {
            tx.set_attribute(attrs, "ifc", "storey", Some(AttrValue::Str(storey.clone())))
                .map_err(io)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A wall extruded from a rectangle, in metres, on a storey, colored.
    const WALL: &str = "ISO-10303-21;\nHEADER;FILE_SCHEMA(('IFC4'));ENDSEC;\nDATA;\n\
#1=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n#2=IFCUNITASSIGNMENT((#1));\n\
#3=IFCCARTESIANPOINT((0.,0.,0.));\n#4=IFCDIRECTION((0.,0.,1.));\n#5=IFCDIRECTION((1.,0.,0.));\n\
#6=IFCAXIS2PLACEMENT3D(#3,#4,#5);\n#7=IFCLOCALPLACEMENT($,#6);\n\
#8=IFCCARTESIANPOINT((2.,0.,0.));\n#9=IFCAXIS2PLACEMENT3D(#8,$,$);\n#10=IFCLOCALPLACEMENT(#7,#9);\n\
#11=IFCAXIS2PLACEMENT2D(#12,$);\n#12=IFCCARTESIANPOINT((2.,0.1));\n\
#13=IFCRECTANGLEPROFILEDEF(.AREA.,$,#11,4.,0.2);\n\
#14=IFCEXTRUDEDAREASOLID(#13,#6,#4,3.);\n\
#15=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#14));\n\
#16=IFCPRODUCTDEFINITIONSHAPE($,$,(#15));\n\
#17=IFCWALL('a',$,'North wall',$,$,#10,#16,$,.NOTDEFINED.);\n\
#18=IFCBUILDINGSTOREY('b',$,'Ground',$,$,#7,$,$,.ELEMENT.,0.);\n\
#19=IFCRELCONTAINEDINSPATIALSTRUCTURE('c',$,$,$,(#17),#18);\n\
#20=IFCCOLOURRGB($,1.,0.,0.);\n#21=IFCSURFACESTYLESHADING(#20,0.);\n\
#22=IFCSURFACESTYLE($,.BOTH.,(#21));\n#23=IFCSTYLEDITEM(#14,(#22),$);\n\
ENDSEC;\nEND-ISO-10303-21;";

    #[test]
    fn an_extruded_wall_in_metres() {
        let (products, warnings) = read(WALL).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(products.len(), 1);
        let p = &products[0];
        assert_eq!(
            (p.name.as_str(), p.class.as_str()),
            ("North wall", "IfcWall")
        );
        assert_eq!(p.storey.as_deref(), Some("Ground"));
        let shape = &p.shapes[0];
        let (lo, hi) = shape.positions.iter().fold(
            (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN)),
            |(lo, hi), q| (lo.min(*q), hi.max(*q)),
        );
        // Placement 2 m, profile centered at (2, 0.1): x from 2 to 6 m.
        assert!(lo.distance(DVec3::new(2000.0, 0.0, 0.0)) < 1e-6, "{lo}");
        assert!(
            hi.distance(DVec3::new(6000.0, 200.0, 3000.0)) < 1e-6,
            "{hi}"
        );
        assert_eq!(shape.colors[0], Some(Rgba8::rgb(255, 0, 0)));
        // 2 caps × 2 triangles + 4 sides × 2.
        assert_eq!(shape.triangles.len(), 12);
    }

    #[test]
    fn units_and_class_names() {
        assert_eq!(class_name("IFCWALLSTANDARDCASE"), "IfcWallStandardCase");
        assert_eq!(class_name("IFCSOMETHINGNEW"), "IfcSomethingnew");
        let empty = "ISO-10303-21;\nDATA;\n#1=IFCPROJECT('x',$,$,$,$,$,$,$,$);\nENDSEC;";
        assert!(read(empty).unwrap().0.is_empty());
    }
}
