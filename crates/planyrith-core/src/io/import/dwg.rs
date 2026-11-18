//! Implements docs/09-file-formats.md §5 DWG import (milestone 1.52): the
//! drawing format of releases 2000 (`AC1015`), 2004 (`AC1018`), 2010
//! (`AC1024`), 2013 (`AC1027`) and 2018 (`AC1032`), read without a library.
//!
//! Release 2000 keeps its sections at offsets the file header lists and its
//! objects at absolute offsets. Later releases (except 2007, `AC1021`, which
//! has a different page coding and is refused) keep an encrypted header
//! pointing to a page map and a section map; sections are split into pages,
//! each compressed with the format's LZ77 variant. The object map gives
//! each object's offset; an object is a bit stream of typed fields (bit
//! shorts, bit doubles, doubles with defaults, handles…), with its handles
//! in a stream of their own and, from 2010 on, its strings in a third.
//!
//! The drawing becomes DXF group codes in memory — layers, block
//! definitions and model-space entities (lines, arcs, circles, ellipses,
//! splines, light polylines, 2D/3D polylines, polyface and polygon meshes,
//! 3D faces, solids, inserts with arrays) — and is brought in by the DXF
//! importer, so both formats give the same result. Paper space, text,
//! dimensions, hatches and other objects are skipped and counted.

use std::collections::BTreeMap;
use std::io::Read;

use glam::{DVec2, DVec3};

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::io::import::{Confidence, ImportFormat, ImportOptions, ImportReport, Importer};
use crate::io::{IoError, Progress};

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "dwg".into(),
        reason: reason.into(),
    }
}

/// A release of the format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Release {
    /// 2000 (`AC1015`).
    R2000,
    /// 2004 (`AC1018`).
    R2004,
    /// 2010 (`AC1024`).
    R2010,
    /// 2013 (`AC1027`).
    R2013,
    /// 2018 (`AC1032`).
    R2018,
}

impl Release {
    fn of(magic: &[u8]) -> Result<Release, IoError> {
        Ok(match magic {
            b"AC1015" => Release::R2000,
            b"AC1018" => Release::R2004,
            b"AC1024" => Release::R2010,
            b"AC1027" => Release::R2013,
            b"AC1032" => Release::R2018,
            b"AC1021" => {
                return Err(IoError::UnsupportedFormat(
                    "DWG 2007 (AC1021) drawings are not read; save the drawing in another release"
                        .into(),
                ));
            }
            b"AC1012" | b"AC1014" | b"AC1009" | b"AC1006" | b"AC1004" | b"AC1003" | b"AC1002" => {
                return Err(IoError::UnsupportedFormat(
                    "DWG drawings before release 2000 are not read; save the drawing in a later \
                     release"
                        .into(),
                ));
            }
            _ => return Err(IoError::UnsupportedFormat("not a DWG drawing".into())),
        })
    }
}

// ----- bits ---------------------------------------------------------------------------------

