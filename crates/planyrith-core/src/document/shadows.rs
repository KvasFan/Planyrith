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
    pub fn direction(&self) -> DVec3 {
        todo!("docs/04 §7 sun direction")
    }
}
