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
                DVec3::new(
                    libm::sin(az) * libm::cos(alt),
                    libm::cos(az) * libm::cos(alt),
                    libm::sin(alt),
                )
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
                let north_axis = DVec3::new(libm::sin(a), libm::cos(a), 0.0);
                let east_axis = DVec3::new(libm::cos(a), -libm::sin(a), 0.0);
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

/// Julian day of a UTC instant: `date` at `utc_hour` (may run outside
/// 0–24; Meeus, Astronomical Algorithms, ch. 7).
fn julian_day(date: Date, utc_hour: f64) -> f64 {
    let (mut y, mut m) = (f64::from(date.year), f64::from(date.month.clamp(1, 12)));
    if m <= 2.0 {
        y -= 1.0;
        m += 12.0;
    }
    let a = (y / 100.0).floor();
    let b = 2.0 - a + (a / 4.0).floor();
    (365.25 * (y + 4716.0)).floor()
        + (30.6001 * (m + 1.0)).floor()
        + f64::from(date.day.clamp(1, 31))
        + b
        - 1524.5
        + utc_hour / 24.0
}

/// Sun direction as `(east, north, up)` components of a unit vector in the
/// local horizon frame: the NOAA solar position algorithm (after Meeus;
/// about 0.01° from 1800 to 2100), geometric (no refraction).
fn solar_enu(date: Date, time: TimeOfDay, lat: f64, lon: f64, utc_offset: f64) -> (f64, f64, f64) {
    let utc_hour = f64::from(time.hour) + f64::from(time.minute) / 60.0 - utc_offset;
    solar_enu_at(date, utc_hour, lat, lon)
}

/// [`solar_enu`] at a fractional UTC hour.
fn solar_enu_at(date: Date, utc_hour: f64, lat: f64, lon: f64) -> (f64, f64, f64) {
    let rad = f64::to_radians;
    let t = (julian_day(date, utc_hour) - 2_451_545.0) / 36_525.0;
    let l0 = (280.466_46 + t * (36_000.769_83 + t * 0.000_303_2)).rem_euclid(360.0);
    let m = 357.529_11 + t * (35_999.050_29 - 0.000_153_7 * t);
    let e = 0.016_708_634 - t * (0.000_042_037 + 0.000_000_126_7 * t);
    let c = libm::sin(rad(m)) * (1.914_602 - t * (0.004_817 + 0.000_014 * t))
        + libm::sin(rad(2.0 * m)) * (0.019_993 - 0.000_101 * t)
        + libm::sin(rad(3.0 * m)) * 0.000_289;
    let omega = 125.04 - 1934.136 * t;
    let lambda = l0 + c - 0.005_69 - 0.004_78 * libm::sin(rad(omega));
    let eps0 =
        23.0 + (26.0 + (21.448 - t * (46.815 + t * (0.000_59 - t * 0.001_813))) / 60.0) / 60.0;
    let eps = eps0 + 0.002_56 * libm::cos(rad(omega));
    let decl = libm::asin(libm::sin(rad(eps)) * libm::sin(rad(lambda)));
    let y = libm::tan(rad(eps / 2.0)) * libm::tan(rad(eps / 2.0));
    let eqtime = 4.0
        * (y * libm::sin(2.0 * rad(l0)) - 2.0 * e * libm::sin(rad(m))
            + 4.0 * e * y * libm::sin(rad(m)) * libm::cos(2.0 * rad(l0))
            - 0.5 * y * y * libm::sin(4.0 * rad(l0))
            - 1.25 * e * e * libm::sin(2.0 * rad(m)))
        .to_degrees();
    // True solar time in minutes, then the hour angle (positive after noon).
    let solar_minutes = utc_hour * 60.0 + eqtime + 4.0 * lon;
    let h = rad(solar_minutes / 4.0 - 180.0);
    let phi = rad(lat);
    let up = libm::sin(phi) * libm::sin(decl) + libm::cos(phi) * libm::cos(decl) * libm::cos(h);
    let east = -libm::cos(decl) * libm::sin(h);
    let north = libm::cos(phi) * libm::sin(decl) - libm::sin(phi) * libm::cos(decl) * libm::cos(h);
    (east, north, up)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn altitude_deg(d: DVec3) -> f64 {
        libm::asin(d.z).to_degrees()
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

    #[test]
    fn matches_the_published_solar_position_reference() {
        // The SPA paper's example (Reda & Andreas, NREL 2004): 2003-10-17
        // 12:30:30 at UTC−7, 39.742476°N 105.1786°W: zenith 50.11162°
        // (refracted; about 0.016° of refraction at 820 mbar, 11 °C) and
        // azimuth 194.34024° from north.
        let date = Date {
            year: 2003,
            month: 10,
            day: 17,
        };
        let utc = 12.0 + 30.0 / 60.0 + 30.0 / 3600.0 + 7.0;
        let (east, north, up) = solar_enu_at(date, utc, 39.742_476, -105.1786);
        let zenith = libm::acos(up).to_degrees();
        let azimuth = libm::atan2(east, north).to_degrees().rem_euclid(360.0);
        assert!((zenith - (50.111_62 + 0.016)).abs() < 0.05, "{zenith}");
        assert!((azimuth - 194.340_24).abs() < 0.05, "{azimuth}");
        // Through the public model, to the minute: well within 0.5°.
        let d = SunModel::Geo {
            date,
            time: TimeOfDay {
                hour: 12,
                minute: 30,
            },
            lat: 39.742_476,
            lon: -105.1786,
            utc_offset: -7.0,
            north_angle: Angle::ZERO,
        }
        .direction();
        let zenith = libm::acos(d.z).to_degrees();
        let azimuth = libm::atan2(d.x, d.y).to_degrees().rem_euclid(360.0);
        assert!((zenith - 50.13).abs() < 0.5, "{zenith}");
        assert!((azimuth - 194.34).abs() < 0.5, "{azimuth}");
    }

    #[test]
    fn equinox_sun_rises_east_and_sets_west_on_the_equator() {
        let at = |hour: f64| {
            let (east, north, up) = solar_enu_at(
                Date {
                    year: 2026,
                    month: 3,
                    day: 20,
                },
                hour,
                0.0,
                0.0,
            );
            (libm::atan2(east, north).to_degrees().rem_euclid(360.0), up)
        };
        let (rise, _) = at(6.2);
        let (set, _) = at(18.0);
        assert!((rise - 90.0).abs() < 0.5, "{rise}");
        assert!((set - 270.0).abs() < 0.5, "{set}");
    }
}
