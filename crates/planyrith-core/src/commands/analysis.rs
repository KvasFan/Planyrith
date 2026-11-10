//! Implements docs/05-modeling-tools.md §8.15 (surface and part analysis,
//! milestone 1.15): `analysis-curvature`, `analysis-zebra`,
//! `analysis-draft`, `analysis-thickness`, `analysis-comb` and
//! `analysis-clear`.
//!
//! An analysis measures faces and shows the result as a colored copy: a
//! group named `Analysis: <kind>` whose definition carries the attribute
//! `planyrith` / `analysis`, its faces lifted a hair off the originals
//! along their normals. Running the same analysis again replaces it;
//! `analysis-clear` removes it. The model itself is not changed.

use std::collections::BTreeMap;

use glam::{DVec2, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{remove_container_entity, targets_of};
use crate::commands::edit::transform::chain;
use crate::commands::fabrication::{new_group, read, store};
use crate::commands::params::{Target, VectorSchema, target_container};
use crate::commands::surfaces::{FaceSet, face_sets, inside_point, rings, rings_area};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Document, Material, Query, Rgba8};
use crate::geom::intersect::ray_face;
use crate::geom::mesh::IndexedEdge;
use crate::geom::surface::indexed;
use crate::geom::tessellate::tessellate_polygon_2d;
use crate::geom::{EdgeFlags, FaceSides, HalfEdgeMesh, IndexedMesh};
use crate::ids::{EdgeId, EntityId, FaceId, Pid};
use crate::math::{Plane, Ray};
use crate::units::{Angle, Length};

/// The key of an analysis group's kind in the `planyrith` dictionary.
pub const ANALYSIS: &str = "analysis";

/// What an analysis shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AnalysisKind {
    /// Convex and concave bending across smooth edges.
    Curvature,
    /// Alternating stripes by the faces' direction.
    Zebra,
    /// Draft angles against a pull direction.
    Draft,
    /// Wall thickness.
    Thickness,
    /// A curvature comb along edges.
    Comb,
    /// Von Mises stress of a static simulation (docs/05 §8.16).
    Stress,
    /// A natural mode's shape.
    Mode,
    /// Temperatures of a thermal simulation.
    Temperature,
}

impl AnalysisKind {
    fn label(self) -> &'static str {
        match self {
            Self::Curvature => "curvature",
            Self::Zebra => "zebra",
            Self::Draft => "draft",
            Self::Thickness => "thickness",
            Self::Comb => "comb",
            Self::Stress => "stress",
            Self::Mode => "mode shape",
            Self::Temperature => "temperature",
        }
    }
}

fn meta(name: &str, label: &str) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        label: label.into(),
        category: CommandCategory::Solid,
        view_only: false,
        steps: Vec::new(),
    }
}

/// One color class of an analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisClass {
    /// Its name (`needs draft`); its material is `Analysis: <name>`.
    pub name: String,
    /// Faces in it.
    pub faces: usize,
    /// Their area, mm².
    pub area_mm2: f64,
}

/// What an analysis found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisOutput {
    /// The colored copy.
    pub group: Option<Pid>,
    /// Faces analyzed.
    pub faces: usize,
    /// The classes, in color order.
    pub classes: Vec<AnalysisClass>,
    /// The smallest value measured (curvature: bend radius, mm; draft:
    /// degrees; thickness: mm).
    pub min: Option<f64>,
    /// The largest value measured.
    pub max: Option<f64>,
}

/// One analyzed face: its loops and normal in edit-context coordinates.
pub(crate) struct Measured {
    pub(crate) rings: Vec<Vec<DVec3>>,
    pub(crate) normal: DVec3,
    pub(crate) class: usize,
}

/// A face of a set, seen from the edit context.
struct Seen {
    face: FaceId,
    rings: Vec<Vec<DVec3>>,
    normal: DVec3,
}

/// The faces of `set` in edit-context coordinates.
fn seen(tx: &Transaction<'_>, set: &FaceSet) -> Result<Vec<Seen>, CommandError> {
    let mesh = mesh_of(tx, set)?;
    let t = set.to_target;
    let flip = t.matrix3.determinant() < 0.0;
    let normal_m = t.matrix3.inverse().transpose();
    let mut out = Vec::new();
    for f in &set.faces {
        let face = mesh.face_ref(*f)?;
        let mut r: Vec<Vec<DVec3>> = rings(mesh, *f)?
            .into_iter()
            .map(|ring| ring.into_iter().map(|q| t.transform_point3(q)).collect())
            .collect();
        if flip {
            for ring in &mut r {
                ring.reverse();
            }
        }
        out.push(Seen {
            face: *f,
            rings: r,
            normal: (normal_m * face.plane.normal).normalize_or_zero(),
        });
    }
    Ok(out)
}

