//! Implements docs/09-file-formats.md §5 USD binary layers (milestone 1.50):
//! the crate file (`PXR-USDC`), decoded into specs — a path, a spec type and
//! named fields — that the USD importer composes like a text layer.
//!
//! Sections: tokens (null-separated, block-compressed), strings (token
//! indices), fields (token index and value representation), field sets,
//! paths (a compressed tree) and specs. Value representations hold small
//! values inline and point to the rest; integer arrays may be compressed
//! (deltas from the previous value, coded in 2 bits each, then
//! block-compressed) and float arrays may be stored as integers or through a
//! lookup table. The block compression is LZ4, decoded here without a
//! library. Files from version 0.4 on are read.

use std::collections::BTreeMap;

use crate::io::IoError;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "usdc".into(),
        reason: reason.into(),
    }
}

/// The most bytes one decompressed buffer may take.
const MAX_BUFFER: usize = 1 << 31;
/// The most elements one array may hold.
const MAX_ELEMENTS: u64 = 1 << 28;

/// A value of a crate field.
#[derive(Debug, Clone, PartialEq)]
pub enum CrateValue {
    /// A number (booleans as 0 or 1).
    Num(f64),
    /// A token or string.
    Text(String),
    /// An asset path.
    Asset(String),
    /// A fixed tuple of numbers (vectors, quaternions as x, y, z, w).
    Tuple(Vec<f64>),
    /// A square matrix, row by row.
    Matrix(usize, Vec<f64>),
    /// An array.
    Array(Vec<CrateValue>),
    /// Paths (relationship targets, attribute connections).
    Paths(Vec<String>),
    /// References: asset path (empty for this layer) and prim path (empty
    /// for the default prim).
    References(Vec<(String, String)>),
    /// A specifier: 0 def, 1 over, 2 class.
    Specifier(u32),
    /// Time samples: the first sample's value.
    Sampled(Box<CrateValue>),
    /// Something the importer does not use.
    Other,
}

/// One spec of the layer.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    /// Its path (`/World/Mesh`, `/World/Mesh.points`, `/` for the layer).
    pub path: String,
    /// Its type: 1 attribute, 6 prim, 7 the layer, 8 relationship, others
    /// unused.
    pub kind: u32,
    /// Its fields.
    pub fields: Vec<(String, CrateValue)>,
}

// ----- block compression --------------------------------------------------------------------

