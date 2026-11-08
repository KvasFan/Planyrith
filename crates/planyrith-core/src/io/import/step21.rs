//! A reader for the exchange structure's text encoding (ISO 10303-21), as
//! IFC files use it (docs/09 Decisions, IFC import): the `DATA` section's
//! instances `#id = TYPE(arguments);` with strings, numbers, enumerations,
//! references, lists, typed values, `$` and `*`. Complex (multi-type)
//! instances are kept by their first type.

use std::collections::BTreeMap;

use crate::io::IoError;

/// One argument.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `#12`.
    Ref(u64),
    /// An integer.
    Int(i64),
    /// A real.
    Real(f64),
    /// A string (decoded).
    Str(String),
    /// `.ENUM.`
    Enum(String),
    /// `( … )`.
    List(Vec<Value>),
    /// `TYPE( value )`.
    Typed(String, Box<Value>),
    /// `$`.
    Null,
    /// `*`.
    Derived,
}

impl Value {
    /// The referenced instance.
    pub fn as_ref_id(&self) -> Option<u64> {
        match self {
            Value::Ref(id) => Some(*id),
            _ => None,
        }
    }

    /// A number (integer or real, typed or not).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Real(r) => Some(*r),
            Value::Typed(_, v) => v.as_f64(),
            _ => None,
        }
    }

    /// A string (typed or not).
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Typed(_, v) => v.as_str(),
            _ => None,
        }
    }

    /// A list's items.
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    /// An enumeration's name.
    pub fn as_enum(&self) -> Option<&str> {
        match self {
            Value::Enum(e) => Some(e),
            _ => None,
        }
    }
}

/// An instance: its type (upper case) and arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    /// Type name, upper case.
    pub kind: String,
    /// Arguments.
    pub args: Vec<Value>,
}

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "ifc".into(),
        reason: reason.into(),
    }
}

/// Decodes a string's `''` and `\X2\…\X0\`, `\X\hh`, `\S\c` escapes.
fn decode(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let rest: String = chars.clone().take(3).collect();
        if rest.starts_with("X2\\") {
            for _ in 0..3 {
                chars.next();
            }
            let mut units = Vec::new();
            loop {
                let hex: String = chars.clone().take(4).collect();
                if hex.starts_with("\\X0") || hex.len() < 4 {
                    for _ in 0..4.min(hex.len()) {
                        chars.next();
                    }
                    break;
                }
                for _ in 0..4 {
                    chars.next();
                }
                if let Ok(u) = u16::from_str_radix(&hex, 16) {
                    units.push(u);
                }
            }
            out.push_str(&String::from_utf16_lossy(&units));
        } else if rest.starts_with("X\\") {
            for _ in 0..2 {
                chars.next();
            }
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(b) = u8::from_str_radix(&hex, 16) {
                out.push(char::from(b));
            }
        } else if rest.starts_with("S\\") {
            for _ in 0..2 {
                chars.next();
            }
            if let Some(c) = chars.next() {
                out.push(char::from_u32(c as u32 + 128).unwrap_or(c));
            }
        } else if rest.starts_with('\\') {
            chars.next();
            out.push('\\');
        } else {
            out.push('\\');
        }
    }
    out
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn skip_space(&mut self) {
        loop {
            while self.at < self.s.len() && self.s[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            if self.s[self.at..].starts_with(b"/*") {
                match self.s[self.at + 2..].windows(2).position(|w| w == b"*/") {
                    Some(end) => self.at += end + 4,
                    None => self.at = self.s.len(),
                }
            } else {
                return;
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_space();
        self.s.get(self.at).copied()
    }

    fn expect(&mut self, c: u8) -> Result<(), IoError> {
        if self.peek() == Some(c) {
            self.at += 1;
            Ok(())
        } else {
            Err(corrupt(format!(
                "expected `{}` at byte {}",
                char::from(c),
                self.at
            )))
        }
    }

    fn word(&mut self) -> String {
        self.skip_space();
        let start = self.at;
        while self.at < self.s.len()
            && (self.s[self.at].is_ascii_alphanumeric() || self.s[self.at] == b'_')
        {
            self.at += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.at]).to_ascii_uppercase()
    }

    fn value(&mut self) -> Result<Value, IoError> {
        match self
            .peek()
            .ok_or_else(|| corrupt("the file ends inside an instance"))?
        {
            b'#' => {
                self.at += 1;
                let start = self.at;
                while self.at < self.s.len() && self.s[self.at].is_ascii_digit() {
                    self.at += 1;
                }
                std::str::from_utf8(&self.s[start..self.at])
                    .ok()
                    .and_then(|t| t.parse().ok())
                    .map(Value::Ref)
                    .ok_or_else(|| corrupt("bad reference"))
            }
            b'\'' => {
                self.at += 1;
                let mut raw = Vec::new();
                loop {
                    let c = *self
                        .s
                        .get(self.at)
                        .ok_or_else(|| corrupt("unclosed string"))?;
                    self.at += 1;
                    if c == b'\'' {
                        if self.s.get(self.at) == Some(&b'\'') {
                            raw.push(b'\'');
                            self.at += 1;
                            continue;
                        }
                        break;
                    }
                    raw.push(c);
                }
                Ok(Value::Str(decode(&String::from_utf8_lossy(&raw))))
            }
            b'.' => {
                self.at += 1;
                let name = self.word();
                self.expect(b'.')?;
                Ok(Value::Enum(name))
            }
            b'(' => {
                self.at += 1;
                let mut items = Vec::new();
                if self.peek() == Some(b')') {
                    self.at += 1;
                    return Ok(Value::List(items));
                }
                loop {
                    items.push(self.value()?);
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b')') => {
                            self.at += 1;
                            return Ok(Value::List(items));
                        }
                        _ => return Err(corrupt(format!("bad list at byte {}", self.at))),
                    }
                }
            }
            b'$' => {
                self.at += 1;
                Ok(Value::Null)
            }
            b'*' => {
                self.at += 1;
                Ok(Value::Derived)
            }
            b'"' => {
                // Binary: kept as a string of hex digits.
                self.at += 1;
                let start = self.at;
                while self.at < self.s.len() && self.s[self.at] != b'"' {
                    self.at += 1;
                }
                let text = String::from_utf8_lossy(&self.s[start..self.at]).into_owned();
                self.at += 1;
                Ok(Value::Str(text))
            }
            c if c.is_ascii_digit() || c == b'-' || c == b'+' => {
                let start = self.at;
                self.at += 1;
                while self.at < self.s.len()
                    && (self.s[self.at].is_ascii_digit()
                        || matches!(self.s[self.at], b'.' | b'E' | b'e' | b'-' | b'+'))
                {
                    self.at += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.at])
                    .map_err(|_| corrupt("bad number"))?;
                if text.contains(['.', 'E', 'e']) {
                    text.parse::<f64>()
                        .map(Value::Real)
                        .map_err(|_| corrupt(format!("bad number `{text}`")))
                } else {
                    text.parse::<i64>()
                        .map(Value::Int)
                        .map_err(|_| corrupt(format!("bad number `{text}`")))
                }
            }
            c if c.is_ascii_alphabetic() => {
                let name = self.word();
                self.expect(b'(')?;
                let inner = if self.peek() == Some(b')') {
                    Value::Null
                } else {
                    self.value()?
                };
                self.expect(b')')?;
                Ok(Value::Typed(name, Box::new(inner)))
            }
            c => Err(corrupt(format!(
                "unexpected `{}` at byte {}",
                char::from(c),
                self.at
            ))),
        }
    }
}