fn mesh_of<'a>(tx: &'a Transaction<'_>, set: &FaceSet) -> Result<&'a HalfEdgeMesh, CommandError> {
    Ok(tx
        .document()
        .container(set.container)
        .ok_or_else(|| CommandError::Internal("a container vanished".into()))?
        .mesh())
}

/// Removes the analysis groups of `kind` (all kinds when `None`) from the
/// edit context; returns how many.
fn clear(
    tx: &mut Transaction<'_>,
    target: &Target,
    kind: Option<AnalysisKind>,
) -> Result<usize, CommandError> {
    let doc = tx.document();
    let ids: Vec<EntityId> = doc
        .container(target.container)
        .map(|c| {
            c.groups()
                .filter(|(_, g)| {
                    read::<AnalysisKind>(doc, g.definition, ANALYSIS)
                        .is_some_and(|k| kind.is_none_or(|w| w == k))
                })
                .map(|(id, _)| EntityId::Group(id))
                .collect()
        })
        .unwrap_or_default();
    for id in &ids {
        remove_container_entity(tx, target.container, *id)?;
    }
    Ok(ids.len())
}

/// Builds the colored copy and the summary.
pub(crate) fn show(
    tx: &mut Transaction<'_>,
    target: &Target,
    kind: AnalysisKind,
    measured: Vec<Measured>,
    classes: &[(&str, Rgba8)],
    range: Option<(f64, f64)>,
) -> Result<AnalysisOutput, CommandError> {
    clear(tx, target, Some(kind))?;
    let tol = *tx.tolerances();
    let mut summary: Vec<AnalysisClass> = classes
        .iter()
        .map(|(name, _)| AnalysisClass {
            name: (*name).to_string(),
            faces: 0,
            area_mm2: 0.0,
        })
        .collect();
    // Lift the copy off the originals by a hair of the analyzed size.
    let (mut lo, mut hi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
    for m in &measured {
        for p in m.rings.iter().flatten() {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
    }
    let lift = ((hi - lo).length() * 1.0e-4).clamp(0.01, 1.0);
    let mut materials = Vec::new();
    for (name, color) in classes {
        let full = format!("Analysis: {name}");
        let id = match tx.document().materials.by_name(&full) {
            Some(id) => id,
            None => tx.insert_material(Material::solid(full, *color))?,
        };
        materials.push(id);
    }
    // Each corner moves along the mean normal of the faces meeting there,
    // so the copy stays one connected skin; a face no longer flat after
    // the move is split into triangles.
    let q = tol.length.max(1e-6);
    let key = |p: DVec3| {
        [
            (p.x / q).round() as i64,
            (p.y / q).round() as i64,
            (p.z / q).round() as i64,
        ]
    };
    let mut normals: BTreeMap<[i64; 3], DVec3> = BTreeMap::new();
    for m in &measured {
        for p in m.rings.iter().flatten() {
            *normals.entry(key(*p)).or_insert(DVec3::ZERO) += m.normal;
        }
    }
    let lifted = |p: DVec3| {
        p + normals
            .get(&key(p))
            .copied()
            .unwrap_or_default()
            .normalize_or_zero()
            * lift
    };
    let mut faces = Vec::new();
    for m in &measured {
        let s = &mut summary[m.class];
        s.faces += 1;
        s.area_mm2 += rings_area(&m.rings);
        let sides = FaceSides {
            material_front: Some(materials[m.class]),
            material_back: Some(materials[m.class]),
            ..FaceSides::default()
        };
        let moved: Vec<Vec<DVec3>> = m
            .rings
            .iter()
            .map(|r| r.iter().map(|p| lifted(*p)).collect())
            .collect();
        let origin = moved[0][0];
        let flat = moved
            .iter()
            .flatten()
            .all(|p| m.normal.dot(*p - origin).abs() <= tol.planarity);
        if flat {
            faces.push((moved, sides));
            continue;
        }
        let frame = Plane::from_point_normal(m.rings[0][0], m.normal).frame();
        let flat2: Vec<Vec<DVec2>> = m
            .rings
            .iter()
            .map(|r| r.iter().map(|p| frame.to_2d(*p)).collect())
            .collect();
        let all: Vec<DVec3> = moved.iter().flatten().copied().collect();
        for t in tessellate_polygon_2d(&flat2[0], &flat2[1..])? {
            faces.push((
                vec![t.iter().map(|i| all[*i as usize]).collect()],
                sides.clone(),
            ));
        }
    }
    let mut piece = indexed(&faces, &tol);
    for e in &mut piece.edges {
        e.flags = EdgeFlags::SOFT | EdgeFlags::SMOOTH;
    }
    let mesh = HalfEdgeMesh::from_indexed(&piece, &tol)?;
    let name = format!("Analysis: {}", kind.label());
    let (pid, d) = new_group(tx, target, &name, mesh)?;
    store(tx, d, ANALYSIS, &kind)?;
    Ok(AnalysisOutput {
        group: Some(pid),
        faces: measured.len(),
        classes: summary,
        min: range.map(|r| r.0),
        max: range.map(|r| r.1),
    })
}

fn widen(range: &mut Option<(f64, f64)>, v: f64) {
    *range = Some(match *range {
        Some((a, b)) => (a.min(v), b.max(v)),
        None => (v, v),
    });
}

/// Common analysis parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisFaces {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Faces of the edit context, or groups and components (all their
    /// faces).
    pub faces: Query,
}

// ----- analysis-curvature ---------------------------------------------------------------

/// The `analysis-curvature` command: how sharply each face bends toward
/// its neighbors across smooth edges, convex or concave: the mean over
/// those edges of the curvature of the circle through both faces' middles
/// square to them.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnalysisCurvature;

const CURVATURE: [(&str, Rgba8); 9] = [
    ("flat", Rgba8::rgb(190, 190, 190)),
    ("convex 1", Rgba8::rgb(250, 220, 150)),
    ("convex 2", Rgba8::rgb(245, 170, 80)),
    ("convex 3", Rgba8::rgb(230, 110, 40)),
    ("convex 4", Rgba8::rgb(200, 40, 30)),
    ("concave 1", Rgba8::rgb(170, 210, 245)),
    ("concave 2", Rgba8::rgb(100, 160, 230)),
    ("concave 3", Rgba8::rgb(50, 100, 210)),
    ("concave 4", Rgba8::rgb(30, 40, 160)),
];

impl Command for AnalysisCurvature {
    type Params = AnalysisFaces;
    type Output = AnalysisOutput;

    fn name(&self) -> &'static str {
        "analysis-curvature"
    }

    fn validate(&self, _: &Document, _: &AnalysisFaces) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: AnalysisFaces,
    ) -> Result<AnalysisOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let tol = *tx.tolerances();
        let sets = face_sets(tx, &target, "faces", &p.faces, true)?;
        let mut found: Vec<(Vec<Vec<DVec3>>, DVec3, f64)> = Vec::new();
        for set in &sets {
            let faces = seen(tx, set)?;
            let mesh = mesh_of(tx, set)?;
            let scale = libm::cbrt(set.to_target.matrix3.determinant().abs()).max(1e-12);
            for s in faces {
                let face = mesh.face_ref(s.face)?;
                let n = face.plane.normal;
                let c = inside_point(mesh, s.face, &tol)?;
                let mut loops = vec![face.outer];
                loops.extend(face.holes.iter().copied());
                let (mut sum, mut count) = (0.0_f64, 0usize);
                for l in loops {
                    for h in mesh.loop_half_edges(l)? {
                        let Some(he) = mesh.half_edge(h) else {
                            continue;
                        };
                        let smooth = mesh.edge(he.edge).is_some_and(|e| {
                            e.flags.contains(EdgeFlags::SOFT) || e.flags.contains(EdgeFlags::SMOOTH)
                        });
                        let Some(g) = mesh.half_edge(he.twin).and_then(|t| t.face) else {
                            continue;
                        };
                        if !smooth || g == s.face {
                            continue;
                        }
                        let ng = mesh.face_ref(g)?.plane.normal;
                        let theta = libm::atan2(n.cross(ng).length(), n.dot(ng));
                        let cg = inside_point(mesh, g, &tol)?;
                        let d = c.distance(cg) * scale;
                        if theta <= tol.angle || d <= tol.length {
                            continue;
                        }
                        count += 1;
                        let sign = if n.dot(cg - c) < 0.0 { 1.0 } else { -1.0 };
                        sum += sign * 2.0 * libm::sin(theta / 2.0) / d;
                    }
                }
                // The mean bending toward the smooth neighbors.
                let k = if count == 0 { 0.0 } else { sum / count as f64 };
                found.push((s.rings, s.normal, k));
            }
        }
        let kmax = found.iter().map(|f| f.2.abs()).fold(0.0, f64::max);
        let mut range = None;
        let measured = found
            .into_iter()
            .map(|(rings, normal, k)| {
                let class = if k == 0.0 {
                    0
                } else {
                    widen(&mut range, 1.0 / k.abs());
                    let bin = ((k.abs() / kmax * 4.0).ceil() as usize).clamp(1, 4);
                    if k > 0.0 { bin } else { 4 + bin }
                };
                Measured {
                    rings,
                    normal,
                    class,
                }
            })
            .collect();
        show(
            tx,
            &target,
            AnalysisKind::Curvature,
            measured,
            &CURVATURE,
            range,
        )
    }

    fn meta(&self) -> CommandMeta {
        meta("analysis-curvature", "Curvature Analysis")
    }
}

