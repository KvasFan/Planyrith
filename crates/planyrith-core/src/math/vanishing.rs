//! Implements docs/05-modeling-tools.md §9.1 (camera from a photograph): the
//! camera that took a picture, from two pairs of picture lines that are
//! parallel to the model's X and Y axes in reality. Each pair meets at a
//! vanishing point; with the picture's center as the principal point, the two
//! vanishing points give the focal length and the camera's orientation, the
//! origin's picture point gives the direction to the model origin, and a known
//! length (or the eye height) gives the distance.
//!
//! Picture coordinates are pixels, `u` to the right and `v` down from the top
//! left corner. Model +X runs from the origin toward the X lines' vanishing
//! point; +Y is the horizontal direction that makes +Z point up in the
//! picture.

use glam::{DMat3, DVec2, DVec3};

/// Why no camera matches the lines.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VanishingError {
    /// The two lines of an axis are parallel in the picture (or one is a
    /// point), so they have no vanishing point.
    #[error("the {0} lines are parallel in the picture: draw them along edges that converge")]
    Parallel(char),
    /// The vanishing points do not belong to a real camera (they lie on the
    /// same side of the picture's center).
    #[error(
        "the X and Y vanishing points give no real camera: the lines must follow \
         two perpendicular directions"
    )]
    NoCamera,
    /// The scale cannot be found from the given points.
    #[error("{0}")]
    Scale(String),
}

/// What sets the camera's distance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    /// A length along a model axis (0 = X, 1 = Y, 2 = Z) from the origin to
    /// the picture point `end`, in millimeters.
    Length {
        /// The axis, 0–2.
        axis: usize,
        /// The picture point at the length's far end.
        end: DVec2,
        /// The length (mm).
        mm: f64,
    },
    /// The eye's height above the origin (mm).
    EyeHeight(f64),
}

/// The camera found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureCamera {
    /// Eye position (mm).
    pub eye: DVec3,
    /// Unit view direction.
    pub forward: DVec3,
    /// Unit screen up.
    pub up: DVec3,
    /// The distance from the eye to the origin's depth along the view (mm).
    pub depth: f64,
    /// Focal length in pixels.
    pub focal_px: f64,
    /// Vertical field of view (degrees).
    pub fov_y_deg: f64,
}

/// Picture point → coordinates about the center, y up.
fn centered(size: DVec2, p: DVec2) -> DVec2 {
    DVec2::new(p.x - size.x * 0.5, size.y * 0.5 - p.y)
}

/// The meeting point of two lines (each two points), or `None` when they
/// are parallel. Coordinates are divided by `unit` for conditioning.
fn meet(a: [DVec2; 2], b: [DVec2; 2], unit: f64) -> Option<DVec2> {
    let h = |p: DVec2| DVec3::new(p.x / unit, p.y / unit, 1.0);
    let la = h(a[0]).cross(h(a[1]));
    let lb = h(b[0]).cross(h(b[1]));
    if la.truncate().length() < 1e-12 || lb.truncate().length() < 1e-12 {
        return None;
    }
    let v = la.normalize().cross(lb.normalize());
    // Nearly parallel lines meet absurdly far away: farther than a thousand
    // picture sizes means no usable focal length.
    (v.z.abs() > 1e-3 * v.truncate().length()).then(|| v.truncate() / v.z * unit)
}

