//! Implements docs/03-geometry-kernel.md §1 (units), docs/08-cli-specification.md
//! §4 (units and numbers) and the value-field grammar of docs/05 §1.
//!
//! Lengths are `f64` millimeters and angles `f64` radians everywhere inside the
//! core; display units are a formatting concern handled here only.
//!
//! Accepted length forms (docs/08 §4, docs/05 §1): `1500`, `1.5m`, `150cm`,
//! `1500mm`, `59in`, `59"`, `4'11"`, `4' 11 1/2"`, `4'-6"`, `3'6`, `4.9ft`,
//! `1/2"`, `11-1/2"`. The decimal separator is always `.`; thousands separators
//! are rejected. A leading `-` negates the whole value (negative values reverse
//! direction in tools). Angles accept `45`, `45deg`, `45°`, `0.7854rad`, slopes
//! `1:12` (rise over run) and grades `8%`.

#![warn(unused_variables, dead_code)]

use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};
use std::str::FromStr;

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

    /// Builds a length from a value in `unit`.
    pub fn in_unit(value: f64, unit: LengthUnit) -> Self {
        Length(value * unit.millimeters())
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

    /// Parses the CLI/value-field length grammar; see [`parse_length`].
    pub fn parse(input: &str, default_unit: LengthUnit) -> Result<Self, UnitsError> {
        parse_length(input, default_unit)
    }

    /// Formats in `unit` with the document precision; see [`format_length`].
    pub fn format(self, unit: LengthUnit, precision: u8) -> String {
        format_length(self, unit, precision)
    }
}

impl From<f64> for Length {
    fn from(mm: f64) -> Self {
        Length(mm)
    }
}

impl FromStr for Length {
    type Err = UnitsError;

    /// Parses with millimeters as the default unit.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_length(s, LengthUnit::Millimeter)
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
    /// A half turn (π).
    pub const HALF_TURN: Angle = Angle(std::f64::consts::PI);
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

    /// Builds an angle from a slope `rise:run` (docs/08 §4 `1:12`).
    pub fn slope(rise: f64, run: f64) -> Self {
        Angle(libm::atan2(rise, run))
    }

    /// Builds an angle from a grade in percent (docs/08 §4 `8%`).
    pub fn percent(grade: f64) -> Self {
        Angle(libm::atan(grade / 100.0))
    }

    /// The value in radians.
    pub const fn as_radians(self) -> f64 {
        self.0
    }

    /// The value in degrees.
    pub fn to_degrees(self) -> f64 {
        self.0.to_degrees()
    }

    /// The value as a grade in percent (`tan · 100`).
    pub fn to_percent(self) -> f64 {
        libm::tan(self.0) * 100.0
    }

    /// Absolute value.
    pub fn abs(self) -> Self {
        Angle(self.0.abs())
    }

    /// Wraps into `[0, 2π)`.
    pub fn normalized(self) -> Self {
        Angle(self.0.rem_euclid(std::f64::consts::TAU))
    }

    /// Wraps into `(-π, π]`.
    pub fn normalized_signed(self) -> Self {
        let a = self.normalized().0;
        if a > std::f64::consts::PI {
            Angle(a - std::f64::consts::TAU)
        } else {
            Angle(a)
        }
    }

    /// Parses the CLI/value-field angle grammar; see [`parse_angle`].
    pub fn parse(input: &str) -> Result<Self, UnitsError> {
        parse_angle(input)
    }

    /// Formats in degrees with `precision` decimals; see [`format_angle`].
    pub fn format(self, precision: u8) -> String {
        format_angle(self, precision)
    }
}

impl From<f64> for Angle {
    fn from(radians: f64) -> Self {
        Angle(radians)
    }
}

impl FromStr for Angle {
    type Err = UnitsError;

    /// Parses with degrees as the default unit.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_angle(s)
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
    /// All display units in the order of docs/04 §10.
    pub const ALL: [LengthUnit; 7] = [
        LengthUnit::Millimeter,
        LengthUnit::Centimeter,
        LengthUnit::Meter,
        LengthUnit::Inch,
        LengthUnit::Foot,
        LengthUnit::FeetInches,
        LengthUnit::ArchitecturalFraction,
    ];

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

    /// The `ModelInfo.display_units` keyword (`mm|cm|m|in|ft|ft_in|arch_fraction`,
    /// docs/04 §10), also accepted by `--units` (docs/08 §2).
    pub const fn keyword(self) -> &'static str {
        match self {
            LengthUnit::Millimeter => "mm",
            LengthUnit::Centimeter => "cm",
            LengthUnit::Meter => "m",
            LengthUnit::Inch => "in",
            LengthUnit::Foot => "ft",
            LengthUnit::FeetInches => "ft_in",
            LengthUnit::ArchitecturalFraction => "arch_fraction",
        }
    }

    /// Parses a unit symbol (`mm`, `cm`, `m`, `in`, `"`, `ft`, `'`, `ft_in`, `arch`).
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        Some(match symbol {
            "mm" => LengthUnit::Millimeter,
            "cm" => LengthUnit::Centimeter,
            "m" => LengthUnit::Meter,
            "in" | "\"" | "″" | "inch" | "inches" => LengthUnit::Inch,
            "ft" | "'" | "′" | "foot" | "feet" => LengthUnit::Foot,
            "ft_in" | "ft-in" => LengthUnit::FeetInches,
            "arch" | "arch_fraction" => LengthUnit::ArchitecturalFraction,
            _ => return None,
        })
    }

    /// `true` for the two fraction-based display modes, whose `precision` is a
    /// denominator power instead of a decimal count (docs/04 §10).
    pub const fn is_fractional(self) -> bool {
        matches!(
            self,
            LengthUnit::FeetInches | LengthUnit::ArchitecturalFraction
        )
    }

    /// The unit a bare number denotes when this is the document unit: the unit
    /// itself, or inches for the fraction-based modes (decision, docs/08 §4).
    pub const fn bare_number_unit(self) -> LengthUnit {
        match self {
            LengthUnit::FeetInches | LengthUnit::ArchitecturalFraction => LengthUnit::Inch,
            other => other,
        }
    }
}