/// Decodes one LZ4 block.
fn lz4_block(src: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<(), IoError> {
    let bad = || corrupt("a compressed block is broken");
    let mut i = 0;
    let start = out.len();
    while i < src.len() {
        let token = src[i];
        i += 1;
        let mut literals = usize::from(token >> 4);
        if literals == 15 {
            loop {
                let b = *src.get(i).ok_or_else(bad)?;
                i += 1;
                literals += usize::from(b);
                if b != 255 {
                    break;
                }
            }
        }
        let end = i
            .checked_add(literals)
            .filter(|e| *e <= src.len())
            .ok_or_else(bad)?;
        out.extend_from_slice(&src[i..end]);
        i = end;
        if i >= src.len() {
            break;
        }
        let offset = usize::from(u16::from_le_bytes([
            *src.get(i).ok_or_else(bad)?,
            *src.get(i + 1).ok_or_else(bad)?,
        ]));
        i += 2;
        let mut length = usize::from(token & 15);
        if length == 15 {
            loop {
                let b = *src.get(i).ok_or_else(bad)?;
                i += 1;
                length += usize::from(b);
                if b != 255 {
                    break;
                }
            }
        }
        length += 4;
        if offset == 0 || offset > out.len() - start || out.len() - start + length > limit {
            return Err(bad());
        }
        let from = out.len() - offset;
        for k in 0..length {
            let b = out[from + k];
            out.push(b);
        }
    }
    if out.len() - start > limit {
        return Err(bad());
    }
    Ok(())
}

/// Decompresses a buffer of the format's block compression: a chunk count,
/// then either one block or that many sized blocks.
fn decompress(src: &[u8], limit: usize) -> Result<Vec<u8>, IoError> {
    let (&chunks, rest) = src
        .split_first()
        .ok_or_else(|| corrupt("an empty compressed buffer"))?;
    let mut out = Vec::new();
    if chunks == 0 {
        lz4_block(rest, &mut out, limit)?;
        return Ok(out);
    }
    let mut at = 0;
    for _ in 0..chunks {
        let size = rest
            .get(at..at + 4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .filter(|s| *s >= 0)
            .ok_or_else(|| corrupt("a compressed chunk is broken"))? as usize;
        at += 4;
        let block = rest
            .get(at..at + size)
            .ok_or_else(|| corrupt("a compressed chunk overruns"))?;
        at += size;
        let room = limit.saturating_sub(out.len());
        lz4_block(block, &mut out, room)?;
    }
    Ok(out)
}

/// Decodes `n` compressed integers (32-bit when `wide` is false).
fn decode_ints(compressed: &[u8], n: usize, wide: bool) -> Result<Vec<i64>, IoError> {
    let width = if wide { 8 } else { 4 };
    let limit = width + n.div_ceil(4) + n * width + 64;
    let data = decompress(compressed, limit)?;
    let bad = || corrupt("compressed integers end early");
    let read = |at: &mut usize, w: usize| -> Result<i64, IoError> {
        let b = data.get(*at..*at + w).ok_or_else(bad)?;
        *at += w;
        Ok(match w {
            1 => i64::from(b[0] as i8),
            2 => i64::from(i16::from_le_bytes([b[0], b[1]])),
            4 => i64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            _ => i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        })
    };
    let mut at = 0;
    let common = read(&mut at, width)?;
    let codes = at;
    let mut vints = codes + n.div_ceil(4);
    let (small, medium, large) = if wide { (2, 4, 8) } else { (1, 2, 4) };
    let mut prev: i64 = 0;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let byte = *data.get(codes + i / 4).ok_or_else(bad)?;
        prev = prev.wrapping_add(match (byte >> (2 * (i % 4))) & 3 {
            0 => common,
            1 => read(&mut vints, small)?,
            2 => read(&mut vints, medium)?,
            _ => read(&mut vints, large)?,
        });
        out.push(if wide { prev } else { i64::from(prev as i32) });
    }
    Ok(out)
}

/// A half-precision float.
fn half(bits: u16) -> f64 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((bits >> 10) & 0x1f);
    let frac = f64::from(bits & 0x3ff);
    sign * match exp {
        0 => frac * 2f64.powi(-24),
        31 => {
            if frac == 0.0 {
                f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => (1.0 + frac / 1024.0) * 2f64.powi(exp - 15),
    }
}

// ----- reading ------------------------------------------------------------------------------

/// A position in the file.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn seek(&mut self, at: u64) -> Result<(), IoError> {
        let at = usize::try_from(at).map_err(|_| corrupt("an offset is out of range"))?;
        if at > self.data.len() {
            return Err(corrupt("an offset is past the end"));
        }
        self.at = at;
        Ok(())
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], IoError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|e| *e <= self.data.len())
            .ok_or_else(|| corrupt("the file ends early"))?;
        let s = &self.data[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, IoError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, IoError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn i64(&mut self) -> Result<i64, IoError> {
        Ok(self.u64()? as i64)
    }
    fn f64(&mut self) -> Result<f64, IoError> {
        Ok(f64::from_bits(self.u64()?))
    }
    fn f32(&mut self) -> Result<f64, IoError> {
        Ok(f64::from(f32::from_bits(self.u32()?)))
    }
    fn count(&mut self) -> Result<usize, IoError> {
        let n = self.u64()?;
        if n > MAX_ELEMENTS {
            return Err(corrupt("a count is too large"));
        }
        Ok(n as usize)
    }
    /// Compressed integers: their compressed size, then the bytes.
    fn ints(&mut self, n: usize, wide: bool) -> Result<Vec<i64>, IoError> {
        let size = self.count()?;
        let bytes = self.take(size)?;
        decode_ints(bytes, n, wide)
    }
}

/// The decoded tables.
struct Crate<'a> {
    data: &'a [u8],
    version: (u8, u8, u8),
    tokens: Vec<String>,
    strings: Vec<u32>,
    paths: Vec<String>,
}

/// A value representation: type, flags and payload.
#[derive(Debug, Clone, Copy)]
struct Rep(u64);

impl Rep {
    fn kind(self) -> u8 {
        ((self.0 >> 48) & 0xff) as u8
    }
    fn array(self) -> bool {
        self.0 & (1 << 63) != 0
    }
    fn inlined(self) -> bool {
        self.0 & (1 << 62) != 0
    }
    fn compressed(self) -> bool {
        self.0 & (1 << 61) != 0
    }
    fn payload(self) -> u64 {
        self.0 & ((1 << 48) - 1)
    }
}

// Value types.
const BOOL: u8 = 1;
const UCHAR: u8 = 2;
const INT: u8 = 3;
const UINT: u8 = 4;
const INT64: u8 = 5;
const UINT64: u8 = 6;
const HALF: u8 = 7;
const FLOAT: u8 = 8;
const DOUBLE: u8 = 9;
const STRING: u8 = 10;
const TOKEN: u8 = 11;
const ASSET: u8 = 12;
const MATRIX2D: u8 = 13;
const MATRIX3D: u8 = 14;
const MATRIX4D: u8 = 15;
const QUATD: u8 = 16;
const QUATF: u8 = 17;
const QUATH: u8 = 18;
const VEC2D: u8 = 19;
const VEC2F: u8 = 20;
const VEC2H: u8 = 21;
const VEC2I: u8 = 22;
const VEC3D: u8 = 23;
const VEC3F: u8 = 24;
const VEC3H: u8 = 25;
const VEC3I: u8 = 26;
const VEC4D: u8 = 27;
const VEC4F: u8 = 28;
const VEC4H: u8 = 29;
const VEC4I: u8 = 30;
const PATH_LIST_OP: u8 = 34;
const REFERENCE_LIST_OP: u8 = 35;
const PATH_VECTOR: u8 = 40;
const TOKEN_VECTOR: u8 = 41;
const SPECIFIER: u8 = 42;
const VARIABILITY: u8 = 44;
const TIME_SAMPLES: u8 = 46;
const DOUBLE_VECTOR: u8 = 48;
const STRING_VECTOR: u8 = 50;

