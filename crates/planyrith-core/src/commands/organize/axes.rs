//! Implements docs/05-modeling-tools.md §5.10 `axes-set` and `axes-reset`:
//! the drawing axes of the edit context — the model's (used by inference:
//! the origin snap, on-axis directions and axis locks) or, inside a group
//! or component, its definition's axes.

use glam::{DAffine3, DMat3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::params::{PointSchema, VectorSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ContainerRef, Document};

fn x_axis() -> DVec3 {
    DVec3::X
}

fn y_axis() -> DVec3 {
    DVec3::Y
}

/// `axes-set` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AxesSetParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// New origin (world).
    #[schemars(with = "PointSchema")]
    pub origin: DVec3,
    /// Red axis direction (+X).
    #[serde(default = "x_axis")]
    #[schemars(with = "VectorSchema")]
    pub x: DVec3,
    /// A direction in the red/green plane (+Y); the green axis is made
    /// perpendicular to red, blue completes the right-handed frame.
    #[serde(default = "y_axis")]
    #[schemars(with = "VectorSchema")]
    pub y: DVec3,
}

/// `axes-reset` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AxesResetParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
}

/// Output of the axes commands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AxesOutput {
    /// Origin (world).
    #[schemars(with = "PointSchema")]
    pub origin: DVec3,
    /// Red, green and blue directions (world).
    #[schemars(with = "[VectorSchema; 3]")]
    pub axes: [DVec3; 3],
}

/// The orthonormal frame of `x` and `y`.
fn frame(x: DVec3, y: DVec3) -> Option<DMat3> {
    let x = x.try_normalize()?;
    let z = x.cross(y).try_normalize()?;
    Some(DMat3::from_cols(x, z.cross(x), z))
}

/// Stores `axes` (container coordinates) for the context's container.
fn store(
    tx: &mut Transaction<'_>,
    container: ContainerRef,
    axes: DAffine3,
) -> Result<(), CommandError> {
    match container {
        ContainerRef::Model => {
            let mut info = tx.document().info.clone();
            info.axes = axes;
            tx.set_info(info)
        }
        ContainerRef::Definition(d) => {
            let mut def = tx
                .document()
                .definition(d)
                .cloned()
                .ok_or_else(|| CommandError::Internal("lost definition".into()))?;
            def.behavior.axes = axes;
            tx.update_definition(d, def)
        }
    }
}

fn output(world: DAffine3) -> AxesOutput {
    AxesOutput {
        origin: world.translation,
        axes: [
            world.matrix3.x_axis.normalize_or_zero(),
            world.matrix3.y_axis.normalize_or_zero(),
            world.matrix3.z_axis.normalize_or_zero(),
        ],
    }
}

/// The `axes-set` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct AxesSet;

impl Command for AxesSet {
    type Params = AxesSetParams;
    type Output = AxesOutput;

    fn name(&self) -> &'static str {
        "axes-set"
    }

    fn validate(&self, _doc: &Document, p: &AxesSetParams) -> Result<(), CommandError> {
        check_point("origin", p.origin)?;
        if !(p.x.is_finite() && p.y.is_finite()) || frame(p.x, p.y).is_none() {
            return Err(CommandError::invalid_param(
                "y",
                "`x` and `y` must be non-zero and not parallel",
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: AxesSetParams,
    ) -> Result<AxesOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let world = DAffine3::from_mat3_translation(
            frame(p.x, p.y).ok_or_else(|| CommandError::invalid_param("y", "parallel to x"))?,
            p.origin,
        );
        // In container coordinates, directions kept unit length.
        let local = target.local * world;
        let local = DAffine3::from_mat3_translation(
            frame(local.matrix3.x_axis, local.matrix3.y_axis).unwrap_or(DMat3::IDENTITY),
            local.translation,
        );
        store(tx, target.container, local)?;
        Ok(output(world))
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "axes-set".into(),
            category: CommandCategory::Organize,
            label: "Axes".into(),
            view_only: false,
            steps: Vec::new(),
        }
    }
}

/// The `axes-reset` command: back to the container's own origin and axes.
#[derive(Debug, Default, Clone, Copy)]
pub struct AxesReset;

impl Command for AxesReset {
    type Params = AxesResetParams;
    type Output = AxesOutput;

    fn name(&self) -> &'static str {
        "axes-reset"
    }

    fn validate(&self, _doc: &Document, _p: &AxesResetParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: AxesResetParams,
    ) -> Result<AxesOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        store(tx, target.container, DAffine3::IDENTITY)?;
        Ok(output(target.world))
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "axes-reset".into(),
            category: CommandCategory::Organize,
            label: "Reset Axes".into(),
            view_only: false,
            steps: Vec::new(),
        }
    }
}
