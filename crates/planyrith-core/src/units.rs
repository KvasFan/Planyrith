//! Implements docs/03-geometry-kernel.md §1 (units), docs/08-cli-specification.md
//! §4 (units and numbers) and the value-field grammar of docs/05 §1.
//!
//! Lengths are `f64` millimeters and angles `f64` radians everywhere inside the
//! core; display units are a formatting concern handled here only.

use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A length in millimeters (docs/03 §1).
#[derive(
    Debug, Clone, Copy, PartialEq, PartialOrd, Default, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Length(pub f64);

impl Length {
    /// Zero length.
    pub const ZERO: Length = Length(0.0);

    /// Builds a length from millimeters.
    pub const fn mm(value: f64) -> Self {
        Length(value)
    }

    /// Builds a length from centimeters.
    pub fn cm(value: f64) -> Self {
        Length(value * LengthUnit::Centimeter.millimeters())
    }

    /// Builds a length from meters.
    pub fn m(value: f64) -> Self {
        Length(value * LengthUnit::Meter.millimeters())
    }

    /// Builds a length from inches.
    pub fn inches(value: f64) -> Self {
        Length(value * LengthUnit::Inch.millimeters())
    }

    /// Builds a length from feet.
    pub fn feet(value: f64) -> Self {
        Length(value * LengthUnit::Foot.millimeters())
    }

    /// The value in millimeters.
    pub const fn as_mm(self) -> f64 {
        self.0
    }

    /// Converts to the given display unit.
    pub fn to_unit(self, unit: LengthUnit) -> f64 {
        self.0 / unit.millimeters()
    }

    /// Absolute value.
    pub fn abs(self) -> Self {
        Length(self.0.abs())
    }

    /// `true` when the value is finite (not NaN or ±∞).
    pub fn is_finite(self) -> bool {
        self.0.is_finite()
    }
}

impl From<f64> for Length {
    fn from(mm: f64) -> Self {
        Length(mm)
    }
}

impl Add for Length {
    type Output = Length;
    fn add(self, rhs: Length) -> Length {
        Length(self.0 + rhs.0)
    }
}

impl AddAssign for Length {
    fn add_assign(&mut self, rhs: Length) {
        self.0 += rhs.0;
    }
}

impl Sub for Length {
    type Output = Length;
    fn sub(self, rhs: Length) -> Length {
        Length(self.0 - rhs.0)
    }
}

impl SubAssign for Length {
    fn sub_assign(&mut self, rhs: Length) {
        self.0 -= rhs.0;
    }
}

impl Mul<f64> for Length {
    type Output = Length;
    fn mul(self, rhs: f64) -> Length {
        Length(self.0 * rhs)
    }
}

impl Div<f64> for Length {
    type Output = Length;
    fn div(self, rhs: f64) -> Length {
        Length(self.0 / rhs)
    }
}

impl Neg for Length {
    type Output = Length;
    fn neg(self) -> Length {
        Length(-self.0)
    }
}

impl fmt::Display for Length {
    /// Millimeters with up to three decimals; use [`format_length`] for
    /// document units and precision.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}mm", trim_float(self.0, 3))
    }
}

/// An angle in radians (docs/03 §1).
#[derive(
    Debug, Clone, Copy, PartialEq, PartialOrd, Default, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Angle(pub f64);

impl Angle {
    /// Zero angle.
    pub const ZERO: Angle = Angle(0.0);
    /// A full turn (2π).
    pub const FULL_TURN: Angle = Angle(std::f64::consts::TAU);
    /// A right angle (π/2).
    pub const RIGHT: Angle = Angle(std::f64::consts::FRAC_PI_2);

    /// Builds an angle from radians.
    pub const fn radians(value: f64) -> Self {
        Angle(value)
    }

    /// Builds an angle from degrees.
    pub fn degrees(value: f64) -> Self {
        Angle(value.to_radians())
    }

    /// The value in radians.
    pub const fn as_radians(self) -> f64 {
        self.0
    }

    /// The value in degrees.
    pub fn to_degrees(self) -> f64 {
        self.0.to_degrees()
    }

