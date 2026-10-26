//! Implements docs/05-modeling-tools.md §9 `shadows-set` (a view command):
//! the shadow settings of docs/04 §7 — on/off, the sun from a place, date
//! and time (docs/06 §6, NOAA algorithm) or from an azimuth and altitude,
//! light and dark intensities, and where shadows fall. Absent fields keep
//! their value; the output reports the resulting sun position.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Date, Document, ShadowSettings, SunModel, TimeOfDay};
use crate::units::Angle;

/// `shadows-set` parameters.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ShadowsSetParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Shadows on or off.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Date `YYYY-MM-DD` (switches to the geographic sun).
    #[serde(default)]
    pub date: Option<String>,
    /// Local time `HH:MM` (switches to the geographic sun).
    #[serde(default)]
    pub time: Option<String>,
    /// Latitude, degrees north.
    #[serde(default)]
    pub latitude: Option<f64>,
    /// Longitude, degrees east.
    #[serde(default)]
    pub longitude: Option<f64>,
    /// Hours from UTC of the local time.
    #[serde(default)]
    pub utc_offset: Option<f64>,
    /// Model north, clockwise from +Y.
    #[serde(default)]
    pub north_angle: Option<Angle>,
    /// Sun azimuth, clockwise from north (switches to the manual sun).
    #[serde(default)]
    pub azimuth: Option<Angle>,
    /// Sun altitude (switches to the manual sun).
    #[serde(default)]
    pub altitude: Option<Angle>,
    /// Lit intensity 0–100.
    #[serde(default)]
    pub light: Option<u8>,
    /// Shadowed intensity 0–100.
    #[serde(default)]
    pub dark: Option<u8>,
    /// Shadows on faces.
    #[serde(default)]
    pub on_faces: Option<bool>,
    /// Shadows on the ground.
    #[serde(default)]
    pub on_ground: Option<bool>,
    /// Edges cast shadows.
    #[serde(default)]
    pub from_edges: Option<bool>,
}

/// `shadows-set` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShadowsSetOutput {
    /// Shadows on.
    pub enabled: bool,
    /// Sun azimuth in the model (degrees, clockwise from +Y).
    pub sun_azimuth_deg: f64,
    /// Sun altitude (degrees; negative below the horizon).
    pub sun_altitude_deg: f64,
}

/// The `shadows-set` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct ShadowsSet;

fn parse_date(text: &str) -> Option<Date> {
    let mut parts = text.trim().split('-');
    let year = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    let days = match month {
        2 => 29,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return None,
    };
    (parts.next().is_none() && (1..=days).contains(&day)).then_some(Date { year, month, day })
}

fn parse_time(text: &str) -> Option<TimeOfDay> {
    let (h, m) = text.trim().split_once(':')?;
    let (hour, minute): (u8, u8) = (h.parse().ok()?, m.parse().ok()?);
    (hour < 24 && minute < 60).then_some(TimeOfDay { hour, minute })
}