/// The camera that sees `x_lines` and `y_lines` (each two picture segments)
/// as parallels of the model's X and Y axes, with the model origin at
/// picture point `origin`, in a picture `size` pixels wide and high.
pub fn solve(
    size: DVec2,
    x_lines: [[DVec2; 2]; 2],
    y_lines: [[DVec2; 2]; 2],
    origin: DVec2,
    scale: Scale,
) -> Result<PictureCamera, VanishingError> {
    let unit = size.max_element().max(1.0);
    let c = |p: DVec2| centered(size, p);
    let cl = |l: [DVec2; 2]| [c(l[0]), c(l[1])];
    let vx = meet(cl(x_lines[0]), cl(x_lines[1]), unit).ok_or(VanishingError::Parallel('X'))?;
    let vy = meet(cl(y_lines[0]), cl(y_lines[1]), unit).ok_or(VanishingError::Parallel('Y'))?;
    let f2 = -vx.dot(vy);
    if f2 <= (unit * 1e-3) * (unit * 1e-3) {
        return Err(VanishingError::NoCamera);
    }
    let f = f2.sqrt();
    // Camera coordinates: x right, y up, z toward the viewer.
    let ray = |p: DVec2| DVec3::new(p.x, p.y, -f).normalize();
    let rx = ray(vx);
    let mut rz = rx.cross(ray(vy)).normalize();
    // +Y is whichever horizontal direction makes +Z point up.
    if rz.y < 0.0 {
        rz = -rz;
    }
    let ry = rz.cross(rx).normalize();
    // Columns: the model axes in camera coordinates.
    let r = DMat3::from_cols(rx, ry, rz);
    let t = ray(c(origin));
    let s = match scale {
        Scale::Length { axis, end, mm } => {
            if !mm.is_finite() || mm <= 0.0 || axis > 2 {
                return Err(VanishingError::Scale("the length must be positive".into()));
            }
            let a = r.col(axis);
            let q = DVec3::new(c(end).x, c(end).y, -f);
            let n = t.cross(q);
            if n.length_squared() < 1e-12 * q.length_squared() {
                return Err(VanishingError::Scale(
                    "the length's end is on the origin in the picture".into(),
                ));
            }
            (mm * a.cross(q).dot(n) / n.length_squared()).abs()
        }
        Scale::EyeHeight(h) => {
            let down = rz.dot(t);
            if !h.is_finite() || h <= 0.0 {
                return Err(VanishingError::Scale(
                    "the eye height must be positive".into(),
                ));
            }
            if down > -1e-6 {
                return Err(VanishingError::Scale(
                    "the origin is not below the horizon: give a length instead".into(),
                ));
            }
            -h / down
        }
    };
    let rt = r.transpose();
    let eye = -(rt * (t * s));
    let forward = (rt * DVec3::NEG_Z).normalize();
    let up = (rt * DVec3::Y).normalize();
    let fov = 2.0 * libm::atan(size.y * 0.5 / f);
    Ok(PictureCamera {
        eye,
        forward,
        up,
        depth: s * -t.z,
        focal_px: f,
        fov_y_deg: fov.to_degrees(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A picture taken from `eye` looking at `target` (up +Z) with focal
    /// length `f` pixels: model point → picture point.
    fn camera(eye: DVec3, target: DVec3, f: f64, size: DVec2) -> impl Fn(DVec3) -> DVec2 {
        let fw = (target - eye).normalize();
        let right = fw.cross(DVec3::Z).normalize();
        let up = right.cross(fw);
        move |p: DVec3| {
            let d = p - eye;
            let (x, y, z) = (d.dot(right), d.dot(up), d.dot(fw));
            DVec2::new(size.x * 0.5 + f * x / z, size.y * 0.5 - f * y / z)
        }
    }

    fn lines(
        pic: &impl Fn(DVec3) -> DVec2,
    ) -> ([[DVec2; 2]; 2], [[DVec2; 2]; 2], DVec2, DVec2, DVec2) {
        let x = [
            [pic(DVec3::ZERO), pic(DVec3::new(2000.0, 0.0, 0.0))],
            [
                pic(DVec3::new(0.0, 0.0, 2500.0)),
                pic(DVec3::new(2000.0, 0.0, 2500.0)),
            ],
        ];
        let y = [
            [pic(DVec3::ZERO), pic(DVec3::new(0.0, 3000.0, 0.0))],
            [
                pic(DVec3::new(0.0, 0.0, 2500.0)),
                pic(DVec3::new(0.0, 3000.0, 2500.0)),
            ],
        ];
        (
            x,
            y,
            pic(DVec3::ZERO),
            pic(DVec3::new(2000.0, 0.0, 0.0)),
            pic(DVec3::new(0.0, 0.0, 2500.0)),
        )
    }

    #[test]
    fn a_known_camera_is_found_again() {
        let size = DVec2::new(1600.0, 1200.0);
        let eye = DVec3::new(-3000.0, -4000.0, 1600.0);
        let pic = camera(eye, DVec3::new(1000.0, 1500.0, 900.0), 1400.0, size);
        let (x, y, origin, x_end, z_end) = lines(&pic);
        for scale in [
            Scale::Length {
                axis: 0,
                end: x_end,
                mm: 2000.0,
            },
            Scale::Length {
                axis: 2,
                end: z_end,
                mm: 2500.0,
            },
            Scale::EyeHeight(1600.0),
        ] {
            let found = solve(size, x, y, origin, scale).unwrap();
            assert!((found.focal_px - 1400.0).abs() < 1e-6, "{found:?}");
            assert!(found.eye.distance(eye) < 1e-6, "{scale:?}: {found:?}");
            assert!(found.up.dot(DVec3::Z) > 0.5);
            let expected = 2.0 * libm::atan(600.0 / 1400.0);
            assert!((found.fov_y_deg - expected.to_degrees()).abs() < 1e-9);
        }
    }

    #[test]
    fn unusable_lines_are_refused() {
        let size = DVec2::new(1000.0, 1000.0);
        let p = |u: f64, v: f64| DVec2::new(u, v);
        let parallel = [[p(0.0, 0.0), p(100.0, 0.0)], [p(0.0, 50.0), p(100.0, 50.0)]];
        let y = [
            [p(500.0, 900.0), p(400.0, 300.0)],
            [p(600.0, 900.0), p(700.0, 300.0)],
        ];
        let scale = Scale::EyeHeight(1600.0);
        assert_eq!(
            solve(size, parallel, y, p(500.0, 900.0), scale),
            Err(VanishingError::Parallel('X'))
        );
        // Both vanishing points to the right of the center: no camera.
        let x = [
            [p(500.0, 600.0), p(900.0, 550.0)],
            [p(500.0, 400.0), p(900.0, 450.0)],
        ];
        let y = [
            [p(500.0, 700.0), p(900.0, 520.0)],
            [p(500.0, 300.0), p(900.0, 480.0)],
        ];
        assert_eq!(
            solve(size, x, y, p(500.0, 900.0), scale),
            Err(VanishingError::NoCamera)
        );
    }

    proptest! {
        #[test]
        fn random_cameras_are_recovered(
            ex in -9000.0..-1500.0f64, ey in -9000.0..-1500.0f64, ez in 300.0..6000.0f64,
            tx in 0.0..3000.0f64, ty in 0.0..3000.0f64, tz in 0.0..1500.0f64,
            f in 600.0..3000.0f64,
        ) {
            let size = DVec2::new(1600.0, 1000.0);
            let eye = DVec3::new(ex, ey, ez);
            let pic = camera(eye, DVec3::new(tx, ty, tz), f, size);
            let (x, y, origin, x_end, _) = lines(&pic);
            let found = solve(size, x, y, origin, Scale::Length { axis: 0, end: x_end, mm: 2000.0 });
            let found = found.unwrap();
            prop_assert!((found.focal_px - f).abs() < 1e-6 * f);
            prop_assert!(found.eye.distance(eye) < 1e-6 * eye.length());
        }
    }
}