/// Reads the `DATA` section of `text`.
pub fn parse(text: &str) -> Result<BTreeMap<u64, Instance>, IoError> {
    if !text.trim_start().starts_with("ISO-10303-21") {
        return Err(corrupt("not an exchange structure (ISO-10303-21)"));
    }
    let data = text
        .find("DATA;")
        .ok_or_else(|| corrupt("no DATA section"))?;
    let mut p = Parser {
        s: text.as_bytes(),
        at: data + 5,
    };
    let mut out = BTreeMap::new();
    loop {
        match p.peek() {
            Some(b'#') => {}
            Some(_) => {
                if p.word() == "ENDSEC" {
                    break;
                }
                return Err(corrupt(format!("unexpected text at byte {}", p.at)));
            }
            None => return Err(corrupt("no ENDSEC after DATA")),
        }
        let Value::Ref(id) = p.value()? else {
            return Err(corrupt("bad instance id"));
        };
        p.expect(b'=')?;
        let instance = if p.peek() == Some(b'(') {
            // A complex instance: its parts `TYPE(args) TYPE(args) …`.
            p.at += 1;
            let mut first: Option<Instance> = None;
            while p.peek() != Some(b')') {
                let kind = p.word();
                let Value::List(args) = p.value()? else {
                    return Err(corrupt("bad complex instance"));
                };
                first.get_or_insert(Instance { kind, args });
            }
            p.at += 1;
            first.ok_or_else(|| corrupt("empty complex instance"))?
        } else {
            let kind = p.word();
            let Value::List(args) = p.value()? else {
                return Err(corrupt(format!("bad arguments of #{id}")));
            };
            Instance { kind, args }
        };
        p.expect(b';')?;
        out.insert(id, instance);
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn instances_values_and_escapes() {
        let text = "ISO-10303-21;\nHEADER;FILE_SCHEMA(('IFC4'));ENDSEC;\nDATA;\n\
            #1=IFCWALL('2O2Fr$t4X7Zf8NOew3FLOH',$,'Muro \\X2\\00E8\\X0\\ d''angolo',$,$,#2,#3,$,.NOTDEFINED.);\n\
            /* a comment */ #2 = IFCCARTESIANPOINT((0.,-1.5E2,3));\n\
            #3=IFCPROPERTYSINGLEVALUE('A',$,IFCLABEL('x'),*);\nENDSEC;\nEND-ISO-10303-21;";
        let data = parse(text).unwrap();
        assert_eq!(data.len(), 3);
        let wall = &data[&1];
        assert_eq!(wall.kind, "IFCWALL");
        assert_eq!(wall.args[2].as_str(), Some("Muro è d'angolo"));
        assert_eq!(wall.args[5].as_ref_id(), Some(2));
        assert_eq!(wall.args[8].as_enum(), Some("NOTDEFINED"));
        let p = data[&2].args[0].as_list().unwrap();
        assert_eq!(
            p.iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>(),
            vec![0.0, -150.0, 3.0]
        );
        assert_eq!(data[&3].args[2].as_str(), Some("x"));
        assert_eq!(data[&3].args[3], Value::Derived);
        assert!(parse("hello").is_err());
        assert!(parse("ISO-10303-21;DATA;#1=X(1,;ENDSEC;").is_err());
    }
}