// ----- analysis-zebra -------------------------------------------------------------------

fn up() -> DVec3 {
    DVec3::Z
}

fn twelve() -> u32 {
    12
}

/// `analysis-zebra` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisZebraParams {
    /// Common fields and faces.
    #[serde(flatten)]
    pub faces: AnalysisFaces,
    /// The direction the stripes are measured from (up).
    #[serde(default = "up")]
    #[schemars(with = "VectorSchema")]
    pub direction: DVec3,
    /// Stripes over a half turn (12).
    #[serde(default = "twelve")]
    pub stripes: u32,
}

/// The `analysis-zebra` command: alternating light and dark stripes by
/// the angle between each face and a direction; on a smooth surface the
/// stripes run on continuously, a kink makes them jump.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnalysisZebra;

const ZEBRA: [(&str, Rgba8); 2] = [
    ("stripe light", Rgba8::rgb(245, 245, 245)),
    ("stripe dark", Rgba8::rgb(25, 25, 25)),
];

impl Command for AnalysisZebra {
    type Params = AnalysisZebraParams;
    type Output = AnalysisOutput;

    fn name(&self) -> &'static str {
        "analysis-zebra"
    }

    fn validate(&self, _: &Document, p: &AnalysisZebraParams) -> Result<(), CommandError> {
        if p.direction.try_normalize().is_none() {
            return Err(CommandError::invalid_param("direction", "must be non-zero"));
        }
        if !(2..=360).contains(&p.stripes) {
            return Err(CommandError::OutOfRange {
                name: "stripes".into(),
                reason: "must be 2 to 360".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: AnalysisZebraParams,
    ) -> Result<AnalysisOutput, CommandError> {
        let target = target_container(tx, &p.faces.common)?;
        let dir = target.local.transform_vector3(p.direction).normalize();
        let sets = face_sets(tx, &target, "faces", &p.faces.faces, true)?;
        let mut measured = Vec::new();
        for set in &sets {
            for s in seen(tx, set)? {
                let angle = libm::acos(s.normal.dot(dir).clamp(-1.0, 1.0));
                let band = (angle / std::f64::consts::PI * f64::from(p.stripes)).floor() as i64;
                measured.push(Measured {
                    rings: s.rings,
                    normal: s.normal,
                    class: band.rem_euclid(2) as usize,
                });
            }
        }
        show(tx, &target, AnalysisKind::Zebra, measured, &ZEBRA, None)
    }

    fn meta(&self) -> CommandMeta {
        meta("analysis-zebra", "Zebra Stripes")
    }
}

// ----- analysis-draft -------------------------------------------------------------------

fn one_degree() -> Angle {
    Angle::radians(1f64.to_radians())
}

/// `analysis-draft` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisDraftParams {
    /// Common fields and faces.
    #[serde(flatten)]
    pub faces: AnalysisFaces,
    /// The direction the part leaves the mold (up).
    #[serde(default = "up")]
    #[schemars(with = "VectorSchema")]
    pub pull: DVec3,
    /// The least draft a face needs (1°).
    #[serde(default = "one_degree")]
    pub angle: Angle,
}

/// The `analysis-draft` command: each face's draft angle against a pull
/// direction — positive (it leaves toward the pull), negative (it leaves
/// the other way), or too little.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnalysisDraft;

const DRAFT: [(&str, Rgba8); 3] = [
    ("positive draft", Rgba8::rgb(60, 170, 90)),
    ("negative draft", Rgba8::rgb(210, 60, 50)),
    ("needs draft", Rgba8::rgb(235, 190, 40)),
];

impl Command for AnalysisDraft {
    type Params = AnalysisDraftParams;
    type Output = AnalysisOutput;

    fn name(&self) -> &'static str {
        "analysis-draft"
    }

    fn validate(&self, _: &Document, p: &AnalysisDraftParams) -> Result<(), CommandError> {
        if p.pull.try_normalize().is_none() {
            return Err(CommandError::invalid_param("pull", "must be non-zero"));
        }
        if !(0.0..std::f64::consts::FRAC_PI_2).contains(&p.angle.as_radians()) {
            return Err(CommandError::OutOfRange {
                name: "angle".into(),
                reason: "must be 0 to 90°".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: AnalysisDraftParams,
    ) -> Result<AnalysisOutput, CommandError> {
        let target = target_container(tx, &p.faces.common)?;
        let pull = target.local.transform_vector3(p.pull).normalize();
        let need = p.angle.as_radians().to_degrees();
        let sets = face_sets(tx, &target, "faces", &p.faces.faces, true)?;
        let mut measured = Vec::new();
        let mut range = None;
        for set in &sets {
            for s in seen(tx, set)? {
                let a = libm::asin(s.normal.dot(pull).clamp(-1.0, 1.0)).to_degrees();
                widen(&mut range, a);
                let class = if a >= need - 1e-9 {
                    0
                } else if a <= -need + 1e-9 {
                    1
                } else {
                    2
                };
                measured.push(Measured {
                    rings: s.rings,
                    normal: s.normal,
                    class,
                });
            }
        }
        show(tx, &target, AnalysisKind::Draft, measured, &DRAFT, range)
    }

    fn meta(&self) -> CommandMeta {
        meta("analysis-draft", "Draft Analysis")
    }
}

// ----- analysis-thickness ---------------------------------------------------------------

/// `analysis-thickness` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisThicknessParams {
    /// Common fields and faces.
    #[serde(flatten)]
    pub faces: AnalysisFaces,
    /// Walls thinner than this are flagged.
    pub minimum: Length,
}

/// The `analysis-thickness` command: the wall thickness behind each face,
/// measured from its middle straight into the material to the next face.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnalysisThickness;

const THICKNESS: [(&str, Rgba8); 5] = [
    ("too thin", Rgba8::rgb(215, 45, 40)),
    ("thickness 1", Rgba8::rgb(240, 200, 60)),
    ("thickness 2", Rgba8::rgb(150, 205, 90)),
    ("thickness 3", Rgba8::rgb(50, 160, 90)),
    ("no opposite face", Rgba8::rgb(190, 190, 190)),
];

/// A face's bounding box, for skipping faces a ray cannot reach.
fn face_box(mesh: &HalfEdgeMesh, f: FaceId) -> Result<(DVec3, DVec3), CommandError> {
    let mut lo = DVec3::splat(f64::MAX);
    let mut hi = DVec3::splat(f64::MIN);
    for p in mesh.loop_positions(mesh.face_ref(f)?.outer)? {
        lo = lo.min(p);
        hi = hi.max(p);
    }
    Ok((lo, hi))
}

fn ray_hits_box(ray: &Ray, (lo, hi): (DVec3, DVec3), pad: f64) -> bool {
    let (mut t0, mut t1) = (0.0_f64, f64::MAX);
    for i in 0..3 {
        let (o, d) = (ray.origin[i], ray.dir[i]);
        let (a, b) = (lo[i] - pad, hi[i] + pad);
        if d.abs() < 1e-15 {
            if o < a || o > b {
                return false;
            }
            continue;
        }
        let (mut ta, mut tb) = ((a - o) / d, (b - o) / d);
        if ta > tb {
            std::mem::swap(&mut ta, &mut tb);
        }
        t0 = t0.max(ta);
        t1 = t1.min(tb);
        if t0 > t1 {
            return false;
        }
    }
    true
}

impl Command for AnalysisThickness {
    type Params = AnalysisThicknessParams;
    type Output = AnalysisOutput;

    fn name(&self) -> &'static str {
        "analysis-thickness"
    }

    fn validate(&self, _: &Document, p: &AnalysisThicknessParams) -> Result<(), CommandError> {
        if !(p.minimum.as_mm() > 0.0 && p.minimum.as_mm().is_finite()) {
            return Err(CommandError::OutOfRange {
                name: "minimum".into(),
                reason: "must be positive".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: AnalysisThicknessParams,
    ) -> Result<AnalysisOutput, CommandError> {
        let target = target_container(tx, &p.faces.common)?;
        let tol = *tx.tolerances();
        let minimum = p.minimum.as_mm();
        let sets = face_sets(tx, &target, "faces", &p.faces.faces, true)?;
        let mut found: Vec<(Vec<Vec<DVec3>>, DVec3, Option<f64>)> = Vec::new();
        for set in &sets {
            let faces = seen(tx, set)?;
            let mesh = mesh_of(tx, set)?;
            let all: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
            let boxes = all
                .iter()
                .map(|f| face_box(mesh, *f))
                .collect::<Result<Vec<_>, _>>()?;
            for s in faces {
                let n = mesh.face_ref(s.face)?.plane.normal;
                let start = inside_point(mesh, s.face, &tol)?;
                let ray = Ray::new(start, -n);
                let mut best: Option<f64> = None;
                for (g, b) in all.iter().zip(&boxes) {
                    if *g == s.face || !ray_hits_box(&ray, *b, tol.length) {
                        continue;
                    }
                    if let Some(hit) = ray_face(&ray, mesh, *g, &tol)?
                        && hit.t > tol.length
                        && best.is_none_or(|t| hit.t < t)
                    {
                        best = Some(hit.t);
                    }
                }
                let t = best.map(|t| set.to_target.transform_vector3(-n * t).length());
                found.push((s.rings, s.normal, t));
            }
        }
        let mut range = None;
        for (_, _, t) in &found {
            if let Some(t) = t {
                widen(&mut range, *t);
            }
        }
        let top = range.map_or(minimum, |r: (f64, f64)| r.1);
        let measured = found
            .into_iter()
            .map(|(rings, normal, t)| {
                let class = match t {
                    None => 4,
                    Some(t) if t < minimum - tol.length => 0,
                    Some(t) => {
                        let span = (top - minimum).max(tol.length);
                        (1 + ((t - minimum) / span * 3.0).floor() as usize).clamp(1, 3)
                    }
                };
                Measured {
                    rings,
                    normal,
                    class,
                }
            })
            .collect();
        show(
            tx,
            &target,
            AnalysisKind::Thickness,
            measured,
            &THICKNESS,
            range,
        )
    }

    fn meta(&self) -> CommandMeta {
        meta("analysis-thickness", "Thickness Analysis")
    }
}

// ----- analysis-comb --------------------------------------------------------------------

/// `analysis-comb` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisCombParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Edges of the edit context forming one chain (open or closed).
    pub edges: Query,
    /// Tooth length per unit of curvature, mm² (by default the longest
    /// tooth is a fifth of the chain's length).
    #[serde(default)]
    pub scale: Option<f64>,
}

/// What `analysis-comb` drew.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisCombOutput {
    /// The comb's group.
    pub group: Option<Pid>,
    /// Teeth drawn (one per bent corner).
    pub teeth: usize,
    /// The tightest bend radius, mm.
    pub min_radius_mm: Option<f64>,
    /// The widest bend radius, mm.
    pub max_radius_mm: Option<f64>,
}

/// The curvature at `b` of the circle through `a`, `b` and `c`, and the
/// unit direction from `b` away from that circle's center.
fn bend(a: DVec3, b: DVec3, c: DVec3) -> Option<(f64, DVec3)> {
    let (ab, cb, ca) = (b.distance(a), c.distance(b), c.distance(a));
    let twice_area = (b - a).cross(c - a).length();
    if ab * cb * ca <= 1e-12 || twice_area <= 1e-12 {
        return None;
    }
    let inward = ((a - b) / ab + (c - b) / cb).normalize_or_zero();
    Some((2.0 * twice_area / (ab * cb * ca), -inward))
}

fn wire(m: &mut IndexedMesh, a: DVec3, b: DVec3, tol: f64) {
    if a.distance(b) <= tol {
        return;
    }
    let mut at = |p: DVec3| {
        if let Some(i) = m.vertices.iter().position(|v| v.distance(p) <= tol) {
            return i as u32;
        }
        m.vertices.push(p);
        (m.vertices.len() - 1) as u32
    };
    let (i, j) = (at(a), at(b));
    m.edges.push(IndexedEdge {
        a: i.min(j),
        b: i.max(j),
        flags: EdgeFlags::NONE,
        curve: None,
    });
}

/// The `analysis-comb` command: a curvature comb along a chain of edges —
/// at each corner a tooth pointing away from the bend, as long as the
/// curvature there times the scale, and a line joining the teeth's tips.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnalysisComb;

impl Command for AnalysisComb {
    type Params = AnalysisCombParams;
    type Output = AnalysisCombOutput;

    fn name(&self) -> &'static str {
        "analysis-comb"
    }

    fn validate(&self, _: &Document, p: &AnalysisCombParams) -> Result<(), CommandError> {
        if p.scale.is_some_and(|s| !(s > 0.0 && s.is_finite())) {
            return Err(CommandError::OutOfRange {
                name: "scale".into(),
                reason: "must be positive".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: AnalysisCombParams,
    ) -> Result<AnalysisCombOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let edges: Vec<EdgeId> = targets_of(tx, &target, "edges", &p.edges)?
            .into_iter()
            .filter_map(|id| match id {
                EntityId::Edge(e) => Some(e),
                _ => None,
            })
            .collect();
        let Some(first) = edges.first() else {
            return Err(CommandError::invalid_param(
                "edges",
                "name a chain of edges",
            ));
        };
        let (mut points, closed) = {
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
                .mesh();
            let near = mesh.position(mesh.edge_endpoints(*first)?.0)?;
            chain(mesh, &edges, near).map_err(|_| {
                CommandError::invalid_param("edges", "the edges must form one chain")
            })?
        };
        if closed {
            points.pop();
        }
        let n = points.len();
        let mut length: f64 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
        if closed {
            length += points[n - 1].distance(points[0]);
        }
        let bends: Vec<Option<(f64, DVec3)>> = (0..n)
            .map(|i| {
                (closed || (i > 0 && i + 1 < n))
                    .then(|| bend(points[(i + n - 1) % n], points[i], points[(i + 1) % n]))
                    .flatten()
            })
            .collect();
        let kmax = bends.iter().flatten().map(|b| b.0).fold(0.0, f64::max);
        if kmax <= 0.0 {
            return Err(CommandError::NotApplicable(
                "the chain is straight: there is no curvature to show".into(),
            ));
        }
        let scale = p.scale.unwrap_or(0.2 * length / kmax);
        let tol = *tx.tolerances();
        let mut piece = IndexedMesh::default();
        let mut range = None;
        let mut tips = Vec::with_capacity(n);
        for (p0, b) in points.iter().zip(&bends) {
            let tip = match b {
                Some((k, out)) => {
                    widen(&mut range, 1.0 / k);
                    wire(&mut piece, *p0, *p0 + *out * (k * scale), tol.length);
                    *p0 + *out * (k * scale)
                }
                None => *p0,
            };
            tips.push(tip);
        }
        for w in tips.windows(2) {
            wire(&mut piece, w[0], w[1], tol.length);
        }
        if closed && n > 2 {
            wire(&mut piece, tips[n - 1], tips[0], tol.length);
        }
        let mesh = HalfEdgeMesh::from_indexed(&piece, &tol)?;
        clear(tx, &target, Some(AnalysisKind::Comb))?;
        let (pid, d) = new_group(tx, &target, "Analysis: comb", mesh)?;
        store(tx, d, ANALYSIS, &AnalysisKind::Comb)?;
        Ok(AnalysisCombOutput {
            group: Some(pid),
            teeth: bends.iter().flatten().count(),
            min_radius_mm: range.map(|r: (f64, f64)| r.0),
            max_radius_mm: range.map(|r: (f64, f64)| r.1),
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("analysis-comb", "Curvature Comb")
    }
}

// ----- analysis-clear -------------------------------------------------------------------

/// `analysis-clear` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisClearParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Only this kind (all kinds by default).
    #[serde(default)]
    pub kind: Option<AnalysisKind>,
}

/// What `analysis-clear` removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisClearOutput {
    /// Analysis groups removed.
    pub removed: usize,
}

/// The `analysis-clear` command: removes the edit context's analysis
/// copies.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnalysisClear;

impl Command for AnalysisClear {
    type Params = AnalysisClearParams;
    type Output = AnalysisClearOutput;

    fn name(&self) -> &'static str {
        "analysis-clear"
    }

    fn validate(&self, _: &Document, _: &AnalysisClearParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: AnalysisClearParams,
    ) -> Result<AnalysisClearOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        Ok(AnalysisClearOutput {
            removed: clear(tx, &target, p.kind)?,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("analysis-clear", "Clear Analysis")
    }
}

/// Registers the analysis commands.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(AnalysisCurvature);
    registry.register(AnalysisZebra);
    registry.register(AnalysisDraft);
    registry.register(AnalysisThickness);
    registry.register(AnalysisComb);
    registry.register(AnalysisClear);
}

/// Whether `d` is an analysis copy (readers skip it in measurements).
pub fn is_analysis(doc: &Document, d: crate::ids::DefinitionId) -> bool {
    read::<AnalysisKind>(doc, d, ANALYSIS).is_some()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn run(e: &mut Engine, name: &str, p: Value) -> Value {
        e.execute_erased(name, p).unwrap()
    }

    fn class(out: &Value, name: &str) -> u64 {
        out["classes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()["faces"]
            .as_u64()
            .unwrap()
    }

    #[test]
    fn draft_and_thickness_of_a_tapered_block() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [100, 60, 0] }),
        );
        run(
            &mut e,
            "push-pull",
            json!({"face": {"Query": "face"}, "distance": 20}),
        );
        let d = run(&mut e, "analysis-draft", json!({"faces": "face"}));
        // Top up, bottom down, four walls with no draft.
        assert_eq!(class(&d, "positive draft"), 1);
        assert_eq!(class(&d, "negative draft"), 1);
        assert_eq!(class(&d, "needs draft"), 4);
        assert_eq!(d["min"], -90.0);
        let t = run(
            &mut e,
            "analysis-thickness",
            json!({"faces": "face", "minimum": 30}),
        );
        // The 20 mm top and bottom are too thin; the 60 and 100 mm walls not.
        assert_eq!(class(&t, "too thin"), 2);
        assert!((t["min"].as_f64().unwrap() - 20.0).abs() < 1e-9);
        assert!((t["max"].as_f64().unwrap() - 100.0).abs() < 1e-9);
        // Running again replaces the copy; clearing removes both.
        run(&mut e, "analysis-draft", json!({"faces": "face"}));
        let c = run(&mut e, "analysis-clear", json!({}));
        assert_eq!(c["removed"], 2);
    }

    #[test]
    fn curvature_and_zebra_of_a_cylinder_and_a_patch() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        run(
            &mut e,
            "circle",
            json!({"center": [0, 0, 0], "radius": 50, "segments": 24}),
        );
        run(
            &mut e,
            "push-pull",
            json!({"face": {"Query": "face"}, "distance": 80}),
        );
        let c = run(&mut e, "analysis-curvature", json!({"faces": "face"}));
        assert_eq!(c["faces"], 26);
        assert_eq!(class(&c, "flat"), 2);
        let convex: u64 = (1..=4).map(|i| class(&c, &format!("convex {i}"))).sum();
        assert_eq!(convex, 24);
        // The sides bend with the circle's radius.
        let r = c["min"].as_f64().unwrap();
        assert!((r - 50.0).abs() < 0.5, "{r}");
        run(&mut e, "erase", json!({"targets": "all"}));
        run(
            &mut e,
            "line",
            json!({"points": [[0,0,0],[100,0,50],[100,100,0],[0,100,50]], "close": true}),
        );
        run(
            &mut e,
            "surface-fill",
            json!({"boundary": "edge", "divisions": 6}),
        );
        let z = run(
            &mut e,
            "analysis-zebra",
            json!({"faces": "group & !name:Analysis*"}),
        );
        assert_eq!(z["faces"], 72);
        assert!(class(&z, "stripe light") > 0 && class(&z, "stripe dark") > 0);
    }

    #[test]
    fn a_comb_along_an_arc() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        run(
            &mut e,
            "arc-2point",
            json!({"center": [0, 0, 0], "start": [100, 0, 0], "end_angle": std::f64::consts::FRAC_PI_2, "segments": 12}),
        );
        let c = run(&mut e, "analysis-comb", json!({"edges": "edge"}));
        // Eleven inner corners, each on the 100 mm circle.
        assert_eq!(c["teeth"], 11);
        let r = c["min_radius_mm"].as_f64().unwrap();
        assert!((r - 100.0).abs() < 1e-6, "{r}");
        assert!(
            e.execute_erased("analysis-comb", json!({"edges": "nothing"}))
                .is_err()
        );
    }
}
