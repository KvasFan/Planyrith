//! Implements docs/05-modeling-tools.md §8.19: `route` and `route-fitting`.
//! Geometry is generated in world millimeters then placed in the edit context.

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::targets_of;
use crate::commands::edit::transform::chain;
use crate::commands::fabrication::{new_group, store};
use crate::commands::params::{PointSchema, VectorSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Document, Query};
use crate::geom::HalfEdgeMesh;
use crate::geom::mesh::transform_indexed;
use crate::geom::routing::{RouteOptions, TubeSize, fitting, route};
use crate::ids::{EntityId, Pid};
use crate::tolerance::Tolerances;
use crate::units::Length;

fn default_size() -> String {
    "20x1.5".into()
}
fn segments() -> u32 {
    24
}
fn bend_segments() -> u32 {
    8
}
fn fitting_length() -> Length {
    Length::mm(40.0)
}
fn up() -> DVec3 {
    DVec3::Z
}

/// Hollow round section, either a preset or two explicit dimensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SectionParams {
    /// Outer diameter × wall preset, mm: 10x1, 12x1, 16x1, 20x1.5,
    /// 25x2, 32x2, 40x2, 50x2. Both custom dimensions override it.
    #[serde(default = "default_size")]
    pub size: String,
    /// Custom outer diameter, mm; give together with wall.
    #[serde(default)]
    pub outer_diameter: Option<Length>,
    /// Custom wall thickness, mm; give together with outer_diameter.
    #[serde(default)]
    pub wall: Option<Length>,
}

impl SectionParams {
    fn resolve(&self, tol: &Tolerances) -> Result<TubeSize, CommandError> {
        let size = match (self.outer_diameter, self.wall) {
            (Some(d), Some(w)) => TubeSize {
                diameter: d.as_mm(),
                wall: w.as_mm(),
            },
            (None, None) => TubeSize::preset(&self.size)
                .map_err(|e| CommandError::invalid_param("size", e.to_string()))?,
            _ => {
                return Err(CommandError::invalid_param(
                    "outer_diameter",
                    "give both outer_diameter and wall",
                ));
            }
        };
        size.check(tol)
            .map_err(|e| CommandError::invalid_param("wall", e.to_string()))?;
        Ok(size)
    }
}

/// `route` parameters; coordinates and dimensions use world millimeters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RouteParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// World points of an open path, alternative to path (2–256).
    #[serde(default)]
    #[schemars(with = "Option<Vec<PointSchema>>")]
    pub points: Option<Vec<DVec3>>,
    /// Connected open edges of the edit context, alternative to points.
    #[serde(default)]
    pub path: Option<Query>,
    /// Section dimensions.
    #[serde(flatten)]
    pub section: SectionParams,
    /// Centerline bend radius, mm; default 1.5 × outer diameter.
    #[serde(default)]
    pub bend_radius: Option<Length>,
    /// Facets around the section (8–128).
    #[serde(default = "segments")]
    pub segments: u32,
    /// Chords per elbow (2–64).
    #[serde(default = "bend_segments")]
    pub bend_segments: u32,
}

impl RouteParams {
    fn options(&self, tol: &Tolerances) -> Result<RouteOptions, CommandError> {
        let size = self.section.resolve(tol)?;
        if !(8..=128).contains(&self.segments) {
            return Err(CommandError::invalid_param(
                "segments",
                "use 8–128 facets around the tube",
            ));
        }
        if !(2..=64).contains(&self.bend_segments) {
            return Err(CommandError::invalid_param(
                "bend_segments",
                "use 2–64 chords per elbow",
            ));
        }
        let options = RouteOptions {
            size,
            bend_radius: self.bend_radius.map_or(1.5 * size.diameter, Length::as_mm),
            segments: self.segments,
            bend_segments: self.bend_segments,
        };
        options
            .check(tol)
            .map_err(|e| CommandError::invalid_param("bend_radius", e.to_string()))?;
        Ok(options)
    }
}

/// The placed hollow wall and its dimensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RouteOutput {
    /// The new group.
    pub group: Pid,
    /// Analytic centerline length (axial length for a fitting), mm.
    pub length_mm: f64,
    /// Wall mesh volume, mm³.
    pub volume_mm3: f64,
    /// Inlet outer diameter, mm.
    pub outer_diameter_mm: f64,
    /// Wall thickness, mm.
    pub wall_mm: f64,
    /// Elbows included in the route.
    pub elbows: usize,
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

/// Places a world-space mesh in a new group, retaining world dimensions.
pub(crate) fn local_mesh(
    mesh: &HalfEdgeMesh,
    local: &DAffine3,
    tol: &Tolerances,
) -> Result<HalfEdgeMesh, CommandError> {
    let (indexed, _) = mesh.to_indexed();
    Ok(HalfEdgeMesh::from_indexed(
        &transform_indexed(&indexed, local),
        tol,
    )?)
}

/// The `route` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Route;

impl Command for Route {
    type Params = RouteParams;
    type Output = RouteOutput;