impl fmt::Display for LengthUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.symbol())
    }
}

impl FromStr for LengthUnit {
    type Err = UnitsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        LengthUnit::from_symbol(s.trim()).ok_or_else(|| UnitsError::UnknownUnit(s.to_string()))
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

impl AngleUnit {
    /// The suffix printed for this unit (`deg`, `rad`, `:` for slopes written as
    /// `1:run`, `%`).
    pub const fn symbol(self) -> &'static str {
        match self {
            AngleUnit::Degrees => "deg",
            AngleUnit::Radians => "rad",
            AngleUnit::Slope => ":",
            AngleUnit::Percent => "%",
        }
    }
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

/// Short name for [`ValueFieldInput`]: the expression typed into the value
/// field, including the `,` pair, `/n` division and `*n`/`nx` multiple forms.
pub type ValueExpr = ValueFieldInput;

impl ValueFieldInput {
    /// Interprets the entry as a length when a tool step expects one: a bare
    /// unsigned integer (`Count`) is a length in `default_unit`'s bare-number
    /// unit (docs/05 §1 "`1200` (document units)"). Other variants give `None`.
    pub fn as_length(&self, default_unit: LengthUnit) -> Option<Length> {
        match self {
            ValueFieldInput::Length(l) => Some(*l),
            ValueFieldInput::Count(n) => Some(Length::in_unit(
                f64::from(*n),
                default_unit.bare_number_unit(),
            )),
            _ => None,
        }
    }

    /// Interprets the entry as an angle when a tool step expects one: a bare
    /// unsigned integer (`Count`) is a value in degrees (docs/08 §4 "bare
    /// numbers are degrees").
    pub fn as_angle(&self) -> Option<Angle> {
        match self {
            ValueFieldInput::Angle(a) => Some(*a),
            ValueFieldInput::Count(n) => Some(Angle::degrees(f64::from(*n))),
            _ => None,
        }
    }

    /// Interprets the entry as a count (`Count` only).
    pub fn as_count(&self) -> Option<u32> {
        match self {
            ValueFieldInput::Count(n) => Some(*n),
            _ => None,
        }
    }
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

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

/// A cursor over the input, shared by the length, angle and value-field
/// parsers. All positions are byte offsets at `char` boundaries.
struct Cursor<'a> {
    src: &'a str,
    pos: usize,
}

/// Unit token following a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitToken {
    Feet,
    Inches,
    Other(LengthUnit),
}