/// Components and their scalar of a vector, matrix or quaternion type.
fn shape(kind: u8) -> Option<(usize, u8, bool)> {
    // (components, scalar type, matrix)
    Some(match kind {
        MATRIX2D => (4, DOUBLE, true),
        MATRIX3D => (9, DOUBLE, true),
        MATRIX4D => (16, DOUBLE, true),
        QUATD => (4, DOUBLE, false),
        QUATF => (4, FLOAT, false),
        QUATH => (4, HALF, false),
        VEC2D => (2, DOUBLE, false),
        VEC2F => (2, FLOAT, false),
        VEC2H => (2, HALF, false),
        VEC2I => (2, INT, false),
        VEC3D => (3, DOUBLE, false),
        VEC3F => (3, FLOAT, false),
        VEC3H => (3, HALF, false),
        VEC3I => (3, INT, false),
        VEC4D => (4, DOUBLE, false),
        VEC4F => (4, FLOAT, false),
        VEC4H => (4, HALF, false),
        VEC4I => (4, INT, false),
        _ => return None,
    })
}

impl Crate<'_> {
    fn at(&self, at: u64) -> Result<Reader<'_>, IoError> {
        let mut r = Reader {
            data: self.data,
            at: 0,
        };
        r.seek(at)?;
        Ok(r)
    }

    fn token(&self, i: u64) -> String {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.tokens.get(i))
            .cloned()
            .unwrap_or_default()
    }

    fn string(&self, i: u64) -> String {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.strings.get(i))
            .map(|t| self.token(u64::from(*t)))
            .unwrap_or_default()
    }

    fn path(&self, i: u64) -> String {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.paths.get(i))
            .cloned()
            .unwrap_or_default()
    }

    /// One scalar of type `kind` from the reader.
    fn scalar(&self, r: &mut Reader<'_>, kind: u8) -> Result<f64, IoError> {
        Ok(match kind {
            BOOL | UCHAR => f64::from(r.take(1)?[0]),
            INT => f64::from(r.u32()? as i32),
            UINT => f64::from(r.u32()?),
            INT64 => r.i64()? as f64,
            UINT64 => r.u64()? as f64,
            HALF => {
                let b = r.take(2)?;
                half(u16::from_le_bytes([b[0], b[1]]))
            }
            FLOAT => r.f32()?,
            DOUBLE => r.f64()?,
            _ => return Err(corrupt("not a number type")),
        })
    }

    /// A tuple or matrix value from the reader (quaternions reordered to
    /// w, x, y, z as text layers write them).
    fn compound(&self, r: &mut Reader<'_>, kind: u8) -> Result<CrateValue, IoError> {
        let (n, scalar, matrix) = shape(kind).ok_or_else(|| corrupt("not a compound type"))?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.scalar(r, scalar)?);
        }
        Ok(if matrix {
            CrateValue::Matrix((n as f64).sqrt() as usize, v)
        } else if matches!(kind, QUATD | QUATF | QUATH) {
            CrateValue::Tuple(vec![v[3], v[0], v[1], v[2]])
        } else {
            CrateValue::Tuple(v)
        })
    }

    /// The element count before an array's data.
    fn array_len(&self, r: &mut Reader<'_>) -> Result<usize, IoError> {
        if self.version < (0, 7, 0) {
            Ok(r.u32()? as usize)
        } else {
            r.count()
        }
    }

    /// An array value.
    fn array(&self, rep: Rep) -> Result<CrateValue, IoError> {
        let kind = rep.kind();
        if rep.payload() == 0 {
            return Ok(CrateValue::Array(Vec::new()));
        }
        let mut r = self.at(rep.payload())?;
        let n = self.array_len(&mut r)?;
        if n as u64 > MAX_ELEMENTS {
            return Err(corrupt("an array is too large"));
        }
        let nums = |v: Vec<f64>| CrateValue::Array(v.into_iter().map(CrateValue::Num).collect());
        match kind {
            INT | UINT | INT64 | UINT64 if rep.compressed() => {
                let wide = matches!(kind, INT64 | UINT64);
                let ints = r.ints(n, wide)?;
                Ok(nums(
                    ints.into_iter()
                        .map(|i| match kind {
                            UINT => f64::from(i as u32),
                            UINT64 => i as u64 as f64,
                            _ => i as f64,
                        })
                        .collect(),
                ))
            }
            HALF | FLOAT | DOUBLE if rep.compressed() => {
                let code = r.take(1)?[0];
                match code {
                    b'i' => Ok(nums(
                        r.ints(n, false)?.into_iter().map(|i| i as f64).collect(),
                    )),
                    b't' => {
                        let size = r.u32()? as usize;
                        let mut table = Vec::with_capacity(size.min(1 << 20));
                        for _ in 0..size {
                            table.push(self.scalar(&mut r, kind)?);
                        }
                        let index = r.ints(n, false)?;
                        Ok(nums(
                            index
                                .into_iter()
                                .map(|i| table.get(i as usize).copied().unwrap_or(0.0))
                                .collect(),
                        ))
                    }
                    _ => Err(corrupt("an unknown float array coding")),
                }
            }
            BOOL | UCHAR | INT | UINT | INT64 | UINT64 | HALF | FLOAT | DOUBLE => {
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.scalar(&mut r, kind)?);
                }
                Ok(nums(v))
            }
            TOKEN | STRING | ASSET => {
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    let i = u64::from(r.u32()?);
                    v.push(match kind {
                        STRING => CrateValue::Text(self.string(i)),
                        ASSET => CrateValue::Asset(self.token(i)),
                        _ => CrateValue::Text(self.token(i)),
                    });
                }
                Ok(CrateValue::Array(v))
            }
            k if shape(k).is_some() => {
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.compound(&mut r, k)?);
                }
                Ok(CrateValue::Array(v))
            }
            _ => Ok(CrateValue::Other),
        }
    }

    /// A vector of path or token indices (a count, then 32-bit indices).
    fn indices(&self, r: &mut Reader<'_>) -> Result<Vec<u64>, IoError> {
        let n = r.count()?;
        (0..n).map(|_| r.u32().map(u64::from)).collect()
    }

    /// A list operation's items: explicit, added, prepended and appended
    /// (deleted and reordered items are skipped).
    fn list_op<T>(
        &self,
        r: &mut Reader<'_>,
        mut item: impl FnMut(&mut Reader<'_>) -> Result<T, IoError>,
    ) -> Result<Vec<T>, IoError> {
        let bits = r.take(1)?[0];
        let mut out = Vec::new();
        // Explicit, added, deleted, ordered, prepended, appended: the
        // header's bits name which lists follow, in this order.
        for (bit, keep) in [
            (1 << 1, true),
            (1 << 2, true),
            (1 << 5, true),
            (1 << 6, true),
            (1 << 3, false),
            (1 << 4, false),
        ] {
            if bits & bit == 0 {
                continue;
            }
            let n = r.count()?;
            for _ in 0..n {
                let v = item(r)?;
                if keep {
                    out.push(v);
                }
            }
        }
        Ok(out)
    }

    /// Skips a dictionary (a count, then string keys and values reached
    /// through relative offsets).
    fn skip_dictionary(&self, r: &mut Reader<'_>) -> Result<(), IoError> {
        let n = r.count()?;
        for _ in 0..n {
            r.u32()?; // key
            let start = r.at as u64;
            let jump = r.i64()?;
            let _ = start.checked_add_signed(jump);
            // The value lies elsewhere; the stream continues after the jump.
        }
        Ok(())
    }

    /// Unpacks a value representation.
    fn value(&self, rep: Rep, depth: usize) -> Result<CrateValue, IoError> {
        if depth > 8 {
            return Err(corrupt("values nest too deeply"));
        }
        let kind = rep.kind();
        if rep.array() {
            return self.array(rep);
        }
        let p = rep.payload();
        if rep.inlined() {
            return Ok(match kind {
                BOOL | UCHAR | UINT => CrateValue::Num(p as u32 as f64),
                INT => CrateValue::Num(f64::from(p as u32 as i32)),
                FLOAT => CrateValue::Num(f64::from(f32::from_bits(p as u32))),
                DOUBLE => CrateValue::Num(f64::from(f32::from_bits(p as u32))),
                HALF => CrateValue::Num(half(p as u16)),
                TOKEN => CrateValue::Text(self.token(p)),
                STRING => CrateValue::Text(self.string(p)),
                ASSET => CrateValue::Asset(self.token(p)),
                SPECIFIER => CrateValue::Specifier(p as u32),
                VARIABILITY => CrateValue::Num(p as u32 as f64),
                k if shape(k).is_some() => {
                    // Small integer components, one signed byte each (a
                    // matrix's diagonal).
                    let (n, _, matrix) = shape(k).ok_or_else(|| corrupt("bad shape"))?;
                    let bytes = p.to_le_bytes();
                    if matrix {
                        let size = (n as f64).sqrt() as usize;
                        let mut m = vec![0.0; n];
                        for d in 0..size {
                            m[d * size + d] = f64::from(bytes[d] as i8);
                        }
                        CrateValue::Matrix(size, m)
                    } else {
                        let v: Vec<f64> = (0..n).map(|i| f64::from(bytes[i] as i8)).collect();
                        if matches!(k, QUATD | QUATF | QUATH) {
                            CrateValue::Tuple(vec![v[3], v[0], v[1], v[2]])
                        } else {
                            CrateValue::Tuple(v)
                        }
                    }
                }
                _ => CrateValue::Other,
            });
        }
        let mut r = self.at(p)?;
        Ok(match kind {
            INT64 | UINT64 | DOUBLE | FLOAT | INT | UINT | HALF => {
                CrateValue::Num(self.scalar(&mut r, kind)?)
            }
            k if shape(k).is_some() => self.compound(&mut r, k)?,
            TOKEN_VECTOR => CrateValue::Array(
                self.indices(&mut r)?
                    .into_iter()
                    .map(|i| CrateValue::Text(self.token(i)))
                    .collect(),
            ),
            STRING_VECTOR => CrateValue::Array(
                self.indices(&mut r)?
                    .into_iter()
                    .map(|i| CrateValue::Text(self.string(i)))
                    .collect(),
            ),
            DOUBLE_VECTOR => {
                let n = r.count()?;
                CrateValue::Array(
                    (0..n)
                        .map(|_| r.f64().map(CrateValue::Num))
                        .collect::<Result<_, _>>()?,
                )
            }
            PATH_VECTOR => CrateValue::Paths(
                self.indices(&mut r)?
                    .into_iter()
                    .map(|i| self.path(i))
                    .collect(),
            ),
            PATH_LIST_OP => {
                CrateValue::Paths(self.list_op(&mut r, |r| Ok(self.path(u64::from(r.u32()?))))?)
            }
            REFERENCE_LIST_OP => CrateValue::References(self.list_op(&mut r, |r| {
                let asset = self.string(u64::from(r.u32()?));
                let prim = self.path(u64::from(r.u32()?));
                r.f64()?; // layer offset
                r.f64()?; // layer scale
                self.skip_dictionary(r)?;
                Ok((asset, prim))
            })?),
            TIME_SAMPLES => {
                // A jump to the times, then a jump to the value count and
                // representations.
                let start = r.at as u64;
                let jump = r.i64()?;
                r.seek(
                    start
                        .checked_add_signed(jump)
                        .ok_or_else(|| corrupt("bad jump"))?,
                )?;
                let _times = Rep(r.u64()?);
                let start = r.at as u64;
                let jump = r.i64()?;
                r.seek(
                    start
                        .checked_add_signed(jump)
                        .ok_or_else(|| corrupt("bad jump"))?,
                )?;
                let n = r.count()?;
                if n == 0 {
                    CrateValue::Other
                } else {
                    CrateValue::Sampled(Box::new(self.value(Rep(r.u64()?), depth + 1)?))
                }
            }
            _ => CrateValue::Other,
        })
    }
}

