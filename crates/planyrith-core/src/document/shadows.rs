//! Implements docs/04-document-model.md §7 `ShadowSettings` and the sun model
//! used by rendering (docs/06) and scenes (docs/04 §6).

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::units::Angle;

/// A calendar date (no time-zone), kept dependency-free.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct Date {
    /// Proleptic Gregorian year.
    pub year: i32,
    /// 1–12.
    pub month: u8,
    /// 1–31.
    pub day: u8,
}

/// A local time of day.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct TimeOfDay {
    /// 0–23.
    pub hour: u8,
    /// 0–59.
    pub minute: u8,
}

/// Sun position model (docs/04 §7 `SunModel`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SunModel {
    /// Computed from geo-location, date and time.
    Geo {
        /// Date.
        date: Date,
        /// Local time.
        time: TimeOfDay,
        /// Latitude in degrees (north positive).
        lat: f64,
        /// Longitude in degrees (east positive).
        lon: f64,
        /// UTC offset in hours.
        utc_offset: f64,
        /// Model north relative to +Y.
        north_angle: Angle,
    },
    /// Explicit direction.
    Manual {
        /// Azimuth from north, clockwise.
        azimuth: Angle,
        /// Altitude above the horizon.
        altitude: Angle,
    },
}

/// Shadow settings (docs/04 §7).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShadowSettings {
    /// Shadows on/off.
    pub enabled: bool,
    /// Sun model.
    pub sun: SunModel,
    /// Light intensity 0–100.
    pub light: u8,
    /// Dark (shadow) intensity 0–100.
    pub dark: u8,
    /// Cast shadows on faces.
    pub on_faces: bool,
    /// Cast shadows on the ground plane.
    pub on_ground: bool,
    /// Edges cast shadows.
    pub from_edges: bool,
}

impl Default for ShadowSettings {
    fn default() -> Self {
        ShadowSettings {
            enabled: false,
            sun: SunModel::Manual {
                azimuth: Angle::degrees(135.0),
                altitude: Angle::degrees(45.0),
            },
            light: 80,
            dark: 45,
            on_faces: true,
            on_ground: true,
            from_edges: false,
        }
    }
}

impl SunModel {
    /// Unit direction *toward* the sun in model space (Z up, +Y = north rotated
    /// by `north_angle`); solar position algorithm accuracy target 0.5°
    /// (docs/13 milestone 0.6).
    ///
    /// Azimuths and `north_angle` turn clockwise seen from above (north → east),
    /// so model north is `(sin a, cos a, 0)` for `north_angle = a`. The geo
    /// model uses the NOAA low-precision solar position series (declination
    /// and equation of time from the fractional year).
    pub fn direction(&self) -> DVec3 {
        match *self {
            SunModel::Manual { azimuth, altitude } => {
                let (az, alt) = (azimuth.as_radians(), altitude.as_radians());
                DVec3::new(az.sin() * alt.cos(), az.cos() * alt.cos(), alt.sin())
            }
            SunModel::Geo {
                date,
                time,
                lat,
                lon,
                utc_offset,
                north_angle,
            } => {
                let (east, north, up) = solar_enu(date, time, lat, lon, utc_offset);
                let a = north_angle.as_radians();
                let north_axis = DVec3::new(a.sin(), a.cos(), 0.0);
                let east_axis = DVec3::new(a.cos(), -a.sin(), 0.0);
                (east_axis * east + north_axis * north + DVec3::Z * up).normalize_or_zero()
            }
        }
    }
}

