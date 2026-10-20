//! Implements docs/05-modeling-tools.md §4.3 `push-pull`.

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::params::{PointSchema, check_point, pid_of, resolve_ref, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, ToolStep, Transaction,
};
use crate::document::{Document, EntityRef};
use crate::geom::extrude::{ExtrudeOptions, extrude_face};
use crate::ids::{EntityId, Pid};
use crate::inference::InferenceKind;
use crate::units::Length;

/// `push-pull` parameters (docs/05 §4.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PushPullParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The face to extrude (`@pid`, `#path` or `{query}`).
    pub face: EntityRef,
    /// Distance along the face normal; negative pushes.
    #[serde(default)]
    pub distance: Option<Length>,
    /// Alternative to `distance`: extrude until the face plane reaches this point.
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub to: Option<DVec3>,
    /// Keep the start face (only possible for a face with nothing on the
    /// other side of its edges).
    #[serde(default)]
    pub keep_start_face: bool,
    /// Interactive "repeat the last distance" (GUI only; the CLI must give a
    /// distance).
    #[serde(default)]
    pub repeat_last: bool,
}

/// `push-pull` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PushPullOutput {
    /// The cap and the side faces that remain after merging.
    pub new_faces: Vec<Pid>,
    /// Faces that disappeared (the start face when it became interior).
    pub removed_faces: Vec<Pid>,
    /// A through-hole was cut.
    pub through_hole: bool,
    /// Whether the container is a closed solid afterwards.
    pub solid_after: Option<bool>,
}

/// The `push-pull` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct PushPull;

impl Command for PushPull {
    type Params = PushPullParams;
    type Output = PushPullOutput;

    fn name(&self) -> &'static str {
        "push-pull"
    }

    fn validate(&self, _doc: &Document, p: &PushPullParams) -> Result<(), CommandError> {
        match (p.distance, p.to) {
            (Some(d), _) if !d.as_mm().is_finite() || d.as_mm() == 0.0 => Err(
                CommandError::invalid_param("distance", "must be a non-zero length"),
            ),
            (Some(_), _) => Ok(()),
            (None, Some(to)) => check_point("to", to),
            (None, None) => Err(CommandError::invalid_param(
                "distance",
                "give a distance or a `to` point",
            )),
        }
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: PushPullParams,
    ) -> Result<PushPullOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let resolved = resolve_ref(tx, &p.face)?;
        let EntityId::Face(face) = resolved.id else {
            return Err(CommandError::NotApplicable(format!(
                "{} is not a face",
                p.face
            )));
        };
        if resolved.container != target.container {
            return Err(CommandError::NotApplicable(
                "the face is not in the edit context; open its group first".into(),
            ));
        }
        let entities = tx
            .document()
            .container(target.container)
            .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
        let plane = entities
            .mesh()
            .face(face)
            .map(|f| f.plane)
            .ok_or_else(|| CommandError::NotFound(p.face.clone()))?;
        let distance = match (p.distance, p.to) {
            (Some(d), _) => d.as_mm(),
            (None, Some(to)) => plane.signed_distance(target.to_local(to)),
            (None, None) => {
                return Err(CommandError::invalid_param("distance", "missing"));
            }
        };
        let start_pid = pid_of(tx, target.container, EntityId::Face(face))?;
        let opts = ExtrudeOptions {
            create_new_start_face: p.keep_start_face,
            ..ExtrudeOptions::default()
        };
        let result = tx.with_mesh(target.container, |mesh, tol, d| {
            extrude_face(mesh, face, Length::mm(distance), opts, tol, d)
        })?;
        let mut new_faces = Vec::new();
        for f in result.cap.iter().chain(&result.side_faces) {
            new_faces.push(pid_of(tx, target.container, EntityId::Face(*f))?);
        }
        let removed_faces = if result.removed_faces.contains(&face) {
            vec![start_pid]
        } else {
            Vec::new()
        };
        let solid_after = tx
            .document()
            .container(target.container)
            .map(|e| e.mesh().is_solid());
        Ok(PushPullOutput {
            new_faces,
            removed_faces,
            through_hole: result.through_hole,
            solid_after,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "push-pull".into(),
            category: CommandCategory::Edit,
            label: "Push/Pull".into(),
            view_only: false,
            steps: vec![
                ToolStep {
                    prompt: "Pick a face".into(),
                    value_field: String::new(),
                    inference: vec![InferenceKind::OnFace],
                },
                ToolStep {
                    prompt: "Move to set the distance".into(),
                    value_field: "distance (negative pushes)".into(),
                    inference: InferenceKind::ALL.to_vec(),
                },
            ],
        }
    }
}