/// Builds the paths from their compressed tree (indices, element tokens,
/// jumps), as the format stores them.
fn build_paths(
    tokens: &[String],
    index: &[i64],
    element: &[i64],
    jumps: &[i64],
    out: &mut [String],
) -> Result<(), IoError> {
    // (position, parent path) to visit.
    let mut stack: Vec<(usize, Option<String>)> = vec![(0, None)];
    let mut guard = 0;
    while let Some((mut at, mut parent)) = stack.pop() {
        loop {
            guard += 1;
            if guard > 4 * index.len() + 16 || at >= index.len() {
                return Err(corrupt("the path tree is broken"));
            }
            let slot = usize::try_from(index[at]).map_err(|_| corrupt("bad path index"))?;
            let path = match &parent {
                None => "/".to_string(),
                Some(p) => {
                    let t = element[at];
                    let name = tokens
                        .get(t.unsigned_abs() as usize)
                        .ok_or_else(|| corrupt("bad path token"))?;
                    if t < 0 {
                        format!("{p}.{name}")
                    } else if p == "/" {
                        format!("/{name}")
                    } else {
                        format!("{p}/{name}")
                    }
                }
            };
            *out.get_mut(slot).ok_or_else(|| corrupt("bad path index"))? = path.clone();
            let jump = jumps[at];
            let child = jump > 0 || jump == -1;
            let sibling = jump >= 0;
            if child {
                if sibling {
                    let s =
                        usize::try_from(at as i64 + jump).map_err(|_| corrupt("bad path jump"))?;
                    stack.push((s, parent.clone()));
                }
                parent = Some(path);
                at += 1;
            } else if sibling {
                at += 1;
            } else {
                break;
            }
        }
    }
    Ok(())
}

