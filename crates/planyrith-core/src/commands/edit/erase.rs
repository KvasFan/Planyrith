//! Implements docs/05-modeling-tools.md §4.2 `erase`, §4.13 `hide`/`unhide`,
//! §4.14 `delete` and §5.13 `lock`/`unlock`.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{refuse_locked, remove_container_entity, targets_in};
use crate::commands::params::target_container;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, ToolStep, Transaction,
};
use crate::document::{Document, Query};
use crate::geom::EdgeFlags;
use crate::ids::{EdgeId, EntityId, FaceId};
use crate::inference::InferenceKind;

/// What the eraser does to the edges it touches (docs/05 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EraseMode {
    /// Delete edges (merge/delete faces per docs/03 §4.6), guides, groups…
    #[default]
    Erase,
    /// Set `SOFT | SMOOTH`.
    Soften,
    /// Set `SMOOTH`.
    Smooth,
    /// Set the `HIDDEN` edge flag.
    Hide,
    /// Clear `SOFT | SMOOTH`.
    Unsoften,
    /// Clear `SMOOTH`.
    Unsmooth,
    /// Clear the `HIDDEN` edge flag.
    Unhide,
}

/// `erase` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EraseParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// What to erase (faces are ignored: `delete` removes faces).
    pub targets: Query,
    /// Erase or change edge flags.
    #[serde(default)]
    pub mode: EraseMode,
    /// Also remove edges left without any face by the erasure.
    #[serde(default)]
    pub erase_orphans: bool,
}

/// `erase` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EraseOutput {
    /// Entities erased or changed.
    pub erased: usize,
    /// Faces deleted by the edge rules.
    pub faces_removed: usize,
    /// Coplanar face pairs merged.
    pub faces_merged: usize,
}

/// The `erase` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Erase;

/// Erases edges by the docs/03 §4.6 rules; returns (faces removed, merged).
fn erase_edges(
    tx: &mut Transaction<'_>,
    container: crate::document::ContainerRef,
    edges: &[EdgeId],
    orphans: bool,
) -> Result<(usize, usize), CommandError> {
    tx.with_mesh(container, |mesh, tol, d| {
        mesh.transact(tol, d, |m, log| {
            let mut removed = 0;
            let mut merged = 0;
            let mut around: BTreeSet<EdgeId> = BTreeSet::new();
            for &e in edges {
                if m.edge(e).is_none() {
                    continue;
                }
                if orphans {
                    for f in m.edge_faces(e)?.into_iter().flatten() {
                        around.extend(m.face_edges(f)?);
                    }
                }
                let r = m.erase_edge_inner(e, tol, log)?;
                removed += r.faces_removed.len();
                merged += usize::from(r.faces_merged.is_some());
            }
            for e in around {
                if m.edge(e).is_some() && m.edge_faces(e)? == [None, None] {
                    let (a, b) = m.edge_endpoints(e)?;
                    m.delete_edge(e, log)?;
                    m.delete_if_isolated(a, log)?;
                    m.delete_if_isolated(b, log)?;
                }
            }
            Ok((removed, merged))
        })
    })
}

impl Command for Erase {
    type Params = EraseParams;
    type Output = EraseOutput;

    fn name(&self) -> &'static str {
        "erase"
    }

    fn validate(&self, _doc: &Document, p: &EraseParams) -> Result<(), CommandError> {
        p.targets
            .parse()
            .map(|_| ())
            .map_err(|e| CommandError::invalid_param("targets", e.to_string()))
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: EraseParams) -> Result<EraseOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let ids = targets_in(tx, &target, &p.targets)?;
        refuse_locked(tx, target.container, &ids)?;
        let edges: Vec<EdgeId> = ids
            .iter()
            .filter_map(|id| match id {
                EntityId::Edge(e) => Some(*e),
                _ => None,
            })
            .collect();
        let mut out = EraseOutput {
            erased: 0,
            faces_removed: 0,
            faces_merged: 0,
        };
        if p.mode != EraseMode::Erase {
            let (set, clear) = match p.mode {
                EraseMode::Soften => (EdgeFlags::SOFT | EdgeFlags::SMOOTH, EdgeFlags::NONE),
                EraseMode::Smooth => (EdgeFlags::SMOOTH, EdgeFlags::NONE),
                EraseMode::Hide => (EdgeFlags::HIDDEN, EdgeFlags::NONE),
                EraseMode::Unsoften => (EdgeFlags::NONE, EdgeFlags::SOFT | EdgeFlags::SMOOTH),
                EraseMode::Unsmooth => (EdgeFlags::NONE, EdgeFlags::SMOOTH),
                EraseMode::Unhide | EraseMode::Erase => (EdgeFlags::NONE, EdgeFlags::HIDDEN),
            };
            out.erased = edges.len();
            tx.with_mesh(target.container, |mesh, _, d| {
                for &e in &edges {
                    let flags = mesh.edge(e).map(|x| x.flags).unwrap_or_default();
                    mesh.set_edge_flags(e, flags.union(set).difference(clear), d)?;
                }
                Ok(())
            })?;
            return Ok(out);
        }
        let (removed, merged) = erase_edges(tx, target.container, &edges, p.erase_orphans)?;
        out.erased = edges.len();
        out.faces_removed = removed;
        out.faces_merged = merged;
        for id in ids
            .into_iter()
            .filter(|id| !matches!(id, EntityId::Edge(_) | EntityId::Face(_)))
        {
            let alive = tx
                .document()
                .container(target.container)
                .is_some_and(|e| e.contains(id));
            if !alive {
                continue;
            }
            match id {
                EntityId::Group(_) | EntityId::Instance(_) => {
                    remove_container_entity(tx, target.container, id)?
                }
                _ => {
                    tx.remove_entity(target.container, id)?;
                }
            }
            out.erased += 1;
        }
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "erase".into(),
            category: CommandCategory::Edit,
            label: "Erase".into(),
            view_only: false,
            steps: vec![ToolStep {
                prompt: "Click or drag over edges (Ctrl soften, Shift hide)".into(),
                value_field: String::new(),
                inference: vec![InferenceKind::OnEdge],
            }],
        }
    }
}

