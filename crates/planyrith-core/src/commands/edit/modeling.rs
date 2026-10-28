//! Implements docs/05-modeling-tools.md §4.8–4.9 and §4.12.

use crate::commands::common::{targets_of, tool_steps};
use crate::commands::params::{pid_of, resolve_ref, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Document, EntityRef, Query};
use crate::geom::{EdgeFlags, GeometryError};
use crate::ids::{EntityId, Pid};
use crate::units::Angle;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn selection() -> Query {
    Query::new("selection")
}
fn yes() -> bool {
    true
}
fn angle() -> Angle {
    Angle::degrees(20.0)
}
fn meta(name: &str, label: &str, steps: &[(&str, &str)]) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        label: label.into(),
        category: CommandCategory::Edit,
        view_only: false,
        steps: tool_steps(steps),
    }
}

/// Sweep parameters; path and profile belong to the edit context.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SweepParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Profile face.
    pub profile: EntityRef,
    /// Connected path edges.
    #[serde(default)]
    pub path: Option<Query>,
    /// Use a face's outer boundary as the path.
    #[serde(default)]
    pub path_face: Option<EntityRef>,
}
/// Sweep result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SweepOutput {
    /// Created faces.
    pub faces: Vec<Pid>,
    /// Whether the path was closed.
    pub closed: bool,
}
/// Sweep a face along a connected path.
#[derive(Debug, Default, Clone, Copy)]
pub struct Sweep;
impl Command for Sweep {
    type Params = SweepParams;
    type Output = SweepOutput;
    fn name(&self) -> &'static str {
        "sweep"
    }
    fn validate(&self, _: &Document, p: &SweepParams) -> Result<(), CommandError> {
        if p.path.is_some() == p.path_face.is_some() {
            return Err(CommandError::invalid_param(
                "path",
                "give exactly one of path or path_face",
            ));
        }
        Ok(())
    }
    fn apply(&self, tx: &mut Transaction<'_>, p: SweepParams) -> Result<SweepOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let r = resolve_ref(tx, &target.context, "profile", &p.profile)?;
        let EntityId::Face(face) = r.id else {
            return Err(CommandError::invalid_param("profile", "must be a face"));
        };
        if r.container != target.container {
            return Err(CommandError::NotApplicable(
                "open the profile's edit context first".into(),
            ));
        }
        let edges = if let Some(q) = p.path {
            targets_of(tx, &target, "path", &q)?
                .into_iter()
                .map(|id| match id {
                    EntityId::Edge(e) => Ok(e),
                    _ => Err(CommandError::invalid_param(
                        "path",
                        "must contain only edges",
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?
        } else if let Some(r) = p.path_face {
            let r = resolve_ref(tx, &target.context, "path_face", &r)?;
            let EntityId::Face(f) = r.id else {
                return Err(CommandError::invalid_param("path_face", "must be a face"));
            };
            if r.container != target.container {
                return Err(CommandError::NotApplicable(
                    "path face must be in the edit context".into(),
                ));
            }
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("context missing".into()))?
                .mesh();
            mesh.loop_edges(
                mesh.face(f)
                    .ok_or_else(|| GeometryError::NotFound(f.to_string()))?
                    .outer,
            )?
        } else {
            return Err(CommandError::invalid_param("path", "missing"));
        };
        let r = tx.with_mesh(target.container, |m, tol, d| {
            crate::geom::sweep::sweep_profile(m, face, &edges, Default::default(), tol, d)
        })?;
        Ok(SweepOutput {
            faces: r
                .faces
                .into_iter()
                .map(|f| pid_of(tx, target.container, EntityId::Face(f)))
                .collect::<Result<_, _>>()?,
            closed: r.closed,
        })
    }
    fn meta(&self) -> CommandMeta {
        meta(
            "sweep",
            "Sweep",
            &[("Pick a profile face", ""), ("Pick the path edges", "path")],
        )
    }
}

/// Intersect Faces parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IntersectFacesParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Faces in the edit context to split.
    #[serde(default = "selection")]
    pub targets: Query,
    /// Which faces to intersect against.
    pub scope: crate::geom::intersect::IntersectScope,
}
/// Intersect Faces result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IntersectFacesOutput {
    /// Number of intersection edges added.
    pub edges_added: usize,
}
/// Split faces at their intersection segments.
#[derive(Debug, Default, Clone, Copy)]
pub struct IntersectFaces;
impl Command for IntersectFaces {
    type Params = IntersectFacesParams;
    type Output = IntersectFacesOutput;
    fn name(&self) -> &'static str {
        "intersect-faces"
    }
    fn validate(&self, _: &Document, p: &Self::Params) -> Result<(), CommandError> {
        p.targets
            .parse()
            .map_err(|e| CommandError::invalid_param("targets", e.to_string()))?;
        Ok(())
    }
    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: Self::Params,
    ) -> Result<Self::Output, CommandError> {
        let target = target_container(tx, &p.common)?;
        let faces = targets_of(tx, &target, "targets", &p.targets)?
            .into_iter()
            .filter_map(|id| {
                if let EntityId::Face(f) = id {
                    Some(f)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if faces.is_empty() {
            return Err(CommandError::invalid_param(
                "targets",
                "select at least one face",
            ));
        }
        let doc = tx.document();
        let mesh = doc
            .container(target.container)
            .ok_or_else(|| CommandError::Internal("context missing".into()))?
            .mesh();
        let mut segments = Vec::new();
        if p.scope == crate::geom::intersect::IntersectScope::Selection {
            segments =
                crate::geom::intersect::pairwise_segments(mesh, &faces, &faces, tx.tolerances())?;
        } else {
            fn collect<'a>(
                doc: &'a Document,
                c: crate::document::ContainerRef,
                t: glam::DAffine3,
                out: &mut Vec<(&'a crate::geom::HalfEdgeMesh, glam::DAffine3)>,
            ) {
                if let Some(e) = doc.container(c) {
                    out.push((e.mesh(), t));
                    for id in e.iter() {
                        if let (Some(def), Some(transform)) =
                            (e.definition_of(id), e.transform_of(id))
                        {
                            collect(
                                doc,
                                crate::document::ContainerRef::Definition(def),
                                t * transform,
                                out,
                            );
                        }
                    }
                }
            }
            let mut others = Vec::new();
            if p.scope == crate::geom::intersect::IntersectScope::Model {
                collect(
                    doc,
                    crate::document::ContainerRef::Model,
                    glam::DAffine3::IDENTITY,
                    &mut others,
                );
            } else {
                collect(doc, target.container, target.world, &mut others);
            }
            for &fa in &faces {
                for &(other, world) in &others {
                    for (fb, _) in other.faces() {
                        if std::ptr::eq(mesh, other)
                            && world.abs_diff_eq(target.world, tx.tolerances().length)
                            && fa == fb
                        {
                            continue;
                        }
                        for s in crate::geom::intersect::face_face(
                            mesh,
                            fa,
                            &target.world,
                            other,
                            fb,
                            &world,
                            tx.tolerances(),
                        )? {
                            segments.push(crate::math::Segment::new(
                                target.to_local(s.a),
                                target.to_local(s.b),
                            ));
                        }
                    }
                }
            }
        }
        let r = tx.with_mesh(target.container, |m, tol, d| {
            crate::geom::intersect::add_segments(m, &segments, tol, d)
        })?;
        Ok(IntersectFacesOutput {
            edges_added: r.edges.len(),
        })
    }
    fn meta(&self) -> CommandMeta {
        meta("intersect-faces", "Intersect Faces", &[])
    }
}

/// Soften/Smooth parameters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SoftenSmoothParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Edges or faces in the edit context.
    #[serde(default = "selection")]
    pub targets: Query,
    /// Maximum dihedral angle (CLI degrees, core radians).
    #[serde(default = "angle")]
    pub angle: Angle,
    /// Smooth shading as well as softening.
    #[serde(default = "yes")]
    pub smooth: bool,
    /// Include coplanar edges.
    #[serde(default)]
    pub soften_coplanar: bool,
}
/// Soften/Smooth result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SoftenSmoothOutput {
    /// Edges whose flags changed.
    pub changed: usize,
}
/// Set edge softening from the dihedral angle.
#[derive(Debug, Default, Clone, Copy)]
pub struct SoftenSmooth;
impl Command for SoftenSmooth {
    type Params = SoftenSmoothParams;
    type Output = SoftenSmoothOutput;
    fn name(&self) -> &'static str {
        "soften-smooth"
    }
    fn validate(&self, _: &Document, p: &Self::Params) -> Result<(), CommandError> {
        if !p.angle.as_radians().is_finite()
            || !(0.0..=std::f64::consts::PI).contains(&p.angle.as_radians())
        {
            return Err(CommandError::invalid_param(
                "angle",
                "must be between 0 and 180 degrees",
            ));
        }
        Ok(())
    }
    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: Self::Params,
    ) -> Result<Self::Output, CommandError> {
        let target = target_container(tx, &p.common)?;
        let ids = targets_of(tx, &target, "targets", &p.targets)?;
        let changed = tx.with_mesh(target.container, |m, tol, d| {
            let mut edges = std::collections::BTreeSet::new();
            for id in ids {
                match id {
                    EntityId::Edge(e) => {
                        edges.insert(e);
                    }
                    EntityId::Face(f) => edges.extend(m.face_edges(f)?),
                    _ => {}
                }
            }
            let mut changed = 0;
            for e in edges {
                let [Some(a), Some(b)] = m.edge_faces(e)? else {
                    continue;
                };
                let na = m.face_normal(a)?;
                let nb = m.face_normal(b)?;
                let dihedral = libm::atan2(na.cross(nb).length(), na.dot(nb));
                let soft = dihedral <= p.angle.as_radians() + tol.angle
                    && (p.soften_coplanar || dihedral > tol.angle);
                let old = m
                    .edge(e)
                    .ok_or_else(|| GeometryError::NotFound(e.to_string()))?
                    .flags;
                let flags = old
                    .with(EdgeFlags::SOFT, soft)
                    .with(EdgeFlags::SMOOTH, soft && p.smooth);
                if old != flags {
                    m.set_edge_flags(e, flags, d)?;
                    changed += 1;
                }
            }
            Ok(changed)
        })?;
        Ok(SoftenSmoothOutput { changed })
    }
    fn meta(&self) -> CommandMeta {
        meta("soften-smooth", "Soften / Smooth", &[])
    }
}
