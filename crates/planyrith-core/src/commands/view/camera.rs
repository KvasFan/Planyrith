//! Implements docs/05-modeling-tools.md §9 (camera view tools): `camera-set`,
//! `camera-standard`, `position-camera`, `orbit`, `pan`, `zoom` and
//! `zoom-extents`. View commands are not undoable and do not dirty the
//! document: they hand the new camera to the engine (`Transaction::set_camera`),
//! which stores it as the document's camera (docs/04 §8). The camera math is
//! `document::camera` (docs/06 §7).

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::params::{PointSchema, VectorSchema, check_point, select, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Camera, Document, Projection, Query, StandardView};
use crate::math::Aabb;
use crate::units::{Angle, Length};

fn meta(name: &str, label: &str) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        category: CommandCategory::View,
        label: label.into(),
        view_only: true,
        steps: Vec::new(),
    }
}

/// The camera after a view command.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct CameraOutput {
    /// The new camera.
    pub camera: Camera,
}

/// Projection kinds of `camera-set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectionKind {
    /// Perspective.
    Perspective,
    /// Parallel (orthographic).
    Parallel,
    /// Two-point perspective (vertical lines stay vertical).
    TwoPoint,
}

/// Default field of view when switching from parallel to perspective.
const DEFAULT_FOV_DEG: f64 = 35.0;

fn check_fov(fov: Option<Angle>) -> Result<(), CommandError> {
    if let Some(a) = fov {
        let deg = a.0.to_degrees();
        if !(1.0..=170.0).contains(&deg) {
            return Err(CommandError::OutOfRange {
                name: "fov".into(),
                reason: format!("{deg}° is outside 1°–170°"),
            });
        }
    }
    Ok(())
}

// ----- camera-set ---------------------------------------------------------------------------

/// `camera-set` parameters (docs/05 §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CameraSetParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// New projection (the target keeps its apparent size).
    #[serde(default)]
    pub projection: Option<ProjectionKind>,
    /// Vertical field of view of perspective and two-point views.
    #[serde(default)]
    pub fov: Option<Angle>,
    /// Optional complete camera eye, mm (Sheets view editing).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub eye: Option<DVec3>,
    /// Optional camera target, mm.
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub target: Option<DVec3>,
    /// Optional camera up vector.
    #[serde(default)]
    #[schemars(with = "Option<VectorSchema>")]
    pub up: Option<DVec3>,
    /// Visible parallel projection height, mm.
    #[serde(default)]
    pub height: Option<Length>,
    /// Viewport width/height ratio.
    #[serde(default)]
    pub aspect: Option<f64>,
}

/// The `camera-set` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct CameraSet;

impl Command for CameraSet {
    type Params = CameraSetParams;
    type Output = CameraOutput;

