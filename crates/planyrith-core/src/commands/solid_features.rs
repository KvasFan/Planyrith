//! Implements docs/05-modeling-tools.md §8.6–§8.11 (solid features,
//! milestone 1.11): `hole`, `shell`, `draft`, `split-solid`, `dome` and
//! `rib` on the closed solid of the edit context that a face belongs to.
//!
//! They work on loose geometry, so they run inside parts (docs/05 §5.16)
//! and inside any open group: the solid is the connected set of faces
//! around the named face, which must close. Results built by Boolean
//! operations (docs/03 §4.13) replace that solid's faces.

use std::collections::BTreeSet;

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{SideRows, definition_uuid, mesh_piece, place_geometry, targets_of};
use crate::commands::params::{PointSchema, Target, VectorSchema, target_container};
use crate::commands::transaction::AttrTarget;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ComponentDefinition, Document, Entities, EntityRecord, Group, Query, Value};
use crate::geom::boolean::{BooleanOp, boolean};
use crate::geom::loft::loft;
use crate::geom::revolve::revolve;
use crate::geom::thicken::thicken;
use crate::geom::{FaceSides, GeometryError, HalfEdgeMesh, TransformOptions};
use crate::ids::{EntityId, FaceId, Pid, VertexId};
use crate::units::{Angle, Length};

/// What a solid feature made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SolidFeatureOutput {
    /// Faces of the resulting solid(s).
    pub faces: usize,
    /// Their volume, mm³.
    pub volume_mm3: f64,
    /// New groups (`split-solid` only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Pid>,
    /// The thread written on a tapped hole (`M8x1.25`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

/// A finite length above zero.
fn positive(v: f64) -> bool {
    v.is_finite() && v > 0.0
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

fn one_face(
    tx: &Transaction<'_>,
    target: &Target,
    param: &str,
    q: &Query,
) -> Result<FaceId, CommandError> {
    let faces: Vec<FaceId> = targets_of(tx, target, param, q)?
        .into_iter()
        .filter_map(|id| match id {
            EntityId::Face(f) => Some(f),
            _ => None,
        })
        .collect();
    match faces.as_slice() {
        [f] => Ok(*f),
        _ => Err(CommandError::invalid_param(
            param,
            "must name exactly one face",
        )),
    }
}

fn faces_of(
    tx: &Transaction<'_>,
    target: &Target,
    param: &str,
    q: &Query,
) -> Result<BTreeSet<FaceId>, CommandError> {
    let faces: BTreeSet<FaceId> = targets_of(tx, target, param, q)?
        .into_iter()
        .filter_map(|id| match id {
            EntityId::Face(f) => Some(f),
            _ => None,
        })
        .collect();
    if faces.is_empty() {
        return Err(CommandError::invalid_param(param, "name at least one face"));
    }
    Ok(faces)
}

/// The closed solid around `seed`: its faces and a copy of it.
fn shell_of(
    tx: &Transaction<'_>,
    target: &Target,
    seed: FaceId,
) -> Result<(Vec<FaceId>, HalfEdgeMesh), CommandError> {
    let entities = tx
        .document()
        .container(target.container)
        .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
    let mesh = entities.mesh();
    let mut seen = BTreeSet::from([seed]);
    let mut todo = vec![seed];
    while let Some(f) = todo.pop() {
        for g in mesh.face_neighbors(f)? {
            if seen.insert(g) {
                todo.push(g);
            }
        }
    }
    let faces: Vec<FaceId> = seen.into_iter().collect();
    let piece = mesh_piece(entities, &faces, &[])?;
    let solid = HalfEdgeMesh::from_indexed(&piece.indexed, tx.tolerances())?;
    if !solid.is_solid() {
        return Err(CommandError::NotApplicable(
            "the face does not belong to a closed solid; check it with solid-check".into(),
        ));
    }
    Ok((faces, solid))
}

/// Replaces the faces `faces` of the edit context by `results`.
fn replace_shell(
    tx: &mut Transaction<'_>,
    target: &Target,
    faces: &[FaceId],
    results: &[HalfEdgeMesh],
) -> Result<(), CommandError> {
    tx.with_mesh(target.container, |m, tol, d| {
        for &f in faces {
            if m.face(f).is_some() {
                m.erase_face(f, true, tol, d)?;
            }
        }
        Ok(())
    })?;
    for r in results {
        let (indexed, _) = r.to_indexed();
        place_geometry(
            tx,
            target.container,
            &indexed,
            vec![SideRows::default(); indexed.edges.len()],
            vec![SideRows::default(); indexed.faces.len()],
        )?;
    }
    Ok(())
}

fn summary(results: &[HalfEdgeMesh]) -> Result<(usize, f64), CommandError> {
    let mut faces = 0;
    let mut volume = 0.0;
    for r in results {
        faces += r.face_count();
        volume += r.volume()?;
    }
    Ok((faces, volume))
}

/// A Boolean of the solid around a face with a tool solid, replacing it.
fn combine(
    tx: &mut Transaction<'_>,
    target: &Target,
    seed: FaceId,
    op: BooleanOp,
    tool: &HalfEdgeMesh,
) -> Result<Vec<HalfEdgeMesh>, CommandError> {
    let (faces, solid) = shell_of(tx, target, seed)?;
    let tol = *tx.tolerances();
    let result = boolean(
        op,
        &solid,
        &DAffine3::IDENTITY,
        tool,
        &DAffine3::IDENTITY,
        &tol,
    )?;
    if result.solids.is_empty() {
        return Err(CommandError::NotApplicable(
            "nothing of the solid would be left".into(),
        ));
    }
    replace_shell(tx, target, &faces, &result.solids)?;
    Ok(result.solids)
}

/// A unit vector perpendicular to `n`.
fn across(n: DVec3) -> DVec3 {
    let helper = if n.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    n.cross(helper).normalize()
}

fn bounds_diagonal(mesh: &HalfEdgeMesh) -> f64 {
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for (_, v) in mesh.vertices() {
        lo = lo.min(v.position);
        hi = hi.max(v.position);
    }
    (hi - lo).length().max(1.0)
}

// ----- hole -----------------------------------------------------------------------------

/// The kind of hole (docs/05 §8.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HoleKind {
    /// A plain hole of `diameter` (or the screw's nominal size).
    #[default]
    Simple,
    /// A clearance hole for the screw (ISO 273).
    Clearance,
    /// A hole drilled for tapping, with a cosmetic thread.
    Tapped,
    /// A clearance hole with a counterbore for a socket head.
    Counterbore,
    /// A clearance hole with a 90° countersink.
    Countersink,
}

