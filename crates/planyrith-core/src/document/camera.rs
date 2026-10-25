//! Implements docs/04-document-model.md §8 (camera): the active view stored in
//! the document and in scenes, and the GPU-agnostic camera math of
//! docs/06 §3.2 and §7: camera-relative, reversed-Z projections consistent
//! with picking rays, depth fitting, orbit/pan/zoom, zoom extents and standard
//! views. Camera changes are not undoable.

use glam::{DAffine3, DMat4, DQuat, DVec3, DVec4};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::math::{Aabb, Ray};

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

    /// Unit right vector (`forward × up`). Looking along the up vector (a
    /// top or bottom view with up = +Z, docs/06 §7) the screen axes come from
    /// the world axis least aligned with the view instead: a top view has
    /// right = +X and screen up = +Y, like a plan.
    pub fn right(&self) -> DVec3 {
        let f = self.forward();
        let right = f.cross(self.up);
        if right.length_squared() > 1e-18 {
            return right.normalize();
        }
        let axis = [DVec3::Y, DVec3::X, DVec3::Z]
            .into_iter()
            .min_by(|a, b| a.dot(f).abs().total_cmp(&b.dot(f).abs()))
            .unwrap_or(DVec3::Y);
        f.cross(axis).normalize_or_zero()
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
            Projection::TwoPoint { fov_y_deg } => {
                // Vertical image plane (verticals stay vertical, docs/06 §7)
                // with the vertical shear that keeps the target centered.
                let half_h = libm::tan(fov_y_deg.to_radians() * 0.5);
                let half_w = half_h * self.aspect;
                let up = self.up.normalize_or_zero();
                let level = (f - up * f.dot(up)).normalize_or_zero();
                if level == DVec3::ZERO {
                    // Looking straight up or down: no horizon to keep level.
                    return Ray::new(self.eye, f + r * (ndc.x * half_w) + u * (ndc.y * half_h));
                }
                let side = level.cross(up).normalize_or_zero();
                let shear = f.dot(up) / f.dot(level);
                let dir = level + side * (ndc.x * half_w) + up * (ndc.y * half_h + shear);
                Ray::new(self.eye, dir)
            }
            Projection::Perspective { fov_y_deg } => {
                let half_h = libm::tan(fov_y_deg.to_radians() * 0.5);
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
                2.0 * t.max(0.0) * libm::tan(fov_y_deg.to_radians() * 0.5) / height_px
            }
            Projection::Parallel { height_mm } => height_mm / height_px,
        }
    }
}

/// Standard view directions (docs/05 §9 `camera-standard`, docs/06 §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StandardView {
    /// From above, looking down −Z.
    Top,
    /// From below, looking up +Z.
    Bottom,
    /// From −Y, looking +Y.
    Front,
    /// From +Y, looking −Y.
    Back,
    /// From −X, looking +X.
    Left,
    /// From +X, looking −X.
    Right,
    /// From the +X+Y side and above, looking along (−1, −1, −1).
    Iso,
}

impl StandardView {
    /// Unit direction from the target to the eye.
    pub fn eye_direction(self) -> DVec3 {
        match self {
            StandardView::Top => DVec3::Z,
            StandardView::Bottom => DVec3::NEG_Z,
            StandardView::Front => DVec3::NEG_Y,
            StandardView::Back => DVec3::Y,
            StandardView::Left => DVec3::NEG_X,
            StandardView::Right => DVec3::X,
            StandardView::Iso => DVec3::ONE.normalize(),
        }
    }
}

/// Depth range of a view (docs/06 §3.2). Perspective views use an infinite
/// far plane (`far` is `f64::INFINITY`); parallel views a finite range that
/// may start behind the eye.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DepthRange {
    /// Near plane distance along the view direction (mm).
    pub near: f64,
    /// Far plane distance (mm).
    pub far: f64,
}

/// Smallest perspective near plane (docs/06 §3.2 "clamped to ≥ 1 mm").
const MIN_NEAR: f64 = 1.0;

/// Orbiting stops this far (radians) from looking straight along the up vector.
const POLE_MARGIN: f64 = 1e-3;

impl Camera {
    /// Unit screen-up vector (`right × forward`).
    pub fn screen_up(&self) -> DVec3 {
        self.right().cross(self.forward())
    }

    /// The world up vector (normalized `up`, +Z when degenerate).
    fn world_up(&self) -> DVec3 {
        self.up.try_normalize().unwrap_or(DVec3::Z)
    }