impl<'a> Cursor<'a> {
    fn new(src: &'a str) -> Self {
        Cursor { src, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn bump(&mut self, c: char) {
        self.pos += c.len_utf8();
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.bump(c);
            } else {
                break;
            }
        }
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.bump(expected);
            true
        } else {
            false
        }
    }

    fn malformed(&self, why: impl Into<String>) -> UnitsError {
        UnitsError::Malformed(self.src.to_string(), why.into())
    }

    /// Reads a run of ASCII digits; `None` when there is none.
    fn read_digits(&mut self) -> Option<&'a str> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.bump(c);
            } else {
                break;
            }
        }
        (self.pos > start).then(|| &self.src[start..self.pos])
    }

    /// Reads `digits`, `digits.digits`, `.digits` or `digits.`; `None` when the
    /// cursor is not at a number. Returns the value and whether it had a `.`.
    fn read_decimal(&mut self) -> Result<Option<(f64, bool)>, UnitsError> {
        let start = self.pos;
        let int_part = self.read_digits();
        let has_dot = self.eat('.');
        let frac_part = if has_dot { self.read_digits() } else { None };
        if int_part.is_none() && frac_part.is_none() {
            self.pos = start;
            return Ok(None);
        }
        let text = &self.src[start..self.pos];
        let value: f64 = text
            .parse()
            .map_err(|_| UnitsError::InvalidNumber(text.to_string()))?;
        Ok(Some((value, has_dot)))
    }

    /// Reads a number in one of the forms `12`, `12.5`, `1/2`, `11 1/2`, `11-1/2`.
    fn read_numeric(&mut self) -> Result<Option<f64>, UnitsError> {
        let start = self.pos;
        let Some((first, first_has_dot)) = self.read_decimal()? else {
            return Ok(None);
        };
        // Simple fraction `a/b`.
        if self.peek() == Some('/') {
            let save = self.pos;
            self.bump('/');
            match self.read_digits() {
                Some(den) if !first_has_dot => {
                    return Ok(Some(first / self.denominator(den, start)?));
                }
                Some(_) => {
                    return Err(UnitsError::InvalidNumber(
                        self.src[start..self.pos].to_string(),
                    ));
                }
                None => {
                    // `12/` with nothing after: let the caller report it.
                    self.pos = save;
                    return Ok(Some(first));
                }
            }
        }
        // Mixed number `a b/c` or `a-b/c` (only after an integer part).
        if !first_has_dot {
            let save = self.pos;
            self.skip_ws();
            if self.pos == save {
                self.eat('-');
            }
            if let Some(num) = self.read_digits()
                && self.eat('/')
                && let Some(den) = self.read_digits()
            {
                let num: f64 = num
                    .parse()
                    .map_err(|_| UnitsError::InvalidNumber(num.to_string()))?;
                return Ok(Some(first + num / self.denominator(den, start)?));
            }
            self.pos = save;
        }
        Ok(Some(first))
    }

    fn denominator(&self, den: &str, start: usize) -> Result<f64, UnitsError> {
        let value: f64 = den
            .parse()
            .map_err(|_| UnitsError::InvalidNumber(den.to_string()))?;
        if value < 0.5 {
            return Err(UnitsError::InvalidNumber(
                self.src[start..self.pos].to_string(),
            ));
        }
        Ok(value)
    }

    /// Reads a unit token: a run of ASCII letters or one of the prime/quote marks.
    fn read_unit(&mut self) -> Result<Option<UnitToken>, UnitsError> {
        let Some(c) = self.peek() else {
            return Ok(None);
        };
        match c {
            '\'' | '′' => {
                self.bump(c);
                return Ok(Some(UnitToken::Feet));
            }
            '"' | '″' => {
                self.bump(c);
                return Ok(Some(UnitToken::Inches));
            }
            _ => {}
        }
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphabetic() {
                self.bump(c);
            } else {
                break;
            }
        }
        if self.pos == start {
            return Ok(None);
        }
        let word = &self.src[start..self.pos];
        match word {
            "ft" | "foot" | "feet" => Ok(Some(UnitToken::Feet)),
            "in" | "inch" | "inches" => Ok(Some(UnitToken::Inches)),
            _ => LengthUnit::from_symbol(word)
                .map(|u| Some(UnitToken::Other(u)))
                .ok_or_else(|| UnitsError::UnknownUnit(word.to_string())),
        }
    }

    /// Reads an optional leading sign, returning `-1.0` or `1.0`.
    fn read_sign(&mut self) -> f64 {
        if self.eat('-') {
            -1.0
        } else {
            self.eat('+');
            1.0
        }
    }

    fn expect_end(&mut self) -> Result<(), UnitsError> {
        self.skip_ws();
        if self.at_end() {
            Ok(())
        } else {
            let rest = self.rest();
            let why = if rest.starts_with(',') {
                "unexpected `,` (thousands separators are not accepted; the decimal separator is `.`)"
                    .to_string()
            } else {
                format!("unexpected `{rest}`")
            };
            Err(self.malformed(why))
        }
    }
}

// ---------------------------------------------------------------------------
// Lengths
// ---------------------------------------------------------------------------

/// Parses a length such as `1500`, `1.5m`, `150cm`, `59in`, `59"`, `4'11"`,
/// `4' 11 1/2"` or `4.9ft` (docs/08 §4). Bare numbers use `default_unit` (inches
/// for the fraction-based display modes). The decimal separator is always `.`;
/// thousands separators are rejected.
pub fn parse_length(input: &str, default_unit: LengthUnit) -> Result<Length, UnitsError> {
    let text = input.trim();
    if text.is_empty() {
        return Err(UnitsError::Empty);
    }
    let mut cur = Cursor::new(text);
    let sign = cur.read_sign();
    cur.skip_ws();

    let first = cur
        .read_numeric()?
        .ok_or_else(|| UnitsError::InvalidNumber(cur.rest().to_string()))?;
    cur.skip_ws();
    let first_unit = cur.read_unit()?;

    let total_mm = match first_unit {
        None => {
            cur.expect_end()?;
            first * default_unit.bare_number_unit().millimeters()
        }
        Some(UnitToken::Other(unit)) => {
            cur.expect_end()?;
            first * unit.millimeters()
        }
        Some(UnitToken::Inches) => {
            cur.expect_end()?;
            first * LengthUnit::Inch.millimeters()
        }
        Some(UnitToken::Feet) => {
            // Optional inches term: `4' 11 1/2"`, `4'-6"`, `3'6`, `4ft 6in`.
            cur.skip_ws();
            let save = cur.pos;
            if cur.eat('-') {
                cur.skip_ws();
            }
            let inches = match cur.read_numeric()? {
                Some(value) => {
                    cur.skip_ws();
                    match cur.read_unit()? {
                        None | Some(UnitToken::Inches) => value,
                        Some(_) => {
                            return Err(cur.malformed("only inches may follow a feet value"));
                        }
                    }
                }
                None => {
                    cur.pos = save;
                    0.0
                }
            };
            cur.expect_end()?;
            // Sum in inches first so `3' 6"` and `42"` give identical millimeters.
            (first * 12.0 + inches) * LengthUnit::Inch.millimeters()
        }
    };

    let value = sign * total_mm;
    if !value.is_finite() {
        return Err(UnitsError::InvalidNumber(text.to_string()));
    }
    Ok(Length(value))
}