    /// Absolute value.
    pub fn abs(self) -> Self {
        Angle(self.0.abs())
    }

    /// Wraps into `[0, 2π)`.
    pub fn normalized(self) -> Self {
        Angle(self.0.rem_euclid(std::f64::consts::TAU))
    }
}

impl From<f64> for Angle {
    fn from(radians: f64) -> Self {
        Angle(radians)
    }
}

impl Add for Angle {
    type Output = Angle;
    fn add(self, rhs: Angle) -> Angle {
        Angle(self.0 + rhs.0)
    }
}

impl Sub for Angle {
    type Output = Angle;
    fn sub(self, rhs: Angle) -> Angle {
        Angle(self.0 - rhs.0)
    }
}

impl Mul<f64> for Angle {
    type Output = Angle;
    fn mul(self, rhs: f64) -> Angle {
        Angle(self.0 * rhs)
    }
}

impl Neg for Angle {
    type Output = Angle;
    fn neg(self) -> Angle {
        Angle(-self.0)
    }
}

impl fmt::Display for Angle {
    /// Degrees with up to two decimals; use [`format_angle`] for document precision.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}deg", trim_float(self.to_degrees(), 2))
    }
}

/// Display units for lengths (docs/04 §10 `display_units`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LengthUnit {
    /// `mm`
    #[default]
    Millimeter,
    /// `cm`
    Centimeter,
    /// `m`
    Meter,
    /// `in` / `"`
    Inch,
    /// `ft` / `'` (decimal feet)
    Foot,
    /// `4' 11 1/2"` style feet and inches
    FeetInches,
    /// Inches with architectural fractions (precision = denominator power)
    ArchitecturalFraction,
}

impl LengthUnit {
    /// Millimeters per one unit (feet-and-inches and architectural fractions are
    /// inch based for conversion purposes).
    pub const fn millimeters(self) -> f64 {
        match self {
            LengthUnit::Millimeter => 1.0,
            LengthUnit::Centimeter => 10.0,
            LengthUnit::Meter => 1000.0,
            LengthUnit::Inch | LengthUnit::ArchitecturalFraction => 25.4,
            LengthUnit::Foot | LengthUnit::FeetInches => 304.8,
        }
    }

    /// The unit suffix accepted and printed by the CLI (docs/08 §4).
    pub const fn symbol(self) -> &'static str {
        match self {
            LengthUnit::Millimeter => "mm",
            LengthUnit::Centimeter => "cm",
            LengthUnit::Meter => "m",
            LengthUnit::Inch | LengthUnit::ArchitecturalFraction => "in",
            LengthUnit::Foot | LengthUnit::FeetInches => "ft",
        }
    }

    /// Parses a unit symbol (`mm`, `cm`, `m`, `in`, `"`, `ft`, `'`, `ft_in`, `arch`).
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        Some(match symbol {
            "mm" => LengthUnit::Millimeter,
            "cm" => LengthUnit::Centimeter,
            "m" => LengthUnit::Meter,
            "in" | "\"" | "inch" => LengthUnit::Inch,
            "ft" | "'" | "foot" | "feet" => LengthUnit::Foot,
            "ft_in" | "ft-in" => LengthUnit::FeetInches,
            "arch" | "arch_fraction" => LengthUnit::ArchitecturalFraction,
            _ => return None,
        })
    }
}

impl fmt::Display for LengthUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.symbol())
    }
}

/// Units accepted for angles on input (docs/08 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AngleUnit {
    /// `45`, `45deg` (default on input and output)
    #[default]
    Degrees,
    /// `0.7854rad`
    Radians,
    /// `1:12` rise over run
    Slope,
    /// `8%` grade
    Percent,
}