    fn name(&self) -> &'static str {
        "camera-set"
    }

    fn validate(&self, doc: &Document, p: &CameraSetParams) -> Result<(), CommandError> {
        check_fov(p.fov)?;
        if let Some(point) = p.eye {
            check_point("eye", point)?;
        }
        if let Some(point) = p.target {
            check_point("target", point)?;
        }
        let eye = p.eye.unwrap_or(doc.camera.eye);
        let target = p.target.unwrap_or(doc.camera.target);
        if (eye - target).length() < doc.tolerances().length {
            return Err(CommandError::invalid_param(
                "target",
                "must differ from the eye",
            ));
        }
        if p.up
            .is_some_and(|u| !u.is_finite() || u.length() < doc.tolerances().angle)
        {
            return Err(CommandError::invalid_param(
                "up",
                "must be a finite nonzero vector",
            ));
        }
        if p.height
            .is_some_and(|h| !h.as_mm().is_finite() || h.as_mm() <= 0.0)
        {
            return Err(CommandError::invalid_param(
                "height",
                "must be positive and finite",
            ));
        }
        if p.aspect.is_some_and(|a| !a.is_finite() || a <= 0.0) {
            return Err(CommandError::invalid_param(
                "aspect",
                "must be positive and finite",
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: CameraSetParams,
    ) -> Result<CameraOutput, CommandError> {
        let mut camera = tx.camera();
        if let Some(eye) = p.eye {
            camera.eye = eye;
        }
        if let Some(target) = p.target {
            camera.target = target;
        }
        if let Some(up) = p.up {
            camera.up = up.normalize_or(DVec3::Z);
        }
        if let Some(aspect) = p.aspect {
            camera.aspect = aspect;
        }
        let distance = camera.distance().max(1.0);
        let current_fov = match camera.projection {
            Projection::Perspective { fov_y_deg } | Projection::TwoPoint { fov_y_deg } => fov_y_deg,
            Projection::Parallel { .. } => DEFAULT_FOV_DEG,
        };
        let fov = p.fov.map_or(current_fov, |a| a.0.to_degrees());
        let half = |deg: f64| libm::tan(deg.to_radians() * 0.5);
        let kind = p.projection.unwrap_or(match camera.projection {
            Projection::Perspective { .. } => ProjectionKind::Perspective,
            Projection::Parallel { .. } => ProjectionKind::Parallel,
            Projection::TwoPoint { .. } => ProjectionKind::TwoPoint,
        });
        camera.projection = match (camera.projection, kind) {
            (
                Projection::Parallel { height_mm },
                ProjectionKind::Perspective | ProjectionKind::TwoPoint,
            ) => {
                // Keep the visible height at the target.
                let d = height_mm * 0.5 / half(fov);
                camera.eye = camera.target - camera.forward() * d;
                if kind == ProjectionKind::Perspective {
                    Projection::Perspective { fov_y_deg: fov }
                } else {
                    Projection::TwoPoint { fov_y_deg: fov }
                }
            }
            (Projection::Parallel { height_mm }, ProjectionKind::Parallel) => {
                Projection::Parallel { height_mm }
            }
            (_, ProjectionKind::Parallel) => Projection::Parallel {
                height_mm: 2.0 * distance * half(current_fov),
            },
            (_, ProjectionKind::Perspective) => Projection::Perspective { fov_y_deg: fov },
            (_, ProjectionKind::TwoPoint) => Projection::TwoPoint { fov_y_deg: fov },
        };
        if let Some(height) = p.height {
            camera.projection = Projection::Parallel {
                height_mm: height.as_mm(),
            };
        }
        tx.set_camera(camera);
        Ok(CameraOutput { camera })
    }

    fn meta(&self) -> CommandMeta {
        meta("camera-set", "Camera Projection")
    }
}

// ----- camera-standard ----------------------------------------------------------------------

/// `camera-standard` parameters (docs/05 §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CameraStandardParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The view.
    pub view: StandardView,
    /// Also fit the model in the view.
    #[serde(default)]
    pub extents: bool,
}

/// The `camera-standard` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct CameraStandard;

impl Command for CameraStandard {
    type Params = CameraStandardParams;
    type Output = CameraOutput;

    fn name(&self) -> &'static str {
        "camera-standard"
    }

    fn validate(&self, _doc: &Document, _p: &CameraStandardParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: CameraStandardParams,
    ) -> Result<CameraOutput, CommandError> {
        let mut camera = tx.camera();
        camera.standard_view(p.view);
        if p.extents {
            camera.zoom_extents(&tx.document().aabb(), ZOOM_MARGIN);
        }
        tx.set_camera(camera);
        Ok(CameraOutput { camera })
    }

    fn meta(&self) -> CommandMeta {
        meta("camera-standard", "Standard View")
    }
}

// ----- position-camera ----------------------------------------------------------------------

/// `position-camera` parameters (docs/05 §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PositionCameraParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Eye position.
    #[schemars(with = "PointSchema")]
    pub eye: DVec3,
    /// Point looked at.
    #[schemars(with = "PointSchema")]
    pub target: DVec3,
}

/// The `position-camera` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct PositionCamera;

impl Command for PositionCamera {
    type Params = PositionCameraParams;
    type Output = CameraOutput;

    fn name(&self) -> &'static str {
        "position-camera"
    }

    fn validate(&self, doc: &Document, p: &PositionCameraParams) -> Result<(), CommandError> {
        check_point("eye", p.eye)?;
        check_point("target", p.target)?;
        if doc.tolerances().same_point(p.eye, p.target) {
            return Err(CommandError::invalid_param(
                "target",
                "must differ from the eye",
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: PositionCameraParams,
    ) -> Result<CameraOutput, CommandError> {
        let mut camera = tx.camera();
        camera.eye = p.eye;
        camera.target = p.target;
        tx.set_camera(camera);
        Ok(CameraOutput { camera })
    }

    fn meta(&self) -> CommandMeta {
        meta("position-camera", "Position Camera")
    }
}

// ----- orbit / pan / zoom -------------------------------------------------------------------

/// `orbit` parameters (docs/05 §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OrbitParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Turn about the vertical axis (counter-clockwise seen from above).
    #[serde(default)]
    pub yaw: Angle,
    /// Tilt: positive raises the view direction.
    #[serde(default)]
    pub pitch: Angle,
    /// Point orbited about (default: the camera target).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub pivot: Option<DVec3>,
}

/// The `orbit` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Orbit;

impl Command for Orbit {
    type Params = OrbitParams;
    type Output = CameraOutput;

    fn name(&self) -> &'static str {
        "orbit"
    }

    fn validate(&self, _doc: &Document, p: &OrbitParams) -> Result<(), CommandError> {
        if let Some(pivot) = p.pivot {
            check_point("pivot", pivot)?;
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: OrbitParams,
    ) -> Result<CameraOutput, CommandError> {
        let mut camera = tx.camera();
        camera.orbit(p.pivot.unwrap_or(camera.target), p.yaw.0, p.pitch.0);
        tx.set_camera(camera);
        Ok(CameraOutput { camera })
    }

    fn meta(&self) -> CommandMeta {
        meta("orbit", "Orbit")
    }
}

/// `pan` parameters (docs/05 §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PanParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Displacement of eye and target.
    #[schemars(with = "VectorSchema")]
    pub delta: DVec3,
}

