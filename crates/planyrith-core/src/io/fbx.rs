//! Implements docs/09-file-formats.md §5 FBX (docs/09 Decisions, FBX): the
//! node tree both encodings share, a reader for the binary encoding (7.x,
//! 32- and 64-bit records, zlib-compressed arrays) and the ASCII encoding
//! (7.x), and a writer for the binary encoding 7.4.
//!
//! A node has a name, a list of typed properties and child nodes. The
//! scene's meaning (objects, connections, settings) is read by
//! `io::import::fbx` and written by `io::export::fbx`.

use std::io::{Read, Write};

use crate::io::IoError;

/// One property of a node.
#[derive(Debug, Clone, PartialEq)]
pub enum Prop {
    /// `C`: a boolean.
    Bool(bool),
    /// `Y`, `I` or `L`: an integer.
    Int(i64),
    /// `F` or `D`: a real.
    Real(f64),
    /// `S`: a string (binary object names keep their `\0\x01` separator).
    Str(String),
    /// `R`: raw bytes.
    Raw(Vec<u8>),
    /// `i`, `l` or `b` arrays.
    Ints(Vec<i64>),
    /// `f` or `d` arrays.
    Reals(Vec<f64>),
}

impl Prop {
    /// An integer (or a real with no fraction).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Prop::Int(i) => Some(*i),
            Prop::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    /// A number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Prop::Int(i) => Some(*i as f64),
            Prop::Real(r) => Some(*r),
            Prop::Bool(b) => Some(f64::from(u8::from(*b))),
            _ => None,
        }
    }

    /// A string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Prop::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// A node.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Node {
    /// Name.
    pub name: String,
    /// Properties.
    pub props: Vec<Prop>,
    /// Child nodes.
    pub children: Vec<Node>,
}

impl Node {
    /// A node named `name` with `props`.
    pub fn new(name: &str, props: Vec<Prop>) -> Self {
        Node {
            name: name.into(),
            props,
            children: Vec::new(),
        }
    }

    /// Adds a child and returns `self`.
    pub fn with(mut self, child: Node) -> Self {
        self.children.push(child);
        self
    }

    /// The first child named `name`.
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every child named `name`.
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// The first property of child `name`.
    pub fn value(&self, name: &str) -> Option<&Prop> {
        self.child(name).and_then(|c| c.props.first())
    }

    /// The `Properties70` (or `Properties60`) entry `name`: its values
    /// after the name, type, label and flags.
    pub fn property(&self, name: &str) -> Option<&[Prop]> {
        let list = self
            .child("Properties70")
            .or_else(|| self.child("Properties60"))?;
        list.children
            .iter()
            .find(|p| p.props.first().and_then(Prop::as_str) == Some(name))
            .map(|p| {
                // 7.x: name, type, label, flags, values…; 6.x: name, type,
                // flags, values….
                let skip = if list.name == "Properties70" { 4 } else { 3 };
                p.props.get(skip..).unwrap_or(&[])
            })
    }
}

/// A parsed file: its version and top-level nodes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Document {
    /// Version (7400 = 7.4).
    pub version: u32,
    /// Top-level nodes.
    pub nodes: Vec<Node>,
}

impl Document {
    /// The first top-level node named `name`.
    pub fn node(&self, name: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.name == name)
    }
}

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "fbx".into(),
        reason: reason.into(),
    }
}

/// The binary encoding's first bytes.
pub const BINARY_MAGIC: &[u8] = b"Kaydara FBX Binary  \0";
/// Deepest node nesting read.
const MAX_DEPTH: usize = 64;
/// Most array elements read (a guard against absurd lengths).
const MAX_ARRAY: usize = 1 << 28;

/// Reads a file in either encoding.
pub fn parse(bytes: &[u8]) -> Result<Document, IoError> {
    if bytes.starts_with(BINARY_MAGIC) {
        parse_binary(bytes)
    } else {
        // Text with stray bytes outside UTF-8 still reads.
        parse_ascii(&String::from_utf8_lossy(bytes))
    }
}

// ----- binary ----------------------------------------------------------------------------------