/// Formats a length in `unit` with the document precision (decimals, or the
/// fraction denominator power for architectural units) (docs/08 §4, docs/04 §10).
///
/// Decimal units print a fixed number of decimals and the unit symbol
/// (`1500.0mm`, `4.90ft`); the fraction-based modes print `4' 11 1/2"` or
/// `59 1/2"` with fractions reduced to lowest terms and rounded to the nearest
/// `1/2^precision` inch. Every output parses back with [`parse_length`].
pub fn format_length(value: Length, unit: LengthUnit, precision: u8) -> String {
    match unit {
        LengthUnit::FeetInches => format_fractional_inches(value, precision, true),
        LengthUnit::ArchitecturalFraction => format_fractional_inches(value, precision, false),
        _ => format!("{}{}", fixed(value.to_unit(unit), precision), unit.symbol()),
    }
}

/// Largest supported fraction denominator power (1/1024 inch).
const MAX_DENOMINATOR_POWER: u8 = 10;

/// Largest number of decimals a precision can request.
const MAX_DECIMALS: u8 = 12;

fn format_fractional_inches(value: Length, denominator_power: u8, with_feet: bool) -> String {
    let denom = 1u64 << denominator_power.min(MAX_DENOMINATOR_POWER);
    let total_inches = value.to_unit(LengthUnit::Inch);
    if !total_inches.is_finite() {
        return format!("{total_inches}\"");
    }
    let negative = total_inches < 0.0;
    // Number of denominator-ths, rounded half away from zero. The saturating
    // cast keeps absurd magnitudes finite instead of panicking.
    // A relative nudge keeps exact ties (`1/16` after an inch→mm→inch trip
    // lands a few ulps below the midpoint) rounding away from zero.
    let scaled = total_inches.abs() * denom as f64;
    let units = (scaled * (1.0 + 1e-12)).round();
    let units = if units >= u64::MAX as f64 {
        u64::MAX
    } else {
        units as u64
    };
    let whole_inches = units / denom;
    let frac_num = units % denom;
    let (feet, inches) = if with_feet {
        (whole_inches / 12, whole_inches % 12)
    } else {
        (0, whole_inches)
    };
    let g = gcd(frac_num, denom);
    let (num, den) = if frac_num == 0 {
        (0, 1)
    } else {
        (frac_num / g, denom / g)
    };

    let mut out = String::new();
    if negative && units > 0 {
        out.push('-');
    }
    if feet > 0 {
        out.push_str(&feet.to_string());
        out.push('\'');
        out.push(' ');
    }
    if inches > 0 || num == 0 {
        out.push_str(&inches.to_string());
        if num > 0 {
            out.push(' ');
        }
    }
    if num > 0 {
        out.push_str(&format!("{num}/{den}"));
    }
    out.push('"');
    out
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a.max(1)
}

/// Fixed-point formatting with `decimals` places and no negative zero.
fn fixed(value: f64, decimals: u8) -> String {
    let decimals = usize::from(decimals.min(MAX_DECIMALS));
    let s = format!("{value:.decimals$}");
    if s.starts_with('-') && s[1..].chars().all(|c| c == '0' || c == '.') {
        s[1..].to_string()
    } else {
        s
    }
}

// ---------------------------------------------------------------------------
// Angles
// ---------------------------------------------------------------------------

/// Parses an angle: `45`, `45deg`, `45°`, `0.7854rad`, slope `1:12`, grade `8%`
/// (docs/08 §4). Bare numbers are degrees. A slope's run must be positive; the
/// rise may be negative (`-1:12` and `1:12` with a leading sign are equivalent).
pub fn parse_angle(input: &str) -> Result<Angle, UnitsError> {
    let text = input.trim();
    if text.is_empty() {
        return Err(UnitsError::Empty);
    }
    let mut cur = Cursor::new(text);
    let sign = cur.read_sign();
    cur.skip_ws();
    let value = cur
        .read_numeric()?
        .ok_or_else(|| UnitsError::InvalidNumber(cur.rest().to_string()))?;
    cur.skip_ws();

    let radians = match cur.peek() {
        Some(':') => {
            cur.bump(':');
            cur.skip_ws();
            if matches!(cur.peek(), Some('-' | '+')) {
                return Err(cur.malformed("slope run must be positive"));
            }
            let run = cur
                .read_numeric()?
                .ok_or_else(|| UnitsError::InvalidNumber(cur.rest().to_string()))?;
            cur.expect_end()?;
            if run <= 0.0 && value.abs() <= 0.0 {
                return Err(cur.malformed("slope `0:0` has no direction"));
            }
            if run < 0.0 {
                return Err(cur.malformed("slope run must be positive"));
            }
            Angle::slope(value, run).0
        }
        Some('%') => {
            cur.bump('%');
            cur.expect_end()?;
            Angle::percent(value).0
        }
        Some('°') => {
            cur.bump('°');
            cur.expect_end()?;
            value.to_radians()
        }
        Some(c) if c.is_ascii_alphabetic() => {
            let start = cur.pos;
            while let Some(c) = cur.peek() {
                if c.is_ascii_alphabetic() {
                    cur.bump(c);
                } else {
                    break;
                }
            }
            let word = &text[start..cur.pos];
            let radians = match word {
                "deg" | "degree" | "degrees" => value.to_radians(),
                "rad" | "radian" | "radians" => value,
                _ => return Err(UnitsError::UnknownUnit(word.to_string())),
            };
            cur.expect_end()?;
            radians
        }
        _ => {
            cur.expect_end()?;
            value.to_radians()
        }
    };
    let radians = sign * radians;
    if !radians.is_finite() {
        return Err(UnitsError::InvalidNumber(text.to_string()));
    }
    Ok(Angle(radians))
}