/// `delete` parameters (docs/05 §4.14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeleteParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// What to delete (any kind).
    pub targets: Query,
}

/// `delete` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeleteOutput {
    /// Entities deleted.
    pub deleted: usize,
}

/// The `delete` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Delete;

impl Command for Delete {
    type Params = DeleteParams;
    type Output = DeleteOutput;

    fn name(&self) -> &'static str {
        "delete"
    }

    fn validate(&self, _doc: &Document, p: &DeleteParams) -> Result<(), CommandError> {
        p.targets
            .parse()
            .map(|_| ())
            .map_err(|e| CommandError::invalid_param("targets", e.to_string()))
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: DeleteParams,
    ) -> Result<DeleteOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let ids = targets_in(tx, &target, &p.targets)?;
        refuse_locked(tx, target.container, &ids)?;
        let faces: Vec<FaceId> = ids
            .iter()
            .filter_map(|id| match id {
                EntityId::Face(f) => Some(*f),
                _ => None,
            })
            .collect();
        let edges: Vec<EdgeId> = ids
            .iter()
            .filter_map(|id| match id {
                EntityId::Edge(e) => Some(*e),
                _ => None,
            })
            .collect();
        let mut deleted = 0;
        if !faces.is_empty() {
            deleted += tx.with_mesh(target.container, |mesh, tol, d| {
                mesh.transact(tol, d, |m, log| {
                    let mut n = 0;
                    for &f in &faces {
                        if m.face(f).is_some() {
                            m.erase_face_inner(f, false, log)?;
                            n += 1;
                        }
                    }
                    Ok(n)
                })
            })?;
        }
        erase_edges(tx, target.container, &edges, false)?;
        deleted += edges.len();
        for id in ids
            .into_iter()
            .filter(|id| !matches!(id, EntityId::Edge(_) | EntityId::Face(_)))
        {
            match id {
                EntityId::Group(_) | EntityId::Instance(_) => {
                    remove_container_entity(tx, target.container, id)?
                }
                _ => {
                    tx.remove_entity(target.container, id)?;
                }
            }
            deleted += 1;
        }
        Ok(DeleteOutput { deleted })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "delete".into(),
            category: CommandCategory::Edit,
            label: "Delete".into(),
            view_only: false,
            steps: Vec::new(),
        }
    }
}

/// Parameters of `hide`, `unhide`, `lock`, `unlock`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StateParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Entities to change.
    pub targets: Query,
}

/// Output of `hide`, `unhide`, `lock`, `unlock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StateOutput {
    /// Entities whose state changed or was confirmed.
    pub changed: usize,
}

macro_rules! state_command {
    ($ty:ident, $name:literal, $label:literal, $doc:literal, |$tx:ident, $c:ident, $id:ident| $body:expr) => {
        #[doc = $doc]
        #[derive(Debug, Default, Clone, Copy)]
        pub struct $ty;

        impl Command for $ty {
            type Params = StateParams;
            type Output = StateOutput;

            fn name(&self) -> &'static str {
                $name
            }

            fn validate(&self, _doc: &Document, p: &StateParams) -> Result<(), CommandError> {
                p.targets
                    .parse()
                    .map(|_| ())
                    .map_err(|e| CommandError::invalid_param("targets", e.to_string()))
            }

            fn apply(
                &self,
                $tx: &mut Transaction<'_>,
                p: StateParams,
            ) -> Result<StateOutput, CommandError> {
                let target = target_container($tx, &p.common)?;
                let ids = targets_in($tx, &target, &p.targets)?;
                let $c = target.container;
                for $id in &ids {
                    let $id = *$id;
                    $body?;
                }
                Ok(StateOutput { changed: ids.len() })
            }

            fn meta(&self) -> CommandMeta {
                CommandMeta {
                    name: $name.into(),
                    category: CommandCategory::Edit,
                    label: $label.into(),
                    view_only: false,
                    steps: Vec::new(),
                }
            }
        }
    };
}

state_command!(
    Hide,
    "hide",
    "Hide",
    "The `hide` command (docs/05 §4.13).",
    |tx, c, id| tx.set_hidden(c, id, true)
);
state_command!(
    Unhide,
    "unhide",
    "Unhide",
    "The `unhide` command (docs/05 §4.13); `unhide --targets hidden` shows everything.",
    |tx, c, id| tx.set_hidden(c, id, false)
);
state_command!(
    Lock,
    "lock",
    "Lock",
    "The `lock` command (docs/05 §5.13): groups and instances only.",
    |tx, c, id| match id {
        EntityId::Group(_) | EntityId::Instance(_) => tx.set_locked(c, id, true),
        _ => Ok(()),
    }
);
state_command!(
    Unlock,
    "unlock",
    "Unlock",
    "The `unlock` command (docs/05 §5.13).",
    |tx, c, id| match id {
        EntityId::Group(_) | EntityId::Instance(_) => tx.set_locked(c, id, false),
        _ => Ok(()),
    }
);