struct Binary<'a> {
    s: &'a [u8],
    at: usize,
    wide: bool,
}

impl Binary<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], IoError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.s.len())
            .ok_or_else(|| corrupt(format!("the file ends early (byte {})", self.at)))?;
        let out = &self.s[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, IoError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, IoError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, IoError> {
        let b = self.take(8)?;
        let mut a = [0; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    fn offset(&mut self) -> Result<usize, IoError> {
        let v = if self.wide {
            self.u64()?
        } else {
            u64::from(self.u32()?)
        };
        usize::try_from(v).map_err(|_| corrupt("an offset is too large"))
    }

    fn array<T, const N: usize>(
        &mut self,
        decode: impl Fn([u8; N]) -> T,
    ) -> Result<Vec<T>, IoError> {
        let count = self.u32()? as usize;
        let encoding = self.u32()?;
        let length = self.u32()? as usize;
        if count > MAX_ARRAY {
            return Err(corrupt("an array is too long"));
        }
        let raw = self.take(length)?;
        let data = match encoding {
            0 => raw.to_vec(),
            1 => {
                let mut out = Vec::with_capacity(count * N);
                flate2::read::ZlibDecoder::new(raw)
                    .take((count * N) as u64)
                    .read_to_end(&mut out)
                    .map_err(|e| corrupt(format!("a compressed array: {e}")))?;
                out
            }
            other => return Err(corrupt(format!("unknown array encoding {other}"))),
        };
        if data.len() < count * N {
            return Err(corrupt("an array is shorter than it says"));
        }
        Ok(data
            .as_chunks::<N>()
            .0
            .iter()
            .take(count)
            .map(|c| decode(*c))
            .collect())
    }

    fn prop(&mut self) -> Result<Prop, IoError> {
        let kind = self.u8()?;
        Ok(match kind {
            b'C' => Prop::Bool(self.u8()? != 0),
            b'Y' => {
                let b = self.take(2)?;
                Prop::Int(i64::from(i16::from_le_bytes([b[0], b[1]])))
            }
            b'I' => Prop::Int(i64::from(self.u32()? as i32)),
            b'L' => Prop::Int(self.u64()? as i64),
            b'F' => Prop::Real(f64::from(f32::from_bits(self.u32()?))),
            b'D' => Prop::Real(f64::from_bits(self.u64()?)),
            b'S' | b'R' => {
                let n = self.u32()? as usize;
                let b = self.take(n)?.to_vec();
                if kind == b'S' {
                    Prop::Str(String::from_utf8_lossy(&b).into_owned())
                } else {
                    Prop::Raw(b)
                }
            }
            b'f' => Prop::Reals(self.array(|b: [u8; 4]| f64::from(f32::from_le_bytes(b)))?),
            b'd' => Prop::Reals(self.array(f64::from_le_bytes)?),
            b'i' => Prop::Ints(self.array(|b: [u8; 4]| i64::from(i32::from_le_bytes(b)))?),
            b'l' => Prop::Ints(self.array(i64::from_le_bytes)?),
            b'b' | b'c' => Prop::Ints(self.array(|b: [u8; 1]| i64::from(b[0]))?),
            other => {
                return Err(corrupt(format!(
                    "unknown property type `{}` at byte {}",
                    char::from(other),
                    self.at - 1
                )));
            }
        })
    }

    /// One node, or `None` at a list's end marker.
    fn node(&mut self, depth: usize) -> Result<Option<Node>, IoError> {
        if depth > MAX_DEPTH {
            return Err(corrupt("nodes are nested too deep"));
        }
        let start = self.at;
        let end = self.offset()?;
        let count = self.offset()?;
        let _list_length = self.offset()?;
        let name_length = self.u8()? as usize;
        if end == 0 {
            return Ok(None);
        }
        if end <= start || end > self.s.len() {
            return Err(corrupt(format!(
                "a node at byte {start} ends outside the file"
            )));
        }
        let name = String::from_utf8_lossy(self.take(name_length)?).into_owned();
        let mut props = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            props.push(self.prop()?);
        }
        let mut children = Vec::new();
        while self.at < end {
            match self.node(depth + 1)? {
                Some(child) => children.push(child),
                None => break,
            }
        }
        self.at = end;
        Ok(Some(Node {
            name,
            props,
            children,
        }))
    }
}

fn parse_binary(bytes: &[u8]) -> Result<Document, IoError> {
    if bytes.len() < 27 {
        return Err(corrupt("the file is too short"));
    }
    if bytes[22] != 0 {
        return Err(corrupt("big-endian FBX files are not supported"));
    }
    let version = u32::from_le_bytes([bytes[23], bytes[24], bytes[25], bytes[26]]);
    let mut b = Binary {
        s: bytes,
        at: 27,
        wide: version >= 7500,
    };
    let mut nodes = Vec::new();
    // The top-level list ends with an end marker, then the footer.
    let record = if b.wide { 25 } else { 13 };
    while b.at + record <= bytes.len() {
        match b.node(0)? {
            Some(n) => nodes.push(n),
            None => break,
        }
    }
    Ok(Document { version, nodes })
}

// ----- ASCII -----------------------------------------------------------------------------------

struct Ascii<'a> {
    s: &'a [u8],
    at: usize,
}

