//! Implements docs/05-modeling-tools.md §3.8 `text-3d`: faces from text
//! glyph outlines (`ttf-parser`), optionally extruded into closed solids,
//! in a new group. Curves are flattened; contours nested an odd number of
//! times are holes. The bundled font is Inter (SIL Open Font License,
//! `assets/fonts/Inter-LICENSE.txt`); another TrueType or OpenType file can
//! be read through the storage.

use glam::{DAffine3, DVec2, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::organize::libraries::storage;
use crate::commands::params::{TransformSchema, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ComponentDefinition, Document, Entities, EntityRecord, Group};
use crate::geom::HalfEdgeMesh;
use crate::geom::NoDeltas;
use crate::geom::mesh::TriangleMergeOptions;
use crate::geom::tessellate::tessellate_polygon_2d;
use crate::ids::Pid;
use crate::io::StorageKey;
use crate::units::Length;

use crate::fonts::INTER;

/// Line segments per quadratic and cubic curve piece.
const QUAD_STEPS: usize = 4;
const CUBIC_STEPS: usize = 6;

fn yes() -> bool {
    true
}

/// Horizontal alignment of lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TextAlign {
    /// Lines start at the origin.
    #[default]
    Left,
    /// Lines are centered on the origin.
    Center,
    /// Lines end at the origin.
    Right,
}

/// `text-3d` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Text3dParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The text (`\n` starts a new line).
    pub text: String,
    /// A font file (TrueType/OpenType); the bundled Inter by default.
    #[serde(default)]
    pub font: Option<String>,
    /// Height of capital letters.
    pub height: Length,
    /// Extrusion (0: flat faces).
    #[serde(default)]
    pub extrude: Length,
    /// Faces (true) or outlines only.
    #[serde(default = "yes")]
    pub filled: bool,
    /// Line alignment.
    #[serde(default)]
    pub align: TextAlign,
    /// Where the text goes (world): the baseline of the first line runs
    /// along local +X from the origin, letters up local +Y, extrusion along
    /// local +Z. Default: flat on the ground at the origin.
    #[serde(default)]
    #[schemars(with = "Option<TransformSchema>")]
    pub placement: Option<DAffine3>,
}

/// `text-3d` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Text3dOutput {
    /// The group holding the text.
    pub group: Pid,
    /// Faces made.
    pub faces: usize,
}

/// The `text-3d` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Text3d;

/// Collects a glyph's contours, curves flattened.
struct Outline {
    contours: Vec<Vec<DVec2>>,
    current: Vec<DVec2>,
    at: DVec2,
}

impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.current.len() > 2 {
            self.contours.push(std::mem::take(&mut self.current));
        }
        self.current.clear();
        self.at = DVec2::new(f64::from(x), f64::from(y));
        self.current.push(self.at);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.at = DVec2::new(f64::from(x), f64::from(y));
        self.current.push(self.at);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (p0, c, p1) = (
            self.at,
            DVec2::new(f64::from(x1), f64::from(y1)),
            DVec2::new(f64::from(x), f64::from(y)),
        );
        for k in 1..=QUAD_STEPS {
            let t = k as f64 / QUAD_STEPS as f64;
            let u = 1.0 - t;
            self.current.push(p0 * u * u + c * 2.0 * u * t + p1 * t * t);
        }
        self.at = p1;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (p0, c1, c2, p1) = (
            self.at,
            DVec2::new(f64::from(x1), f64::from(y1)),
            DVec2::new(f64::from(x2), f64::from(y2)),
            DVec2::new(f64::from(x), f64::from(y)),
        );
        for k in 1..=CUBIC_STEPS {
            let t = k as f64 / CUBIC_STEPS as f64;
            let u = 1.0 - t;
            self.current.push(
                p0 * u * u * u + c1 * 3.0 * u * u * t + c2 * 3.0 * u * t * t + p1 * t * t * t,
            );
        }
        self.at = p1;
    }

    fn close(&mut self) {
        if self.current.len() > 2 {
            self.contours.push(std::mem::take(&mut self.current));
        }
        self.current.clear();
    }
}

fn signed_area(c: &[DVec2]) -> f64 {
    let n = c.len();
    (0..n).map(|i| c[i].perp_dot(c[(i + 1) % n])).sum::<f64>() / 2.0
}

