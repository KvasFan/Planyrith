//! Implements docs/05-modeling-tools.md §3. Drawing tools (category `draw`): `line`, `freehand`, `rectangle`, `rotated-rectangle`, `circle`, `polygon`, `arc-*`, `pie`, `text-3d`.
//!
//! Each tool is one `Command` implementation in its own file with a serde
//! `Params` struct (flattening `commands::CommonParams`) and an `Output`
//! struct, registered in `CommandRegistry::with_builtins` under its stable
//! kebab-case name (AGENTS.md §4). Milestone 0.1 provides `line`,
//! `rectangle`, `circle`, `polygon`, `arc-2point`, `arc-3point`, `arc-bulge`
//! and `pie`.
//!
//! Points in params are world coordinates; the shapes are built in the edit
//! context's container (docs/05 Decisions).
//!
//! Owner key: `later`.

#![warn(unused_variables, dead_code)]

mod arc;
mod circle;
mod line;
mod rectangle;
pub mod text3d;

pub use arc::{
    Arc2Point, Arc2PointParams, Arc3Point, Arc3PointParams, ArcBulge, ArcBulgeParams, Pie,
};
pub use circle::{Circle, CircleParams, Polygon, PolygonParams, ShapeOutput};
pub use line::{Line, LineOutput, LineParams};
pub use rectangle::{AspectLock, Rectangle, RectangleOutput, RectangleParams};

use std::collections::BTreeSet;

use glam::DVec3;

use crate::commands::params::{Target, pid_of};
use crate::commands::{CommandError, CommandRegistry, Transaction};
use crate::geom::CurveDescriptor;
use crate::ids::{EdgeId, EntityId, FaceId, Pid};

/// Registers this category's commands (called by
/// [`CommandRegistry::with_builtins`]).
pub fn register(registry: &mut CommandRegistry) {
    registry.register(Line);
    registry.register(Rectangle);
    registry.register(Circle);
    registry.register(Polygon);
    registry.register(Arc2Point);
    registry.register(Arc3Point);
    registry.register(ArcBulge);
    registry.register(Pie);
    registry.register(text3d::Text3d);
}

/// What a drawn chain produced, as pids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Drawn {
    /// Edges covering the drawn segments, in drawing order (deduplicated).
    pub edges: Vec<Pid>,
    /// Faces created in empty space or inside a face (hole fill).
    pub faces_created: Vec<Pid>,
    /// `(kept, new)` pids of each face split by the drawn edges.
    pub split_from: Vec<(Pid, Pid)>,
    /// New pieces of faces split by the drawn edges.
    pub faces_split: Vec<Pid>,
}