/// Formats an angle in degrees with `precision` decimals (docs/04 §10
/// `angle_precision`), e.g. `45.0deg`. The output parses back with
/// [`parse_angle`].
pub fn format_angle(value: Angle, precision: u8) -> String {
    format!("{}deg", fixed(value.to_degrees(), precision))
}

// ---------------------------------------------------------------------------
// Value field
// ---------------------------------------------------------------------------

/// Parses one value-field entry per the shared grammar of docs/05 §1.
///
/// Dispatch, in order: `[x,y,z]` / `<x,y,z>` coordinates (empty components keep
/// the current value), `/n` divisions, `*n` and `nx` multiples, `a,b` pairs,
/// angles (anything containing `:` or ending in `%`, `deg`, `rad` or `°`), a
/// bare unsigned integer as a `Count`, and finally a length. Tools expecting a
/// length or an angle coerce a `Count` with [`ValueFieldInput::as_length`] /
/// [`ValueFieldInput::as_angle`].
pub fn parse_value_field(
    input: &str,
    default_unit: LengthUnit,
) -> Result<ValueFieldInput, UnitsError> {
    let text = input.trim();
    if text.is_empty() {
        return Err(UnitsError::Empty);
    }
    let malformed = |why: &str| UnitsError::Malformed(text.to_string(), why.to_string());

    if let Some(body) = text.strip_prefix('[') {
        let body = body
            .strip_suffix(']')
            .ok_or_else(|| malformed("missing closing `]`"))?;
        return parse_coordinate(body, true, default_unit, text);
    }
    if let Some(body) = text.strip_prefix('<') {
        let body = body
            .strip_suffix('>')
            .ok_or_else(|| malformed("missing closing `>`"))?;
        return parse_coordinate(body, false, default_unit, text);
    }
    if let Some(rest) = text.strip_prefix('/') {
        return parse_count(rest.trim(), text, "division").map(ValueFieldInput::Division);
    }
    if let Some(rest) = text.strip_prefix('*') {
        return parse_count(rest.trim(), text, "multiple").map(ValueFieldInput::Multiple);
    }
    if let Some(rest) = text.strip_suffix(['x', 'X']) {
        let rest = rest.trim_end();
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return parse_count(rest, text, "multiple").map(ValueFieldInput::Multiple);
        }
    }
    if text.contains(',') {
        let mut parts = text.split(',');
        let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(malformed("a pair has exactly two values `w,h`"));
        };
        return Ok(ValueFieldInput::Pair(
            parse_length(a, default_unit)?,
            parse_length(b, default_unit)?,
        ));
    }
    if text.contains(':')
        || text.ends_with('%')
        || text.ends_with('°')
        || ends_with_word(text, "deg")
        || ends_with_word(text, "rad")
    {
        return parse_angle(text).map(ValueFieldInput::Angle);
    }
    if text.chars().all(|c| c.is_ascii_digit())
        && let Ok(n) = text.parse::<u32>()
    {
        return Ok(ValueFieldInput::Count(n));
    }
    parse_length(text, default_unit).map(ValueFieldInput::Length)
}

fn ends_with_word(text: &str, word: &str) -> bool {
    text.strip_suffix(word)
        .is_some_and(|rest| !rest.ends_with(|c: char| c.is_ascii_alphabetic()))
}

fn parse_count(digits: &str, whole: &str, what: &str) -> Result<u32, UnitsError> {
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(UnitsError::Malformed(
            whole.to_string(),
            format!("a {what} needs a positive integer"),
        ));
    }
    let n: u32 = digits
        .parse()
        .map_err(|_| UnitsError::InvalidNumber(digits.to_string()))?;
    if n == 0 {
        return Err(UnitsError::Malformed(
            whole.to_string(),
            format!("a {what} must be at least 1"),
        ));
    }
    Ok(n)
}

fn parse_coordinate(
    body: &str,
    absolute: bool,
    default_unit: LengthUnit,
    whole: &str,
) -> Result<ValueFieldInput, UnitsError> {
    let mut xyz: [Option<Length>; 3] = [None, None, None];
    let mut given = 0usize;
    for (i, part) in body.split(',').enumerate() {
        if i >= 3 {
            return Err(UnitsError::Malformed(
                whole.to_string(),
                "a coordinate has at most three components".to_string(),
            ));
        }
        let part = part.trim();
        xyz[i] = if part.is_empty() {
            None
        } else {
            Some(parse_length(part, default_unit)?)
        };
        given += usize::from(xyz[i].is_some());
    }
    if given == 0 {
        return Err(UnitsError::Malformed(
            whole.to_string(),
            "a coordinate needs at least one non-empty component".to_string(),
        ));
    }
    Ok(ValueFieldInput::Coordinate { absolute, xyz })
}