    /// Half extents of the image plane at unit distance (perspective) or in
    /// mm (parallel): `(half_width, half_height)`.
    fn half_extents(&self) -> (f64, f64) {
        let half_h = match self.projection {
            Projection::Perspective { fov_y_deg } | Projection::TwoPoint { fov_y_deg } => {
                libm::tan(fov_y_deg.to_radians() * 0.5)
            }
            Projection::Parallel { height_mm } => height_mm * 0.5,
        };
        (half_h * self.aspect, half_h)
    }

    /// The horizontal view direction and the vertical shear of a two-point
    /// view, `None` when looking straight along the up vector.
    fn two_point_frame(&self) -> Option<(DVec3, DVec3, DVec3, f64)> {
        let f = self.forward();
        let up = self.world_up();
        let level = (f - up * f.dot(up)).try_normalize()?;
        let side = level.cross(up).normalize_or_zero();
        Some((level, side, up, f.dot(up) / f.dot(level)))
    }

    /// Distance of `p` from the eye along the depth direction of the view
    /// (the horizontal direction for two-point views).
    pub fn depth_of(&self, p: DVec3) -> f64 {
        let v = p - self.eye;
        match (self.projection, self.two_point_frame()) {
            (Projection::TwoPoint { .. }, Some((level, ..))) => level.dot(v),
            _ => self.forward().dot(v),
        }
    }