/// Reads a crate file's specs.
pub fn read(data: &[u8]) -> Result<Vec<Spec>, IoError> {
    if !data.starts_with(b"PXR-USDC") || data.len() < 88 {
        return Err(IoError::UnsupportedFormat("not a binary USD layer".into()));
    }
    let version = (data[8], data[9], data[10]);
    if version < (0, 4, 0) {
        return Err(IoError::UnsupportedFormat(format!(
            "binary USD version {}.{}.{} is older than this reader (0.4.0 and later)",
            version.0, version.1, version.2
        )));
    }
    let mut r = Reader { data, at: 16 };
    let toc = r.u64()?;
    r.seek(toc)?;
    let n = r.count()?;
    let mut sections: BTreeMap<String, u64> = BTreeMap::new();
    for _ in 0..n.min(64) {
        let name = r.take(16)?;
        let name: String = name
            .iter()
            .take_while(|b| **b != 0)
            .map(|b| char::from(*b))
            .collect();
        let start = r.u64()?;
        r.u64()?; // size
        sections.insert(name, start);
    }
    let section = |name: &str| -> Result<Reader<'_>, IoError> {
        let at = *sections
            .get(name)
            .ok_or_else(|| corrupt(format!("the {name} section is missing")))?;
        let mut r = Reader { data, at: 0 };
        r.seek(at)?;
        Ok(r)
    };
    // Tokens.
    let mut r = section("TOKENS")?;
    let count = r.count()?;
    let size = r.count()?;
    let compressed = r.count()?;
    let raw = decompress(r.take(compressed)?, size.min(MAX_BUFFER))?;
    let mut tokens: Vec<String> = raw
        .split(|b| *b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    tokens.truncate(count);
    // Strings.
    let mut r = section("STRINGS")?;
    let n = r.count()?;
    let strings: Vec<u32> = (0..n).map(|_| r.u32()).collect::<Result<_, _>>()?;
    // Fields.
    let mut r = section("FIELDS")?;
    let n = r.count()?;
    let names = r.ints(n, false)?;
    let size = r.count()?;
    let reps = decompress(r.take(size)?, n * 8)?;
    let fields: Vec<(u64, Rep)> = names
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let b = reps.get(i * 8..i * 8 + 8).unwrap_or(&[0; 8]);
            (
                *t as u32 as u64,
                Rep(u64::from_le_bytes([
                    b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                ])),
            )
        })
        .collect();
    // Field sets: field indices, each set ended by an invalid index.
    let mut r = section("FIELDSETS")?;
    let n = r.count()?;
    let sets = r.ints(n, false)?;
    // Paths.
    let mut r = section("PATHS")?;
    let total = r.count()?;
    let n = r.count()?;
    let index = r.ints(n, false)?;
    let element = r.ints(n, false)?;
    let jumps = r.ints(n, false)?;
    let mut paths = vec![String::new(); total];
    if n > 0 {
        build_paths(&tokens, &index, &element, &jumps, &mut paths)?;
    }
    let file = Crate {
        data,
        version,
        tokens,
        strings,
        paths,
    };
    // Specs.
    let mut r = section("SPECS")?;
    let n = r.count()?;
    let spec_paths = r.ints(n, false)?;
    let spec_sets = r.ints(n, false)?;
    let spec_kinds = r.ints(n, false)?;
    let mut specs = Vec::with_capacity(n);
    for k in 0..n {
        let path = file.path(spec_paths[k] as u64);
        let mut at = usize::try_from(spec_sets[k]).map_err(|_| corrupt("bad field set"))?;
        let mut list = Vec::new();
        while let Some(&f) = sets.get(at) {
            if f as u32 == u32::MAX {
                break;
            }
            let (name, rep) = *fields
                .get(f as u32 as usize)
                .ok_or_else(|| corrupt("bad field index"))?;
            let value = file.value(rep, 0).unwrap_or(CrateValue::Other);
            list.push((file.token(name), value));
            at += 1;
        }
        specs.push(Spec {
            path,
            kind: spec_kinds[k] as u32,
            fields: list,
        });
    }
    Ok(specs)
}