fn inside(p: DVec2, c: &[DVec2]) -> bool {
    let n = c.len();
    let mut odd = false;
    for i in 0..n {
        let (a, b) = (c[i], c[(i + 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            odd = !odd;
        }
    }
    odd
}

/// Outer contours (counter-clockwise) with their holes (clockwise), by
/// nesting depth.
fn polygons(mut contours: Vec<Vec<DVec2>>) -> Vec<(Vec<DVec2>, Vec<Vec<DVec2>>)> {
    // Drop repeated closing points and degenerate contours.
    for c in &mut contours {
        c.dedup_by(|a, b| (*a - *b).length() < 1e-9);
        if c.len() > 1 && (c[0] - c[c.len() - 1]).length() < 1e-9 {
            c.pop();
        }
    }
    contours.retain(|c| c.len() >= 3 && signed_area(c).abs() > 1e-9);
    let depth: Vec<usize> = contours
        .iter()
        .enumerate()
        .map(|(i, c)| {
            contours
                .iter()
                .enumerate()
                .filter(|(j, o)| *j != i && inside(c[0], o))
                .count()
        })
        .collect();
    let mut outers: Vec<(usize, Vec<DVec2>, Vec<Vec<DVec2>>)> = Vec::new();
    for (i, c) in contours.iter().enumerate() {
        if depth[i].is_multiple_of(2) {
            let mut c = c.clone();
            if signed_area(&c) < 0.0 {
                c.reverse();
            }
            outers.push((i, c, Vec::new()));
        }
    }
    for (i, c) in contours.iter().enumerate() {
        if depth[i] % 2 == 1 {
            let mut hole = c.clone();
            if signed_area(&hole) > 0.0 {
                hole.reverse();
            }
            // The smallest outer around it.
            if let Some(owner) = outers
                .iter_mut()
                .filter(|(_, o, _)| inside(hole[0], o))
                .min_by(|a, b| signed_area(&a.1).total_cmp(&signed_area(&b.1)))
            {
                owner.2.push(hole);
            }
        }
    }
    outers.into_iter().map(|(_, o, h)| (o, h)).collect()
}

/// Lines laid out: polygons in model units (baseline at y = 0 for the
/// first line).
fn layout(
    face: &ttf_parser::Face<'_>,
    text: &str,
    height: f64,
    align: TextAlign,
) -> Vec<(Vec<DVec2>, Vec<Vec<DVec2>>)> {
    let em = f64::from(face.units_per_em());
    let cap = face
        .capital_height()
        .filter(|h| *h > 0)
        .map_or(em * 0.7, f64::from);
    let scale = height / cap;
    let line_height = f64::from(face.height().max(1)) + f64::from(face.line_gap());
    let mut out = Vec::new();
    for (row, line) in text.split('\n').enumerate() {
        let mut pen = 0.0f64;
        let mut glyphs: Vec<(Vec<Vec<DVec2>>, f64)> = Vec::new();
        for ch in line.chars() {
            let Some(id) = face.glyph_index(ch) else {
                continue;
            };
            let mut outline = Outline {
                contours: Vec::new(),
                current: Vec::new(),
                at: DVec2::ZERO,
            };
            if face.outline_glyph(id, &mut outline).is_some() {
                ttf_parser::OutlineBuilder::close(&mut outline);
                glyphs.push((outline.contours, pen));
            }
            pen += f64::from(face.glyph_hor_advance(id).unwrap_or(0));
        }
        let shift = match align {
            TextAlign::Left => 0.0,
            TextAlign::Center => -pen / 2.0,
            TextAlign::Right => -pen,
        };
        let y = -(row as f64) * line_height;
        for (contours, x) in glyphs {
            let moved: Vec<Vec<DVec2>> = contours
                .into_iter()
                .map(|c| {
                    c.into_iter()
                        .map(|p| (p + DVec2::new(x + shift, y)) * scale)
                        .collect()
                })
                .collect();
            out.extend(polygons(moved));
        }
    }
    out
}

impl Command for Text3d {
    type Params = Text3dParams;
    type Output = Text3dOutput;

    fn name(&self) -> &'static str {
        "text-3d"
    }

    fn validate(&self, _doc: &Document, p: &Text3dParams) -> Result<(), CommandError> {
        if p.text.trim().is_empty() {
            return Err(CommandError::invalid_param("text", "must not be empty"));
        }
        if !(p.height.as_mm() > 0.0 && p.height.as_mm().is_finite()) {
            return Err(CommandError::OutOfRange {
                name: "height".into(),
                reason: "must be a positive length".into(),
            });
        }
        if !(p.extrude.as_mm() >= 0.0 && p.extrude.as_mm().is_finite()) {
            return Err(CommandError::OutOfRange {
                name: "extrude".into(),
                reason: "must be zero or a positive length".into(),
            });
        }
        if p.placement
            .is_some_and(|t| !t.is_finite() || t.matrix3.determinant().abs() < 1e-12)
        {
            return Err(CommandError::invalid_param(
                "placement",
                "must be invertible",
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: Text3dParams,
    ) -> Result<Text3dOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let font_bytes = match p.font.as_deref() {
            None | Some("Inter") => INTER.to_vec(),
            Some(path) => storage(tx, path)?.read(&StorageKey::new(path))?,
        };
        let face = ttf_parser::Face::parse(&font_bytes, 0)
            .map_err(|e| CommandError::invalid_param("font", format!("not a usable font: {e}")))?;
        let polys = layout(&face, &p.text, p.height.as_mm(), p.align);
        if polys.is_empty() {
            return Err(CommandError::invalid_param(
                "text",
                "has no visible glyphs in this font",
            ));
        }
        let tol = *tx.tolerances();
        let h = p.extrude.as_mm();
        let mesh = if p.filled {
            let mut positions: Vec<DVec3> = Vec::new();
            let mut triangles: Vec<[u32; 3]> = Vec::new();
            let mut keys: Vec<u32> = Vec::new();
            let mut tri = |a: DVec3, b: DVec3, c: DVec3, key: u32| {
                let base = positions.len() as u32;
                positions.extend([a, b, c]);
                triangles.push([base, base + 1, base + 2]);
                keys.push(key);
            };
            for (k, (outer, holes)) in polys.iter().enumerate() {
                let key = k as u32;
                let Ok(cap) = tessellate_polygon_2d(outer, holes) else {
                    continue;
                };
                let all: Vec<DVec2> = outer
                    .iter()
                    .chain(holes.iter().flatten())
                    .copied()
                    .collect();
                for t in cap {
                    let [a, b, c] = t.map(|i| all[i as usize]);
                    // Counter-clockwise seen from +Z.
                    let (a, b, c) = if (b - a).perp_dot(c - a) < 0.0 {
                        (a, c, b)
                    } else {
                        (a, b, c)
                    };
                    let at = |q: DVec2, z: f64| DVec3::new(q.x, q.y, z);
                    if h > 0.0 {
                        tri(at(a, h), at(b, h), at(c, h), key);
                        tri(at(a, 0.0), at(c, 0.0), at(b, 0.0), key);
                    } else {
                        tri(at(a, 0.0), at(b, 0.0), at(c, 0.0), key);
                    }
                }
                if h > 0.0 {
                    // Sides: interior on the left of every ring's direction.
                    for ring in std::iter::once(outer).chain(holes.iter()) {
                        let n = ring.len();
                        for i in 0..n {
                            let (p0, q0) = (ring[i], ring[(i + 1) % n]);
                            let (a0, b0) =
                                (DVec3::new(p0.x, p0.y, 0.0), DVec3::new(q0.x, q0.y, 0.0));
                            let (a1, b1) = (a0 + DVec3::Z * h, b0 + DVec3::Z * h);
                            tri(a0, b0, b1, key);
                            tri(a0, b1, a1, key);
                        }
                    }
                }
            }
            HalfEdgeMesh::from_triangles_keyed(
                &positions,
                &triangles,
                &keys,
                TriangleMergeOptions {
                    merge_coplanar: true,
                    soften_angle: crate::units::Angle::ZERO,
                },
                &tol,
            )
            .map_err(|e| CommandError::Internal(e.to_string()))?
            .mesh
        } else {
            let mut mesh = HalfEdgeMesh::new();
            for (outer, holes) in &polys {
                for ring in std::iter::once(outer).chain(holes.iter()) {
                    let n = ring.len();
                    for i in 0..n {
                        let (a, b) = (ring[i], ring[(i + 1) % n]);
                        let _ = mesh.add_edge(
                            DVec3::new(a.x, a.y, 0.0),
                            DVec3::new(b.x, b.y, 0.0),
                            &tol,
                            &mut NoDeltas,
                        );
                    }
                }
            }
            // Outlines only: the faces the closed contours formed go.
            let faces: Vec<_> = mesh.faces().map(|(f, _)| f).collect();
            for f in faces {
                let _ = mesh.erase_face(f, false, &tol, &mut NoDeltas);
            }
            mesh
        };
        let faces = mesh.face_count();
        let name = format!(
            "Group {}",
            crate::commands::common::group_number(tx.document())
        );
        let entities = Entities::from_mesh(mesh, || tx.allocate_pid());
        let uuid = crate::commands::common::definition_uuid(tx.document(), &name, &entities);
        let mut def = ComponentDefinition::new(uuid, name);
        def.is_internal_group = true;
        def.entities = entities;
        let def = tx.insert_definition(def)?;
        let transform = target.local * p.placement.unwrap_or(DAffine3::IDENTITY);
        let (_, group) = tx.insert_entity(
            target.container,
            EntityRecord::Group(Group {
                definition: def,
                transform,
                name: p
                    .text
                    .lines()
                    .next()
                    .unwrap_or("Text")
                    .chars()
                    .take(32)
                    .collect(),
            }),
        )?;
        Ok(Text3dOutput { group, faces })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "text-3d".into(),
            category: CommandCategory::Draw,
            label: "3D Text".into(),
            view_only: false,
            steps: Vec::new(),
        }
    }
}