/// The settings `p` asks for, from `current`.
fn updated(current: &ShadowSettings, p: &ShadowsSetParams) -> Result<ShadowSettings, CommandError> {
    let mut s = *current;
    s.enabled = p.enabled.unwrap_or(s.enabled);
    s.light = p.light.unwrap_or(s.light);
    s.dark = p.dark.unwrap_or(s.dark);
    s.on_faces = p.on_faces.unwrap_or(s.on_faces);
    s.on_ground = p.on_ground.unwrap_or(s.on_ground);
    s.from_edges = p.from_edges.unwrap_or(s.from_edges);
    let geo = p.date.is_some()
        || p.time.is_some()
        || p.latitude.is_some()
        || p.longitude.is_some()
        || p.utc_offset.is_some()
        || p.north_angle.is_some();
    let manual = p.azimuth.is_some() || p.altitude.is_some();
    if geo && manual {
        return Err(CommandError::invalid_param(
            "azimuth",
            "give a place and time, or an azimuth and altitude, not both",
        ));
    }
    if geo {
        let (mut date, mut time, mut lat, mut lon, mut utc, mut north) = match s.sun {
            SunModel::Geo {
                date,
                time,
                lat,
                lon,
                utc_offset,
                north_angle,
            } => (date, time, lat, lon, utc_offset, north_angle),
            SunModel::Manual { .. } => (
                Date {
                    year: 2026,
                    month: 6,
                    day: 21,
                },
                TimeOfDay {
                    hour: 12,
                    minute: 0,
                },
                45.0,
                0.0,
                0.0,
                Angle::ZERO,
            ),
        };
        if let Some(d) = &p.date {
            date = parse_date(d).ok_or_else(|| {
                CommandError::invalid_param("date", format!("`{d}`: expected YYYY-MM-DD"))
            })?;
        }
        if let Some(t) = &p.time {
            time = parse_time(t).ok_or_else(|| {
                CommandError::invalid_param("time", format!("`{t}`: expected HH:MM"))
            })?;
        }
        lat = p.latitude.unwrap_or(lat);
        lon = p.longitude.unwrap_or(lon);
        utc = p.utc_offset.unwrap_or(utc);
        north = p.north_angle.unwrap_or(north);
        s.sun = SunModel::Geo {
            date,
            time,
            lat,
            lon,
            utc_offset: utc,
            north_angle: north,
        };
    } else if manual {
        let (az, alt) = match s.sun {
            SunModel::Manual { azimuth, altitude } => (azimuth, altitude),
            SunModel::Geo { .. } => (Angle::degrees(135.0), Angle::degrees(45.0)),
        };
        s.sun = SunModel::Manual {
            azimuth: p.azimuth.unwrap_or(az),
            altitude: p.altitude.unwrap_or(alt),
        };
    }
    Ok(s)
}

impl Command for ShadowsSet {
    type Params = ShadowsSetParams;
    type Output = ShadowsSetOutput;

    fn name(&self) -> &'static str {
        "shadows-set"
    }

    fn validate(&self, doc: &Document, p: &ShadowsSetParams) -> Result<(), CommandError> {
        for (name, v, lo, hi) in [
            ("latitude", p.latitude, -90.0, 90.0),
            ("longitude", p.longitude, -180.0, 180.0),
            ("utc_offset", p.utc_offset, -14.0, 14.0),
        ] {
            if let Some(v) = v
                && !(lo..=hi).contains(&v)
            {
                return Err(CommandError::OutOfRange {
                    name: name.into(),
                    reason: format!("must be between {lo} and {hi}"),
                });
            }
        }
        if let Some(alt) = p.altitude
            && !(-90.0..=90.0).contains(&alt.to_degrees())
        {
            return Err(CommandError::OutOfRange {
                name: "altitude".into(),
                reason: "must be between -90° and 90°".into(),
            });
        }
        for (name, v) in [("light", p.light), ("dark", p.dark)] {
            if v.is_some_and(|v| v > 100) {
                return Err(CommandError::OutOfRange {
                    name: name.into(),
                    reason: "must be between 0 and 100".into(),
                });
            }
        }
        updated(&doc.rendering.shadows, p).map(|_| ())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ShadowsSetParams,
    ) -> Result<ShadowsSetOutput, CommandError> {
        let shadows = updated(&tx.document().rendering.shadows, &p)?;
        let d = shadows.sun.direction();
        let out = ShadowsSetOutput {
            enabled: shadows.enabled,
            sun_azimuth_deg: libm::atan2(d.x, d.y).to_degrees().rem_euclid(360.0),
            sun_altitude_deg: libm::asin(d.z.clamp(-1.0, 1.0)).to_degrees(),
        };
        if shadows != tx.document().rendering.shadows {
            let mut rendering = tx.document().rendering.clone();
            rendering.shadows = shadows;
            tx.set_rendering(rendering)?;
        }
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "shadows-set".into(),
            category: CommandCategory::View,
            label: "Shadows".into(),
            view_only: true,
            steps: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_times_parse_strictly() {
        assert_eq!(
            parse_date("2026-02-29"),
            Some(Date {
                year: 2026,
                month: 2,
                day: 29
            })
        );
        assert!(parse_date("2026-13-01").is_none() && parse_date("2026-04-31").is_none());
        assert_eq!(parse_time("07:05"), Some(TimeOfDay { hour: 7, minute: 5 }));
        assert!(parse_time("24:00").is_none());
    }
}
