//! Implements docs/05-modeling-tools.md §8.18: `mold-split`.

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::targets_of;
use crate::commands::fabrication::{new_group, store};
use crate::commands::params::{PointSchema, VectorSchema, check_point, target_container};
use crate::commands::routing::local_mesh;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Document, Query};
use crate::geom::mesh::transform_indexed;
use crate::geom::{HalfEdgeMesh, mold};
use crate::ids::{EntityId, Pid};
use crate::math::Plane;
use crate::units::Length;

fn margin() -> Length {
    Length::mm(10.0)
}
fn factor() -> f64 {
    1.0
}

/// `mold-split` parameters, in world millimeters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MoldParams {
    /// One solid group or component, without nested contents.
    pub body: Query,
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Point of the planar division, in world millimeters.
    #[schemars(with = "PointSchema")]
    pub point: DVec3,
    /// Plane normal, from core toward cavity.
    #[schemars(with = "VectorSchema")]
    pub normal: DVec3,
    /// Blank margin on all six sides, mm (above the planarity tolerance).
    #[serde(default = "margin")]
    pub margin: Length,
    /// Linear compensation multiplier (1.02 enlarges the part by 2%).
    #[serde(default = "factor")]
    pub shrink_factor: f64,
    /// World scaling origin; default is the part's world bounding-box center.
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub origin: Option<DVec3>,
    /// Steps of the parting surface (milestone 1.40): beyond each `at`
    /// along `step_direction` from `point`, the surface stands `offset`
    /// above the plane (toward the cavity).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<MoldStep>,
    /// Along which steps are placed, in the plane (world red, else green,
    /// projected into the plane).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<VectorSchema>")]
    pub step_direction: Option<DVec3>,
    /// A freeform parting surface instead of the plane (milestone 1.48):
    /// faces, or groups holding faces, seen from one side along their mean
    /// normal (turned to agree with `normal`), spanning the blank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<Query>,
    /// Cut side actions out of the halves for undercuts that a sideways
    /// pull frees (milestone 1.48).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub side_actions: bool,
}

/// One step of a stepped parting surface.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MoldStep {
    /// Where the step stands, along the step direction from the point.
    pub at: Length,
    /// The surface's height beyond it, over the plane.
    pub offset: Length,
}

/// The parting surface a request describes, in world coordinates.
fn surface_of(
    point: DVec3,
    normal: DVec3,
    steps: &[MoldStep],
    direction: Option<DVec3>,
) -> Result<mold::Stepped, CommandError> {
    let n = normal
        .try_normalize()
        .ok_or_else(|| CommandError::invalid_param("normal", "must be finite and nonzero"))?;
    let in_plane = |d: DVec3| (d - n * d.dot(n)).try_normalize();
    let axis = match direction {
        Some(d) => in_plane(d).ok_or_else(|| {
            CommandError::invalid_param("step_direction", "must not be along the normal")
        })?,
        None => in_plane(DVec3::X)
            .filter(|_| n.dot(DVec3::X).abs() < 0.9)
            .or_else(|| in_plane(DVec3::Y))
            .ok_or_else(|| CommandError::Internal("no direction in the plane".into()))?,
    };
    let mut steps: Vec<(f64, f64)> = steps
        .iter()
        .map(|s| (s.at.as_mm(), s.offset.as_mm()))
        .collect();
    steps.sort_by(|a, b| a.0.total_cmp(&b.0));
    if steps.windows(2).any(|w| (w[1].0 - w[0].0).abs() < 1e-9) {
        return Err(CommandError::invalid_param(
            "steps",
            "two steps stand at one place",
        ));
    }
    Ok(mold::Stepped {
        origin: point,
        normal: n,
        axis,
        steps,
    })
}

