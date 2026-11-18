//! Implements docs/05-modeling-tools.md §3.8 `text-3d`: faces from text
//! glyph outlines (`ttf-parser`), optionally extruded into closed solids,
//! in a new group. Curves are flattened; contours nested an odd number of
//! times are holes, and a glyph whose contours cross (a crossbar drawn over
//! its stems) is filled by the font's nonzero winding rule instead
//! (docs/05 Decisions, 2026-11-18). The bundled font is Inter (SIL Open Font License,
//! `assets/fonts/Inter-LICENSE.txt`); another TrueType or OpenType file can
//! be read through the storage.

use std::collections::BTreeSet;

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
use crate::geom::tessellate::tessellate_polygon_2d;
use crate::ids::{FaceId, Pid};
use crate::io::StorageKey;
use crate::tolerance::Tolerances;
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

/// One glyph: its contours (curves flattened) and the boundary of what
/// they fill.
struct Glyph {
    contours: Vec<Vec<DVec2>>,
    boundary: Vec<(DVec2, DVec2)>,
}

/// The segments of each ring, closing back to its start.
fn ring_segments(ring: &[DVec2]) -> impl Iterator<Item = (DVec2, DVec2)> + '_ {
    (0..ring.len()).map(move |i| (ring[i], ring[(i + 1) % ring.len()]))
}

/// Flattened curve points within this many length tolerances of their
/// neighbors' chord, or closer than this many planarity tolerances to the
/// point before, are dropped: details below the kernel's resolution make
/// faces it cannot tell apart (docs/05 Decisions, 2026-11-18).
const SLIVER: f64 = 4.0;
const SHORTEST: f64 = 2.0;

/// `ring` without the points within `eps` of their neighbors' chord or
/// within `shortest` of the point before.
fn straighten(mut ring: Vec<DVec2>, eps: f64, shortest: f64) -> Vec<DVec2> {
    let mut i = 0;
    let mut kept = 0;
    while ring.len() > 3 && kept < ring.len() {
        let n = ring.len();
        let (a, b, c) = (ring[(i + n - 1) % n], ring[i % n], ring[(i + 1) % n]);
        if crate::math::polygon::distance_to_segment_2d(b, a, c) <= eps
            || (b - a).length() <= shortest
        {
            ring.remove(i % n);
            kept = 0;
        } else {
            i = (i + 1) % n;
            kept += 1;
        }
    }
    ring
}

/// Winding number of `ring` around `p`.
fn winding(p: DVec2, ring: &[DVec2]) -> i32 {
    let mut w = 0;
    for (a, b) in ring_segments(ring) {
        let left = (b - a).perp_dot(p - a);
        if a.y <= p.y {
            if b.y > p.y && left > 0.0 {
                w += 1;
            }
        } else if b.y <= p.y && left < 0.0 {
            w -= 1;
        }
    }
    w
}

/// A glyph from its contours: their segments bound what they fill unless
/// two contours cross; then the boundary is that of the regions the
/// contours fill by the nonzero winding rule.
fn glyph(contours: Vec<Vec<DVec2>>, tol: &Tolerances) -> Glyph {
    let mut contours: Vec<Vec<DVec2>> = contours
        .into_iter()
        .map(|mut c| {
            c.dedup_by(|a, b| (*a - *b).length() < 1e-9);
            if c.len() > 1 && (c[0] - c[c.len() - 1]).length() < 1e-9 {
                c.pop();
            }
            straighten(c, SLIVER * tol.length, SHORTEST * tol.planarity)
        })
        .collect();
    contours.retain(|c| c.len() >= 3 && signed_area(c).abs() > 1e-9);
    let crossing = contours.iter().enumerate().any(|(i, a)| {
        contours[i..]
            .iter()
            .any(|b| crate::math::polygon::rings_cross(a, b, tol))
    });
    let boundary = match crossing.then(|| nonzero_boundary(&contours, tol)) {
        Some(Ok(Some(boundary))) => boundary,
        _ => contours.iter().flat_map(|c| ring_segments(c)).collect(),
    };
    Glyph { contours, boundary }
}

/// A point inside the polygon (`outer` and `holes` in any orientation):
/// the centroid of its largest triangle.
fn probe(outer: &[DVec2], holes: &[Vec<DVec2>]) -> Option<DVec2> {
    let all: Vec<DVec2> = outer
        .iter()
        .chain(holes.iter().flatten())
        .copied()
        .collect();
    tessellate_polygon_2d(outer, holes)
        .ok()?
        .into_iter()
        .map(|t| t.map(|i| all[i as usize]))
        .max_by(|a, b| {
            let area = |t: &[DVec2; 3]| (t[1] - t[0]).perp_dot(t[2] - t[0]).abs();
            area(a).total_cmp(&area(b))
        })
        .map(|t| (t[0] + t[1] + t[2]) / 3.0)
}

/// `true` when the contours fill `p` by the nonzero winding rule.
fn fills(contours: &[Vec<DVec2>], p: DVec2) -> bool {
    contours.iter().map(|c| winding(p, c)).sum::<i32>() != 0
}