impl Ascii<'_> {
    fn skip(&mut self) {
        loop {
            while self.at < self.s.len() && self.s[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            if self.s.get(self.at) == Some(&b';') {
                while self.at < self.s.len() && self.s[self.at] != b'\n' {
                    self.at += 1;
                }
            } else {
                return;
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.s.get(self.at).copied()
    }

    fn word(&mut self) -> String {
        let start = self.at;
        while self.at < self.s.len()
            && (self.s[self.at].is_ascii_alphanumeric()
                || matches!(self.s[self.at], b'_' | b'|' | b'-'))
        {
            self.at += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.at]).into_owned()
    }

    fn value(&mut self) -> Result<Prop, IoError> {
        match self.peek() {
            Some(b'"') => {
                self.at += 1;
                let start = self.at;
                while self.at < self.s.len() && self.s[self.at] != b'"' {
                    self.at += 1;
                }
                let text = String::from_utf8_lossy(&self.s[start..self.at]).into_owned();
                self.at += 1;
                // `&quot;` stands for a quote in the text encoding.
                Ok(Prop::Str(text.replace("&quot;", "\"")))
            }
            Some(b'*') => {
                // An array: `*count { a: v, v, … }`.
                self.at += 1;
                let _ = self.number_text();
                if self.peek() != Some(b'{') {
                    return Err(corrupt(format!("bad array at byte {}", self.at)));
                }
                self.at += 1;
                self.skip();
                let _ = self.word();
                if self.peek() == Some(b':') {
                    self.at += 1;
                }
                let mut ints = Vec::new();
                let mut reals = Vec::new();
                let mut real = false;
                loop {
                    match self.peek() {
                        Some(b'}') => {
                            self.at += 1;
                            break;
                        }
                        Some(b',') => self.at += 1,
                        Some(_) => {
                            let text = self.number_text();
                            if text.is_empty() {
                                return Err(corrupt(format!("bad array at byte {}", self.at)));
                            }
                            real |= text.contains(['.', 'e', 'E']);
                            reals.push(text.parse::<f64>().map_err(|_| corrupt("bad number"))?);
                            ints.push(text.parse::<i64>().unwrap_or(0));
                        }
                        None => return Err(corrupt("an array is not closed")),
                    }
                }
                Ok(if real {
                    Prop::Reals(reals)
                } else {
                    Prop::Ints(ints)
                })
            }
            Some(c) if c.is_ascii_digit() || c == b'-' || c == b'+' || c == b'.' => {
                let text = self.number_text();
                if text.contains(['.', 'e', 'E']) {
                    text.parse()
                        .map(Prop::Real)
                        .map_err(|_| corrupt(format!("bad number `{text}`")))
                } else {
                    text.parse()
                        .map(Prop::Int)
                        .map_err(|_| corrupt(format!("bad number `{text}`")))
                }
            }
            Some(c) if c.is_ascii_alphabetic() => {
                // Bare words: `T`/`Y` true, `F`/`N` false, others as text.
                let w = self.word();
                Ok(match w.as_str() {
                    "T" | "Y" => Prop::Bool(true),
                    "F" | "N" => Prop::Bool(false),
                    _ => Prop::Str(w),
                })
            }
            _ => Err(corrupt(format!("unexpected text at byte {}", self.at))),
        }
    }

    fn number_text(&mut self) -> String {
        self.skip();
        let start = self.at;
        while self.at < self.s.len()
            && (self.s[self.at].is_ascii_digit()
                || matches!(self.s[self.at], b'.' | b'-' | b'+' | b'e' | b'E'))
        {
            self.at += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.at]).into_owned()
    }

    /// The nodes up to `}` (or the end of the file at depth 0).
    fn nodes(&mut self, depth: usize) -> Result<Vec<Node>, IoError> {
        if depth > MAX_DEPTH {
            return Err(corrupt("nodes are nested too deep"));
        }
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None if depth == 0 => return Ok(out),
                None => return Err(corrupt("a `{` is not closed")),
                Some(b'}') if depth > 0 => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(_) => {}
            }
            let name = self.word();
            if name.is_empty() || self.peek() != Some(b':') {
                return Err(corrupt(format!("expected a node name at byte {}", self.at)));
            }
            self.at += 1;
            let mut node = Node {
                name,
                ..Node::default()
            };
            // Properties up to the line's end or a `{`; a line ending in a
            // comma goes on to the next.
            let mut comma = false;
            loop {
                while self.at < self.s.len() && matches!(self.s[self.at], b' ' | b'\t' | b'\r') {
                    self.at += 1;
                }
                match self.s.get(self.at) {
                    Some(b'\n') | Some(b';') if comma => self.skip(),
                    None | Some(b'\n') | Some(b';') => break,
                    Some(b'{') => {
                        self.at += 1;
                        node.children = self.nodes(depth + 1)?;
                        break;
                    }
                    Some(b',') => {
                        self.at += 1;
                        comma = true;
                    }
                    Some(b'}') => break,
                    Some(_) => {
                        node.props.push(self.value()?);
                        comma = false;
                    }
                }
            }
            out.push(node);
        }
    }
}