/// How tight a clearance hole is (ISO 273).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    /// Close.
    Close,
    /// Medium.
    #[default]
    Normal,
    /// Loose.
    Loose,
}

/// Sizes of one metric screw, mm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricSize {
    /// Nominal diameter.
    pub d: f64,
    /// Coarse pitch.
    pub pitch: f64,
    /// Tap drill.
    pub tap: f64,
    /// Clearance: close, normal, loose.
    pub clearance: [f64; 3],
    /// Counterbore diameter and depth for a socket head cap screw.
    pub counterbore: (f64, f64),
    /// Countersink diameter for a 90° countersunk head.
    pub countersink: f64,
}

/// The metric sizes Planyrith knows (coarse threads, ISO 273 clearances).
pub const METRIC: [(&str, MetricSize); 12] = [
    (
        "M2",
        MetricSize {
            d: 2.0,
            pitch: 0.4,
            tap: 1.6,
            clearance: [2.2, 2.4, 2.6],
            counterbore: (4.3, 2.0),
            countersink: 4.4,
        },
    ),
    (
        "M2.5",
        MetricSize {
            d: 2.5,
            pitch: 0.45,
            tap: 2.05,
            clearance: [2.7, 2.9, 3.1],
            counterbore: (5.0, 2.5),
            countersink: 5.5,
        },
    ),
    (
        "M3",
        MetricSize {
            d: 3.0,
            pitch: 0.5,
            tap: 2.5,
            clearance: [3.2, 3.4, 3.6],
            counterbore: (6.5, 3.3),
            countersink: 6.3,
        },
    ),
    (
        "M4",
        MetricSize {
            d: 4.0,
            pitch: 0.7,
            tap: 3.3,
            clearance: [4.3, 4.5, 4.8],
            counterbore: (8.0, 4.4),
            countersink: 9.4,
        },
    ),
    (
        "M5",
        MetricSize {
            d: 5.0,
            pitch: 0.8,
            tap: 4.2,
            clearance: [5.3, 5.5, 5.8],
            counterbore: (10.0, 5.4),
            countersink: 10.4,
        },
    ),
    (
        "M6",
        MetricSize {
            d: 6.0,
            pitch: 1.0,
            tap: 5.0,
            clearance: [6.4, 6.6, 7.0],
            counterbore: (11.0, 6.4),
            countersink: 12.6,
        },
    ),
    (
        "M8",
        MetricSize {
            d: 8.0,
            pitch: 1.25,
            tap: 6.8,
            clearance: [8.4, 9.0, 10.0],
            counterbore: (15.0, 8.6),
            countersink: 17.3,
        },
    ),
    (
        "M10",
        MetricSize {
            d: 10.0,
            pitch: 1.5,
            tap: 8.5,
            clearance: [10.5, 11.0, 12.0],
            counterbore: (18.0, 10.8),
            countersink: 20.0,
        },
    ),
    (
        "M12",
        MetricSize {
            d: 12.0,
            pitch: 1.75,
            tap: 10.2,
            clearance: [13.0, 13.5, 14.5],
            counterbore: (20.0, 13.0),
            countersink: 24.0,
        },
    ),
    (
        "M16",
        MetricSize {
            d: 16.0,
            pitch: 2.0,
            tap: 14.0,
            clearance: [17.0, 17.5, 18.5],
            counterbore: (26.0, 17.5),
            countersink: 30.0,
        },
    ),
    (
        "M20",
        MetricSize {
            d: 20.0,
            pitch: 2.5,
            tap: 17.5,
            clearance: [21.0, 22.0, 24.0],
            counterbore: (33.0, 21.5),
            countersink: 36.0,
        },
    ),
    (
        "M24",
        MetricSize {
            d: 24.0,
            pitch: 3.0,
            tap: 21.0,
            clearance: [25.0, 26.0, 28.0],
            counterbore: (40.0, 25.5),
            countersink: 44.0,
        },
    ),
];