/// The boundary of the bounded regions of the contours' arrangement that
/// the contours fill: sides two filled regions share are left out.
fn nonzero_boundary(
    contours: &[Vec<DVec2>],
    tol: &Tolerances,
) -> Result<Option<Vec<(DVec2, DVec2)>>, crate::geom::GeometryError> {
    let lift = |p: DVec2| DVec3::new(p.x, p.y, 0.0);
    let segments: Vec<(DVec3, DVec3)> = contours
        .iter()
        .flat_map(|c| ring_segments(c))
        .map(|(a, b)| (lift(a), lift(b)))
        .collect();
    let mut mesh = HalfEdgeMesh::new();
    mesh.add_edges(&segments, tol, &mut NoDeltas)?;
    let flat = |l| -> Result<Vec<DVec2>, crate::geom::GeometryError> {
        Ok(mesh
            .loop_positions(l)?
            .iter()
            .map(|p| p.truncate())
            .collect())
    };
    let mut rings: Vec<Vec<DVec2>> = Vec::new();
    for (_, face) in mesh.faces() {
        let outer = flat(face.outer)?;
        let holes = face
            .holes
            .iter()
            .map(|&l| flat(l))
            .collect::<Result<Vec<_>, _>>()?;
        if probe(&outer, &holes).is_some_and(|p| fills(contours, p)) {
            rings.push(outer);
            rings.extend(holes);
        }
    }
    if rings.is_empty() {
        return Ok(None);
    }
    let key = |a: DVec2, b: DVec2| {
        let (a, b) = (
            [a.x.to_bits(), a.y.to_bits()],
            [b.x.to_bits(), b.y.to_bits()],
        );
        if a <= b { (a, b) } else { (b, a) }
    };
    let mut uses: std::collections::BTreeMap<_, usize> = std::collections::BTreeMap::new();
    for (a, b) in rings.iter().flat_map(|r| ring_segments(r)) {
        *uses.entry(key(a, b)).or_default() += 1;
    }
    Ok(Some(
        rings
            .iter()
            .flat_map(|r| ring_segments(r))
            .filter(|&(a, b)| uses[&key(a, b)] == 1)
            .collect(),
    ))
}

/// Lines laid out: glyphs in model units (baseline at y = 0 for the
/// first line).
fn layout(
    face: &ttf_parser::Face<'_>,
    text: &str,
    height: f64,
    align: TextAlign,
    tol: &Tolerances,
) -> Vec<Glyph> {
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
            let g = glyph(moved, tol);
            if !g.boundary.is_empty() {
                out.push(g);
            }
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
        let tol = *tx.tolerances();
        let glyphs = layout(&face, &p.text, p.height.as_mm(), p.align, &tol);
        if glyphs.is_empty() {
            return Err(CommandError::invalid_param(
                "text",
                "has no visible glyphs in this font",
            ));
        }
        let h = p.extrude.as_mm();
        // The kernel bounds faces by the glyph outlines, as drawing them
        // would; faces the outlines do not fill (counters) go, and the
        // rest are pulled up (docs/05 Decisions, 2026-11-18).
        let segments: Vec<(DVec3, DVec3)> = glyphs
            .iter()
            .flat_map(|g| &g.boundary)
            .map(|&(a, b)| (DVec3::new(a.x, a.y, 0.0), DVec3::new(b.x, b.y, 0.0)))
            .collect();
        let mut mesh = HalfEdgeMesh::new();
        mesh.add_edges(&segments, &tol, &mut NoDeltas)?;
        let all: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
        let mut kept = Vec::new();
        for f in all {
            let face = mesh.face_ref(f)?;
            let flat = |l| -> Result<Vec<DVec2>, crate::geom::GeometryError> {
                Ok(mesh
                    .loop_positions(l)?
                    .iter()
                    .map(|q| q.truncate())
                    .collect())
            };
            let outer = flat(face.outer)?;
            let holes = face
                .holes
                .iter()
                .map(|&l| flat(l))
                .collect::<Result<Vec<_>, _>>()?;
            let filled = p.filled
                && probe(&outer, &holes)
                    .is_some_and(|q| glyphs.iter().any(|g| fills(&g.contours, q)));
            if filled {
                kept.push(f);
            } else {
                mesh.erase_face(f, false, &tol, &mut NoDeltas)?;
            }
        }
        // Filled faces that touch (letters run together at a small size)
        // become one, so each side wall is built once.
        let shared: Vec<_> = mesh.edges().map(|(e, _)| e).collect();
        for e in shared {
            if mesh.edge(e).is_some()
                && let [Some(a), Some(b)] = mesh.edge_faces(e)?
                && a != b
            {
                mesh.erase_edge(e, &tol, &mut NoDeltas)?;
            }
        }
        let kept: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
        // Faces front up; pulled up by the extrusion height.
        let down: BTreeSet<FaceId> = kept
            .iter()
            .copied()
            .filter(|&f| mesh.face_normal(f).is_ok_and(|n| n.z < 0.0))
            .collect();
        if !down.is_empty() {
            crate::geom::planar::reverse_faces(&mut mesh, &down, &tol, &mut NoDeltas)?;
        }
        if h > 0.0 {
            let opts = crate::geom::extrude::ExtrudeOptions {
                create_new_start_face: true,
                merge_coplanar: false,
                push_through: false,
            };
            for f in kept {
                crate::geom::extrude::extrude_face(
                    &mut mesh,
                    f,
                    p.extrude,
                    opts,
                    &tol,
                    &mut NoDeltas,
                )?;
            }
        }
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
            steps: crate::commands::common::tool_steps(&[("Pick where the text starts", "")]),
        }
    }
}
