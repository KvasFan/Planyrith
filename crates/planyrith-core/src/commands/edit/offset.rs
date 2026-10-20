//! Implements docs/05-modeling-tools.md §4.7 `offset`.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::targets_in;
use crate::commands::params::{pid_of, resolve_ref, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, ToolStep, Transaction,
};
use crate::document::{Document, EntityRef, Query};
use crate::geom::offset::offset_face_boundary;
use crate::ids::{EdgeId, EntityId, FaceId, Pid};
use crate::inference::InferenceKind;
use crate::units::Length;

/// `offset` parameters (docs/05 §4.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OffsetParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The face whose boundary is offset.
    #[serde(default)]
    pub face: Option<EntityRef>,
    /// Alternatively, the edges of a face's outer boundary.
    #[serde(default)]
    pub edges: Option<Query>,
    /// Offset distance; negative = inward.
    pub distance: Length,
    /// Interactive "repeat the last offset" (GUI only).
    #[serde(default)]
    pub repeat_last: bool,
}

/// `offset` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OffsetOutput {
    /// Edges of the offset loops.
    pub edges: Vec<Pid>,
    /// The face bounded by the offset outer loop, when one formed.
    pub face: Option<Pid>,
    /// New pieces of faces split by the offset.
    pub faces_split: Vec<Pid>,
}

/// The `offset` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Offset;

impl Command for Offset {
    type Params = OffsetParams;
    type Output = OffsetOutput;

    fn name(&self) -> &'static str {
        "offset"
    }

    fn validate(&self, _doc: &Document, p: &OffsetParams) -> Result<(), CommandError> {
        if p.face.is_none() && p.edges.is_none() {
            return Err(CommandError::invalid_param("face", "give face or edges"));
        }
        if !p.distance.as_mm().is_finite() || p.distance.as_mm() == 0.0 {
            return Err(CommandError::invalid_param(
                "distance",
                "must be a non-zero length",
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: OffsetParams,
    ) -> Result<OffsetOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let face = match (&p.face, &p.edges) {
            (Some(r), _) => {
                let resolved = resolve_ref(tx, r)?;
                if resolved.container != target.container {
                    return Err(CommandError::NotApplicable(
                        "the face is not in the edit context; open its group first".into(),
                    ));
                }
                match resolved.id {
                    EntityId::Face(f) => f,
                    _ => return Err(CommandError::NotApplicable(format!("{r} is not a face"))),
                }
            }
            (None, Some(q)) => face_bounded_by(tx, &target, q)?,
            (None, None) => return Err(CommandError::invalid_param("face", "give face or edges")),
        };
        let r = tx.with_mesh(target.container, |m, tol, d| {
            offset_face_boundary(m, face, p.distance, tol, d)
        })?;
        let c = target.container;
        Ok(OffsetOutput {
            edges: r
                .edges
                .iter()
                .map(|&e| pid_of(tx, c, EntityId::Edge(e)))
                .collect::<Result<_, _>>()?,
            face: r
                .face
                .map(|f| pid_of(tx, c, EntityId::Face(f)))
                .transpose()?,
            faces_split: r
                .faces_split
                .iter()
                .map(|&f| pid_of(tx, c, EntityId::Face(f)))
                .collect::<Result<_, _>>()?,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "offset".into(),
            category: CommandCategory::Edit,
            label: "Offset".into(),
            view_only: false,
            steps: vec![
                ToolStep {
                    prompt: "Pick a face or an edge chain".into(),
                    value_field: String::new(),
                    inference: vec![InferenceKind::OnFace, InferenceKind::OnEdge],
                },
                ToolStep {
                    prompt: "Move to set the distance".into(),
                    value_field: "distance (negative = inward)".into(),
                    inference: InferenceKind::ALL.to_vec(),
                },
            ],
        }
    }
}

/// The face whose outer loop is exactly the queried edges (open chains are
/// not supported in 0.1, docs/05 Decisions).
fn face_bounded_by(
    tx: &Transaction<'_>,
    target: &crate::commands::params::Target,
    q: &Query,
) -> Result<FaceId, CommandError> {
    let edges: BTreeSet<EdgeId> = targets_in(tx, target, q)?
        .into_iter()
        .filter_map(|id| match id {
            EntityId::Edge(e) => Some(e),
            _ => None,
        })
        .collect();
    let mesh = tx
        .document()
        .container(target.container)
        .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
        .mesh();
    for (f, face) in mesh.faces() {
        let outer: BTreeSet<EdgeId> = mesh.loop_edges(face.outer)?.into_iter().collect();
        if !edges.is_empty() && outer == edges {
            return Ok(f);
        }
    }
    Err(CommandError::NotApplicable(
        "the edges are not the whole boundary of one face (open chains arrive later)".into(),
    ))
}