/// The sizes of a metric screw by name (`M8`).
pub fn metric(size: &str) -> Option<MetricSize> {
    let size = size.trim().to_ascii_uppercase();
    METRIC.iter().find(|(n, _)| *n == size).map(|(_, s)| *s)
}

/// `hole` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HoleParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The face to drill into (one face of a closed solid).
    pub face: Query,
    /// The hole's center (projected onto the face's plane).
    #[schemars(with = "PointSchema")]
    pub center: DVec3,
    /// The kind of hole.
    #[serde(default)]
    pub kind: HoleKind,
    /// The screw size (`M2` to `M24`); needed by every kind but `simple`.
    #[serde(default)]
    pub size: Option<String>,
    /// A simple hole's diameter (instead of a size).
    #[serde(default)]
    pub diameter: Option<Length>,
    /// How deep (through everything when absent).
    #[serde(default)]
    pub depth: Option<Length>,
    /// Clearance fit.
    #[serde(default)]
    pub fit: Fit,
    /// Straight pieces around the hole.
    #[serde(default = "segments")]
    pub segments: u32,
}

fn segments() -> u32 {
    32
}

/// A hole's profile as (radius, depth) points and its thread.
type HoleProfile = (Vec<(f64, f64)>, Option<String>);

/// The hole's profile as (radius, depth) points from above the face down,
/// for a hole of `depth`, starting `lift` above the face.
fn hole_profile(p: &HoleParams, depth: f64, lift: f64) -> Result<HoleProfile, CommandError> {
    let size = p.size.as_deref().map(|s| {
        metric(s).ok_or_else(|| {
            CommandError::invalid_param("size", format!("unknown size `{s}`; use M2 to M24"))
        })
    });
    let size = size.transpose()?;
    let need = || {
        size.ok_or_else(|| CommandError::invalid_param("size", "this kind of hole needs a size"))
    };
    let fit = match p.fit {
        Fit::Close => 0,
        Fit::Normal => 1,
        Fit::Loose => 2,
    };
    let mut thread = None;
    let points = match p.kind {
        HoleKind::Simple => {
            let d = match (p.diameter, size) {
                (Some(d), _) => d.as_mm(),
                (None, Some(s)) => s.d,
                (None, None) => {
                    return Err(CommandError::invalid_param(
                        "diameter",
                        "give a diameter or a size",
                    ));
                }
            };
            vec![
                (0.0, -lift),
                (d / 2.0, -lift),
                (d / 2.0, depth),
                (0.0, depth),
            ]
        }
        HoleKind::Clearance => {
            let r = need()?.clearance[fit] / 2.0;
            vec![(0.0, -lift), (r, -lift), (r, depth), (0.0, depth)]
        }
        HoleKind::Tapped => {
            let s = need()?;
            let r = s.tap / 2.0;
            thread = Some(format!(
                "{}x{}",
                p.size
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_uppercase(),
                s.pitch
            ));
            vec![(0.0, -lift), (r, -lift), (r, depth), (0.0, depth)]
        }
        HoleKind::Counterbore => {
            let s = need()?;
            let r = s.clearance[fit] / 2.0;
            let (cd, cdepth) = s.counterbore;
            if cdepth >= depth {
                return Err(CommandError::OutOfRange {
                    name: "depth".into(),
                    reason: "the hole must be deeper than its counterbore".into(),
                });
            }
            vec![
                (0.0, -lift),
                (cd / 2.0, -lift),
                (cd / 2.0, cdepth),
                (r, cdepth),
                (r, depth),
                (0.0, depth),
            ]
        }
        HoleKind::Countersink => {
            let s = need()?;
            let r = s.clearance[fit] / 2.0;
            let top = s.countersink / 2.0;
            // 90° included: the radius shrinks by the depth.
            let bottom = top - r;
            if bottom >= depth {
                return Err(CommandError::OutOfRange {
                    name: "depth".into(),
                    reason: "the hole must be deeper than its countersink".into(),
                });
            }
            vec![
                (0.0, -lift),
                (top + lift, -lift),
                (r, bottom),
                (r, depth),
                (0.0, depth),
            ]
        }
    };
    Ok((points, thread))
}