/// Adds the straight segments `points[i] → points[i + 1]` (and back to the
/// start when `closed`) to the target container in one atomic kernel step,
/// optionally attaching a curve descriptor to the resulting edges.
pub(crate) fn draw_chain(
    tx: &mut Transaction<'_>,
    target: &Target,
    points: &[DVec3],
    closed: bool,
    curve: Option<CurveDescriptor>,
) -> Result<Drawn, CommandError> {
    let local: Vec<DVec3> = points.iter().map(|&p| target.to_local(p)).collect();
    let n = local.len();
    let segments = if closed { n } else { n.saturating_sub(1) };
    let (edges, created, split) = tx.with_mesh(target.container, |mesh, tol, d| {
        mesh.transact(tol, d, |m, d| {
            let mut edges: Vec<EdgeId> = Vec::new();
            let mut created: Vec<FaceId> = Vec::new();
            let mut split: Vec<(FaceId, FaceId)> = Vec::new();
            for i in 0..segments {
                let r = m.add_edge(local[i], local[(i + 1) % n], tol, d)?;
                for e in r.edges {
                    if !edges.contains(&e) {
                        edges.push(e);
                    }
                }
                created.extend(r.new_faces);
                split.extend(r.split_faces.iter().map(|&(_, kept, new)| (kept, new)));
            }
            // Edges a later segment split no longer exist; keep the live ones.
            edges.retain(|e| m.edge(*e).is_some());
            if let Some(descriptor) = curve {
                m.add_curve(descriptor, &edges, d)?;
            }
            Ok((edges, created, split))
        })
    })?;
    let container = target.container;
    let alive = |tx: &Transaction<'_>, f: &FaceId| {
        tx.document()
            .container(container)
            .is_some_and(|e| e.mesh().face(*f).is_some())
    };
    let mut seen = BTreeSet::new();
    let mut out = Drawn::default();
    for e in edges {
        out.edges.push(pid_of(tx, container, EntityId::Edge(e))?);
    }
    for f in created.iter().filter(|f| alive(tx, f)) {
        if seen.insert(*f) {
            out.faces_created
                .push(pid_of(tx, container, EntityId::Face(*f))?);
        }
    }
    for (kept, f) in split.iter().filter(|(_, f)| alive(tx, f)) {
        if seen.insert(*f) {
            let pid = pid_of(tx, container, EntityId::Face(*f))?;
            out.faces_split.push(pid);
            if alive(tx, kept) {
                out.split_from
                    .push((pid_of(tx, container, EntityId::Face(*kept))?, pid));
            }
        }
    }
    Ok(out)
}

/// The face bounded exactly by the drawn closed chain, if one exists (the
/// shape's own face; a hole-fill inside an existing face counts).
///
/// When the shape is the larger piece of a face it split, it kept that
/// face's pid (docs/03 Decisions, face identity) and the leftover got the
/// new one. The two pids are then swapped, so the face the user drew is the
/// new face that `last` names and the leftover keeps the old identity
/// (docs/05 Decisions).
pub(crate) fn face_of_chain(
    tx: &mut Transaction<'_>,
    target: &Target,
    drawn: &Drawn,
) -> Result<Option<Pid>, CommandError> {
    let Some(entities) = tx.document().container(target.container) else {
        return Ok(None);
    };
    let mesh = entities.mesh();
    let edges: BTreeSet<EdgeId> = drawn
        .edges
        .iter()
        .filter_map(|p| match entities.id_of(*p) {
            Some(EntityId::Edge(e)) => Some(e),
            _ => None,
        })
        .collect();
    let mut shape = None;
    'search: for &e in &edges {
        for f in mesh.edge_faces(e)?.into_iter().flatten() {
            let outer: BTreeSet<EdgeId> = mesh
                .loop_edges(mesh.face_ref(f)?.outer)?
                .into_iter()
                .collect();
            if outer == edges {
                shape = Some(f);
                break 'search;
            }
        }
    }
    let Some(f) = shape else {
        return Ok(None);
    };
    let mut pid = pid_of(tx, target.container, EntityId::Face(f))?;
    if !drawn.faces_created.contains(&pid)
        && !drawn.faces_split.contains(&pid)
        && let Some(&(_, leftover)) = drawn.split_from.iter().find(|(kept, _)| *kept == pid)
        && let Some(other) = tx
            .document()
            .container(target.container)
            .and_then(|e| e.id_of(leftover))
    {
        tx.swap_pids(target.container, EntityId::Face(f), other)?;
        pid = leftover;
    }
    // The other pieces of the faces the shape split are fragments of faces
    // the user already had: `last` names the shape (docs/08 Decisions).
    for &piece in &drawn.faces_split {
        if piece != pid {
            tx.forget_created(piece);
        }
    }
    Ok(Some(pid))
}

/// `E1001` unless `n` is within `lo..=hi`.
pub(crate) fn check_count(name: &str, n: u32, lo: u32, hi: u32) -> Result<(), CommandError> {
    if n < lo || n > hi {
        return Err(CommandError::OutOfRange {
            name: name.into(),
            reason: format!("must be between {lo} and {hi}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
