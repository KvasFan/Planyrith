//! Implements docs/04-document-model.md §8 (camera): the active view stored in
//! the document and in scenes. Camera changes are not undoable.

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::math::Ray;

/// Projection model (docs/04 §8).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Projection {
    /// Perspective with a vertical field of view in degrees.
    Perspective {
        /// Vertical field of view in degrees.
        fov_y_deg: f64,
    },
    /// Parallel (orthographic) with the visible height in mm.
    Parallel {
        /// Visible height in mm.
        height_mm: f64,
    },
    /// Two-point perspective (vertical lines stay vertical).
    TwoPoint {
        /// Vertical field of view in degrees.
        fov_y_deg: f64,
    },
}

/// A camera (docs/04 §8).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Camera {
    /// Eye position (mm).
    #[schemars(with = "[f64; 3]")]
    pub eye: DVec3,
    /// Look-at target (mm).
    #[schemars(with = "[f64; 3]")]
    pub target: DVec3,
    /// Up vector (normally +Z).
    #[schemars(with = "[f64; 3]")]
    pub up: DVec3,
    /// Projection.
    pub projection: Projection,
    /// Width / height, provided by the frontend.
    pub aspect: f64,
}

impl Default for Camera {
    /// A deterministic three-quarter view of the origin.
    fn default() -> Self {
        Camera {
            eye: DVec3::new(5000.0, -5000.0, 4000.0),
            target: DVec3::ZERO,
            up: DVec3::Z,
            projection: Projection::Perspective { fov_y_deg: 35.0 },
            aspect: 16.0 / 9.0,
        }
    }
}

impl Camera {
    /// Unit view direction.
    pub fn forward(&self) -> DVec3 {
        (self.target - self.eye).normalize_or_zero()
    }

    /// Unit right vector (`forward × up`).
    pub fn right(&self) -> DVec3 {
        self.forward().cross(self.up).normalize_or_zero()
    }

    /// Distance from eye to target.
    pub fn distance(&self) -> f64 {
        self.eye.distance(self.target)
    }

    /// World ray through normalized device coordinates `(x, y) ∈ [-1, 1]²`
    /// (y up), used by picking and the CLI `infer` verb (docs/03 §7).
    pub fn ray_through_ndc(&self, ndc: glam::DVec2) -> Ray {
        let f = self.forward();
        let r = self.right();
        let u = r.cross(f);
        match self.projection {
            Projection::Perspective { fov_y_deg } | Projection::TwoPoint { fov_y_deg } => {
                let half_h = (fov_y_deg.to_radians() * 0.5).tan();
                let half_w = half_h * self.aspect;
                Ray::new(self.eye, f + r * (ndc.x * half_w) + u * (ndc.y * half_h))
            }
            Projection::Parallel { height_mm } => {
                let half_h = height_mm * 0.5;
                let half_w = half_h * self.aspect;
                Ray::new(self.eye + r * (ndc.x * half_w) + u * (ndc.y * half_h), f)
            }
        }
    }

    /// World-space size of one pixel at depth `t` along the view direction for a
    /// viewport of `height_px` pixels (docs/03 §7 pixel tolerance conversion).
    pub fn world_per_pixel_at(&self, t: f64, height_px: f64) -> f64 {
        if height_px <= 0.0 {
            return 0.0;
        }
        match self.projection {
            Projection::Perspective { fov_y_deg } | Projection::TwoPoint { fov_y_deg } => {
                2.0 * t.max(0.0) * (fov_y_deg.to_radians() * 0.5).tan() / height_px
            }
            Projection::Parallel { height_mm } => height_mm / height_px,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_camera_is_well_formed() {
        let c = Camera::default();
        assert!((c.forward().length() - 1.0).abs() < 1e-12);
        assert!(c.right().length() > 0.99);
        assert!(c.distance() > 0.0);
    }

    #[test]
    fn center_ray_looks_at_the_target_and_pixels_scale_with_depth() {
        let c = Camera::default();
        let ray = c.ray_through_ndc(glam::DVec2::ZERO);
        assert!((ray.dir - c.forward()).length() < 1e-12);
        let top = c.ray_through_ndc(glam::DVec2::new(0.0, 1.0));
        let angle = top.dir.angle_between(ray.dir).to_degrees();
        assert!((angle - 17.5).abs() < 1e-9);
        assert!(
            (c.world_per_pixel_at(2.0, 100.0) - 2.0 * c.world_per_pixel_at(1.0, 100.0)).abs()
                < 1e-15
        );

        let ortho = Camera {
            projection: Projection::Parallel { height_mm: 1000.0 },
            ..c
        };
        let a = ortho.ray_through_ndc(glam::DVec2::new(0.0, 1.0));
        assert!((a.dir - ortho.forward()).length() < 1e-12);
        assert!(((a.origin - ortho.eye).length() - 500.0).abs() < 1e-9);
        assert_eq!(ortho.world_per_pixel_at(5.0, 1000.0), 1.0);
    }
}