/// `true` for Gregorian leap years.
fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Day of the year, 1-based.
fn day_of_year(date: Date) -> u32 {
    const CUMULATIVE: [u32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let month = usize::from(date.month.clamp(1, 12)) - 1;
    let leap_day = u32::from(is_leap(date.year) && month >= 2);
    CUMULATIVE[month] + u32::from(date.day.clamp(1, 31)) + leap_day
}

/// Sun direction as `(east, north, up)` components of a unit vector in the
/// local horizon frame.
fn solar_enu(date: Date, time: TimeOfDay, lat: f64, lon: f64, utc_offset: f64) -> (f64, f64, f64) {
    use std::f64::consts::TAU;
    let year_days = if is_leap(date.year) { 366.0 } else { 365.0 };
    let hour = f64::from(time.hour) + f64::from(time.minute) / 60.0;
    let utc_hour = hour - utc_offset;
    let g = TAU / year_days * (f64::from(day_of_year(date)) - 1.0 + (utc_hour - 12.0) / 24.0);
    let eqtime = 229.18
        * (0.000075 + 0.001868 * g.cos()
            - 0.032077 * g.sin()
            - 0.014615 * (2.0 * g).cos()
            - 0.040849 * (2.0 * g).sin());
    let decl = 0.006918 - 0.399912 * g.cos() + 0.070257 * g.sin() - 0.006758 * (2.0 * g).cos()
        + 0.000907 * (2.0 * g).sin()
        - 0.002697 * (3.0 * g).cos()
        + 0.00148 * (3.0 * g).sin();
    // True solar time in minutes, then the hour angle (positive after noon).
    let solar_minutes = utc_hour * 60.0 + eqtime + 4.0 * lon;
    let h = (solar_minutes / 4.0 - 180.0).to_radians();
    let phi = lat.to_radians();
    let up = phi.sin() * decl.sin() + phi.cos() * decl.cos() * h.cos();
    let east = -decl.cos() * h.sin();
    let north = phi.cos() * decl.sin() - phi.sin() * decl.cos() * h.cos();
    (east, north, up)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn altitude_deg(d: DVec3) -> f64 {
        d.z.asin().to_degrees()
    }

    #[test]
    fn manual_sun_points_by_azimuth_and_altitude() {
        let d = SunModel::Manual {
            azimuth: Angle::degrees(90.0),
            altitude: Angle::degrees(30.0),
        }
        .direction();
        assert!(d.x > 0.8 && d.y.abs() < 1e-12);
        assert!((altitude_deg(d) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn summer_solstice_noon_altitude_matches_latitude() {
        // 45°N on the solstice at solar noon: altitude ≈ 90 − 45 + 23.44.
        let d = SunModel::Geo {
            date: Date {
                year: 2026,
                month: 6,
                day: 21,
            },
            time: TimeOfDay {
                hour: 12,
                minute: 2,
            },
            lat: 45.0,
            lon: 0.0,
            utc_offset: 0.0,
            north_angle: Angle::ZERO,
        }
        .direction();
        assert!((altitude_deg(d) - 68.44).abs() < 0.5, "{}", altitude_deg(d));
        // The sun is due south at noon in the northern hemisphere.
        assert!(d.y < 0.0 && d.x.abs() < 0.02);
        // Rotating model north by 90° turns "south" into model −X.
        let rotated = SunModel::Geo {
            date: Date {
                year: 2026,
                month: 6,
                day: 21,
            },
            time: TimeOfDay {
                hour: 12,
                minute: 2,
            },
            lat: 45.0,
            lon: 0.0,
            utc_offset: 0.0,
            north_angle: Angle::degrees(90.0),
        }
        .direction();
        assert!(rotated.x < 0.0 && rotated.y.abs() < 0.02);
    }

    #[test]
    fn afternoon_sun_is_in_the_west_and_night_is_below_the_horizon() {
        let at = |hour| {
            SunModel::Geo {
                date: Date {
                    year: 2026,
                    month: 3,
                    day: 20,
                },
                time: TimeOfDay { hour, minute: 0 },
                lat: 45.0,
                lon: 0.0,
                utc_offset: 0.0,
                north_angle: Angle::ZERO,
            }
            .direction()
        };
        assert!(at(16).x < 0.0);
        assert!(at(0).z < 0.0);
    }
}