    /// Depth range enclosing `bounds` (docs/06 §3.2): perspective near plane
    /// at 90 % of the nearest corner, at least 1 mm, and an infinite far
    /// plane; parallel range from the nearest to the farthest corner with a
    /// margin. Empty bounds give a range around the target.
    pub fn fit_depth(&self, bounds: &Aabb) -> DepthRange {
        let (dmin, dmax) = if bounds.is_empty() {
            let d = self.depth_of(self.target);
            (d - 1000.0, d + 1000.0)
        } else {
            bounds
                .corners()
                .iter()
                .map(|&c| self.depth_of(c))
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), d| {
                    (lo.min(d), hi.max(d))
                })
        };
        match self.projection {
            Projection::Parallel { .. } => {
                let margin = (dmax - dmin) * 0.01 + 1.0;
                DepthRange {
                    near: dmin - margin,
                    far: dmax + margin,
                }
            }
            _ => DepthRange {
                near: (dmin * 0.9).max(MIN_NEAR),
                far: f64::INFINITY,
            },
        }
    }

    /// Clip-from-eye-relative matrix (docs/06 §3.2): applied to `p - eye`,
    /// it gives wgpu clip coordinates with reversed Z (depth 1 at the near
    /// plane, decreasing with distance). It inverts
    /// [`Camera::ray_through_ndc`]: a point on the ray through `ndc` projects
    /// back to `ndc`.
    pub fn projection_from_eye(&self, depth: DepthRange) -> DMat4 {
        let (hw, hh) = self.half_extents();
        let rows: [DVec4; 4] = match (self.projection, self.two_point_frame()) {
            (Projection::Parallel { .. }, _) => {
                let f = self.forward();
                let r = self.right();
                let u = self.screen_up();
                let span = (depth.far - depth.near).max(f64::MIN_POSITIVE);
                [
                    (r / hw).extend(0.0),
                    (u / hh).extend(0.0),
                    (-f / span).extend(depth.far / span),
                    DVec4::W,
                ]
            }
            (Projection::TwoPoint { .. }, Some((level, side, up, shear))) => [
                (side / hw).extend(0.0),
                ((up - level * shear) / hh).extend(0.0),
                DVec4::new(0.0, 0.0, 0.0, depth.near),
                level.extend(0.0),
            ],
            _ => {
                let f = self.forward();
                let r = self.right();
                let u = self.screen_up();
                [
                    (r / hw).extend(0.0),
                    (u / hh).extend(0.0),
                    DVec4::new(0.0, 0.0, 0.0, depth.near),
                    f.extend(0.0),
                ]
            }
        };
        DMat4::from_cols_array_2d(&[
            rows[0].to_array(),
            rows[1].to_array(),
            rows[2].to_array(),
            rows[3].to_array(),
        ])
        .transpose()
    }

    /// Clip-from-local matrix of a container occurrence with container-to-world
    /// transform `world`, composed in `f64` relative to the eye so it can be
    /// narrowed to `f32` without jitter far from the origin (docs/06 §3.2).
    pub fn clip_from_local(&self, world: &DAffine3, depth: DepthRange) -> DMat4 {
        let relative = DAffine3 {
            matrix3: world.matrix3,
            translation: world.translation - self.eye,
        };
        self.projection_from_eye(depth) * DMat4::from(relative)
    }

    /// Normalized device coordinates `(x, y, depth)` of a world point, `None`
    /// behind the eye (perspective) or outside the depth range (parallel).
    pub fn project(&self, p: DVec3, depth: DepthRange) -> Option<DVec3> {
        let clip = self.projection_from_eye(depth) * (p - self.eye).extend(1.0);
        (clip.w > 0.0).then(|| clip.truncate() / clip.w)
    }

    /// Orbits about `pivot` (docs/06 §7): `yaw` radians about the world up
    /// vector, then `pitch` radians about the screen right vector (positive
    /// raises the view direction). The view never turns past looking straight
    /// up or down; the up vector stays the world up.
    pub fn orbit(&mut self, pivot: DVec3, yaw: f64, pitch: f64) {
        let up = self.world_up();
        let f = self.forward();
        let elevation = libm::asin(f.dot(up).clamp(-1.0, 1.0));
        let limit = std::f64::consts::FRAC_PI_2 - POLE_MARGIN;
        let pitch = if elevation.abs() >= limit {
            // Already at a pole (a top view): only move away from it.
            if elevation > 0.0 {
                pitch.min(0.0)
            } else {
                pitch.max(0.0)
            }
        } else {
            pitch.clamp(-limit - elevation, limit - elevation)
        };
        let rotation =
            DQuat::from_axis_angle(up, yaw) * DQuat::from_axis_angle(self.right(), pitch);
        self.eye = pivot + rotation * (self.eye - pivot);
        self.target = pivot + rotation * (self.target - pivot);
    }

    /// Moves eye and target by `delta` (world mm).
    pub fn pan(&mut self, delta: DVec3) {
        self.eye += delta;
        self.target += delta;
    }

    /// The world translation that moves the image by `(dx, dy)` pixels (y
    /// down) for points at depth `depth_t` in a viewport `height_px` high:
    /// the point under the cursor stays under it. Pass the result to
    /// [`Camera::pan`].
    pub fn pan_delta_for_pixels(&self, dx: f64, dy: f64, height_px: f64, depth_t: f64) -> DVec3 {
        let per_px = self.world_per_pixel_at(depth_t, height_px);
        -(self.right() * dx - self.screen_up() * dy) * per_px
    }

    /// Zooms by `factor` (> 1 zooms in) toward `toward` (the point under the
    /// cursor, docs/06 §7), or toward the target. Parallel views change their
    /// height and keep `toward` fixed on screen.
    pub fn zoom(&mut self, factor: f64, toward: Option<DVec3>) {
        if !(factor.is_finite() && factor > 0.0) {
            return;
        }
        match &mut self.projection {
            Projection::Parallel { height_mm } => {
                *height_mm /= factor;
                if let Some(p) = toward {
                    let v = p - self.eye;
                    let f = (self.target - self.eye).normalize_or_zero();
                    let shift = (v - f * v.dot(f)) * (1.0 - 1.0 / factor);
                    self.eye += shift;
                    self.target += shift;
                }
            }
            _ => {
                let p = toward.unwrap_or(self.target);
                self.eye = p + (self.eye - p) / factor;
                self.target = p + (self.target - p) / factor;
                if self.eye.distance(self.target) < MIN_NEAR {
                    // Keep a usable orbit distance when zooming through.
                    let f = self.forward();
                    self.target = self.eye + f * MIN_NEAR;
                }
            }
        }
    }

    /// Fits `bounds` in the view with `margin` (0.05 = 5 %, docs/06 §7),
    /// keeping the view direction and projection. Empty bounds change nothing.
    pub fn zoom_extents(&mut self, bounds: &Aabb, margin: f64) {
        if bounds.is_empty() {
            return;
        }
        let center = bounds.center();
        let radius = (bounds.diagonal() * 0.5).max(MIN_NEAR);
        let f = self.forward();
        let f = if f == DVec3::ZERO { DVec3::NEG_Z } else { f };
        let (hw, hh) = self.half_extents();
        let r = self.right();
        match &mut self.projection {
            Projection::Parallel { height_mm } => {
                let u = r.cross(f);
                let (ex, ey) = bounds
                    .corners()
                    .iter()
                    .fold((0.0f64, 0.0f64), |(x, y), &c| {
                        let v = c - center;
                        (x.max(r.dot(v).abs()), y.max(u.dot(v).abs()))
                    });
                let aspect = (hw / hh).max(f64::MIN_POSITIVE);
                *height_mm = (2.0 * ey).max(2.0 * ex / aspect).max(MIN_NEAR) * (1.0 + margin);
                self.target = center;
                self.eye = center - f * (2.0 * radius + MIN_NEAR);
            }
            _ => {
                let half = libm::atan(hh.min(hw));
                let mut distance = radius / libm::sin(half).max(1e-6) * (1.0 + margin);
                self.target = center;
                self.eye = center - f * distance;
                // The sphere fit is exact for perspective views only up to
                // the corners' layout; two-point views shear vertically.
                // Back off until every corner is inside the margin.
                for _ in 0..16 {
                    let depth = self.fit_depth(bounds);
                    let widest = bounds
                        .corners()
                        .iter()
                        .map(|&c| {
                            self.project(c, depth)
                                .map_or(f64::INFINITY, |n| n.x.abs().max(n.y.abs()))
                        })
                        .fold(0.0f64, f64::max);
                    let wanted = 1.0 / (1.0 + margin);
                    if widest <= wanted {
                        break;
                    }
                    distance *= (widest / wanted).clamp(1.01, 4.0);
                    self.eye = center - f * distance;
                }
            }
        }
    }

    /// Looks at the target from a standard direction at the current distance
    /// (docs/06 §7: the projection is kept).
    pub fn standard_view(&mut self, view: StandardView) {
        let distance = self.distance().max(MIN_NEAR);
        self.eye = self.target + view.eye_direction() * distance;
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

        let top = Camera {
            eye: DVec3::new(0.0, 0.0, 10_000.0),
            target: DVec3::ZERO,
            ..c
        };
        let left = top.ray_through_ndc(glam::DVec2::new(-1.0, 0.0));
        let right = top.ray_through_ndc(glam::DVec2::new(1.0, 0.0));
        assert!(
            left.dir.x < 0.0 && right.dir.x > 0.0,
            "a top view has +X to the right"
        );
        let two_point = Camera {
            projection: Projection::TwoPoint { fov_y_deg: 35.0 },
            aspect: 1.0,
            ..c
        };
        // A vertical line maps to one screen column: its rays share the
        // horizontal direction.
        let ray = two_point.ray_through_ndc(glam::DVec2::new(0.3, -0.5));
        let other = two_point.ray_through_ndc(glam::DVec2::new(0.3, 0.5));
        let flat = |d: DVec3| DVec3::new(d.x, d.y, 0.0).normalize();
        assert!((flat(ray.dir) - flat(other.dir)).length() < 1e-12);
        let center = two_point.ray_through_ndc(glam::DVec2::ZERO);
        assert!(center.dir.normalize().distance(two_point.forward()) < 1e-12);

        let ortho = Camera {
            projection: Projection::Parallel { height_mm: 1000.0 },
            ..c
        };
        let a = ortho.ray_through_ndc(glam::DVec2::new(0.0, 1.0));
        assert!((a.dir - ortho.forward()).length() < 1e-12);
        assert!(((a.origin - ortho.eye).length() - 500.0).abs() < 1e-9);
        assert_eq!(ortho.world_per_pixel_at(5.0, 1000.0), 1.0);
    }

    fn cameras() -> Vec<Camera> {
        let base = Camera {
            eye: DVec3::new(3000.0, -4000.0, 2500.0),
            target: DVec3::new(200.0, 100.0, 300.0),
            aspect: 1.5,
            ..Camera::default()
        };
        vec![
            base,
            Camera {
                projection: Projection::Parallel { height_mm: 2400.0 },
                ..base
            },
            Camera {
                projection: Projection::TwoPoint { fov_y_deg: 40.0 },
                ..base
            },
        ]
    }

    #[test]
    fn projection_inverts_picking_rays() {
        let bounds = Aabb::from_points([DVec3::splat(-500.0), DVec3::splat(1500.0)]);
        for c in cameras() {
            let depth = c.fit_depth(&bounds);
            for (x, y) in [(0.0, 0.0), (0.7, -0.4), (-0.9, 0.95)] {
                let ray = c.ray_through_ndc(glam::DVec2::new(x, y));
                for t in [500.0, 2000.0, 7000.0] {
                    let p = ray.origin + ray.dir.normalize() * t;
                    let ndc = c.project(p, depth).unwrap();
                    assert!(
                        (ndc.x - x).abs() < 1e-9 && (ndc.y - y).abs() < 1e-9,
                        "{:?}: {ndc} for ({x}, {y})",
                        c.projection
                    );
                }
            }
            // The same through the local-to-clip matrix of a moved occurrence.
            let world = DAffine3::from_translation(DVec3::new(1e6, -2e6, 0.0));
            let local = DVec3::new(-1e6 + 100.0, 2e6 + 50.0, 20.0);
            let clip = c.clip_from_local(&world, depth) * local.extend(1.0);
            let direct = c.project(world.transform_point3(local), depth).unwrap();
            assert!((clip.truncate() / clip.w - direct).length() < 1e-9);
        }
    }

    #[test]
    fn depth_is_reversed_and_fitted() {
        let bounds = Aabb::from_points([DVec3::splat(-500.0), DVec3::splat(1500.0)]);
        for c in cameras() {
            let depth = c.fit_depth(&bounds);
            let f = c.forward();
            let near = c.project(c.eye + f * (c.depth_of(bounds.center()) - 100.0), depth);
            let far = c.project(c.eye + f * (c.depth_of(bounds.center()) + 100.0), depth);
            let (near, far) = (near.unwrap().z, far.unwrap().z);
            assert!(near > far, "{:?}: {near} <= {far}", c.projection);
            for corner in bounds.corners() {
                let z = c.project(corner, depth).unwrap().z;
                assert!((0.0..=1.0).contains(&z), "{:?}: {z}", c.projection);
            }
        }
        let inside = Camera {
            eye: DVec3::splat(500.0),
            ..Camera::default()
        };
        assert_eq!(inside.fit_depth(&bounds).near, 1.0);
    }

    #[test]
    fn orbit_keeps_distance_and_stops_at_the_poles() {
        let mut c = Camera::default();
        let pivot = DVec3::new(100.0, 0.0, 0.0);
        let before = c.eye.distance(pivot);
        c.orbit(pivot, 0.7, 0.3);
        assert!((c.eye.distance(pivot) - before).abs() < 1e-6);
        assert!(c.up == DVec3::Z);
        for _ in 0..10 {
            c.orbit(pivot, 0.0, -1.0);
        }
        let f = c.forward();
        assert!(f.z < -0.99 && f.z > -1.0, "{f}");
        assert!(c.right().length() > 0.99);
        // At the pole only moves away from it are taken.
        let at_pole = c;
        c.orbit(pivot, 0.0, -0.5);
        assert!((c.forward() - at_pole.forward()).length() < 1e-9);
        c.orbit(pivot, 0.0, 0.5);
        assert!(c.forward().z > at_pole.forward().z);
    }

    #[test]
    fn zoom_keeps_the_cursor_point_and_extents_fit() {
        let bounds = Aabb::from_points([DVec3::ZERO, DVec3::new(6000.0, 4000.0, 2700.0)]);
        for mut c in cameras() {
            let depth = c.fit_depth(&bounds);
            let p = DVec3::new(1000.0, 500.0, 0.0);
            let before = c.project(p, depth).unwrap();
            c.zoom(1.5, Some(p));
            let after = c.project(p, c.fit_depth(&bounds)).unwrap();
            assert!((before.truncate() - after.truncate()).length() < 1e-9);

            c.zoom_extents(&bounds, 0.05);
            let depth = c.fit_depth(&bounds);
            let mut widest = 0.0f64;
            for corner in bounds.corners() {
                let ndc = c.project(corner, depth).unwrap();
                assert!(
                    ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0,
                    "{:?}: {ndc}",
                    c.projection
                );
                widest = widest.max(ndc.x.abs()).max(ndc.y.abs());
            }
            assert!(
                widest > 0.5,
                "{:?} fits too loosely: {widest}",
                c.projection
            );
        }
    }

    #[test]
    fn pan_follows_the_cursor_and_standard_views_keep_distance() {
        let mut c = Camera::default();
        let depth = c.fit_depth(&Aabb::from_points([DVec3::ZERO, DVec3::splat(100.0)]));
        let p = c.target;
        let before = c.project(p, depth).unwrap();
        let t = c.depth_of(p);
        c.pan(c.pan_delta_for_pixels(40.0, -25.0, 800.0, t));
        let after = c.project(p, depth).unwrap();
        // 40 px right and 25 px up in a 800 px high viewport.
        assert!((after.x - before.x - 80.0 / 800.0 / c.aspect).abs() < 1e-9);
        assert!((after.y - before.y - 50.0 / 800.0).abs() < 1e-9);

        let distance = c.distance();
        c.standard_view(StandardView::Top);
        assert!((c.distance() - distance).abs() < 1e-9);
        assert!((c.forward() + DVec3::Z).length() < 1e-12);
        assert!((c.right() - DVec3::X).length() < 1e-12);
        c.standard_view(StandardView::Front);
        assert!((c.forward() - DVec3::Y).length() < 1e-12);
    }
}
