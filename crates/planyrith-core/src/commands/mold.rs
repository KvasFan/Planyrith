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
        Ok(())
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: MoldParams) -> Result<MoldOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let ids = targets_of(tx, &target, "body", &p.body)?;
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
        let definition = here
            .definition_of(*id)
            .and_then(|d| tx.document().definition(d))
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
        let tol = *tx.tolerances();
        let (source, _) = definition.entities.mesh().to_indexed();
        let part = HalfEdgeMesh::from_indexed(&transform_indexed(&source, &world), &tol)?;
        let result = mold::split(
            &part,
            Plane::from_point_normal(p.point, p.normal),
            p.margin.as_mm(),
            p.shrink_factor,
            p.origin.unwrap_or_else(|| part.aabb().center()),
            &tol,
        )?;
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

/// Registers mold design commands for CLI, RPC and scripting.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(MoldSplit);
}