    fn name(&self) -> &'static str {
        "route"
    }

    fn validate(&self, doc: &Document, p: &RouteParams) -> Result<(), CommandError> {
        p.options(&doc.info.tolerances)?;
        if p.points.is_some() == p.path.is_some() {
            return Err(CommandError::invalid_param(
                "points",
                "give exactly one of points or path",
            ));
        }
        if let Some(points) = &p.points {
            if !(2..=256).contains(&points.len()) {
                return Err(CommandError::invalid_param("points", "give 2–256 points"));
            }
            for point in points {
                check_point("points", *point)?;
            }
        }
        Ok(())
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: RouteParams) -> Result<RouteOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let tol = *tx.tolerances();
        let options = p.options(&tol)?;
        let points = if let Some(q) = &p.path {
            let ids = targets_of(tx, &target, "path", q)?;
            let edges = ids
                .into_iter()
                .map(|id| match id {
                    EntityId::Edge(e) => Ok(e),
                    _ => Err(CommandError::invalid_param("path", "name only edges")),
                })
                .collect::<Result<Vec<_>, _>>()?;
            if edges.len() > 255 {
                return Err(CommandError::invalid_param("path", "at most 255 edges"));
            }
            let here = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("context vanished".into()))?;
            let (mut points, closed) = chain(here.mesh(), &edges, DVec3::ZERO)?;
            if closed {
                return Err(CommandError::invalid_param(
                    "path",
                    "the route must be open",
                ));
            }
            // Stable geometric orientation even when chain endpoints tie in distance.
            if let (Some(a), Some(b)) = (points.first(), points.last())
                && a.x
                    .total_cmp(&b.x)
                    .then(a.y.total_cmp(&b.y))
                    .then(a.z.total_cmp(&b.z))
                    .is_gt()
            {
                points.reverse();
            }
            points
                .into_iter()
                .map(|v| target.world.transform_point3(v))
                .collect()
        } else {
            p.points
                .clone()
                .ok_or_else(|| CommandError::invalid_param("points", "missing path"))?
        };
        let result = route(&points, options, &tol)?;
        let volume_mm3 = result.mesh.volume()?;
        let mesh = local_mesh(&result.mesh, &target.local, &tol)?;
        let (group, d) = new_group(tx, &target, "Route", mesh)?;
        store(tx, d, "route", &p)?;
        Ok(RouteOutput {
            group,
            length_mm: result.length_mm,
            volume_mm3,
            outer_diameter_mm: options.size.diameter,
            wall_mm: options.size.wall,
            elbows: result.elbows,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("route", "Route Tube")
    }
}

/// An independent hollow fitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FittingKind {
    /// A straight sleeve.
    Coupling,
    /// A concentric reducer.
    Reducer,
}

/// `route-fitting` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FittingParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Sleeve or concentric reducer.
    pub kind: FittingKind,
    /// The fitting's own section, not a mating pipe's nominal size.
    #[serde(flatten)]
    pub section: SectionParams,
    /// Outlet outer diameter, mm; required only for a reducer.
    #[serde(default)]
    pub outlet_diameter: Option<Length>,
    /// Axial length, mm.
    #[serde(default = "fitting_length")]
    pub length: Length,
    /// Inlet center in world millimeters (origin by default).
    #[serde(default)]
    #[schemars(with = "PointSchema")]
    pub position: DVec3,
    /// Axis in world coordinates (+Z by default).
    #[serde(default = "up")]
    #[schemars(with = "VectorSchema")]
    pub axis: DVec3,
}

/// The `route-fitting` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct RouteFitting;

impl Command for RouteFitting {
    type Params = FittingParams;
    type Output = RouteOutput;

    fn name(&self) -> &'static str {
        "route-fitting"
    }

    fn validate(&self, doc: &Document, p: &FittingParams) -> Result<(), CommandError> {
        let tol = &doc.info.tolerances;
        let size = p.section.resolve(tol)?;
        if matches!(p.kind, FittingKind::Reducer) != p.outlet_diameter.is_some() {
            return Err(CommandError::invalid_param(
                "outlet_diameter",
                "give an outlet diameter only for a reducer",
            ));
        }
        if let Some(d) = p.outlet_diameter {
            TubeSize {
                diameter: d.as_mm(),
                wall: size.wall,
            }
            .check(tol)
            .map_err(|e| CommandError::invalid_param("outlet_diameter", e.to_string()))?;
        }
        check_point("position", p.position)?;
        if !p.axis.is_finite() || p.axis.try_normalize().is_none() {
            return Err(CommandError::invalid_param(
                "axis",
                "must be finite and nonzero",
            ));
        }
        if !p.length.as_mm().is_finite() || p.length.as_mm() <= tol.length {
            return Err(CommandError::invalid_param(
                "length",
                "must exceed the length tolerance",
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: FittingParams,
    ) -> Result<RouteOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let tol = *tx.tolerances();
        let size = p.section.resolve(&tol)?;
        let result = fitting(
            size,
            p.outlet_diameter.map_or(size.diameter, Length::as_mm),
            p.length.as_mm(),
            p.position,
            p.axis,
            &tol,
        )?;
        let volume_mm3 = result.volume()?;
        let mesh = local_mesh(&result, &target.local, &tol)?;
        let (group, d) = new_group(
            tx,
            &target,
            match p.kind {
                FittingKind::Coupling => "Coupling",
                FittingKind::Reducer => "Reducer",
            },
            mesh,
        )?;
        store(tx, d, "route-fitting", &p)?;
        Ok(RouteOutput {
            group,
            length_mm: p.length.as_mm(),
            volume_mm3,
            outer_diameter_mm: size.diameter,
            wall_mm: size.wall,
            elbows: 0,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("route-fitting", "Route Fitting")
    }
}

/// Registers routing commands for CLI, RPC and scripting.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(Route);
    registry.register(RouteFitting);
}