// ----- writing (for tests) ------------------------------------------------------------------

/// Encodes integers as the format's compressed integers (32-bit), for
/// writing test layers.
pub(crate) fn encode_ints(values: &[i64]) -> Vec<u8> {
    // The most common delta, then 2-bit codes and the other deltas in the
    // smallest width that holds them.
    let mut deltas = Vec::with_capacity(values.len());
    let mut prev = 0i64;
    for &v in values {
        deltas.push(v - prev);
        prev = v;
    }
    let mut counts: BTreeMap<i64, usize> = BTreeMap::new();
    for d in &deltas {
        *counts.entry(*d).or_default() += 1;
    }
    let common = counts
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        .map_or(0, |(d, _)| *d);
    let mut out = (common as i32).to_le_bytes().to_vec();
    let mut codes = vec![0u8; values.len().div_ceil(4)];
    let mut vints = Vec::new();
    for (i, &d) in deltas.iter().enumerate() {
        let code = if d == common {
            0
        } else if i8::try_from(d).is_ok() {
            vints.push(d as i8 as u8);
            1
        } else if i16::try_from(d).is_ok() {
            vints.extend((d as i16).to_le_bytes());
            2
        } else {
            vints.extend((d as i32).to_le_bytes());
            3
        };
        codes[i / 4] |= code << (2 * (i % 4));
    }
    out.extend(codes);
    out.extend(vints);
    store_block(&out)
}

/// The format's block compression of `raw` as literal-only LZ4 (valid,
/// though not smaller), for writing test layers.
pub(crate) fn store_block(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8];
    let n = raw.len();
    if n >= 15 {
        out.push(0xF0);
        let mut rest = n - 15;
        while rest >= 255 {
            out.push(255);
            rest -= 255;
        }
        out.push(rest as u8);
    } else {
        out.push((n as u8) << 4);
    }
    out.extend_from_slice(raw);
    out
}