/// The `hole` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Hole;

impl Command for Hole {
    type Params = HoleParams;
    type Output = SolidFeatureOutput;

    fn name(&self) -> &'static str {
        "hole"
    }

    fn validate(&self, _: &Document, p: &HoleParams) -> Result<(), CommandError> {
        if p.depth.is_some_and(|d| !positive(d.as_mm())) {
            return Err(CommandError::OutOfRange {
                name: "depth".into(),
                reason: "must be positive".into(),
            });
        }
        if p.diameter.is_some_and(|d| !positive(d.as_mm())) {
            return Err(CommandError::OutOfRange {
                name: "diameter".into(),
                reason: "must be positive".into(),
            });
        }
        if !(8..=256).contains(&p.segments) {
            return Err(CommandError::OutOfRange {
                name: "segments".into(),
                reason: "must be between 8 and 256".into(),
            });
        }
        hole_profile(p, 10.0, 1.0).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: HoleParams,
    ) -> Result<SolidFeatureOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let face = one_face(tx, &target, "face", &p.face)?;
        let (_, solid) = shell_of(tx, &target, face)?;
        let plane = {
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
                .mesh();
            mesh.face_ref(face)?.plane
        };
        let n = plane.normal;
        let c = target.local.transform_point3(p.center);
        let c = c - n * (n.dot(c) + plane.d);
        let axis = -n;
        let depth = p.depth.map_or(2.0 * bounds_diagonal(&solid), |d| d.as_mm());
        let lift = 1.0;
        let (profile, thread) = hole_profile(&p, depth, lift)?;
        let u = across(axis);
        let ring: Vec<DVec3> = profile
            .iter()
            .map(|(r, z)| c + u * *r + axis * *z)
            .collect();
        let cutter = revolve(
            &[ring],
            c,
            axis,
            std::f64::consts::TAU,
            p.segments,
            FaceSides::default(),
            tx.tolerances(),
        )?;
        let before = tx.created().len();
        let results = combine(tx, &target, face, BooleanOp::Subtract, &cutter)?;
        let (faces, volume_mm3) = summary(&results)?;
        // The cosmetic thread: on the faces of the tapped bore.
        if let (Some(t), Some(size)) = (&thread, p.size.as_deref().and_then(metric)) {
            let r = size.tap / 2.0;
            let created: Vec<Pid> = tx.created()[before..].to_vec();
            let mut bore = Vec::new();
            {
                let entities = tx
                    .document()
                    .container(target.container)
                    .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
                let mesh = entities.mesh();
                for pid in created {
                    let Some(EntityId::Face(f)) = entities.id_of(pid) else {
                        continue;
                    };
                    let points: Vec<DVec3> = mesh
                        .face_vertices(f)?
                        .into_iter()
                        .map(|v: VertexId| mesh.position(v))
                        .collect::<Result<_, _>>()?;
                    let on_bore = points.iter().all(|q| {
                        let w = *q - c;
                        let off = (w - axis * w.dot(axis)).length();
                        (off - r).abs() <= 0.02 * r.max(1.0)
                    });
                    if on_bore {
                        bore.push(EntityId::Face(f));
                    }
                }
            }
            for id in bore {
                tx.set_attribute(
                    AttrTarget::Entity {
                        container: target.container,
                        id,
                    },
                    "thread",
                    "designation",
                    Some(Value::Str(t.clone())),
                )?;
            }
        }
        Ok(SolidFeatureOutput {
            faces,
            volume_mm3,
            groups: Vec::new(),
            thread,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("hole", "Hole")
    }
}

// ----- shell ----------------------------------------------------------------------------

/// `shell` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShellParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The faces to open (all of one closed solid).
    pub faces: Query,
    /// Wall thickness, inward.
    pub thickness: Length,
}

/// The `shell` command: hollows a solid, opening the named faces.
#[derive(Debug, Default, Clone, Copy)]
pub struct Shell;

impl Command for Shell {
    type Params = ShellParams;
    type Output = SolidFeatureOutput;

