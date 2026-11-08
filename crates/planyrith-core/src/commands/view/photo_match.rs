//! Implements docs/05-modeling-tools.md §9.1 (camera from a photograph):
//! `camera-match` sets the camera to the one that took a picture, from two
//! picture lines along the model's X axis, two along its Y axis, the origin's
//! picture point and a known length or the eye height. The geometry is
//! `math::vanishing`. Like the other view commands it is not undoable.

use glam::DVec2;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Camera, Document, Projection};
use crate::math::Axis;
use crate::math::vanishing::{PictureCamera, Scale, solve};
use crate::units::Length;

fn default_eye_height() -> Length {
    Length(1600.0)
}

fn default_axis() -> Axis {
    Axis::X
}

/// `camera-match` parameters (docs/05 §9.1). Picture points are pixels from
/// the picture's top left corner, `v` down.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CameraMatchParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The picture's width and height (pixels).
    pub image_size: [f64; 2],
    /// A picture line along the model's X axis: u1, v1, u2, v2 (pixels).
    pub x_line_1: [f64; 4],
    /// Another picture line along X (pixels).
    pub x_line_2: [f64; 4],
    /// A picture line along the model's Y axis (pixels).
    pub y_line_1: [f64; 4],
    /// Another picture line along Y (pixels).
    pub y_line_2: [f64; 4],
    /// The model origin's picture point: u, v (pixels).
    pub origin: [f64; 2],
    /// A known length from the origin along `length_axis`, to `length_end`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<Length>,
    /// The axis the known length runs along.
    #[serde(default = "default_axis")]
    pub length_axis: Axis,
    /// The picture point at the known length's far end: u, v (pixels).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_end: Option<[f64; 2]>,
    /// Without a length: the eye's height above the origin.
    #[serde(default = "default_eye_height")]
    pub eye_height: Length,
}

/// `camera-match` output.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct CameraMatchOutput {
    /// The new camera.
    pub camera: Camera,
    /// The focal length found (pixels of the picture).
    pub focal_px: f64,
    /// The vertical field of view (degrees).
    pub fov_y_deg: f64,
}

/// The `camera-match` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct CameraMatch;

fn line(l: [f64; 4]) -> [DVec2; 2] {
    [DVec2::new(l[0], l[1]), DVec2::new(l[2], l[3])]
}

/// The picture's camera for `p`.
pub fn picture_camera(p: &CameraMatchParams) -> Result<PictureCamera, CommandError> {
    let [w, h] = p.image_size;
    if !(w >= 1.0 && h >= 1.0 && w.is_finite() && h.is_finite()) {
        return Err(CommandError::invalid_param(
            "image_size",
            "width and height of at least one pixel",
        ));
    }
    let lines = [p.x_line_1, p.x_line_2, p.y_line_1, p.y_line_2];
    let mut numbers = lines
        .iter()
        .flatten()
        .chain(p.origin.iter())
        .chain(p.length_end.iter().flatten());
    if numbers.any(|v| !v.is_finite()) {
        return Err(CommandError::invalid_param(
            "x_line_1",
            "picture points must be finite numbers",
        ));
    }
    let scale = match (p.length, p.length_end) {
        (Some(mm), Some(end)) => Scale::Length {
            axis: p.length_axis.index(),
            end: DVec2::new(end[0], end[1]),
            mm: mm.0,
        },
        (None, None) => Scale::EyeHeight(p.eye_height.0),
        _ => {
            return Err(CommandError::invalid_param(
                "length",
                "give both length and length_end, or neither",
            ));
        }
    };
    solve(
        DVec2::new(w, h),
        [line(p.x_line_1), line(p.x_line_2)],
        [line(p.y_line_1), line(p.y_line_2)],
        DVec2::new(p.origin[0], p.origin[1]),
        scale,
    )
    .map_err(|e| CommandError::NotApplicable(e.to_string()))
}

impl Command for CameraMatch {
    type Params = CameraMatchParams;
    type Output = CameraMatchOutput;

    fn name(&self) -> &'static str {
        "camera-match"
    }

    fn validate(&self, _: &Document, p: &CameraMatchParams) -> Result<(), CommandError> {
        picture_camera(p).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: CameraMatchParams,
    ) -> Result<CameraMatchOutput, CommandError> {
        let found = picture_camera(&p)?;
        let camera = Camera {
            eye: found.eye,
            target: found.eye + found.forward * found.depth.max(1.0),
            up: found.up,
            projection: Projection::Perspective {
                fov_y_deg: found.fov_y_deg,
            },
            aspect: p.image_size[0] / p.image_size[1],
        };
        tx.set_camera(camera);
        Ok(CameraMatchOutput {
            camera,
            focal_px: found.focal_px,
            fov_y_deg: found.fov_y_deg,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "camera-match".into(),
            category: CommandCategory::View,
            label: "Match Photo".into(),
            view_only: true,
            steps: Vec::new(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;

    #[test]
    fn the_camera_follows_the_picture_and_bad_lines_change_nothing() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        // A camera at (-3000, -4000, 1600) looking at (1000, 1500, 900),
        // focal length 1400 px, 1600 × 1200 picture (math::vanishing tests).
        let size = glam::DVec2::new(1600.0, 1200.0);
        let eye = glam::DVec3::new(-3000.0, -4000.0, 1600.0);
        let fw = (glam::DVec3::new(1000.0, 1500.0, 900.0) - eye).normalize();
        let right = fw.cross(glam::DVec3::Z).normalize();
        let up = right.cross(fw);
        let pic = |x: f64, y: f64, z: f64| {
            let d = glam::DVec3::new(x, y, z) - eye;
            let (a, b, c) = (d.dot(right), d.dot(up), d.dot(fw));
            [size.x * 0.5 + 1400.0 * a / c, size.y * 0.5 - 1400.0 * b / c]
        };
        let seg = |a: [f64; 2], b: [f64; 2]| json!([a[0], a[1], b[0], b[1]]);
        let params = json!({
            "image_size": [1600, 1200],
            "x_line_1": seg(pic(0.0, 0.0, 0.0), pic(2000.0, 0.0, 0.0)),
            "x_line_2": seg(pic(0.0, 0.0, 2500.0), pic(2000.0, 0.0, 2500.0)),
            "y_line_1": seg(pic(0.0, 0.0, 0.0), pic(0.0, 3000.0, 0.0)),
            "y_line_2": seg(pic(0.0, 0.0, 2500.0), pic(0.0, 3000.0, 2500.0)),
            "origin": pic(0.0, 0.0, 0.0),
            "length": 2000.0,
            "length_end": pic(2000.0, 0.0, 0.0),
        });
        let out = e.execute_erased("camera-match", params.clone()).unwrap();
        let got: Vec<f64> = serde_json::from_value(out["camera"]["eye"].clone()).unwrap();
        assert!(glam::DVec3::from_slice(&got).distance(eye) < 1e-6, "{out}");
        assert!((out["focal_px"].as_f64().unwrap() - 1400.0).abs() < 1e-6);
        assert!(e.document().camera.eye.distance(eye) < 1e-6);

        let before = e.document().camera;
        let mut parallel = params.clone();
        parallel["x_line_2"] = json!([0, 100, 1600, 100]);
        parallel["x_line_1"] = json!([0, 200, 1600, 200]);
        let err = e.execute_erased("camera-match", parallel).unwrap_err();
        assert!(err.to_string().contains("parallel"), "{err}");
        let mut half = params;
        half.as_object_mut().unwrap().remove("length");
        assert!(e.execute_erased("camera-match", half).is_err());
        assert_eq!(e.document().camera, before);
    }
}