/// The `pan` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Pan;

impl Command for Pan {
    type Params = PanParams;
    type Output = CameraOutput;

    fn name(&self) -> &'static str {
        "pan"
    }

    fn validate(&self, _doc: &Document, p: &PanParams) -> Result<(), CommandError> {
        check_point("delta", p.delta)
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: PanParams) -> Result<CameraOutput, CommandError> {
        let mut camera = tx.camera();
        camera.pan(p.delta);
        tx.set_camera(camera);
        Ok(CameraOutput { camera })
    }

    fn meta(&self) -> CommandMeta {
        meta("pan", "Pan")
    }
}

/// `zoom` parameters (docs/05 §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZoomParams {
    /// Factor (> 1 zooms in).
    pub factor: f64,
    /// Point that stays in place on screen (default: the target).
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub toward: Option<DVec3>,
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
}

/// The `zoom` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Zoom;

impl Command for Zoom {
    type Params = ZoomParams;
    type Output = CameraOutput;

    fn name(&self) -> &'static str {
        "zoom"
    }

    fn validate(&self, _doc: &Document, p: &ZoomParams) -> Result<(), CommandError> {
        if !(p.factor.is_finite() && p.factor > 0.0) {
            return Err(CommandError::invalid_param(
                "factor",
                "must be a positive number",
            ));
        }
        if let Some(t) = p.toward {
            check_point("toward", t)?;
        }
        Ok(())
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: ZoomParams) -> Result<CameraOutput, CommandError> {
        let mut camera = tx.camera();
        camera.zoom(p.factor, p.toward);
        tx.set_camera(camera);
        Ok(CameraOutput { camera })
    }

    fn meta(&self) -> CommandMeta {
        meta("zoom", "Zoom")
    }
}

// ----- zoom-extents -------------------------------------------------------------------------

/// Margin around the fitted bounds (docs/06 §7: 5 %).
const ZOOM_MARGIN: f64 = 0.05;

/// `zoom-extents` parameters (docs/05 §9; with `targets` it is "zoom to
/// selection").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZoomExtentsParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Fit these entities instead of the whole model.
    #[serde(default)]
    pub targets: Option<Query>,
}

/// The `zoom-extents` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct ZoomExtents;

impl Command for ZoomExtents {
    type Params = ZoomExtentsParams;
    type Output = CameraOutput;

    fn name(&self) -> &'static str {
        "zoom-extents"
    }

    fn validate(&self, _doc: &Document, p: &ZoomExtentsParams) -> Result<(), CommandError> {
        if let Some(q) = &p.targets {
            q.parse()
                .map_err(|e| CommandError::invalid_param("targets", e.to_string()))?;
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ZoomExtentsParams,
    ) -> Result<CameraOutput, CommandError> {
        let bounds = match &p.targets {
            None => tx.document().aabb(),
            Some(q) => {
                let target = target_container(tx, &p.common)?;
                let selection = select(tx, &target.context, "targets", q)?;
                selection
                    .iter()
                    .filter_map(|path| tx.document().occurrence_aabb(path))
                    .fold(Aabb::EMPTY, |a, b| a.union(&b))
            }
        };
        if bounds.is_empty() {
            return Err(CommandError::NotApplicable(
                "there is nothing to fit".into(),
            ));
        }
        let mut camera = tx.camera();
        camera.zoom_extents(&bounds, ZOOM_MARGIN);
        tx.set_camera(camera);
        Ok(CameraOutput { camera })
    }

    fn meta(&self) -> CommandMeta {
        meta("zoom-extents", "Zoom Extents")
    }
}