fn parse_ascii(text: &str) -> Result<Document, IoError> {
    let mut a = Ascii {
        s: text.as_bytes(),
        at: 0,
    };
    let nodes = a.nodes(0)?;
    let version = nodes
        .iter()
        .find(|n| n.name == "FBXHeaderExtension")
        .and_then(|h| h.value("FBXVersion"))
        .and_then(Prop::as_i64)
        .unwrap_or(0) as u32;
    if !text.trim_start().starts_with(';') && version == 0 {
        return Err(corrupt("not an FBX file"));
    }
    Ok(Document { version, nodes })
}

// ----- binary writer ---------------------------------------------------------------------------

/// The writer's version: 7.4, the last with 32-bit records.
pub const WRITE_VERSION: u32 = 7400;
/// A fixed file id (the binary encoding requires one; any 16 bytes do).
pub const FILE_ID: [u8; 16] = [
    0x28, 0xb3, 0x2a, 0xeb, 0xb6, 0x24, 0xcc, 0xc2, 0xbf, 0xc8, 0xb0, 0x2a, 0xa9, 0x2b, 0xfc, 0xf1,
];
/// The footer's id that goes with [`FILE_ID`] and the fixed creation time.
const FOOT_ID: [u8; 16] = [
    0xfa, 0xbc, 0xab, 0x09, 0xd0, 0xc8, 0xd4, 0x66, 0xb1, 0x76, 0xfb, 0x83, 0x1c, 0xf7, 0x26, 0x7e,
];
/// The binary encoding's closing bytes.
const FOOT_MAGIC: [u8; 16] = [
    0xf8, 0x5a, 0x8c, 0x6a, 0xde, 0xf5, 0xd9, 0x7e, 0xec, 0xe9, 0x0c, 0xe3, 0x75, 0x8f, 0x29, 0x0b,
];
/// The creation time written (fixed, for byte-identical output).
pub const CREATION_TIME: &str = "1970-01-01 10:00:00:000";