/// A small crate-file writer for tests: a 10 mm cube mesh under a
/// translated transform, Z up, in millimeters.
#[cfg(test)]
pub(crate) fn test_cube() -> Vec<u8> {
    let tokens = [
        "",
        "Box",
        "xformOp:translate",
        "xformOpOrder",
        "Cube",
        "points",
        "faceVertexCounts",
        "faceVertexIndices",
        "upAxis",
        "Z",
        "metersPerUnit",
        "defaultPrim",
        "primChildren",
        "specifier",
        "typeName",
        "Xform",
        "Mesh",
        "default",
    ];
    let t = |name: &str| tokens.iter().position(|x| *x == name).unwrap() as u64;
    let rep = |kind: u8, flags: u64, payload: u64| (u64::from(kind) << 48) | flags | payload;
    const ARRAY: u64 = 1 << 63;
    const INLINE: u64 = 1 << 62;
    const COMPRESSED: u64 = 1 << 61;
    let mut out = b"PXR-USDC".to_vec();
    out.extend([0, 8, 0, 0, 0, 0, 0, 0]);
    out.extend([0u8; 72]); // table of contents offset, reserved
    let here = |out: &Vec<u8>| out.len() as u64;
    // Values.
    let meters = here(&out);
    out.extend(0.001f64.to_le_bytes());
    let children_root = here(&out);
    out.extend(1u64.to_le_bytes());
    out.extend((t("Box") as u32).to_le_bytes());
    let children_box = here(&out);
    out.extend(1u64.to_le_bytes());
    out.extend((t("Cube") as u32).to_le_bytes());
    let translate = here(&out);
    for v in [10.0f64, 0.0, 0.0] {
        out.extend(v.to_le_bytes());
    }
    let order = here(&out);
    out.extend(1u64.to_le_bytes());
    out.extend((t("xformOp:translate") as u32).to_le_bytes());
    let points = here(&out);
    out.extend(8u64.to_le_bytes());
    for i in 0..8u32 {
        for bit in [1, 2, 4] {
            out.extend(if i & bit != 0 { 10.0f32 } else { 0.0f32 }.to_le_bytes());
        }
    }
    let ints = |out: &mut Vec<u8>, values: &[i64]| {
        let at = out.len() as u64;
        let coded = encode_ints(values);
        out.extend((values.len() as u64).to_le_bytes());
        out.extend((coded.len() as u64).to_le_bytes());
        out.extend(coded);
        at
    };
    let counts = ints(&mut out, &[4; 6]);
    // Corners 0–7 by their bits (x 1, y 2, z 4), faces turned outward.
    let indices = ints(
        &mut out,
        &[
            0, 2, 3, 1, 4, 5, 7, 6, 0, 1, 5, 4, 2, 6, 7, 3, 0, 4, 6, 2, 1, 3, 7, 5,
        ],
    );
    // Fields: (token, representation).
    let fields: Vec<(u64, u64)> = vec![
        (t("upAxis"), rep(TOKEN, INLINE, t("Z"))),
        (t("metersPerUnit"), rep(DOUBLE, 0, meters)),
        (t("defaultPrim"), rep(TOKEN, INLINE, t("Box"))),
        (t("primChildren"), rep(TOKEN_VECTOR, 0, children_root)),
        (t("specifier"), rep(SPECIFIER, INLINE, 0)),
        (t("typeName"), rep(TOKEN, INLINE, t("Xform"))),
        (t("primChildren"), rep(TOKEN_VECTOR, 0, children_box)),
        (t("default"), rep(VEC3D, 0, translate)),
        (t("default"), rep(TOKEN, ARRAY, order)),
        (t("typeName"), rep(TOKEN, INLINE, t("Mesh"))),
        (t("default"), rep(VEC3F, ARRAY, points)),
        (t("default"), rep(INT, ARRAY | COMPRESSED, counts)),
        (t("default"), rep(INT, ARRAY | COMPRESSED, indices)),
    ];
    // Field sets per spec, each ended by an invalid index.
    let sets: Vec<Vec<i64>> = vec![
        vec![0, 1, 2, 3],
        vec![4, 5, 6],
        vec![7],
        vec![8],
        vec![4, 9],
        vec![10],
        vec![11],
        vec![12],
    ];
    let mut set_values = Vec::new();
    let mut set_starts = Vec::new();
    for set in &sets {
        set_starts.push(set_values.len() as i64);
        set_values.extend(set);
        set_values.push(-1);
    }
    // Paths in tree order, with element tokens (negative for properties)
    // and jumps (−1 child only, 0 sibling only, −2 leaf).
    let element = [
        0,
        t("Box") as i64,
        -(t("xformOp:translate") as i64),
        -(t("xformOpOrder") as i64),
        t("Cube") as i64,
        -(t("points") as i64),
        -(t("faceVertexCounts") as i64),
        -(t("faceVertexIndices") as i64),
    ];
    let jumps = [-1, -1, 0, 0, -1, 0, 0, -2];
    let kinds = [7, 6, 1, 1, 6, 1, 1, 1];
    let mut sections: Vec<(&str, u64)> = Vec::new();
    let compressed = |out: &mut Vec<u8>, values: &[i64]| {
        let coded = encode_ints(values);
        out.extend((coded.len() as u64).to_le_bytes());
        out.extend(coded);
    };
    sections.push(("TOKENS", here(&out)));
    let raw: Vec<u8> = tokens.iter().flat_map(|x| x.bytes().chain([0])).collect();
    let block = store_block(&raw);
    out.extend((tokens.len() as u64).to_le_bytes());
    out.extend((raw.len() as u64).to_le_bytes());
    out.extend((block.len() as u64).to_le_bytes());
    out.extend(block);
    sections.push(("STRINGS", here(&out)));
    out.extend(0u64.to_le_bytes());
    sections.push(("FIELDS", here(&out)));
    out.extend((fields.len() as u64).to_le_bytes());
    compressed(
        &mut out,
        &fields.iter().map(|f| f.0 as i64).collect::<Vec<_>>(),
    );
    let reps: Vec<u8> = fields.iter().flat_map(|f| f.1.to_le_bytes()).collect();
    let block = store_block(&reps);
    out.extend((block.len() as u64).to_le_bytes());
    out.extend(block);
    sections.push(("FIELDSETS", here(&out)));
    out.extend((set_values.len() as u64).to_le_bytes());
    compressed(&mut out, &set_values);
    sections.push(("PATHS", here(&out)));
    out.extend(8u64.to_le_bytes());
    out.extend(8u64.to_le_bytes());
    compressed(&mut out, &(0..8).collect::<Vec<_>>());
    compressed(&mut out, &element);
    compressed(&mut out, &jumps);
    sections.push(("SPECS", here(&out)));
    out.extend(8u64.to_le_bytes());
    compressed(&mut out, &(0..8).collect::<Vec<_>>());
    compressed(&mut out, &set_starts);
    compressed(&mut out, &kinds);
    let toc = here(&out);
    out.extend((sections.len() as u64).to_le_bytes());
    for (name, start) in &sections {
        let mut n = [0u8; 16];
        n[..name.len()].copy_from_slice(name.as_bytes());
        out.extend(n);
        out.extend(start.to_le_bytes());
        out.extend(0u64.to_le_bytes());
    }
    out[16..24].copy_from_slice(&toc.to_le_bytes());
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_written_cube_reads_back() {
        let bytes = test_cube();
        let specs = read(&bytes).unwrap();
        let paths: Vec<&str> = specs.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "/",
                "/Box",
                "/Box.xformOp:translate",
                "/Box.xformOpOrder",
                "/Box/Cube",
                "/Box/Cube.points",
                "/Box/Cube.faceVertexCounts",
                "/Box/Cube.faceVertexIndices"
            ]
        );
        let field = |k: usize, name: &str| {
            specs[k]
                .fields
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(field(0, "metersPerUnit"), CrateValue::Num(0.001));
        assert_eq!(field(2, "default"), CrateValue::Tuple(vec![10.0, 0.0, 0.0]));
        let CrateValue::Array(counts) = field(6, "default") else {
            panic!()
        };
        assert_eq!(counts, vec![CrateValue::Num(4.0); 6]);
        let CrateValue::Array(points) = field(5, "default") else {
            panic!()
        };
        assert_eq!(points[7], CrateValue::Tuple(vec![10.0, 10.0, 10.0]));
        assert!(read(b"PXR-USDC").is_err());
        let mut broken = bytes.clone();
        broken.truncate(200);
        assert!(read(&broken).is_err());
    }

    #[test]
    fn lz4_literals_and_matches() {
        // "abcabcabcabc": three literals, then a match of nine at offset 3.
        let block = [0x35, b'a', b'b', b'c', 3, 0];
        let mut out = Vec::new();
        lz4_block(&block, &mut out, 64).unwrap();
        assert_eq!(out, b"abcabcabcabc");
        assert!(
            lz4_block(&[0x05, 9, 0], &mut Vec::new(), 64).is_err(),
            "offset past start"
        );
        let raw: Vec<u8> = (0..300u32).map(|i| (i % 7) as u8).collect();
        assert_eq!(decompress(&store_block(&raw), 1000).unwrap(), raw);
    }

    #[test]
    fn integers_round_trip_through_the_coding() {
        let values: Vec<i64> = vec![0, 1, 2, 3, 4, 100, 101, -5, 70_000, 70_001, 3];
        let coded = encode_ints(&values);
        assert_eq!(decode_ints(&coded, values.len(), false).unwrap(), values);
    }

    #[test]
    fn half_floats() {
        assert_eq!(half(0x3C00), 1.0);
        assert_eq!(half(0xC000), -2.0);
        assert_eq!(half(0x3800), 0.5);
        assert_eq!(half(0), 0.0);
    }

    #[test]
    fn a_path_tree() {
        let tokens: Vec<String> = ["", "World", "Mesh", "points", "Light"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // / → World → (Mesh → .points), Light (sibling of Mesh).
        let index = [0, 1, 2, 3, 4];
        let element = [0, 1, 2, -3, 4];
        // Root: child. World: child. Mesh: child and sibling at +2.
        // points: leaf. Light: leaf.
        let jumps = [-1, -1, 2, -2, -2];
        let mut out = vec![String::new(); 5];
        build_paths(&tokens, &index, &element, &jumps, &mut out).unwrap();
        assert_eq!(
            out,
            [
                "/",
                "/World",
                "/World/Mesh",
                "/World/Mesh.points",
                "/World/Light"
            ]
        );
    }
}