/// The freeform parting surface `q` names, in world coordinates, facing
/// along `normal` (milestone 1.48).
fn freeform_of(
    tx: &Transaction<'_>,
    target: &crate::commands::params::Target,
    q: &Query,
    normal: DVec3,
) -> Result<mold::Freeform, CommandError> {
    let faces = crate::commands::surfaces::cutter_faces(tx, target, "surface", q)?;
    let world: Vec<Vec<Vec<DVec3>>> = faces
        .into_iter()
        .map(|f| {
            f.into_iter()
                .map(|r| {
                    r.into_iter()
                        .map(|p| target.world.transform_point3(p))
                        .collect()
                })
                .collect()
        })
        .collect();
    Ok(mold::Freeform::new(world, normal, tx.tolerances())?)
}

/// The body's definition and world placement.
fn body_of(
    tx: &mut Transaction<'_>,
    common: &CommonParams,
    body: &Query,
) -> Result<
    (
        crate::ids::DefinitionId,
        glam::DAffine3,
        crate::commands::params::Target,
    ),
    CommandError,
> {
    let target = target_container(tx, common)?;
    let ids = targets_of(tx, &target, "body", body)?;
    let [id] = ids.as_slice() else {
        return Err(CommandError::invalid_param(
            "body",
            "select exactly one solid group or component",
        ));
    };
    if !matches!(id, EntityId::Group(_) | EntityId::Instance(_)) {
        return Err(CommandError::invalid_param(
            "body",
            "select a solid group or component",
        ));
    }
    let here = tx
        .document()
        .container(target.container)
        .ok_or_else(|| CommandError::Internal("context vanished".into()))?;
    let d = here
        .definition_of(*id)
        .ok_or_else(|| CommandError::Internal("body definition vanished".into()))?;
    let definition = tx
        .document()
        .definition(d)
        .ok_or_else(|| CommandError::Internal("body definition vanished".into()))?;
    if definition.entities.groups().next().is_some()
        || definition.entities.instances().next().is_some()
        || !definition.entities.mesh().is_solid()
    {
        return Err(CommandError::NotApplicable(
            "the body must be a closed mesh without nested groups or components".into(),
        ));
    }
    let world = target.world
        * here
            .transform_of(*id)
            .ok_or_else(|| CommandError::Internal("body transform vanished".into()))?;
    Ok((d, world, target))
}

/// The four generated groups and their world measurements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MoldOutput {
    /// Negative-side mold half.
    pub core: Pid,
    /// Positive-side mold half.
    pub cavity: Pid,
    /// Section wire group.
    pub parting_line: Pid,
    /// Planar surface group.
    pub parting_surface: Pid,
    /// Core material volume, mm³.
    pub core_volume_mm3: f64,
    /// Cavity material volume, mm³.
    pub cavity_volume_mm3: f64,
    /// Compensated part volume, mm³.
    pub compensated_volume_mm3: f64,
    /// Section perimeter, mm.
    pub parting_length_mm: f64,
    /// Whether a boolean used the optional B-rep backend.
    pub fallback_used: bool,
    /// Faces of the part that cannot be released (see `mold-undercuts`).
    #[serde(default)]
    pub undercuts: usize,
    /// Their area, mm².
    #[serde(default)]
    pub undercut_area_mm2: f64,
    /// Side action groups, when asked for (milestone 1.48).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub side_actions: Vec<Pid>,
    /// The direction each side action slides out, world.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub side_directions: Vec<[f64; 3]>,
    /// Undercut faces no side action frees (the undercuts without them).
    #[serde(default)]
    pub undercuts_left: usize,
}

/// Makes a planar mold without modifying its source part.
#[derive(Debug, Default, Clone, Copy)]
pub struct MoldSplit;

impl Command for MoldSplit {
    type Params = MoldParams;
    type Output = MoldOutput;