    fn name(&self) -> &'static str {
        "shell"
    }

    fn validate(&self, _: &Document, p: &ShellParams) -> Result<(), CommandError> {
        if !positive(p.thickness.as_mm()) {
            return Err(CommandError::OutOfRange {
                name: "thickness".into(),
                reason: "must be positive".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ShellParams,
    ) -> Result<SolidFeatureOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let open = faces_of(tx, &target, "faces", &p.faces)?;
        let seed = *open
            .iter()
            .next()
            .ok_or_else(|| CommandError::Internal("no face".into()))?;
        let (faces, _) = shell_of(tx, &target, seed)?;
        if open.iter().any(|f| !faces.contains(f)) {
            return Err(CommandError::invalid_param(
                "faces",
                "the faces must belong to one solid",
            ));
        }
        let keep: BTreeSet<FaceId> = faces
            .iter()
            .copied()
            .filter(|f| !open.contains(f))
            .collect();
        if keep.is_empty() {
            return Err(CommandError::invalid_param(
                "faces",
                "leave at least one face",
            ));
        }
        let walls = {
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
                .mesh();
            thicken(mesh, &keep, -p.thickness.as_mm(), false, tx.tolerances())?
        };
        let results = vec![walls];
        replace_shell(tx, &target, &faces, &results)?;
        let (faces, volume_mm3) = summary(&results)?;
        Ok(SolidFeatureOutput {
            faces,
            volume_mm3,
            groups: Vec::new(),
            thread: None,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("shell", "Shell")
    }
}

// ----- draft ----------------------------------------------------------------------------

fn up() -> DVec3 {
    DVec3::Z
}

/// `draft` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DraftParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The faces to slope.
    pub faces: Query,
    /// The draft angle; positive tapers the faces inward along the pull.
    pub angle: Angle,
    /// The direction a mold opens (+Z).
    #[serde(default = "up")]
    #[schemars(with = "VectorSchema")]
    pub pull: DVec3,
    /// A point of the neutral plane, where the faces keep their place
    /// (the origin).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub neutral: Option<DVec3>,
}

/// The `draft` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Draft;

impl Command for Draft {
    type Params = DraftParams;
    type Output = SolidFeatureOutput;

    fn name(&self) -> &'static str {
        "draft"
    }

    fn validate(&self, _: &Document, p: &DraftParams) -> Result<(), CommandError> {
        let a = p.angle.as_radians();
        if !(a.is_finite() && a.abs() < 89f64.to_radians()) {
            return Err(CommandError::OutOfRange {
                name: "angle".into(),
                reason: "must be less than 89° either way".into(),
            });
        }
        if p.pull.try_normalize().is_none() {
            return Err(CommandError::invalid_param("pull", "must be non-zero"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: DraftParams,
    ) -> Result<SolidFeatureOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let faces = faces_of(tx, &target, "faces", &p.faces)?;
        let pull = target.local.transform_vector3(p.pull).normalize();
        let neutral = target
            .local
            .transform_point3(p.neutral.unwrap_or(DVec3::ZERO));
        let tan = libm::tan(p.angle.as_radians());
        let mut moves: std::collections::BTreeMap<VertexId, DVec3> = Default::default();
        {
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
                .mesh();
            let mut shift: std::collections::BTreeMap<VertexId, DVec3> = Default::default();
            for &f in &faces {
                let n = mesh.face_ref(f)?.plane.normal;
                let side = n - pull * n.dot(pull);
                let Some(side) = side.try_normalize() else {
                    return Err(CommandError::NotApplicable(
                        "a face square to the pull direction cannot be drafted".into(),
                    ));
                };
                for v in mesh.face_vertices(f)? {
                    let h = (mesh.position(v)? - neutral).dot(pull);
                    *shift.entry(v).or_insert(DVec3::ZERO) -= side * h * tan;
                }
            }
            for (v, s) in shift {
                if s.length() > 1e-12 {
                    moves.insert(v, mesh.position(v)? + s);
                }
            }
        }
        tx.with_mesh(target.container, |m, tol, d| {
            m.move_vertices(
                &moves,
                TransformOptions {
                    auto_fold: true,
                    check_intersections: false,
                },
                tol,
                d,
            )
        })?;
        let seed = *faces
            .iter()
            .next()
            .ok_or_else(|| CommandError::Internal("no face".into()))?;
        let (count, volume_mm3) = match tx
            .document()
            .container(target.container)
            .map(|c| c.mesh().face(seed).is_some())
        {
            Some(true) => match shell_of(tx, &target, seed) {
                Ok((faces, solid)) => (faces.len(), solid.volume()?),
                Err(_) => (0, 0.0),
            },
            _ => (0, 0.0),
        };
        Ok(SolidFeatureOutput {
            faces: count,
            volume_mm3,
            groups: Vec::new(),
            thread: None,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("draft", "Draft")
    }
}

// ----- split-solid ----------------------------------------------------------------------

/// `split-solid` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SplitParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// A face of the solid to split.
    pub face: Query,
    /// A point of the cutting plane.
    #[schemars(with = "PointSchema")]
    pub point: DVec3,
    /// The cutting plane's normal.
    #[schemars(with = "VectorSchema")]
    pub normal: DVec3,
}

/// A box on the `n` side of the plane through `p`, covering `size`.
fn half_space(
    p: DVec3,
    n: DVec3,
    size: f64,
    tol: &crate::tolerance::Tolerances,
) -> Result<HalfEdgeMesh, GeometryError> {
    let u = across(n);
    let v = n.cross(u);
    let square = |o: DVec3| {
        vec![
            o - u * size - v * size,
            o + u * size - v * size,
            o + u * size + v * size,
            o - u * size + v * size,
        ]
    };
    loft(
        &[square(p), square(p + n * size)],
        FaceSides::default(),
        tol,
    )
}

/// The `split-solid` command: two groups, one each side of a plane.
#[derive(Debug, Default, Clone, Copy)]
pub struct SplitSolid;

impl Command for SplitSolid {
    type Params = SplitParams;
    type Output = SolidFeatureOutput;

    fn name(&self) -> &'static str {
        "split-solid"
    }

    fn validate(&self, _: &Document, p: &SplitParams) -> Result<(), CommandError> {
        if p.normal.try_normalize().is_none() {
            return Err(CommandError::invalid_param("normal", "must be non-zero"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SplitParams,
    ) -> Result<SolidFeatureOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let face = one_face(tx, &target, "face", &p.face)?;
        let (faces, solid) = shell_of(tx, &target, face)?;
        let point = target.local.transform_point3(p.point);
        let n = target.local.transform_vector3(p.normal).normalize();
        let tol = *tx.tolerances();
        let size = 4.0 * bounds_diagonal(&solid)
            + solid
                .vertices()
                .map(|(_, v)| (v.position - point).length())
                .fold(0.0, f64::max);
        let tool = half_space(point, n, size, &tol)?;
        let above = boolean(
            BooleanOp::Intersect,
            &solid,
            &DAffine3::IDENTITY,
            &tool,
            &DAffine3::IDENTITY,
            &tol,
        )?;
        let below = boolean(
            BooleanOp::Subtract,
            &solid,
            &DAffine3::IDENTITY,
            &tool,
            &DAffine3::IDENTITY,
            &tol,
        )?;
        if above.solids.is_empty() || below.solids.is_empty() {
            return Err(CommandError::NotApplicable(
                "the plane does not cross the solid".into(),
            ));
        }
        replace_shell(tx, &target, &faces, &[])?;
        let mut groups = Vec::new();
        let mut all = Vec::new();
        for (i, piece) in above.solids.into_iter().chain(below.solids).enumerate() {
            all.push(piece.clone());
            let name = format!("Split {}", i + 1);
            let entities = Entities::from_mesh(piece, || tx.allocate_pid());
            let uuid = definition_uuid(tx.document(), &name, &entities);
            let mut def = ComponentDefinition::new(uuid, name.clone());
            def.is_internal_group = true;
            def.entities = entities;
            let definition = tx.insert_definition(def)?;
            let (_, pid) = tx.insert_entity(
                target.container,
                EntityRecord::Group(Group {
                    definition,
                    transform: DAffine3::IDENTITY,
                    name,
                }),
            )?;
            groups.push(pid);
        }
        let (faces, volume_mm3) = summary(&all)?;
        Ok(SolidFeatureOutput {
            faces,
            volume_mm3,
            groups,
            thread: None,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("split-solid", "Split Solid")
    }
}

// ----- dome -----------------------------------------------------------------------------

fn layers() -> u32 {
    8
}

/// `dome` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DomeParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The planar face to dome (no holes).
    pub face: Query,
    /// The dome's height above the face.
    pub height: Length,
    /// Rings from the face to the top.
    #[serde(default = "layers")]
    pub layers: u32,
}

/// The `dome` command: an elliptic cap on a face, joined to its solid.
#[derive(Debug, Default, Clone, Copy)]
pub struct Dome;

impl Command for Dome {
    type Params = DomeParams;
    type Output = SolidFeatureOutput;

    fn name(&self) -> &'static str {
        "dome"
    }

    fn validate(&self, _: &Document, p: &DomeParams) -> Result<(), CommandError> {
        if !positive(p.height.as_mm()) {
            return Err(CommandError::OutOfRange {
                name: "height".into(),
                reason: "must be positive".into(),
            });
        }
        if !(2..=64).contains(&p.layers) {
            return Err(CommandError::OutOfRange {
                name: "layers".into(),
                reason: "must be between 2 and 64".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: DomeParams,
    ) -> Result<SolidFeatureOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let face = one_face(tx, &target, "face", &p.face)?;
        let (outline, n, sides) = {
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
                .mesh();
            let f = mesh.face_ref(face)?;
            if !f.holes.is_empty() {
                return Err(CommandError::NotApplicable(
                    "a face with holes cannot be domed".into(),
                ));
            }
            (
                mesh.loop_positions(f.outer)?,
                f.plane.normal,
                f.sides.clone(),
            )
        };
        let c = outline.iter().copied().sum::<DVec3>() / outline.len() as f64;
        let h = p.height.as_mm();
        let mut profiles = Vec::new();
        for k in 0..=p.layers {
            let theta = std::f64::consts::FRAC_PI_2 * f64::from(k) / f64::from(p.layers);
            // A small flat top closes the dome.
            let s = libm::cos(theta).max(0.05);
            let lift = h * libm::sin(theta);
            profiles.push(
                outline
                    .iter()
                    .map(|q| c + (*q - c) * s + n * lift)
                    .collect::<Vec<_>>(),
            );
        }
        let cap = loft(&profiles, sides, tx.tolerances())?;
        let results = combine(tx, &target, face, BooleanOp::Union, &cap)?;
        let (faces, volume_mm3) = summary(&results)?;
        Ok(SolidFeatureOutput {
            faces,
            volume_mm3,
            groups: Vec::new(),
            thread: None,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("dome", "Dome")
    }
}

// ----- rib ------------------------------------------------------------------------------

/// `rib` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RibParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The rib's line: one edge of the edit context.
    pub edge: Query,
    /// A face of the solid the rib joins.
    pub solid: Query,
    /// The rib's thickness, centered on the line.
    pub thickness: Length,
    /// How far the rib extends from the line.
    pub height: Length,
    /// The direction it extends in (+Z).
    #[serde(default = "up")]
    #[schemars(with = "VectorSchema")]
    pub direction: DVec3,
}

/// The `rib` command: a wall along a line, joined to a solid.
#[derive(Debug, Default, Clone, Copy)]
pub struct Rib;

impl Command for Rib {
    type Params = RibParams;
    type Output = SolidFeatureOutput;

    fn name(&self) -> &'static str {
        "rib"
    }

    fn validate(&self, _: &Document, p: &RibParams) -> Result<(), CommandError> {
        if !(p.thickness.as_mm() > 0.0 && p.height.as_mm() > 0.0) {
            return Err(CommandError::OutOfRange {
                name: "thickness".into(),
                reason: "thickness and height must be positive".into(),
            });
        }
        if p.direction.try_normalize().is_none() {
            return Err(CommandError::invalid_param("direction", "must be non-zero"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: RibParams,
    ) -> Result<SolidFeatureOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let edges: Vec<_> = targets_of(tx, &target, "edge", &p.edge)?
            .into_iter()
            .filter_map(|id| match id {
                EntityId::Edge(e) => Some(e),
                _ => None,
            })
            .collect();
        let [edge] = edges.as_slice() else {
            return Err(CommandError::invalid_param(
                "edge",
                "must name exactly one edge",
            ));
        };
        let seg = tx
            .document()
            .container(target.container)
            .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
            .mesh()
            .edge_segment(*edge)?;
        let face = one_face(tx, &target, "solid", &p.solid)?;
        let d = target.local.transform_vector3(p.direction).normalize();
        let along = seg.b - seg.a;
        let Some(w) = along.cross(d).try_normalize() else {
            return Err(CommandError::NotApplicable(
                "the direction must not run along the line".into(),
            ));
        };
        let w = w * p.thickness.as_mm() / 2.0;
        let base = vec![seg.a - w, seg.b - w, seg.b + w, seg.a + w];
        let top: Vec<DVec3> = base.iter().map(|q| *q + d * p.height.as_mm()).collect();
        let wall = loft(&[base, top], FaceSides::default(), tx.tolerances())?;
        // The line gives way to the wall.
        tx.with_mesh(target.container, |m, tol, dl| {
            if m.edge(*edge).is_some() && m.edge_faces(*edge)? == [None, None] {
                m.erase_edge(*edge, tol, dl)?;
            }
            Ok(())
        })?;
        let results = combine(tx, &target, face, BooleanOp::Union, &wall)?;
        let (faces, volume_mm3) = summary(&results)?;
        Ok(SolidFeatureOutput {
            faces,
            volume_mm3,
            groups: Vec::new(),
            thread: None,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("rib", "Rib")
    }
}

/// Registers the solid features.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(Hole);
    registry.register(Shell);
    registry.register(Draft);
    registry.register(SplitSolid);
    registry.register(Dome);
    registry.register(Rib);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn block(size: [f64; 3]) -> Engine {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        e.execute_erased(
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [size[0], size[1], 0] }),
        )
        .unwrap();
        e.execute_erased(
            "push-pull",
            json!({ "face": { "Query": "face" }, "distance": size[2] }),
        )
        .unwrap();
        e
    }

    fn volume(e: &Engine) -> f64 {
        e.document().model.mesh().volume().unwrap()
    }

    fn top(z: f64) -> Value {
        json!(format!("face & plane:xy@{z}"))
    }

    #[test]
    fn holes_of_each_kind() {
        let mut e = block([100.0, 100.0, 20.0]);
        let out = e
            .execute_erased(
                "hole",
                json!({ "face": top(20.0), "center": [50, 50, 20], "kind": "tapped",
                        "size": "M8", "segments": 64 }),
            )
            .unwrap();
        assert_eq!(out["thread"], "M8x1.25");
        // A 6.8 mm bore through 20 mm, as a 64-gon.
        let r = 3.4f64;
        let gon = 0.5 * 64.0 * r * r * libm::sin(std::f64::consts::TAU / 64.0);
        assert!((volume(&e) - (200_000.0 - gon * 20.0)).abs() < 1e-3);
        assert!(e.document().model.mesh().is_solid());
        // The bore carries the thread.
        let threaded = e
            .document()
            .model
            .iter()
            .filter(|id| {
                e.document()
                    .model
                    .attrs(*id)
                    .and_then(|a| a.get("thread", "designation").cloned())
                    .is_some()
            })
            .count();
        assert!(threaded >= 64, "{threaded}");
        for kind in ["counterbore", "countersink", "clearance"] {
            let mut e = block([100.0, 100.0, 20.0]);
            let before = volume(&e);
            e.execute_erased(
                "hole",
                json!({ "face": top(20.0), "center": [30, 30, 20], "kind": kind, "size": "M6" }),
            )
            .unwrap();
            assert!(
                volume(&e) < before && e.document().model.mesh().is_solid(),
                "{kind}"
            );
        }
        // A blind hole goes only so deep.
        let mut e = block([100.0, 100.0, 20.0]);
        e.execute_erased(
            "hole",
            json!({ "face": top(20.0), "center": [50, 50, 20], "diameter": 10, "depth": 5 }),
        )
        .unwrap();
        assert!(e.document().model.mesh().is_solid());
        assert!(
            e.execute_erased(
                "hole",
                json!({ "face": top(20.0), "center": [5, 5, 20], "kind": "tapped" })
            )
            .is_err()
        );
    }

    #[test]
    fn a_shelled_box_and_a_drafted_block() {
        let mut e = block([100.0, 100.0, 100.0]);
        e.execute_erased("shell", json!({ "faces": top(100.0), "thickness": 10 }))
            .unwrap();
        // Walls and floor: 1e6 minus the 80 × 80 × 90 hollow.
        assert!((volume(&e) - (1_000_000.0 - 80.0 * 80.0 * 90.0)).abs() < 1e-3);
        assert!(e.document().model.mesh().is_solid());

        let mut e = block([100.0, 100.0, 100.0]);
        e.execute_erased(
            "draft",
            json!({ "faces": "face & normal:x", "angle": 5f64.to_radians() }),
        )
        .unwrap();
        // The +X side leans in: a prism of trapezoids.
        let lean = 100.0 * libm::tan(5f64.to_radians());
        let expect = 100.0 * (100.0 * 100.0 - 0.5 * lean * 100.0);
        assert!((volume(&e) - expect).abs() < 1e-3, "{}", volume(&e));
        assert!(e.document().model.mesh().is_solid());
    }

    #[test]
    fn split_dome_and_rib() {
        let mut e = block([100.0, 60.0, 40.0]);
        let out = e
            .execute_erased(
                "split-solid",
                json!({ "face": top(40.0), "point": [30, 0, 0], "normal": [1, 0, 0] }),
            )
            .unwrap();
        assert_eq!(out["groups"].as_array().unwrap().len(), 2);
        assert!((out["volume_mm3"].as_f64().unwrap() - 240_000.0).abs() < 1e-3);
        assert_eq!(e.document().model.mesh().face_count(), 0);

        let mut e = block([100.0, 100.0, 20.0]);
        e.execute_erased("dome", json!({ "face": top(20.0), "height": 30 }))
            .unwrap();
        assert!(volume(&e) > 200_000.0 && e.document().model.mesh().is_solid());

        let mut e = block([200.0, 100.0, 10.0]);
        e.execute_erased("line", json!({ "points": [[20, 50, 10], [180, 50, 10]] }))
            .unwrap();
        let out = e
            .execute_erased(
                "rib",
                json!({ "edge": "edge & length>150 & length<170", "solid": "face & plane:xy@0",
                        "thickness": 4, "height": 30 }),
            )
            .unwrap();
        assert!(
            (out["volume_mm3"].as_f64().unwrap() - (200_000.0 + 160.0 * 4.0 * 30.0)).abs() < 1e-3
        );
        assert!(e.document().model.mesh().is_solid());
    }
}