fn write_array<T: Copy>(
    out: &mut Vec<u8>,
    kind: u8,
    values: &[T],
    size: usize,
    bytes: impl Fn(T) -> Vec<u8>,
) {
    out.push(kind);
    let mut raw = Vec::with_capacity(values.len() * size);
    for v in values {
        raw.extend(bytes(*v));
    }
    out.extend((values.len() as u32).to_le_bytes());
    // Arrays of more than a few values are compressed, as readers expect.
    if raw.len() > 128 {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        let _ = z.write_all(&raw);
        let packed = z.finish().unwrap_or_default();
        out.extend(1u32.to_le_bytes());
        out.extend((packed.len() as u32).to_le_bytes());
        out.extend(packed);
    } else {
        out.extend(0u32.to_le_bytes());
        out.extend((raw.len() as u32).to_le_bytes());
        out.extend(raw);
    }
}

fn write_prop(out: &mut Vec<u8>, p: &Prop) {
    match p {
        Prop::Bool(b) => {
            out.push(b'C');
            out.push(u8::from(*b));
        }
        Prop::Int(i) => match i32::try_from(*i) {
            Ok(small) => {
                out.push(b'I');
                out.extend(small.to_le_bytes());
            }
            Err(_) => {
                out.push(b'L');
                out.extend(i.to_le_bytes());
            }
        },
        Prop::Real(r) => {
            out.push(b'D');
            out.extend(r.to_le_bytes());
        }
        Prop::Str(s) => {
            out.push(b'S');
            out.extend((s.len() as u32).to_le_bytes());
            out.extend(s.as_bytes());
        }
        Prop::Raw(b) => {
            out.push(b'R');
            out.extend((b.len() as u32).to_le_bytes());
            out.extend(b);
        }
        Prop::Ints(v) => {
            if v.iter().all(|i| i32::try_from(*i).is_ok()) {
                write_array(out, b'i', v, 4, |i| (i as i32).to_le_bytes().to_vec());
            } else {
                write_array(out, b'l', v, 8, |i: i64| i.to_le_bytes().to_vec());
            }
        }
        Prop::Reals(v) => write_array(out, b'd', v, 8, |r: f64| r.to_le_bytes().to_vec()),
    }
}

fn write_node(out: &mut Vec<u8>, node: &Node) {
    let start = out.len();
    out.extend([0u8; 12]);
    out.push(node.name.len().min(255) as u8);
    out.extend(&node.name.as_bytes()[..node.name.len().min(255)]);
    let props_start = out.len();
    for p in &node.props {
        write_prop(out, p);
    }
    let props_length = out.len() - props_start;
    // A node with children, or with none and no properties, ends its list
    // with an end marker.
    if !node.children.is_empty() || node.props.is_empty() {
        for c in &node.children {
            write_node(out, c);
        }
        out.extend([0u8; 13]);
    }
    let end = out.len() as u32;
    out[start..start + 4].copy_from_slice(&end.to_le_bytes());
    out[start + 4..start + 8].copy_from_slice(&(node.props.len() as u32).to_le_bytes());
    out[start + 8..start + 12].copy_from_slice(&(props_length as u32).to_le_bytes());
}