    fn name(&self) -> &'static str {
        "mold-split"
    }

    fn validate(&self, doc: &Document, p: &MoldParams) -> Result<(), CommandError> {
        check_point("point", p.point)?;
        if let Some(origin) = p.origin {
            check_point("origin", origin)?;
        }
        if !p.normal.is_finite() || p.normal.try_normalize().is_none() {
            return Err(CommandError::invalid_param(
                "normal",
                "must be finite and nonzero",
            ));
        }
        if !p.margin.as_mm().is_finite() || p.margin.as_mm() <= doc.info.tolerances.planarity {
            return Err(CommandError::invalid_param(
                "margin",
                "must exceed the planarity tolerance",
            ));
        }
        if !p.shrink_factor.is_finite() || p.shrink_factor <= 0.0 {
            return Err(CommandError::invalid_param(
                "shrink_factor",
                "must be positive and finite",
            ));
        }
        if p.surface.is_some() && !p.steps.is_empty() {
            return Err(CommandError::invalid_param(
                "surface",
                "give a surface or steps, not both",
            ));
        }
        Ok(())
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: MoldParams) -> Result<MoldOutput, CommandError> {
        let (d, world, target) = body_of(tx, &p.common, &p.body)?;
        let tol = *tx.tolerances();
        let source_mesh = tx
            .document()
            .definition(d)
            .map(|def| def.entities.mesh().clone())
            .ok_or_else(|| CommandError::Internal("body definition vanished".into()))?;
        let (source, _) = source_mesh.to_indexed();
        let part = HalfEdgeMesh::from_indexed(&transform_indexed(&source, &world), &tol)?;
        let surface = surface_of(p.point, p.normal, &p.steps, p.step_direction)?;
        let freeform = match &p.surface {
            Some(q) => Some(freeform_of(tx, &target, q, p.normal)?),
            None => None,
        };
        let origin = p.origin.unwrap_or_else(|| part.aabb().center());
        let mut result = if let Some(f) = &freeform {
            mold::split_freeform(&part, f, p.margin.as_mm(), p.shrink_factor, origin, &tol)?
        } else if p.steps.is_empty() {
            mold::split(
                &part,
                Plane::from_point_normal(p.point, p.normal),
                p.margin.as_mm(),
                p.shrink_factor,
                origin,
                &tol,
            )?
        } else {
            mold::split_stepped(
                &part,
                &surface,
                p.margin.as_mm(),
                p.shrink_factor,
                origin,
                &tol,
            )?
        };
        let parting: &dyn mold::Parting = match &freeform {
            Some(f) => f,
            None => &surface,
        };
        let found = mold::undercuts_of(&source_mesh, &world, parting, &tol)?;
        let (actions, undercuts_left) = if p.side_actions {
            mold::side_actions(&part, &mut result, parting, p.shrink_factor, origin, &tol)?
        } else {
            (Vec::new(), found.len())
        };
        let core_volume_mm3 = result.core.volume()?;
        let cavity_volume_mm3 = result.cavity.volume()?;
        let mut groups = Vec::new();
        for (name, mesh) in [
            ("Mold Core", &result.core),
            ("Mold Cavity", &result.cavity),
            ("Parting Line", &result.parting_line),
            ("Parting Surface", &result.parting_surface),
        ] {
            let mesh = local_mesh(mesh, &target.local, &tol)?;
            let (pid, d) = new_group(tx, &target, name, mesh)?;
            store(tx, d, "mold", &p)?;
            groups.push(pid);
        }
        let mut side_actions = Vec::new();
        let mut side_directions = Vec::new();
        for (k, action) in actions.iter().enumerate() {
            let mesh = local_mesh(&action.mesh, &target.local, &tol)?;
            let (pid, d) = new_group(tx, &target, &format!("Side Action {}", k + 1), mesh)?;
            store(tx, d, "mold", &p)?;
            side_actions.push(pid);
            side_directions.push(action.direction.to_array());
        }
        Ok(MoldOutput {
            core: groups[0],
            cavity: groups[1],
            parting_line: groups[2],
            parting_surface: groups[3],
            core_volume_mm3,
            cavity_volume_mm3,
            compensated_volume_mm3: result.compensated_volume_mm3,
            parting_length_mm: result.parting_length_mm,
            fallback_used: result.fallback_used,
            undercuts: found.len(),
            undercut_area_mm2: found.iter().map(|u| u.area_mm2).sum(),
            side_actions,
            side_directions,
            undercuts_left,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "mold-split".into(),
            label: "Split Mold".into(),
            category: CommandCategory::Solid,
            view_only: false,
            steps: Vec::new(),
        }
    }
}