/// A bit stream: bits are read from each byte's high end.
#[derive(Clone)]
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> Bits<'a> {
    fn at(data: &'a [u8], pos: usize, end: usize) -> Self {
        Bits {
            data,
            pos,
            end: end.min(data.len() * 8),
        }
    }

    fn bits(&mut self, n: usize) -> Result<u64, IoError> {
        if self.pos + n > self.end {
            return Err(corrupt("an object ends early"));
        }
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self.data[self.pos / 8];
            let bit = (byte >> (7 - self.pos % 8)) & 1;
            v = (v << 1) | u64::from(bit);
            self.pos += 1;
        }
        Ok(v)
    }
    fn b(&mut self) -> Result<bool, IoError> {
        Ok(self.bits(1)? == 1)
    }
    fn bb(&mut self) -> Result<u8, IoError> {
        Ok(self.bits(2)? as u8)
    }
    fn rc(&mut self) -> Result<u8, IoError> {
        Ok(self.bits(8)? as u8)
    }
    fn raw(&mut self, n: usize) -> Result<Vec<u8>, IoError> {
        (0..n).map(|_| self.rc()).collect()
    }
    fn rs(&mut self) -> Result<u16, IoError> {
        let lo = self.rc()?;
        let hi = self.rc()?;
        Ok(u16::from_le_bytes([lo, hi]))
    }
    fn rl(&mut self) -> Result<u32, IoError> {
        let b = self.raw(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn rd(&mut self) -> Result<f64, IoError> {
        let b = self.raw(8)?;
        Ok(f64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn bs(&mut self) -> Result<u16, IoError> {
        Ok(match self.bb()? {
            0 => self.rs()?,
            1 => u16::from(self.rc()?),
            2 => 0,
            _ => 256,
        })
    }
    fn bl(&mut self) -> Result<u32, IoError> {
        Ok(match self.bb()? {
            0 => self.rl()?,
            1 => u32::from(self.rc()?),
            _ => 0,
        })
    }
    fn bll(&mut self) -> Result<u64, IoError> {
        let n = self.bits(3)? as usize;
        let b = self.raw(n)?;
        let mut v = 0u64;
        for (i, x) in b.iter().enumerate() {
            v |= u64::from(*x) << (8 * i);
        }
        Ok(v)
    }
    fn bd(&mut self) -> Result<f64, IoError> {
        Ok(match self.bb()? {
            0 => self.rd()?,
            1 => 1.0,
            2 => 0.0,
            _ => return Err(corrupt("a bit double has no value")),
        })
    }
    /// A double with a default: kept, or some of its bytes replaced.
    fn dd(&mut self, default: f64) -> Result<f64, IoError> {
        let mut b = default.to_le_bytes();
        match self.bb()? {
            0 => {}
            1 => {
                let r = self.raw(4)?;
                b[..4].copy_from_slice(&r);
            }
            2 => {
                let r = self.raw(6)?;
                b[4] = r[0];
                b[5] = r[1];
                b[..4].copy_from_slice(&r[2..6]);
            }
            _ => return self.rd(),
        }
        Ok(f64::from_le_bytes(b))
    }
    fn bd3(&mut self) -> Result<DVec3, IoError> {
        Ok(DVec3::new(self.bd()?, self.bd()?, self.bd()?))
    }
    fn rd2(&mut self) -> Result<DVec2, IoError> {
        Ok(DVec2::new(self.rd()?, self.rd()?))
    }
    fn dd2(&mut self, d: DVec2) -> Result<DVec2, IoError> {
        Ok(DVec2::new(self.dd(d.x)?, self.dd(d.y)?))
    }
    fn dd3(&mut self, d: DVec3) -> Result<DVec3, IoError> {
        Ok(DVec3::new(self.dd(d.x)?, self.dd(d.y)?, self.dd(d.z)?))
    }
    /// Extrusion: the bit set means +Z.
    fn be(&mut self) -> Result<DVec3, IoError> {
        if self.b()? { Ok(DVec3::Z) } else { self.bd3() }
    }
    /// Thickness: the bit set means zero.
    fn bt(&mut self) -> Result<f64, IoError> {
        if self.b()? { Ok(0.0) } else { self.bd() }
    }
    /// A handle reference: its code and value.
    fn h(&mut self) -> Result<(u8, u64), IoError> {
        let code = self.bits(4)? as u8;
        let n = self.bits(4)? as usize;
        let mut v = 0u64;
        for _ in 0..n {
            v = (v << 8) | u64::from(self.rc()?);
        }
        Ok((code, v))
    }
    /// The object type.
    fn ot(&mut self, r: Release) -> Result<u16, IoError> {
        if r >= Release::R2010 {
            Ok(match self.bb()? {
                0 => u16::from(self.rc()?),
                1 => u16::from(self.rc()?) + 0x1f0,
                _ => self.rs()?,
            })
        } else {
            self.bs()
        }
    }
    /// A text: 8-bit before 2007, UTF-16 since.
    fn tv(&mut self, r: Release) -> Result<String, IoError> {
        let n = usize::from(self.bs()?);
        if r >= Release::R2010 {
            let units = (0..n).map(|_| self.rs()).collect::<Result<Vec<u16>, _>>()?;
            Ok(String::from_utf16_lossy(&units)
                .trim_end_matches('\0')
                .to_string())
        } else {
            let bytes = self.raw(n)?;
            Ok(bytes
                .iter()
                .take_while(|b| **b != 0)
                .map(|b| char::from(*b))
                .collect())
        }
    }
    /// A table color (2004 on: index, true color and names).
    fn cmc(&mut self, r: Release) -> Result<i16, IoError> {
        let index = self.bs()? as i16;
        if r >= Release::R2004 {
            self.bl()?;
            let flags = self.rc()?;
            if flags & 1 != 0 {
                self.tv(r)?;
            }
            if flags & 2 != 0 {
                self.tv(r)?;
            }
        }
        Ok(index)
    }
    /// An entity color: its index and whether a color book handle follows.
    fn enc(&mut self, r: Release) -> Result<(i16, bool), IoError> {
        if r < Release::R2004 {
            return Ok((self.bs()? as i16, false));
        }
        let raw = self.bs()?;
        let flags = raw >> 8;
        let index = (raw & 0x1ff) as i16;
        if flags & 0x80 != 0 {
            self.bl()?;
        }
        if flags & 0x20 != 0 {
            self.bl()?;
        }
        Ok((index, flags & 0x40 != 0))
    }
}

/// A modular char at `*at` (unsigned, or signed by the last byte's 0x40).
fn mc(data: &[u8], at: &mut usize, signed: bool) -> Result<i64, IoError> {
    let mut v: i64 = 0;
    let mut shift = 0;
    loop {
        let b = *data
            .get(*at)
            .ok_or_else(|| corrupt("a number ends early"))?;
        *at += 1;
        if b & 0x80 != 0 {
            v |= i64::from(b & 0x7f) << shift;
            shift += 7;
            if shift > 56 {
                return Err(corrupt("a number is too long"));
            }
            continue;
        }
        if signed {
            v |= i64::from(b & 0x3f) << shift;
            if b & 0x40 != 0 {
                v = -v;
            }
        } else {
            v |= i64::from(b & 0x7f) << shift;
        }
        return Ok(v);
    }
}

/// A modular short at `*at`.
fn ms(data: &[u8], at: &mut usize) -> Result<u64, IoError> {
    let mut v: u64 = 0;
    let mut shift = 0;
    loop {
        let b = data
            .get(*at..*at + 2)
            .ok_or_else(|| corrupt("a size ends early"))?;
        *at += 2;
        let w = u16::from_le_bytes([b[0], b[1]]);
        v |= u64::from(w & 0x7fff) << shift;
        if w & 0x8000 == 0 {
            return Ok(v);
        }
        shift += 15;
        if shift > 60 {
            return Err(corrupt("a size is too long"));
        }
    }
}

fn le32(data: &[u8], at: usize) -> Result<u32, IoError> {
    let b = data
        .get(at..at + 4)
        .ok_or_else(|| corrupt("the file ends early"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn le64(data: &[u8], at: usize) -> Result<u64, IoError> {
    Ok(u64::from(le32(data, at)?) | (u64::from(le32(data, at + 4)?) << 32))
}

// ----- 2004 pages ---------------------------------------------------------------------------

/// Decompresses a page of the 2004 format's LZ77 variant.
fn decompress(src: &[u8], limit: usize) -> Result<Vec<u8>, IoError> {
    let bad = || corrupt("a compressed page is broken");
    let mut out: Vec<u8> = Vec::with_capacity(limit);
    let mut i = 0;
    let next = |i: &mut usize| -> Result<u8, IoError> {
        let b = *src.get(*i).ok_or_else(bad)?;
        *i += 1;
        Ok(b)
    };
    // A literal length, or 0 and the opcode that comes instead.
    let literal = |i: &mut usize, op: &mut u8| -> Result<usize, IoError> {
        let b = *src.get(*i).ok_or_else(bad)?;
        *i += 1;
        *op = 0;
        if (0x01..=0x0f).contains(&b) {
            return Ok(usize::from(b) + 3);
        }
        if b == 0 {
            let mut total = 0x0f;
            loop {
                let c = *src.get(*i).ok_or_else(bad)?;
                *i += 1;
                if c != 0 {
                    return Ok(total + usize::from(c) + 3);
                }
                total += 0xff;
            }
        }
        *op = b;
        Ok(0)
    };
    let long = |i: &mut usize| -> Result<usize, IoError> {
        let mut b = *src.get(*i).ok_or_else(bad)?;
        *i += 1;
        let mut total = 0;
        if b == 0 {
            total = 0xff;
            loop {
                b = *src.get(*i).ok_or_else(bad)?;
                *i += 1;
                if b != 0 {
                    break;
                }
                total += 0xff;
            }
        }
        Ok(total + usize::from(b))
    };
    let two = |i: &mut usize| -> Result<(usize, usize), IoError> {
        let a = *src.get(*i).ok_or_else(bad)?;
        let b = *src.get(*i + 1).ok_or_else(bad)?;
        *i += 2;
        Ok((
            (usize::from(a) >> 2) | (usize::from(b) << 6),
            usize::from(a & 3),
        ))
    };
    let mut op = 0u8;
    let first = literal(&mut i, &mut op)?;
    out.extend_from_slice(src.get(i..i + first).ok_or_else(bad)?);
    i += first;
    while i < src.len() {
        if op == 0 {
            op = next(&mut i)?;
        }
        let (count, offset, mut lit) = match op {
            0x11 => break,
            0x40..=0xff => {
                let b = next(&mut i)?;
                let count = usize::from(op >> 4) - 1;
                let offset = (usize::from(b) << 2) | usize::from((op & 0x0c) >> 2);
                let lit = usize::from(op & 3);
                (count, offset, lit)
            }
            0x21..=0x3f => {
                let (offset, lit) = two(&mut i)?;
                (usize::from(op) - 0x1e, offset, lit)
            }
            0x20 => {
                let count = long(&mut i)? + 0x21;
                let (offset, lit) = two(&mut i)?;
                (count, offset, lit)
            }
            0x12..=0x1f => {
                let (offset, lit) = two(&mut i)?;
                (usize::from(op & 0x0f) + 2, offset + 0x3fff, lit)
            }
            0x10 => {
                let count = long(&mut i)? + 9;
                let (offset, lit) = two(&mut i)?;
                (count, offset + 0x3fff, lit)
            }
            _ => return Err(bad()),
        };
        if lit == 0 {
            lit = literal(&mut i, &mut op)?;
        } else {
            op = 0;
        }
        if offset + 1 > out.len() || out.len() + count > limit {
            return Err(bad());
        }
        let from = out.len() - offset - 1;
        for k in 0..count {
            let b = out[from + k];
            out.push(b);
        }
        out.extend_from_slice(src.get(i..i + lit).ok_or_else(bad)?);
        i += lit;
        if out.len() > limit {
            return Err(bad());
        }
    }
    Ok(out)
}

/// The sections of a 2004-family file, by name.
fn sections_2004(data: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, IoError> {
    // The encrypted header at 0x80.
    let mut header = data
        .get(0x80..0x80 + 0x6c)
        .ok_or_else(|| corrupt("the file header ends early"))?
        .to_vec();
    let mut seed: u32 = 1;
    for b in header.iter_mut() {
        seed = seed.wrapping_mul(0x343fd).wrapping_add(0x269ec3);
        *b ^= (seed >> 16) as u8;
    }
    if !header.starts_with(b"AcFssFcAJMB") {
        return Err(corrupt("the file header does not decrypt"));
    }
    let map_id = le32(&header, 0x50)?;
    let map_at = le64(&header, 0x54)? as usize + 0x100;
    let section_map_id = le32(&header, 0x5c)?;
    // The page map: page numbers with sizes, from 0x100 on.
    let system = |at: usize, kind: u32| -> Result<Vec<u8>, IoError> {
        if le32(data, at)? != kind {
            return Err(corrupt("a system page has the wrong type"));
        }
        let size = le32(data, at + 4)? as usize;
        let compressed = le32(data, at + 8)? as usize;
        let body = data
            .get(at + 20..at + 20 + compressed)
            .ok_or_else(|| corrupt("a system page ends early"))?;
        if le32(data, at + 12)? == 2 {
            decompress(body, size)
        } else {
            Ok(body.to_vec())
        }
    };
    let pages_raw = system(map_at, 0x4163_0e3b)?;
    let _ = map_id;
    let mut pages: BTreeMap<i32, usize> = BTreeMap::new();
    let mut address = 0x100usize;
    let mut k = 0;
    while k + 8 <= pages_raw.len() {
        let number = le32(&pages_raw, k)? as i32;
        let size = le32(&pages_raw, k + 4)? as usize;
        k += 8;
        if number < 0 {
            k += 16;
        } else {
            pages.insert(number, address);
        }
        address += size;
    }
    let map_page = *pages
        .get(&(section_map_id as i32))
        .ok_or_else(|| corrupt("the section map page is missing"))?;
    let map = system(map_page, 0x4163_003b)?;
    let count = le32(&map, 0)? as usize;
    let mut at = 20;
    let mut out = BTreeMap::new();
    for _ in 0..count.min(256) {
        let size = le64(&map, at)? as usize;
        let page_count = le32(&map, at + 8)? as usize;
        let max = le32(&map, at + 12)? as usize;
        let compressed = le32(&map, at + 20)?;
        let name: String = map
            .get(at + 32..at + 96)
            .ok_or_else(|| corrupt("the section map ends early"))?
            .iter()
            .take_while(|b| **b != 0)
            .map(|b| char::from(*b))
            .collect();
        at += 96;
        if size > 1 << 31 {
            return Err(corrupt("a section is too large"));
        }
        let mut buffer = vec![0u8; size];
        for _ in 0..page_count {
            let number = le32(&map, at)? as i32;
            let data_size = le32(&map, at + 4)? as usize;
            let start = le64(&map, at + 8)? as usize;
            at += 16;
            let Some(&page) = pages.get(&number) else {
                continue;
            };
            let body = data
                .get(page + 32..page + 32 + data_size)
                .ok_or_else(|| corrupt("a page ends early"))?;
            let bytes = if compressed == 2 {
                decompress(body, max)?
            } else {
                body.to_vec()
            };
            let end = (start + bytes.len()).min(buffer.len());
            if start < end {
                buffer[start..end].copy_from_slice(&bytes[..end - start]);
            }
        }
        out.insert(name, buffer);
    }
    Ok(out)
}

/// Object offsets by handle, from an object map.
fn object_map(data: &[u8], mut at: usize) -> Result<Vec<(u64, usize)>, IoError> {
    let mut out = Vec::new();
    loop {
        let size = data
            .get(at..at + 2)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
            .ok_or_else(|| corrupt("the object map ends early"))?;
        at += 2;
        if size <= 2 {
            break;
        }
        let end = at + size - 2;
        let (mut handle, mut offset) = (0i64, 0i64);
        while at < end {
            handle += mc(data, &mut at, false)?;
            offset += mc(data, &mut at, true)?;
            if handle >= 0 && offset >= 0 {
                out.push((handle as u64, offset as usize));
            }
        }
        at = end + 2;
        if out.len() > 50_000_000 {
            return Err(corrupt("the object map is too large"));
        }
    }
    Ok(out)
}

// ----- objects ------------------------------------------------------------------------------

/// What an entity's common part gives.
struct Common {
    /// 0 owned (the owner's handle follows), 1 paper space, 2 model space.
    mode: u8,
    reactors: u32,
    xdic: bool,
    links: bool,
    color: i16,
    book: bool,
    ltype: bool,
    plotstyle: bool,
    material: bool,
    styles: usize,
}

/// One object of the drawing, decoded as far as the import needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Object {
    /// Its handle.
    pub handle: u64,
    /// Its type number.
    pub kind: u16,
    /// The owner (an entity's block header or polyline), when written.
    pub owner: Option<u64>,
    /// 1 paper space, 2 model space (entities), else 0.
    pub space: u8,
    /// The layer's handle (entities).
    pub layer: Option<u64>,
    /// Color index (entities and layers).
    pub color: i16,
    /// A name (layers and block headers; the block of an insert).
    pub name: String,
    /// Handles the object refers to (an insert's block header).
    pub refs: Vec<u64>,
    /// The entity as DXF group codes (without type and layer).
    pub codes: Vec<(i32, String)>,
    /// Its DXF type name.
    pub dxf: &'static str,
    /// A block header's base point.
    pub base: DVec3,
}

/// A handle reference resolved against the object's own handle.
fn resolve((code, v): (u8, u64), own: u64) -> u64 {
    match code {
        6 => own + 1,
        8 => own.saturating_sub(1),
        0xa => own + v,
        0xc => own.saturating_sub(v),
        _ => v,
    }
}

fn num(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "0".into()
    }
}

/// Group codes of a point.
fn point(codes: &mut Vec<(i32, String)>, base: i32, p: DVec3) {
    codes.push((base, num(p.x)));
    codes.push((base + 10, num(p.y)));
    codes.push((base + 20, num(p.z)));
}

/// Decodes the object at byte `offset` of `data`.
fn object(data: &[u8], offset: usize, r: Release) -> Result<Object, IoError> {
    let mut at = offset;
    let size = ms(data, &mut at)? as usize;
    let handle_bits = if r >= Release::R2010 {
        Some(mc(data, &mut at, false)? as usize)
    } else {
        None
    };
    let start = at * 8;
    let end = start + size * 8;
    if end > data.len() * 8 {
        return Err(corrupt("an object overruns its section"));
    }
    let mut b = Bits::at(data, start, end);
    let kind = b.ot(r)?;
    let handles_at = match handle_bits {
        Some(h) => end
            .checked_sub(h)
            .ok_or_else(|| corrupt("bad handle stream size"))?,
        None => start + b.rl()? as usize,
    };
    // Strings live in their own stream from 2010 on, before the handles.
    let mut strings = Bits::at(data, 0, 0);
    if r >= Release::R2010 && handles_at > start + 17 {
        let mut s = Bits::at(data, handles_at - 1, end);
        if s.b()? {
            let mut pos = handles_at - 1 - 16;
            let mut probe = Bits::at(data, pos, end);
            let mut len = usize::from(probe.rs()?);
            if len & 0x8000 != 0 {
                pos -= 16;
                let mut probe = Bits::at(data, pos, end);
                let hi = usize::from(probe.rs()?);
                len = (len & 0x7fff) | (hi << 15);
            }
            if let Some(from) = pos.checked_sub(len) {
                strings = Bits::at(data, from, pos);
            }
        }
    }
    b.end = handles_at.min(end);
    let own = b.h()?.1;
    let mut o = Object {
        handle: own,
        kind,
        ..Default::default()
    };
    // Extended data.
    loop {
        let n = usize::from(b.bs()?);
        if n == 0 {
            break;
        }
        b.h()?;
        b.pos += n * 8;
    }
    let mut hs = Bits::at(data, handles_at, end);
    let text = |b: &mut Bits<'_>, strings: &mut Bits<'_>| -> Result<String, IoError> {
        if r >= Release::R2010 {
            strings.tv(r)
        } else {
            b.tv(r)
        }
    };
    let entity = matches!(kind, 1..=41 | 43..=47 | 74 | 77 | 78);
    if entity {
        // Graphics.
        if b.b()? {
            let n = if r >= Release::R2010 {
                b.bll()? as usize
            } else {
                b.rl()? as usize
            };
            b.pos += n * 8;
        }
        let mode = b.bb()?;
        let reactors = b.bl()?;
        let xdic = if r >= Release::R2004 { !b.b()? } else { true };
        if r >= Release::R2013 {
            b.b()?;
        }
        let links = if r < Release::R2004 { !b.b()? } else { false };
        let (color, book) = b.enc(r)?;
        b.bd()?;
        let ltype = b.bb()? == 3;
        let plotstyle = b.bb()? == 3;
        let mut material = false;
        if r >= Release::R2010 {
            material = b.bb()? == 3;
            b.rc()?;
        }
        let mut styles = 0;
        if r >= Release::R2010 {
            for _ in 0..3 {
                styles += usize::from(b.b()?);
            }
        }
        b.bs()?;
        b.rc()?;
        let common = Common {
            mode,
            reactors,
            xdic,
            links,
            color,
            book,
            ltype,
            plotstyle,
            material,
            styles,
        };
        o.space = if mode == 0 { 0 } else { mode };
        o.color = common.color;
        if common.mode == 0 {
            o.owner = Some(resolve(hs.h()?, own));
        }
        for _ in 0..common.reactors.min(10_000) {
            hs.h()?;
        }
        if common.xdic {
            hs.h()?;
        }
        if common.links {
            hs.h()?;
            hs.h()?;
        }
        if common.book {
            hs.h()?;
        }
        o.layer = Some(resolve(hs.h()?, own));
        if common.ltype {
            hs.h()?;
        }
        if common.material {
            hs.h()?;
        }
        if common.plotstyle {
            hs.h()?;
        }
        for _ in 0..common.styles {
            hs.h()?;
        }
        entity_data(&mut o, &mut b, &mut hs, r)?;
    } else {
        // Non-entity objects: reactors, extension dictionary.
        let reactors = b.bl()?;
        let xdic = if r >= Release::R2004 { !b.b()? } else { true };
        if r >= Release::R2013 {
            b.b()?;
        }
        match kind {
            // Layer: name, flags, color.
            51 => {
                o.dxf = "LAYER";
                o.name = text(&mut b, &mut strings)?;
                b.b()?;
                b.bs()?;
                b.b()?;
                b.bs()?;
                o.color = b.cmc(r)?;
            }
            // Block header: name, base point.
            49 => {
                o.dxf = "BLOCK_RECORD";
                o.name = text(&mut b, &mut strings)?;
                b.b()?;
                b.bs()?;
                b.b()?;
                b.b()?; // anonymous
                b.b()?; // has attributes
                let xref = b.b()?;
                let overlaid = b.b()?;
                b.b()?; // loaded
                if r >= Release::R2004 && !xref && !overlaid {
                    b.bl()?;
                }
                o.base = b.bd3()?;
            }
            _ => {}
        }
        let _ = (reactors, xdic);
    }
    Ok(o)
}

/// An entity's own data as DXF group codes.
fn entity_data(
    o: &mut Object,
    b: &mut Bits<'_>,
    hs: &mut Bits<'_>,
    r: Release,
) -> Result<(), IoError> {
    let mut c: Vec<(i32, String)> = Vec::new();
    let own = o.handle;
    let extrusion = |c: &mut Vec<(i32, String)>, n: DVec3| {
        if n != DVec3::Z {
            point(c, 210, n);
        }
    };
    o.dxf = match o.kind {
        19 => {
            let flat = b.b()?;
            let sx = b.rd()?;
            let ex = b.dd(sx)?;
            let sy = b.rd()?;
            let ey = b.dd(sy)?;
            let (sz, ez) = if flat {
                (0.0, 0.0)
            } else {
                let sz = b.rd()?;
                (sz, b.dd(sz)?)
            };
            b.bt()?;
            let n = b.be()?;
            point(&mut c, 10, DVec3::new(sx, sy, sz));
            point(&mut c, 11, DVec3::new(ex, ey, ez));
            extrusion(&mut c, n);
            "LINE"
        }
        17 | 18 => {
            let center = b.bd3()?;
            let radius = b.bd()?;
            b.bt()?;
            let n = b.be()?;
            point(&mut c, 10, center);
            c.push((40, num(radius)));
            extrusion(&mut c, n);
            if o.kind == 17 {
                let a0 = b.bd()?;
                let a1 = b.bd()?;
                c.push((50, num(a0.to_degrees())));
                c.push((51, num(a1.to_degrees())));
                "ARC"
            } else {
                "CIRCLE"
            }
        }
        35 => {
            let center = b.bd3()?;
            let major = b.bd3()?;
            let n = b.bd3()?;
            let ratio = b.bd()?;
            let a0 = b.bd()?;
            let a1 = b.bd()?;
            point(&mut c, 10, center);
            point(&mut c, 11, major);
            extrusion(&mut c, n);
            c.push((40, num(ratio)));
            c.push((41, num(a0)));
            c.push((42, num(a1)));
            "ELLIPSE"
        }
        28 => {
            let no_flags = b.b()?;
            let flat = b.b()?;
            let x = b.rd()?;
            let y = b.rd()?;
            let z = if flat { 0.0 } else { b.rd()? };
            let p1 = DVec3::new(x, y, z);
            let p2 = b.dd3(p1)?;
            let p3 = b.dd3(p2)?;
            let p4 = b.dd3(p3)?;
            if !no_flags {
                b.bs()?;
            }
            for (k, p) in [p1, p2, p3, p4].into_iter().enumerate() {
                point(&mut c, 10 + k as i32, p);
            }
            "3DFACE"
        }
        31 => {
            b.bt()?;
            let z = b.bd()?;
            let corners = [b.rd2()?, b.rd2()?, b.rd2()?, b.rd2()?];
            let n = b.be()?;
            for (k, p) in corners.into_iter().enumerate() {
                point(&mut c, 10 + k as i32, p.extend(z));
            }
            extrusion(&mut c, n);
            "SOLID"
        }
        77 => {
            let flag = b.bs()?;
            if flag & 4 != 0 {
                b.bd()?;
            }
            let elevation = if flag & 8 != 0 { b.bd()? } else { 0.0 };
            if flag & 2 != 0 {
                b.bd()?;
            }
            let n = if flag & 1 != 0 { b.bd3()? } else { DVec3::Z };
            let points = b.bl()? as usize;
            let bulges = if flag & 16 != 0 { b.bl()? as usize } else { 0 };
            let ids = if r >= Release::R2010 && flag & 1024 != 0 {
                b.bl()? as usize
            } else {
                0
            };
            let widths = if flag & 32 != 0 { b.bl()? as usize } else { 0 };
            if points > 1_000_000 || bulges > points || widths > points {
                return Err(corrupt("a polyline is too large"));
            }
            let mut verts = Vec::with_capacity(points);
            for k in 0..points {
                verts.push(if k == 0 {
                    b.rd2()?
                } else {
                    b.dd2(verts[k - 1])?
                });
            }
            let bulge: Vec<f64> = (0..bulges).map(|_| b.bd()).collect::<Result<_, _>>()?;
            for _ in 0..ids {
                b.bl()?;
            }
            c.push((90, points.to_string()));
            c.push((70, if flag & 512 != 0 { "1" } else { "0" }.into()));
            c.push((38, num(elevation)));
            for (k, p) in verts.iter().enumerate() {
                c.push((10, num(p.x)));
                c.push((20, num(p.y)));
                if let Some(v) = bulge.get(k) {
                    c.push((42, num(*v)));
                }
            }
            extrusion(&mut c, n);
            "LWPOLYLINE"
        }
        15 => {
            let flags = b.bs()?;
            b.bs()?;
            b.bd()?;
            b.bd()?;
            b.bt()?;
            let elevation = b.bd()?;
            let n = b.be()?;
            c.push((66, "1".into()));
            c.push((70, (flags & 0xff).to_string()));
            point(&mut c, 10, DVec3::new(0.0, 0.0, elevation));
            extrusion(&mut c, n);
            "POLYLINE"
        }
        16 => {
            b.rc()?;
            let flags = b.rc()?;
            c.push((66, "1".into()));
            c.push((70, (8 | u16::from(flags & 1)).to_string()));
            "POLYLINE"
        }
        29 => {
            let verts = b.bs()?;
            let faces = b.bs()?;
            c.push((66, "1".into()));
            c.push((70, "64".into()));
            c.push((71, verts.to_string()));
            c.push((72, faces.to_string()));
            "POLYLINE"
        }
        30 => {
            let flags = b.bs()?;
            b.bs()?;
            let m = b.bs()?;
            let n = b.bs()?;
            c.push((66, "1".into()));
            c.push((70, (16 | (flags & 0x21)).to_string()));
            c.push((71, m.to_string()));
            c.push((72, n.to_string()));
            "POLYLINE"
        }
        10 => {
            b.rc()?;
            let p = b.bd3()?;
            let w = b.bd()?;
            if w >= 0.0 {
                b.bd()?;
            }
            let bulge = b.bd()?;
            point(&mut c, 10, p);
            c.push((42, num(bulge)));
            c.push((70, "0".into()));
            "VERTEX"
        }
        11..=13 => {
            b.rc()?;
            let p = b.bd3()?;
            point(&mut c, 10, p);
            let flags = match o.kind {
                11 => 32,
                12 => 64,
                _ => 192,
            };
            c.push((70, flags.to_string()));
            "VERTEX"
        }
        14 => {
            c.push((10, "0".into()));
            c.push((20, "0".into()));
            c.push((30, "0".into()));
            c.push((70, "128".into()));
            for k in 0..4 {
                c.push((71 + k, (b.bs()? as i16).to_string()));
            }
            "VERTEX"
        }
        7 | 8 => {
            let at = b.bd3()?;
            let scale = match b.bb()? {
                0 => {
                    let x = b.rd()?;
                    DVec3::new(x, b.dd(x)?, b.dd(x)?)
                }
                1 => DVec3::new(1.0, b.dd(1.0)?, b.dd(1.0)?),
                2 => DVec3::splat(b.rd()?),
                _ => DVec3::ONE,
            };
            let rotation = b.bd()?;
            let n = b.bd3()?;
            let attribs = b.b()?;
            if r >= Release::R2004 && attribs {
                b.bl()?;
            }
            point(&mut c, 10, at);
            c.push((41, num(scale.x)));
            c.push((42, num(scale.y)));
            c.push((43, num(scale.z)));
            c.push((50, num(rotation.to_degrees())));
            extrusion(&mut c, n);
            if o.kind == 8 {
                let cols = b.bs()?;
                let rows = b.bs()?;
                let dx = b.bd()?;
                let dy = b.bd()?;
                c.push((70, cols.to_string()));
                c.push((71, rows.to_string()));
                c.push((44, num(dx)));
                c.push((45, num(dy)));
            }
            o.refs.push(resolve(hs.h()?, own));
            "INSERT"
        }
        36 => {
            let mut scenario = b.bl()?;
            if r >= Release::R2013 {
                let flags = b.bl()?;
                b.bl()?;
                if flags & 1 != 0 {
                    scenario = 2;
                } else if scenario == 0 {
                    scenario = 1;
                }
            }
            let degree = b.bl()?;
            let (mut knots, mut controls, mut weights, mut fits) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let mut flags = 8;
            if scenario == 2 {
                b.bd()?;
                b.bd3()?;
                b.bd3()?;
                let n = b.bl()? as usize;
                if n > 100_000 {
                    return Err(corrupt("a spline is too large"));
                }
                for _ in 0..n {
                    fits.push(b.bd3()?);
                }
            } else {
                let rational = b.b()?;
                let closed = b.b()?;
                b.b()?;
                b.bd()?;
                b.bd()?;
                let nk = b.bl()? as usize;
                let nc = b.bl()? as usize;
                let weighted = b.b()?;
                if nk > 100_000 || nc > 100_000 {
                    return Err(corrupt("a spline is too large"));
                }
                for _ in 0..nk {
                    knots.push(b.bd()?);
                }
                for _ in 0..nc {
                    controls.push(b.bd3()?);
                    if weighted {
                        weights.push(b.bd()?);
                    }
                }
                if rational {
                    flags |= 4;
                }
                if closed {
                    flags |= 1;
                }
            }
            c.push((70, flags.to_string()));
            c.push((71, degree.to_string()));
            c.push((72, knots.len().to_string()));
            c.push((73, controls.len().to_string()));
            c.push((74, fits.len().to_string()));
            for k in &knots {
                c.push((40, num(*k)));
            }
            for w in &weights {
                c.push((41, num(*w)));
            }
            for p in &controls {
                point(&mut c, 10, *p);
            }
            for p in &fits {
                point(&mut c, 11, *p);
            }
            "SPLINE"
        }
        1 => "TEXT",
        2 => "ATTRIB",
        3 => "ATTDEF",
        4 => "BLOCK",
        5 => "ENDBLK",
        6 => "SEQEND",
        20..=26 => "DIMENSION",
        27 => "POINT",
        32 => "TRACE",
        33 => "SHAPE",
        34 => "VIEWPORT",
        37 => "REGION",
        38 => "3DSOLID",
        39 => "BODY",
        40 => "RAY",
        41 => "XLINE",
        44 => "MTEXT",
        45 => "LEADER",
        46 => "TOLERANCE",
        47 => "MLINE",
        78 => "HATCH",
        _ => "ENTITY",
    };
    o.codes = c;
    Ok(())
}

/// A decoded drawing: its objects by handle, in the object map's order.
pub struct Drawing {
    /// The release.
    pub release: Release,
    /// Objects in map order.
    pub objects: Vec<Object>,
    /// Objects that could not be decoded.
    pub broken: usize,
}

/// Decodes a DWG file's objects.
pub fn read(data: &[u8]) -> Result<Drawing, IoError> {
    let r = Release::of(data.get(..6).ok_or_else(|| corrupt("not a DWG drawing"))?)?;
    let (objects_data, map): (std::borrow::Cow<'_, [u8]>, Vec<(u64, usize)>) =
        if r == Release::R2000 {
            // Section locators: number, seeker, size.
            let count = le32(data, 0x15)? as usize;
            let mut handles = None;
            for k in 0..count.min(16) {
                let at = 0x19 + k * 9;
                let number = *data.get(at).ok_or_else(|| corrupt("bad section locator"))?;
                let seeker = le32(data, at + 1)? as usize;
                if number == 2 {
                    handles = Some(seeker);
                }
            }
            let at = handles.ok_or_else(|| corrupt("the object map is missing"))?;
            (std::borrow::Cow::Borrowed(data), object_map(data, at)?)
        } else {
            let mut sections = sections_2004(data)?;
            let handles = sections
                .remove("AcDb:Handles")
                .ok_or_else(|| corrupt("the object map is missing"))?;
            let objects = sections
                .remove("AcDb:AcDbObjects")
                .ok_or_else(|| corrupt("the objects are missing"))?;
            (std::borrow::Cow::Owned(objects), object_map(&handles, 0)?)
        };
    let mut drawing = Drawing {
        release: r,
        objects: Vec::with_capacity(map.len()),
        broken: 0,
    };
    for (handle, offset) in map {
        match object(&objects_data, offset, r) {
            Ok(o) if o.handle == handle => drawing.objects.push(o),
            _ => drawing.broken += 1,
        }
    }
    Ok(drawing)
}

/// The drawing as DXF group codes: layers, blocks and model-space
/// entities, polylines followed by their vertices.
pub fn to_dxf(d: &Drawing) -> Vec<(i32, String)> {
    let by_handle: BTreeMap<u64, &Object> = d.objects.iter().map(|o| (o.handle, o)).collect();
    let layer_name = |h: Option<u64>| -> String {
        h.and_then(|h| by_handle.get(&h))
            .filter(|o| o.kind == 51)
            .map_or_else(|| "0".to_string(), |o| o.name.clone())
    };
    let mut out: Vec<(i32, String)> = Vec::new();
    let put = |out: &mut Vec<(i32, String)>, code: i32, v: &str| out.push((code, v.into()));
    // Vertices by their polyline.
    let mut vertices: BTreeMap<u64, Vec<&Object>> = BTreeMap::new();
    for o in &d.objects {
        if o.dxf == "VERTEX"
            && let Some(owner) = o.owner
        {
            vertices.entry(owner).or_default().push(o);
        }
    }
    let emit = |out: &mut Vec<(i32, String)>, o: &Object| {
        let kind = o.dxf;
        out.push((0, kind.into()));
        out.push((8, layer_name(o.layer)));
        if o.color != 256 && o.color != 0 {
            out.push((62, o.color.to_string()));
        }
        if kind == "INSERT"
            && let Some(block) = o.refs.first().and_then(|h| by_handle.get(h))
        {
            out.push((2, block.name.clone()));
        }
        out.extend(o.codes.iter().cloned());
        if kind == "POLYLINE" {
            for v in vertices.get(&o.handle).map(Vec::as_slice).unwrap_or(&[]) {
                out.push((0, "VERTEX".into()));
                out.push((8, layer_name(v.layer)));
                out.extend(v.codes.iter().cloned());
            }
            out.push((0, "SEQEND".into()));
        }
    };
    // Layers.
    put(&mut out, 0, "SECTION");
    put(&mut out, 2, "TABLES");
    put(&mut out, 0, "TABLE");
    put(&mut out, 2, "LAYER");
    for o in d.objects.iter().filter(|o| o.kind == 51) {
        put(&mut out, 0, "LAYER");
        out.push((2, o.name.clone()));
        out.push((62, o.color.to_string()));
    }
    put(&mut out, 0, "ENDTAB");
    put(&mut out, 0, "ENDSEC");
    // Blocks: entities owned by a block header other than the spaces.
    let drawn = |o: &Object| {
        !matches!(o.dxf, "VERTEX" | "SEQEND" | "BLOCK" | "ENDBLK" | "") && o.layer.is_some()
    };
    put(&mut out, 0, "SECTION");
    put(&mut out, 2, "BLOCKS");
    let model = d
        .objects
        .iter()
        .find(|o| o.kind == 49 && o.name.eq_ignore_ascii_case("*Model_Space"))
        .map(|o| o.handle);
    for block in d.objects.iter().filter(|o| o.kind == 49) {
        if block.name.starts_with('*')
            && (block.name.eq_ignore_ascii_case("*Model_Space")
                || block.name.to_ascii_lowercase().starts_with("*paper_space"))
        {
            continue;
        }
        put(&mut out, 0, "BLOCK");
        out.push((2, block.name.clone()));
        point(&mut out, 10, block.base);
        for o in d
            .objects
            .iter()
            .filter(|o| o.owner == Some(block.handle) && drawn(o))
        {
            emit(&mut out, o);
        }
        put(&mut out, 0, "ENDBLK");
    }
    put(&mut out, 0, "ENDSEC");
    put(&mut out, 0, "SECTION");
    put(&mut out, 2, "ENTITIES");
    for o in d
        .objects
        .iter()
        .filter(|o| drawn(o) && (o.space == 2 || (o.owner.is_some() && o.owner == model)))
    {
        emit(&mut out, o);
    }
    put(&mut out, 0, "ENDSEC");
    put(&mut out, 0, "EOF");
    out
}

/// DWG import.
#[derive(Debug, Default, Clone, Copy)]
pub struct DwgImporter;

impl Importer for DwgImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Dwg
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        if bytes.len() > 6 && bytes.starts_with(b"AC10") && Release::of(&bytes[..6]).is_ok() {
            Confidence::CERTAIN
        } else if name.to_ascii_lowercase().ends_with(".dwg") {
            Confidence::EXTENSION
        } else {
            Confidence::NONE
        }
    }

    fn import(
        &self,
        src: &mut dyn Read,
        opts: &ImportOptions,
        tx: &mut Transaction<'_>,
        target: &Target,
        progress: &dyn Progress,
    ) -> Result<ImportReport, IoError> {
        let mut bytes = Vec::new();
        src.take(512 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| corrupt(e.to_string()))?;
        if bytes.len() > 512 * 1024 * 1024 {
            return Err(IoError::LimitExceeded("DWG input exceeds 512 MiB".into()));
        }
        progress.report(0.1, "reading DWG");
        let drawing = read(&bytes)?;
        let codes = to_dxf(&drawing);
        let pairs: Vec<(i32, &str)> = codes.iter().map(|(c, v)| (*c, v.as_str())).collect();
        let mut report =
            crate::io::import::dxf::import_pairs(&pairs, &bytes, opts, tx, target, progress)?;
        if drawing.broken > 0 {
            report.warnings.push(crate::diagnostics::Warning::new(
                crate::diagnostics::WarningCode::W2004,
                format!("{} DWG objects could not be decoded", drawing.broken),
            ));
        }
        Ok(report)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn bit_fields() {
        // BS 01 + RC 5, BD 01 (1.0), BB 10, B 1.
        let data = [0b0100_0001, 0b0101_1010, 0b0000_0000];
        let mut b = Bits::at(&data, 0, 24);
        assert_eq!(b.bs().unwrap(), 5);
        assert_eq!(b.bd().unwrap(), 1.0);
        assert_eq!(b.bb().unwrap(), 2);
        assert!(b.b().unwrap());
        // Modular chars: 0x82 0x01 = 130; signed 0x41 = -1.
        let mut at = 0;
        assert_eq!(mc(&[0x82, 0x01], &mut at, false).unwrap(), 130);
        let mut at = 0;
        assert_eq!(mc(&[0x41], &mut at, true).unwrap(), -1);
        let mut at = 0;
        assert_eq!(
            ms(&[0x05, 0x80, 0x01, 0x00], &mut at).unwrap(),
            5 | (1 << 15)
        );
    }

    #[test]
    fn the_page_compression() {
        // Four literals "abcd", then a copy of 4 from 4 back (opcode 0x21 +
        // 2: count 5? no: 0x22 copies 4), then the end.
        let src = [0x01, b'a', b'b', b'c', b'd', 0x22, 0x0c, 0x00, 0x11];
        let out = decompress(&src, 64).unwrap();
        assert_eq!(&out, b"abcdabcd");
        assert!(decompress(&[0x01, b'a', b'b', b'c', b'd', 0x22, 0xff, 0x00, 0x11], 64).is_err());
    }

    /// A bit writer for test drawings.
    #[derive(Default)]
    struct Writer {
        bytes: Vec<u8>,
        bit: usize,
    }

    impl Writer {
        fn bits(&mut self, v: u64, n: usize) {
            for k in (0..n).rev() {
                if self.bit.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                if (v >> k) & 1 == 1 {
                    let last = self.bytes.len() - 1;
                    self.bytes[last] |= 1 << (7 - self.bit % 8);
                }
                self.bit += 1;
            }
        }
        fn b(&mut self, v: bool) {
            self.bits(u64::from(v), 1);
        }
        fn rc(&mut self, v: u8) {
            self.bits(u64::from(v), 8);
        }
        fn rs(&mut self, v: u16) {
            for x in v.to_le_bytes() {
                self.rc(x);
            }
        }
        fn rl(&mut self, v: u32) {
            for x in v.to_le_bytes() {
                self.rc(x);
            }
        }
        fn rd(&mut self, v: f64) {
            for x in v.to_le_bytes() {
                self.rc(x);
            }
        }
        fn bs(&mut self, v: u16) {
            self.bits(0, 2);
            self.rs(v);
        }
        fn bl(&mut self, v: u32) {
            self.bits(0, 2);
            self.rl(v);
        }
        fn bd(&mut self, v: f64) {
            self.bits(0, 2);
            self.rd(v);
        }
        fn dd(&mut self, v: f64) {
            self.bits(3, 2);
            self.rd(v);
        }
        fn h(&mut self, code: u8, v: u8) {
            self.bits(u64::from(code), 4);
            if v == 0 {
                self.bits(0, 4);
            } else {
                self.bits(1, 4);
                self.rc(v);
            }
        }
        fn tv(&mut self, s: &str) {
            self.bs(s.len() as u16);
            for c in s.bytes() {
                self.rc(c);
            }
        }
    }

    /// An object: its size, type, handle-stream offset, handle, no extended
    /// data, then `data` and `handles`.
    fn object(
        kind: u16,
        handle: u8,
        data: &dyn Fn(&mut Writer),
        handles: &dyn Fn(&mut Writer),
    ) -> Vec<u8> {
        // Two passes: the handle stream's offset needs the data's length.
        let body = |offset: u32| {
            let mut w = Writer::default();
            w.bs(kind);
            w.rl(offset);
            w.h(0, handle);
            w.bs(0);
            data(&mut w);
            let at = w.bit;
            handles(&mut w);
            (w.bytes, at)
        };
        let (_, at) = body(0);
        let (bytes, _) = body(at as u32);
        let mut out = Vec::new();
        out.extend((bytes.len() as u16).to_le_bytes());
        out.extend(bytes);
        out
    }

    /// The common part of a model-space entity on layer handle `layer`.
    fn entity_common(w: &mut Writer) {
        w.b(false); // no graphics
        w.bits(2, 2); // model space
        w.bits(2, 2); // no reactors
        w.b(true); // no links
        w.bs(256); // by layer
        w.bits(1, 2); // line type scale 1
        w.bits(0, 2);
        w.bits(0, 2);
        w.bits(2, 2); // visible
        w.rc(0x1d);
    }

    fn unsigned_mc(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    fn signed_mc(out: &mut Vec<u8>, v: i64) {
        let (mut m, negative) = (v.unsigned_abs(), v < 0);
        loop {
            if m < 0x40 {
                out.push(m as u8 | if negative { 0x40 } else { 0 });
                return;
            }
            out.push((m & 0x7f) as u8 | 0x80);
            m >>= 7;
        }
    }

    /// A release 2000 drawing: layer `Walls` (green), a line and a closed
    /// 100 × 50 rectangle on it.
    pub(super) fn test_drawing() -> Vec<u8> {
        let layer = object(
            51,
            0x10,
            &|w| {
                w.bits(2, 2); // no reactors
                w.tv("Walls");
                w.b(false);
                w.bs(0);
                w.b(false);
                w.bs(0);
                w.bs(3);
            },
            &|_| {},
        );
        let line = object(
            19,
            0x20,
            &|w| {
                entity_common(w);
                w.b(true);
                w.rd(0.0);
                w.dd(200.0);
                w.rd(0.0);
                w.dd(0.0);
                w.b(true);
                w.b(true);
            },
            &|w| {
                w.h(3, 0); // extension dictionary
                w.h(5, 0x10); // layer
            },
        );
        let rectangle = object(
            77,
            0x21,
            &|w| {
                entity_common(w);
                w.bs(512);
                w.bl(4);
                w.rd(0.0);
                w.rd(100.0);
                for (x, y) in [(100.0, 100.0), (100.0, 150.0), (0.0, 150.0)] {
                    w.dd(x);
                    w.dd(y);
                }
            },
            &|w| {
                w.h(3, 0);
                w.h(5, 0x10);
            },
        );
        let mut file = b"AC1015".to_vec();
        file.resize(0x15, 0);
        file.extend(3u32.to_le_bytes());
        let records_at = file.len();
        file.resize(records_at + 27 + 4, 0);
        let mut offsets = Vec::new();
        for o in [&layer, &line, &rectangle] {
            offsets.push(file.len());
            file.extend(o.iter());
        }
        let map_at = file.len();
        let mut entries = Vec::new();
        let (mut h, mut at) = (0i64, 0i64);
        for (handle, offset) in [(0x10, offsets[0]), (0x20, offsets[1]), (0x21, offsets[2])] {
            unsigned_mc(&mut entries, (handle - h) as u64);
            signed_mc(&mut entries, offset as i64 - at);
            h = handle;
            at = offset as i64;
        }
        file.extend(((entries.len() + 2) as u16).to_be_bytes());
        file.extend(&entries);
        file.extend([0, 0]);
        file.extend(2u16.to_be_bytes());
        file.extend([0, 0]);
        for (k, (number, seeker)) in [(0u8, 0usize), (1, 0), (2, map_at)].into_iter().enumerate() {
            let r = records_at + k * 9;
            file[r] = number;
            file[r + 1..r + 5].copy_from_slice(&(seeker as u32).to_le_bytes());
        }
        file
    }

    #[test]
    fn a_written_drawing_reads_and_imports() {
        use crate::commands::{CommandRegistry, Engine};
        use crate::document::Document;
        use std::sync::Arc;
        let bytes = test_drawing();
        crate::io::step_structure::tests::golden_bytes("dwg/walls-2000.dwg", &bytes);
        let d = read(&bytes).unwrap();
        assert_eq!(
            (d.release, d.objects.len(), d.broken),
            (Release::R2000, 3, 0)
        );
        let codes = to_dxf(&d);
        let text: Vec<String> = codes.iter().map(|(c, v)| format!("{c}:{v}")).collect();
        assert!(text.contains(&"2:Walls".to_string()), "{text:?}");
        assert!(text.contains(&"11:200".to_string()), "{text:?}");
        assert!(text.contains(&"0:LWPOLYLINE".to_string()), "{text:?}");
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        let storage = Arc::new(crate::io::MemoryStorage::new());
        e.set_storage(storage.clone());
        crate::io::Storage::write(
            storage.as_ref(),
            &crate::io::StorageKey::new("w.dwg"),
            &bytes,
        )
        .unwrap();
        let out = e
            .execute_erased("import", serde_json::json!({ "path": "w.dwg" }))
            .unwrap();
        assert_eq!(out["faces"], 1, "{out}");
        assert_eq!(out["edges"], 5, "{out}");
        assert!(e.document().tags.iter().any(|(_, t)| t.name == "Walls"));
    }

    #[test]
    fn releases() {
        assert_eq!(Release::of(b"AC1015").unwrap(), Release::R2000);
        assert_eq!(Release::of(b"AC1032").unwrap(), Release::R2018);
        assert!(
            Release::of(b"AC1021")
                .unwrap_err()
                .to_string()
                .contains("2007")
        );
        assert!(Release::of(b"AC1014").is_err());
        assert!(Release::of(b"PK\x03\x04..").is_err());
    }

    /// Reads every `.dwg` in `PLANYRITH_DWG_DIR` (drawings from other
    /// writers, not kept in the repository) and compares its entities with
    /// the `.dxf` beside it when there is one.
    #[test]
    fn drawings_from_a_folder() {
        let Some(dir) = std::env::var_os("PLANYRITH_DWG_DIR") else {
            return;
        };
        let mut files = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(dir)];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "dwg") {
                    files.push(p);
                }
            }
        }
        files.sort();
        for path in files {
            let bytes = std::fs::read(&path).unwrap();
            let drawing = match read(&bytes) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("{}: {e}", path.display());
                    continue;
                }
            };
            let codes = to_dxf(&drawing);
            let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
            let mut in_entities = false;
            for (c, v) in &codes {
                if *c == 2 && v == "ENTITIES" {
                    in_entities = true;
                }
                if in_entities && *c == 0 && v != "ENDSEC" && v != "EOF" {
                    *kinds.entry(v.clone()).or_default() += 1;
                }
            }
            let mut want: BTreeMap<String, usize> = BTreeMap::new();
            let dxf = path.with_extension("dxf");
            if let Ok(text) = std::fs::read(&dxf) {
                let text = String::from_utf8_lossy(&text);
                let pairs = crate::io::import::dxf::pairs(&text).unwrap_or_default();
                let mut on = false;
                for (c, v) in pairs {
                    if c == 2 && v == "ENTITIES" {
                        on = true;
                    } else if c == 0 && v == "ENDSEC" {
                        on = false;
                    } else if on && c == 0 {
                        *want.entry(v.to_string()).or_default() += 1;
                    }
                }
            }
            // Line ends, rounded, as the DWG and the DXF give them.
            let lines = |pairs: Vec<(i32, String)>| {
                let mut out: Vec<String> = Vec::new();
                let mut cur: Option<Vec<String>> = None;
                let mut on = false;
                for (c, v) in pairs {
                    if c == 2 && v == "ENTITIES" {
                        on = true;
                    }
                    if !on {
                        continue;
                    }
                    if c == 0 {
                        if let Some(l) = cur.take() {
                            out.push(l.join(","));
                        }
                        if v == "LINE" {
                            cur = Some(Vec::new());
                        }
                    } else if let Some(l) = cur.as_mut()
                        && matches!(c, 10 | 20 | 30 | 11 | 21 | 31)
                    {
                        l.push(format!("{:.4}", v.trim().parse::<f64>().unwrap_or(0.0)));
                    }
                }
                out.sort();
                out
            };
            if let Ok(text) = std::fs::read(&dxf) {
                let text = String::from_utf8_lossy(&text);
                let theirs: Vec<(i32, String)> = crate::io::import::dxf::pairs(&text)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(c, v)| (c, v.to_string()))
                    .collect();
                let (a, b) = (lines(codes.clone()), lines(theirs));
                eprintln!("  lines equal: {} ({} vs {})", a == b, a.len(), b.len());
                if a != b {
                    eprintln!(
                        "  ours {:?}\n  dxf  {:?}",
                        &a[..a.len().min(3)],
                        &b[..b.len().min(3)]
                    );
                }
            }
            eprintln!(
                "{}: {:?}, {} objects, {} broken\n  got  {kinds:?}\n  want {want:?}",
                path.display(),
                drawing.release,
                drawing.objects.len(),
                drawing.broken
            );
        }
    }
}