/// One parsed value-field entry (docs/05 §1 "Common value field grammar").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValueFieldInput {
    /// A single length (`1200`, `1.2m`, `3' 6"`).
    Length(Length),
    /// Two lengths separated by `,` (`800,600`).
    Pair(Length, Length),
    /// A division count (`/5`).
    Division(u32),
    /// A multiple count (`*3`, `3x`).
    Multiple(u32),
    /// An angle (`45`, `45deg`, `0.785rad`, `1:12`).
    Angle(Angle),
    /// A plain count (`3`), e.g. segments or array copies.
    Count(u32),
    /// Absolute `[x,y,z]` or relative `<x,y,z>` coordinates.
    Coordinate {
        /// `true` for `[x,y,z]`, `false` for `<x,y,z>`.
        absolute: bool,
        /// Components, missing ones are `None` (keep current).
        xyz: [Option<Length>; 3],
    },
}

/// Errors from unit and value parsing (docs/08 §4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnitsError {
    /// The input was empty or whitespace only.
    #[error("empty value")]
    Empty,
    /// The numeric part could not be parsed.
    #[error("invalid number `{0}`")]
    InvalidNumber(String),
    /// The unit suffix is not recognized.
    #[error("unknown unit `{0}`")]
    UnknownUnit(String),
    /// The overall shape does not match any accepted form.
    #[error("malformed value `{0}`: {1}")]
    Malformed(String, String),
}

/// Parses a length such as `1500`, `1.5m`, `150cm`, `59in`, `59"`, `4'11"`,
/// `4' 11 1/2"` or `4.9ft` (docs/08 §4). Bare numbers use `default_unit`.
/// The decimal separator is always `.`; thousands separators are rejected.
pub fn parse_length(input: &str, default_unit: LengthUnit) -> Result<Length, UnitsError> {
    todo!("docs/08 §4 parse_length")
}

/// Formats a length in `unit` with the document precision (decimals, or the
/// fraction denominator power for architectural units) (docs/08 §4, docs/04 §10).
pub fn format_length(value: Length, unit: LengthUnit, precision: u8) -> String {
    todo!("docs/08 §4 format_length")
}

/// Parses an angle: `45`, `45deg`, `0.7854rad`, slope `1:12`, grade `8%`
/// (docs/08 §4). Bare numbers are degrees.
pub fn parse_angle(input: &str) -> Result<Angle, UnitsError> {
    todo!("docs/08 §4 parse_angle")
}

/// Formats an angle in degrees with `precision` decimals (docs/04 §10
/// `angle_precision`).
pub fn format_angle(value: Angle, precision: u8) -> String {
    todo!("docs/08 §4 format_angle")
}

/// Parses one value-field entry per the shared grammar of docs/05 §1.
pub fn parse_value_field(
    input: &str,
    default_unit: LengthUnit,
) -> Result<ValueFieldInput, UnitsError> {
    todo!("docs/05 §1 value field grammar")
}

/// Formats `value` with at most `max_decimals` decimals and no trailing zeros.
fn trim_float(value: f64, max_decimals: usize) -> String {
    let s = format!("{value:.max_decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_constructors_convert_to_millimeters() {
        assert!((Length::m(1.5).as_mm() - 1500.0).abs() < 1e-9);
        assert!((Length::inches(1.0).as_mm() - 25.4).abs() < 1e-9);
        assert!((Length::feet(1.0).to_unit(LengthUnit::Inch) - 12.0).abs() < 1e-9);
    }

    #[test]
    fn angle_round_trips_degrees() {
        assert!((Angle::degrees(90.0).as_radians() - Angle::RIGHT.as_radians()).abs() < 1e-12);
        assert!((Angle::degrees(-90.0).normalized().to_degrees() - 270.0).abs() < 1e-9);
    }

    #[test]
    fn display_trims_zeros() {
        assert_eq!(Length::mm(1200.0).to_string(), "1200mm");
        assert_eq!(Length::mm(0.5).to_string(), "0.5mm");
        assert_eq!(Angle::degrees(45.0).to_string(), "45deg");
    }

    #[test]
    fn unit_symbols_round_trip() {
        for unit in [
            LengthUnit::Millimeter,
            LengthUnit::Centimeter,
            LengthUnit::Meter,
            LengthUnit::Inch,
            LengthUnit::Foot,
        ] {
            assert_eq!(LengthUnit::from_symbol(unit.symbol()), Some(unit));
        }
    }
}