/// Formats `value` with at most `max_decimals` decimals and no trailing zeros.
fn trim_float(value: f64, max_decimals: usize) -> String {
    let s = format!("{value:.max_decimals$}");
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    if s == "-0" { "0".to_string() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const IN: f64 = 25.4;

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() <= eps
    }

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
        assert!((Angle::degrees(270.0).normalized_signed().to_degrees() + 90.0).abs() < 1e-9);
    }

    #[test]
    fn display_trims_zeros() {
        assert_eq!(Length::mm(1200.0).to_string(), "1200mm");
        assert_eq!(Length::mm(0.5).to_string(), "0.5mm");
        assert_eq!(Length::mm(-0.0001).to_string(), "0mm");
        assert_eq!(Angle::degrees(45.0).to_string(), "45deg");
    }

    #[test]
    fn unit_symbols_round_trip() {
        for unit in LengthUnit::ALL {
            assert_eq!(LengthUnit::from_symbol(unit.keyword()), Some(unit));
        }
        for unit in [
            LengthUnit::Millimeter,
            LengthUnit::Centimeter,
            LengthUnit::Meter,
            LengthUnit::Inch,
            LengthUnit::Foot,
        ] {
            assert_eq!(LengthUnit::from_symbol(unit.symbol()), Some(unit));
        }
        assert_eq!("ft_in".parse::<LengthUnit>(), Ok(LengthUnit::FeetInches));
        assert!("km".parse::<LengthUnit>().is_err());
    }

    #[test]
    fn parses_spec_length_examples() {
        let mm = LengthUnit::Millimeter;
        let cases: [(&str, f64); 18] = [
            ("1500", 1500.0),
            ("1.5m", 1500.0),
            ("150cm", 1500.0),
            ("1500mm", 1500.0),
            ("59in", 59.0 * IN),
            ("59\"", 59.0 * IN),
            ("4'11\"", 59.0 * IN),
            ("4' 11 1/2\"", 59.5 * IN),
            ("4.9ft", 4.9 * 304.8),
            ("3' 6\"", 42.0 * IN),
            ("3'6", 42.0 * IN),
            ("4'-6\"", 54.0 * IN),
            ("1/2\"", 0.5 * IN),
            ("11-1/2\"", 11.5 * IN),
            ("4ft 6in", 54.0 * IN),
            ("-1.2m", -1200.0),
            (".5m", 500.0),
            ("  120 cm ", 1200.0),
        ];
        for (text, expected) in cases {
            let got = parse_length(text, mm).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert!(close(got.as_mm(), expected, 1e-9), "{text}: {got:?}");
        }
    }

    #[test]
    fn bare_numbers_use_default_unit() {
        assert!(close(
            parse_length("12", LengthUnit::Inch).unwrap().as_mm(),
            12.0 * IN,
            1e-9
        ));
        assert!(close(
            parse_length("2", LengthUnit::Meter).unwrap().as_mm(),
            2000.0,
            1e-9
        ));
        // Fraction-based display modes read bare numbers as inches.
        assert!(close(
            parse_length("12", LengthUnit::FeetInches).unwrap().as_mm(),
            12.0 * IN,
            1e-9
        ));
        assert!(close(
            parse_length("1/2", LengthUnit::ArchitecturalFraction)
                .unwrap()
                .as_mm(),
            0.5 * IN,
            1e-9
        ));
    }

    #[test]
    fn rejects_malformed_lengths() {
        let mm = LengthUnit::Millimeter;
        assert_eq!(parse_length("", mm), Err(UnitsError::Empty));
        assert_eq!(parse_length("   ", mm), Err(UnitsError::Empty));
        assert!(matches!(
            parse_length("1,500", mm),
            Err(UnitsError::Malformed(_, _))
        ));
        assert_eq!(
            parse_length("12km", mm),
            Err(UnitsError::UnknownUnit("km".to_string()))
        );
        assert!(matches!(
            parse_length("abc", mm),
            Err(UnitsError::InvalidNumber(_))
        ));
        assert!(matches!(
            parse_length("1/0\"", mm),
            Err(UnitsError::InvalidNumber(_))
        ));
        assert!(matches!(
            parse_length("1.5/2\"", mm),
            Err(UnitsError::InvalidNumber(_))
        ));
        assert!(matches!(
            parse_length("4' 6m", mm),
            Err(UnitsError::Malformed(_, _))
        ));
        assert!(matches!(
            parse_length("4 11\"", mm),
            Err(UnitsError::Malformed(_, _))
        ));
        assert!(matches!(
            parse_length("1e3", mm),
            Err(UnitsError::UnknownUnit(_))
        ));
    }

    #[test]
    fn formats_decimal_units() {
        assert_eq!(
            format_length(Length::mm(1500.0), LengthUnit::Millimeter, 1),
            "1500.0mm"
        );
        assert_eq!(
            format_length(Length::mm(1500.0), LengthUnit::Meter, 2),
            "1.50m"
        );
        assert_eq!(
            format_length(Length::mm(1234.5678), LengthUnit::Centimeter, 0),
            "123cm"
        );
        assert_eq!(
            format_length(Length::inches(59.0), LengthUnit::Inch, 1),
            "59.0in"
        );
        assert_eq!(
            format_length(Length::feet(4.9), LengthUnit::Foot, 2),
            "4.90ft"
        );
        // No negative zero.
        assert_eq!(
            format_length(Length::mm(-0.0001), LengthUnit::Millimeter, 1),
            "0.0mm"
        );
    }

    #[test]
    fn formats_feet_inches_and_architectural_fractions() {
        let fi = LengthUnit::FeetInches;
        let arch = LengthUnit::ArchitecturalFraction;
        assert_eq!(format_length(Length::inches(59.5), fi, 4), "4' 11 1/2\"");
        assert_eq!(format_length(Length::inches(59.5), arch, 4), "59 1/2\"");
        assert_eq!(format_length(Length::inches(12.0), fi, 4), "1' 0\"");
        assert_eq!(format_length(Length::inches(12.5), fi, 4), "1' 1/2\"");
        assert_eq!(format_length(Length::inches(0.5), fi, 4), "1/2\"");
        assert_eq!(format_length(Length::ZERO, fi, 4), "0\"");
        assert_eq!(format_length(Length::inches(6.0), fi, 4), "6\"");
        assert_eq!(format_length(Length::inches(-54.0), fi, 4), "-4' 6\"");
        assert_eq!(format_length(Length::inches(0.3), fi, 0), "0\"");
        assert_eq!(format_length(Length::inches(0.6), fi, 0), "1\"");
        assert_eq!(format_length(Length::inches(0.1875), arch, 4), "3/16\"");
        assert_eq!(format_length(Length::inches(0.1875), arch, 3), "1/4\"");
        assert_eq!(format_length(Length::inches(11.9999), fi, 4), "1' 0\"");
    }

    #[test]
    #[allow(clippy::approx_constant)] // `0.7854rad` is the spec's own example
    fn parses_spec_angle_examples() {
        let cases: [(&str, f64); 10] = [
            ("45", 45.0),
            ("45deg", 45.0),
            ("45°", 45.0),
            ("0.7854rad", 0.7854f64.to_degrees()),
            ("1:12", libm::atan(1.0 / 12.0).to_degrees()),
            ("8%", libm::atan(0.08).to_degrees()),
            ("-30", -30.0),
            ("-1:12", -libm::atan(1.0 / 12.0).to_degrees()),
            ("1:0", 90.0),
            (" 90 deg ", 90.0),
        ];
        for (text, expected_deg) in cases {
            let got = parse_angle(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert!(
                close(got.to_degrees(), expected_deg, 1e-9),
                "{text}: {got:?}"
            );
        }
        assert_eq!(parse_angle(""), Err(UnitsError::Empty));
        assert!(matches!(
            parse_angle("0:0"),
            Err(UnitsError::Malformed(_, _))
        ));
        assert!(matches!(
            parse_angle("1:-12"),
            Err(UnitsError::Malformed(_, _))
        ));
        assert_eq!(
            parse_angle("3grad"),
            Err(UnitsError::UnknownUnit("grad".to_string()))
        );
        assert!(matches!(
            parse_angle("4,5"),
            Err(UnitsError::Malformed(_, _))
        ));
    }

    #[test]
    fn formats_angles() {
        assert_eq!(format_angle(Angle::degrees(45.0), 1), "45.0deg");
        assert_eq!(format_angle(Angle::degrees(-0.00001), 2), "0.00deg");
        assert_eq!(format_angle(Angle::RIGHT, 0), "90deg");
    }

    #[test]
    fn value_field_grammar() {
        let mm = LengthUnit::Millimeter;
        let p = |s: &str| parse_value_field(s, mm).unwrap_or_else(|e| panic!("{s}: {e}"));
        assert_eq!(p("1200"), ValueFieldInput::Count(1200));
        assert_eq!(p("1200.0"), ValueFieldInput::Length(Length::mm(1200.0)));
        assert_eq!(p("-1200"), ValueFieldInput::Length(Length::mm(-1200.0)));
        assert_eq!(p("1.2m"), ValueFieldInput::Length(Length::mm(1200.0)));
        assert_eq!(p("3' 6\""), ValueFieldInput::Length(Length::inches(42.0)));
        assert_eq!(
            p("800,600"),
            ValueFieldInput::Pair(Length::mm(800.0), Length::mm(600.0))
        );
        assert_eq!(
            p("1m, 2m"),
            ValueFieldInput::Pair(Length::mm(1000.0), Length::mm(2000.0))
        );
        assert_eq!(p("/5"), ValueFieldInput::Division(5));
        assert_eq!(p("*3"), ValueFieldInput::Multiple(3));
        assert_eq!(p("3x"), ValueFieldInput::Multiple(3));
        assert_eq!(p("3X"), ValueFieldInput::Multiple(3));
        assert_eq!(p("45deg"), ValueFieldInput::Angle(Angle::degrees(45.0)));
        assert_eq!(p("1:12"), ValueFieldInput::Angle(Angle::slope(1.0, 12.0)));
        assert_eq!(p("8%"), ValueFieldInput::Angle(Angle::percent(8.0)));
        assert_eq!(p("0.785rad"), ValueFieldInput::Angle(Angle::radians(0.785)));
        assert_eq!(
            p("[0,0,1m]"),
            ValueFieldInput::Coordinate {
                absolute: true,
                xyz: [
                    Some(Length::ZERO),
                    Some(Length::ZERO),
                    Some(Length::mm(1000.0))
                ],
            }
        );
        assert_eq!(
            p("<,,500>"),
            ValueFieldInput::Coordinate {
                absolute: false,
                xyz: [None, None, Some(Length::mm(500.0))],
            }
        );
        assert_eq!(
            p("[10]"),
            ValueFieldInput::Coordinate {
                absolute: true,
                xyz: [Some(Length::mm(10.0)), None, None],
            }
        );
        // Coercions.
        assert_eq!(
            p("3").as_length(LengthUnit::Meter),
            Some(Length::mm(3000.0))
        );
        assert_eq!(p("3").as_angle(), Some(Angle::degrees(3.0)));
        assert_eq!(p("3").as_count(), Some(3));
        assert_eq!(p("3.0").as_count(), None);
        assert_eq!(p("1.5m").as_length(mm), Some(Length::mm(1500.0)));

        // Errors.
        assert_eq!(parse_value_field("", mm), Err(UnitsError::Empty));
        assert!(matches!(
            parse_value_field("/0", mm),
            Err(UnitsError::Malformed(_, _))
        ));
        assert!(matches!(
            parse_value_field("/a", mm),
            Err(UnitsError::Malformed(_, _))
        ));
        assert!(matches!(
            parse_value_field("1,2,3", mm),
            Err(UnitsError::Malformed(_, _))
        ));
        assert!(matches!(
            parse_value_field("[1,2,3,4]", mm),
            Err(UnitsError::Malformed(_, _))
        ));
        assert!(matches!(
            parse_value_field("[1,2", mm),
            Err(UnitsError::Malformed(_, _))
        ));
        assert!(matches!(
            parse_value_field("[]", mm),
            Err(UnitsError::Malformed(_, _))
        ));
    }

    #[test]
    fn from_str_impls_use_default_units() {
        assert_eq!("1500".parse::<Length>(), Ok(Length::mm(1500.0)));
        assert_eq!("90".parse::<Angle>(), Ok(Angle::degrees(90.0)));
    }

    fn arb_unit() -> impl Strategy<Value = LengthUnit> {
        prop::sample::select(LengthUnit::ALL.to_vec())
    }

    proptest! {
        /// Formatting then parsing lands within half a display step.
        #[test]
        fn format_parse_round_trip_lengths(
            mm in -1.0e9f64..1.0e9,
            unit in arb_unit(),
            precision in 0u8..=6,
        ) {
            let text = format_length(Length::mm(mm), unit, precision);
            let back = parse_length(&text, unit).unwrap_or_else(|e| panic!("{text}: {e}"));
            let step = if unit.is_fractional() {
                25.4 / f64::from(1u32 << precision)
            } else {
                unit.millimeters() * 10f64.powi(-i32::from(precision))
            };
            // Half a step plus relative slack for the f64 formatting of large values.
            let slack = step * 0.5 + mm.abs() * 1e-12 + 1e-9;
            prop_assert!(
                (back.as_mm() - mm).abs() <= slack,
                "{mm} -> {text} -> {} (step {step})", back.as_mm()
            );
        }

        /// Exact values (multiples of the display step) round-trip exactly.
        #[test]
        fn exact_fraction_round_trip(
            sixteenths in -2_000_000i64..2_000_000,
            with_feet in any::<bool>(),
        ) {
            let unit = if with_feet { LengthUnit::FeetInches } else { LengthUnit::ArchitecturalFraction };
            let inches = sixteenths as f64 / 16.0;
            let text = format_length(Length::inches(inches), unit, 4);
            let back = parse_length(&text, unit).unwrap_or_else(|e| panic!("{text}: {e}"));
            prop_assert!((back.to_unit(LengthUnit::Inch) - inches).abs() < 1e-9, "{text}");
        }

        #[test]
        fn format_parse_round_trip_angles(deg in -720.0f64..720.0, precision in 0u8..=6) {
            let text = format_angle(Angle::degrees(deg), precision);
            let back = parse_angle(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            let step = 10f64.powi(-i32::from(precision));
            prop_assert!((back.to_degrees() - deg).abs() <= step * 0.5 + 1e-9, "{deg} -> {text}");
        }

        /// Slopes and grades agree with their closed forms.
        #[test]
        fn slopes_and_grades(rise in -100.0f64..100.0, run in 0.01f64..100.0) {
            let text = format!("{rise}:{run}");
            let slope = parse_angle(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            prop_assert!((slope.as_radians() - libm::atan2(rise, run)).abs() < 1e-12);
            let grade = parse_angle(&format!("{rise}%")).unwrap();
            prop_assert!((grade.to_percent() - rise).abs() < 1e-9);
        }

        /// Any decimal number with any unit symbol parses to the right millimeters.
        #[test]
        fn unit_suffixes_scale(value in -1.0e6f64..1.0e6, unit in arb_unit()) {
            let unit = unit.bare_number_unit();
            let text = format!("{value}{}", unit.symbol());
            let parsed = parse_length(&text, LengthUnit::Meter).unwrap_or_else(|e| panic!("{text}: {e}"));
            let expected = value * unit.millimeters();
            prop_assert!((parsed.as_mm() - expected).abs() <= expected.abs() * 1e-12 + 1e-9);
        }

        /// Bare unsigned integers are counts; everything else that is a plain
        /// decimal is a length in the default unit.
        #[test]
        fn value_field_counts_and_lengths(n in 0u32..u32::MAX, frac in 1u32..1000) {
            prop_assert_eq!(parse_value_field(&n.to_string(), LengthUnit::Millimeter), Ok(ValueFieldInput::Count(n)));
            let text = format!("{n}.{frac:03}");
            match parse_value_field(&text, LengthUnit::Centimeter) {
                Ok(ValueFieldInput::Length(l)) => {
                    let expected = (f64::from(n) + f64::from(frac) / 1000.0) * 10.0;
                    prop_assert!((l.as_mm() - expected).abs() <= expected.abs() * 1e-12 + 1e-9);
                }
                other => prop_assert!(false, "{text}: {other:?}"),
            }
        }
    }
}