// ----- mold-undercuts ---------------------------------------------------------------------

/// `mold-undercuts` parameters, in world millimeters (milestone 1.40).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MoldUndercutsParams {
    /// One solid group or component, without nested contents.
    pub body: Query,
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Point of the parting plane.
    #[schemars(with = "PointSchema")]
    pub point: DVec3,
    /// Plane normal, from core toward cavity (the cavity's pull).
    #[schemars(with = "VectorSchema")]
    pub normal: DVec3,
    /// Steps of the parting surface, as for `mold-split`.
    #[serde(default)]
    pub steps: Vec<MoldStep>,
    /// Along which steps are placed.
    #[serde(default)]
    #[schemars(with = "Option<VectorSchema>")]
    pub step_direction: Option<DVec3>,
    /// A freeform parting surface, as for `mold-split` (milestone 1.48).
    #[serde(default)]
    pub surface: Option<Query>,
}

/// One face that cannot be released.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UndercutFace {
    /// The face, in the body's definition.
    pub face: Pid,
    /// `core` or `cavity`: the half that forms it.
    pub side: String,
    /// Why it is trapped.
    pub kind: mold::UndercutKind,
    /// Its area, mm².
    pub area_mm2: f64,
}

/// What `mold-undercuts` found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MoldUndercutsOutput {
    /// Faces checked.
    pub checked: usize,
    /// Faces that cannot be released, in face order.
    pub undercuts: Vec<UndercutFace>,
    /// Their total area, mm².
    pub area_mm2: f64,
}

/// Reports which faces of a body cannot be pulled from the mold halves a
/// parting surface defines; changes nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct MoldUndercuts;

impl Command for MoldUndercuts {
    type Params = MoldUndercutsParams;
    type Output = MoldUndercutsOutput;

    fn name(&self) -> &'static str {
        "mold-undercuts"
    }

    fn validate(&self, _: &Document, p: &MoldUndercutsParams) -> Result<(), CommandError> {
        check_point("point", p.point)?;
        surface_of(p.point, p.normal, &p.steps, p.step_direction).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MoldUndercutsParams,
    ) -> Result<MoldUndercutsOutput, CommandError> {
        let (d, world, target) = body_of(tx, &p.common, &p.body)?;
        let tol = *tx.tolerances();
        let surface = surface_of(p.point, p.normal, &p.steps, p.step_direction)?;
        let freeform = match &p.surface {
            Some(q) => Some(freeform_of(tx, &target, q, p.normal)?),
            None => None,
        };
        let parting: &dyn mold::Parting = match &freeform {
            Some(f) => f,
            None => &surface,
        };
        let def = tx
            .document()
            .definition(d)
            .ok_or_else(|| CommandError::Internal("body definition vanished".into()))?;
        let mesh = def.entities.mesh();
        let found = mold::undercuts_of(mesh, &world, parting, &tol)?;
        let undercuts: Vec<UndercutFace> = found
            .iter()
            .filter_map(|u| {
                Some(UndercutFace {
                    face: def.entities.pid_of(EntityId::Face(u.face))?,
                    side: if u.cavity { "cavity" } else { "core" }.into(),
                    kind: u.kind,
                    area_mm2: u.area_mm2,
                })
            })
            .collect();
        Ok(MoldUndercutsOutput {
            checked: mesh.face_count(),
            area_mm2: undercuts.iter().map(|u| u.area_mm2).sum(),
            undercuts,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "mold-undercuts".into(),
            label: "Find Undercuts".into(),
            category: CommandCategory::Solid,
            view_only: true,
            steps: Vec::new(),
        }
    }
}

/// Registers mold design commands for CLI, RPC and scripting.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(MoldSplit);
    registry.register(MoldUndercuts);
}