/// Writes `nodes` in the binary encoding, version [`WRITE_VERSION`].
pub fn write_binary(nodes: &[Node]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(BINARY_MAGIC);
    out.extend([0x1a, 0x00]);
    out.extend(WRITE_VERSION.to_le_bytes());
    for n in nodes {
        write_node(&mut out, n);
    }
    out.extend([0u8; 13]);
    out.extend(FOOT_ID);
    // Padding to a multiple of 16, then the version and the closing bytes.
    let pad = (16 - out.len() % 16) % 16;
    let pad = if pad == 0 { 16 } else { pad };
    out.extend(std::iter::repeat_n(0u8, pad));
    out.extend(0u32.to_le_bytes());
    out.extend(WRITE_VERSION.to_le_bytes());
    out.extend([0u8; 120]);
    out.extend(FOOT_MAGIC);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sample() -> Vec<Node> {
        vec![
            Node::new("FBXHeaderExtension", vec![])
                .with(Node::new("FBXVersion", vec![Prop::Int(7400)])),
            Node::new("Objects", vec![]).with(
                Node::new(
                    "Geometry",
                    vec![
                        Prop::Int(1_000_000_000_001),
                        Prop::Str("Box\0\x01Geometry".into()),
                        Prop::Str("Mesh".into()),
                    ],
                )
                .with(Node::new(
                    "Vertices",
                    vec![Prop::Reals((0..300).map(f64::from).collect())],
                ))
                .with(Node::new(
                    "PolygonVertexIndex",
                    vec![Prop::Ints(vec![0, 1, -3])],
                ))
                .with(Node::new(
                    "Smooth",
                    vec![Prop::Bool(true), Prop::Real(0.5), Prop::Raw(vec![1, 2])],
                )),
            ),
        ]
    }

    #[test]
    fn binary_files_read_back_what_was_written() {
        let nodes = sample();
        let bytes = write_binary(&nodes);
        assert!(bytes.starts_with(BINARY_MAGIC));
        assert!(bytes.ends_with(&FOOT_MAGIC));
        let doc = parse(&bytes).unwrap();
        assert_eq!(doc.version, WRITE_VERSION);
        assert_eq!(doc.nodes, nodes);
        // The same bytes every time.
        assert_eq!(write_binary(&nodes), bytes);
    }

    #[test]
    fn the_text_encoding_reads_nodes_arrays_and_properties() {
        let text = r#"; FBX 7.4.0 project file
FBXHeaderExtension:  {
	FBXVersion: 7400
}
Objects:  {
	Geometry: 123, "Geometry::Box", "Mesh" {
		Vertices: *6 {
			a: 0,0,0,1.5,-2,3e2
		}
		PolygonVertexIndex: *3 {
			a: 0,1,-3
		}
		Properties70:  {
			P: "Lcl Translation", "Lcl Translation", "", "A",1,2,3
		}
		Visible: T
	}
}
"#;
        let doc = parse(text.as_bytes()).unwrap();
        assert_eq!(doc.version, 7400);
        let g = doc.node("Objects").unwrap().child("Geometry").unwrap();
        assert_eq!(g.props[0], Prop::Int(123));
        assert_eq!(g.props[1].as_str(), Some("Geometry::Box"));
        assert_eq!(
            g.value("Vertices"),
            Some(&Prop::Reals(vec![0.0, 0.0, 0.0, 1.5, -2.0, 300.0]))
        );
        assert_eq!(
            g.value("PolygonVertexIndex"),
            Some(&Prop::Ints(vec![0, 1, -3]))
        );
        let t = g.property("Lcl Translation").unwrap();
        assert_eq!(
            t.iter().map(|p| p.as_f64().unwrap()).collect::<Vec<_>>(),
            vec![1.0, 2.0, 3.0]
        );
        assert_eq!(g.value("Visible"), Some(&Prop::Bool(true)));
        assert!(parse(b"Kaydara FBX Binary  \0\x1a\0").is_err());
    }

    /// Parses every file of the folder `PLANYRITH_FBX_CORPUS` (a local
    /// survey; the files are not part of the repository).
    #[test]
    #[ignore = "needs a local corpus"]
    fn corpus_parses() {
        let Ok(dir) = std::env::var("PLANYRITH_FBX_CORPUS") else {
            return;
        };
        let mut failed = Vec::new();
        let mut count = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "fbx") {
                count += 1;
                if let Err(e) = parse(&std::fs::read(&path).unwrap()) {
                    failed.push(format!("{}: {e}", path.display()));
                }
            }
        }
        assert!(
            failed.is_empty(),
            "{} of {count}:\n{}",
            failed.len(),
            failed.join("\n")
        );
    }
}
