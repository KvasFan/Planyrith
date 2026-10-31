//! Classic 2018–2020 object-archive reader (docs/09 §5).
//!
//! Ported from the MIT-licensed legacy-format reference at
//! iamahsanmehmood/openskp, revision c0bbeaa62191d776ea25e979555c5d6c464d54fa.
//! Copyright (c) 2024–2026 Ahsan Mehmood. The original MIT grant is retained
//! in tests/fixtures/intermediate/LICENSE. No reference interpreter is used.
use super::Result;
use openskp::{GeometryRun, MeshEdge, MeshFace, Model};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default)]
struct Draw {
    material: u16,
    layer: u16,
    hidden: bool,
    soft: bool,
    smooth: bool,
}
#[derive(Clone)]
enum Body {
    Pending,
    Vertex([f64; 3]),
    Edge(usize, usize, usize),
    Use(usize, bool),
    Loop(Vec<usize>),
    Face([f64; 4], Vec<usize>, u16),
    Layer(openskp::Layer),
    Material(openskp::Material),
    Curve(Option<openskp::Arc>),
    Definition {
        items: Vec<usize>,
        name: String,
        guid: String,
        group: bool,
    },
    Instance {
        definition: usize,
        transform: [f64; 13],
        name: String,
        image: bool,
    },
    Attrs(Vec<usize>),
    Dict(openskp::Dictionary),
    Texture(Box<openskp::FaceTexture>),
    Dib(Vec<u8>),
    Section([f64; 4], String, String),
    Point([f64; 3]),
    Guide([f64; 8]),
    Polyline(Vec<[f64; 3]>),
    Font(openskp::Font),
    Text(Box<openskp::Text>, usize),
    Dimension(Box<openskp::Dimension>, usize),
    Radial(Box<openskp::RadialDimension>, usize),
    Other,
}
#[derive(Clone)]
struct Node {
    pid: u32,
    attrs: usize,
    draw: Draw,
    body: Body,
}
impl Default for Node {
    fn default() -> Self {
        Self {
            pid: 0,
            attrs: 0,
            draw: Draw::default(),
            body: Body::Pending,
        }
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    next: usize,
    base: usize,
    objects: BTreeMap<usize, Node>,
    classes: BTreeMap<usize, String>,
    depth: usize,
    version: u16,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], pos: usize, base: usize, version: u16) -> Self {
        Self {
            bytes,
            pos,
            next: base,
            base,
            objects: BTreeMap::new(),
            classes: BTreeMap::new(),
            depth: 0,
            version,
        }
    }
    fn raw(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or("archive size overflow")?;
        let b = self
            .bytes
            .get(self.pos..end)
            .ok_or("truncated classic archive")?;
        self.pos = end;
        Ok(b)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.raw(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.raw(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.raw(4)?.try_into()?))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.raw(8)?.try_into()?))
    }
    fn floats<const N: usize>(&mut self) -> Result<[f64; N]> {
        let mut a = [0.; N];
        for x in &mut a {
            *x = self.f64()?;
        }
        Ok(a)
    }
    fn peek(&self, n: usize) -> &[u8] {
        self.bytes
            .get(self.pos..self.pos.saturating_add(n))
            .unwrap_or(&[])
    }
    fn tag(&self) -> Result<u16> {
        Ok(u16::from_le_bytes(
            self.peek(2)
                .try_into()
                .map_err(|_| "truncated archive tag")?,
        ))
    }
    fn next_class_is(&self, name: &str) -> bool {
        let Ok(tag) = self.tag() else {
            return false;
        };
        if tag == 65535 {
            return self.peek(6 + name.len()).get(4..6) == Some(&(name.len() as u16).to_le_bytes())
                && self.peek(6 + name.len()).get(6..) == Some(name.as_bytes());
        }
        let slot = if tag == 32767 {
            let Some(raw) = self.peek(6).get(2..6) else {
                return false;
            };
            let value = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
            if value & 0x80000000 == 0 {
                return false;
            }
            (value & 0x7fffffff) as usize
        } else if tag & 32768 != 0 {
            usize::from(tag & 32767)
        } else {
            return false;
        };
        self.classes.get(&slot).is_some_and(|n| n == name)
    }
    fn string(&mut self) -> Result<String> {
        if self.raw(3)? != [255, 254, 255] {
            return Err(format!("missing string at {:#x}", self.pos - 3).into());
        }
        let mut n = usize::from(self.u8()?);
        if n == 255 {
            n = usize::from(self.u16()?);
            if n == 65535 {
                n = self.u32()? as usize;
            }
        }
        if n > 16 * 1024 * 1024 {
            return Err("classic string exceeds limit".into());
        }
        let data = self.raw(n * 2)?;
        let chars = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>();
        Ok(String::from_utf16(&chars)?)
    }
    fn count(&mut self) -> Result<usize> {
        let at = self.pos;
        let n = self.u32()? as usize;
        if n > 5_000_000 {
            if self.version == 20
                && let Some(marker) = (at..(at + 12).min(self.bytes.len().saturating_sub(4)))
                    .find(|p| self.bytes.get(*p..*p + 4) == Some(&[255, 254, 255, 0]))
            {
                let mut end = marker + 4;
                while self.bytes.get(end) == Some(&0) {
                    end += 1;
                }
                for back in 0..4 {
                    let p = end.saturating_sub(back);
                    if p < marker + 4 {
                        continue;
                    }
                    let Some(bytes) = self.bytes.get(p..p + 6) else {
                        continue;
                    };
                    let count = u32::from_le_bytes(bytes[..4].try_into()?) as usize;
                    let tag = u16::from_le_bytes(bytes[4..6].try_into()?);
                    if count > 0
                        && count <= 5_000_000
                        && (tag == 65535
                            || tag == 32767
                            || tag == 0
                            || self.classes.contains_key(&usize::from(tag & 32767)))
                    {
                        self.pos = p + 4;
                        return Ok(count);
                    }
                }
            }
            return Err(format!("implausible count {n} at {:#x}", self.pos - 4).into());
        }
        Ok(n)
    }
    fn pointer(&mut self, expected: Option<&str>) -> Result<usize> {
        if self.depth >= 128 {
            return Err("classic object nesting exceeds limit".into());
        }
        let tag = self.u16()?;
        if tag == 0 {
            return Ok(0);
        }
        let (new_class, slot) = if tag == 65535 {
            let _schema = self.u16()?;
            let n = usize::from(self.u16()?);
            if n > 128 {
                return Err("invalid class name length".into());
            }
            let name = std::str::from_utf8(self.raw(n)?)?.to_owned();
            let s = self.next;
            self.next += 1;
            self.classes.insert(s, name);
            (true, s)
        } else if tag == 32767 {
            let n = self.u32()?;
            (n & 0x80000000 != 0, (n & 0x7fffffff) as usize)
        } else {
            (tag & 32768 != 0, usize::from(tag & 32767))
        };
        if !new_class {
            if slot >= self.base && !self.objects.contains_key(&slot) {
                return Err(format!("unresolved object {slot} at {:#x}", self.pos).into());
            }
            return Ok(slot);
        }
        let name = match self.classes.get(&slot) {
            Some(s) => s.clone(),
            None if slot < self.base => {
                let n = expected.ok_or("unknown pre-model class")?.to_owned();
                self.classes.insert(slot, n.clone());
                n
            }
            None => return Err(format!("unknown class {slot} at {:#x}", self.pos).into()),
        };
        let id = self.next;
        self.next += 1;
        if self.next - self.base > 10_000_000 {
            return Err("classic object limit".into());
        }
        self.objects.insert(id, Node::default());
        self.depth += 1;
        let mut node = Node::default();
        let result = self.body(&name, &mut node);
        self.depth -= 1;
        result.map_err(|e| format!("{name} at {:#x}: {e}", self.pos))?;
        self.objects.insert(id, node);
        Ok(id)
    }
    fn preamble(&mut self, n: &mut Node) -> Result<()> {
        n.attrs = self.pointer(Some("CAttributeContainer"))?;
        let mask = self.u8()?;
        if mask > 15 {
            return Err("invalid persistent-id mask".into());
        }
        for k in 0..4 {
            if mask & (1 << k) != 0 {
                n.pid |= u32::from(self.u8()?) << (8 * k);
            }
        }
        Ok(())
    }
    fn draw(&mut self) -> Result<Draw> {
        let b = self.raw(8)?.to_vec();
        let layer = if self.tag()? & 32768 != 0 || self.tag()? == 32767 {
            u16::try_from(self.pointer(Some("CLayer"))?)
                .map_err(|_| "layer reference exceeds u16")?
        } else {
            self.u16()?
        };
        Ok(Draw {
            material: u16::from_le_bytes([b[0], b[1]]),
            hidden: b[2] != 0,
            soft: b[5] != 0,
            smooth: b[6] != 0,
            layer,
        })
    }
    fn list(&mut self) -> Result<Vec<usize>> {
        let count = self.count()?;
        self.items(count)
    }
    fn items(&mut self, count: usize) -> Result<Vec<usize>> {
        let mut v = Vec::new();
        for _ in 0..count {
            let p = self.pointer(None)?;
            if p != 0 {
                v.push(p);
            }
        }
        Ok(v)
    }
    fn value(&mut self, t: u8, depth: usize) -> Result<openskp::Value> {
        if depth > 64 {
            return Err("attribute nesting limit".into());
        }
        use openskp::Value as V;
        Ok(match t {
            0 => V::Nil,
            4 => V::Int(self.u32()? as i32),
            5 => V::Float(f32::from_le_bytes(self.raw(4)?.try_into()?)),
            6 | 12 => V::Double(self.f64()?),
            7 => V::Bool(self.u8()? != 0),
            // Packed RGBA uses the same four-byte storage as an integer.
            // Retain every channel, including alpha, in the attribute value.
            8 => V::Int(self.u32()? as i32),
            9 => V::Time(self.u32()?),
            10 => V::Text(self.string()?),
            11 => {
                let n = self.count()?;
                let mut v = Vec::new();
                for _ in 0..n {
                    let t = self.u8()?;
                    v.push(self.value(t, depth + 1)?);
                }
                V::Array(v)
            }
            17 => V::Vector(self.floats()?),
            18 => V::Direction(self.floats()?),
            _ => return Err(format!("unsupported classic attribute type {t}").into()),
        })
    }
    fn body(&mut self, name: &str, n: &mut Node) -> Result<()> {
        if !matches!(name, "CDib" | "CCamera") {
            self.preamble(n)?;
        }
        n.body = match name {
            "CVertex" => Body::Vertex(self.floats()?),
            "CEdge" => {
                n.draw = self.draw()?;
                let a = self.pointer(Some("CVertex"))?;
                let b = self.pointer(Some("CVertex"))?;
                let c = self.pointer(None)?;
                Body::Edge(a, b, c)
            }
            "CEdgeUse" => {
                let edge = self.pointer(Some("CEdge"))?;
                let reverse = self.u8()? != 0;
                self.pointer(Some("CLoop"))?;
                Body::Use(edge, reverse)
            }
            "CLoop" => {
                self.raw(2)?;
                let mut uses = Vec::new();
                while self.tag()? != 0 {
                    uses.push(self.pointer(Some("CEdgeUse"))?);
                }
                self.u16()?;
                Body::Loop(uses)
            }
            "CFace" => {
                n.draw = self.draw()?;
                let plane = self.floats()?;
                let count = self.count()?;
                let mut loops = Vec::new();
                for _ in 0..count {
                    loops.push(self.pointer(Some("CLoop"))?);
                }
                Body::Face(plane, loops, self.u16()?)
            }
            "CCurve" => {
                self.u8()?;
                self.u32()?;
                Body::Curve(None)
            }
            "CArcCurve" => {
                self.raw(5)?;
                let center = self.floats::<3>()?;
                let normal = self.floats()?;
                let x = self.floats::<3>()?;
                let start = self.f64()?;
                let end = self.f64()?;
                let y = self.floats::<3>()?;
                let radius = glam::DVec3::from_array(x).length() * 0.0254;
                Body::Curve(Some(openskp::Arc {
                    center_m: center.map(|v| v * 0.0254),
                    normal,
                    x_axis_m: x.map(|v| v * 0.0254),
                    y_axis_m: y.map(|v| v * 0.0254),
                    radius_m: radius,
                    start_rad: start,
                    end_rad: end,
                }))
            }
            "CAttributeContainer" => {
                let mut c = Vec::new();
                while self.tag()? != 0 {
                    c.push(self.pointer(Some("CAttributeNamed"))?);
                }
                self.u16()?;
                Body::Attrs(c)
            }
            "CAttributeNamed" => {
                self.raw(4)?;
                let name = self.string()?;
                let mut entries = Vec::new();
                loop {
                    let key = self.string()?;
                    if key.is_empty() {
                        break;
                    }
                    let t = self.u8()?;
                    entries.push((key, self.value(t, 0)?));
                }
                self.u32()?;
                Body::Dict(openskp::Dictionary {
                    name,
                    entries,
                    flag: 0,
                })
            }
            "CFaceTextureCoords" => {
                self.u32()?;
                let values = self.floats::<24>()?;
                let mut pins = Vec::new();
                for _ in 0..2 {
                    let count = self.count()?;
                    let mut p = Vec::new();
                    for _ in 0..count {
                        p.push(self.floats::<4>()?);
                    }
                    pins.push(p);
                }
                let front = values[..9].try_into()?;
                let back = values[12..21].try_into()?;
                Body::Texture(Box::new(openskp::FaceTexture {
                    front,
                    back,
                    front_extra: values[9..12].try_into()?,
                    back_extra: values[21..].try_into()?,
                    front_pins: pins.remove(0),
                    back_pins: pins.remove(0),
                    flags: [self.u32()?, self.u32()?],
                }))
            }
            "CLayer" => {
                let name = self.string()?;
                let begin = self.pos;
                while self.peek(3) != [255, 254, 255] && self.pos - begin < 8 {
                    self.u8()?;
                }
                let hidden = self.bytes.get(begin).is_some_and(|b| *b != 0);
                self.string()?;
                let flag = self.u16()?;
                if flag & 255 != 0 {
                    return Err("textured classic layer is unsupported".into());
                }
                let rgba = self.raw(4)?.try_into()?;
                self.string()?;
                self.raw(21)?;
                Body::Layer(openskp::Layer {
                    name,
                    visible: !hidden,
                    rgba,
                })
            }
            "CMaterial" => {
                let name = self.string()?;
                let flag = self.u16()?;
                if flag == 0 {
                    let rgba = self.raw(4)?.try_into()?;
                    self.string()?;
                    self.raw(8)?;
                    let transparency = self.f64()?;
                    let use_alpha = self.u8()? != 0;
                    Body::Material(openskp::Material::Solid {
                        name,
                        rgba,
                        opacity: if use_alpha { 1.0 - transparency } else { 1.0 },
                        colorize_type: 0,
                    })
                } else {
                    self.pos -= 1;
                    let mut texture = Node::default();
                    self.preamble(&mut texture)?;
                    let dib = self.pointer(Some("CDib"))?;
                    let offset = self
                        .bytes
                        .get(self.pos..self.pos.saturating_add(28))
                        .unwrap_or(&[])
                        .windows(3)
                        .position(|s| s == [255, 254, 255])
                        .ok_or("missing texture size")?;
                    if offset == 20 {
                        self.u32()?;
                    } else if offset != 16 {
                        return Err("invalid texture size block".into());
                    }
                    let w = self.f64()?;
                    let h = self.f64()?;
                    let file = self.string()?;
                    let avg = self.raw(9)?.to_vec();
                    self.string()?;
                    let flags = self.raw(8)?.to_vec();
                    let transparency = self.f64()?;
                    let alpha = self.u8()? != 0;
                    let bytes = match self.objects.get(&dib).map(|n| &n.body) {
                        Some(Body::Dib(b)) => Some(b.clone()),
                        _ => return Err("texture image missing".into()),
                    };
                    Body::Material(openskp::Material::Textured {
                        name,
                        texture: Some(file),
                        applied_size_in: Some((w, h)),
                        jpeg_quality: None,
                        image_bytes: bytes,
                        avg_rgba: Some(avg[..4].try_into()?),
                        texture_avg_rgba: Some(avg[5..9].try_into()?),
                        colorized: flags[4] != 0,
                        colorize_type: u32::from(flags[4]),
                        opacity: if alpha { 1.0 - transparency } else { 1.0 },
                    })
                }
            }
            "CDib" => {
                self.u32()?;
                let count = self.u32()? as usize;
                Body::Dib(self.raw(count)?.to_vec())
            }
            "CCamera" => {
                self.raw(137)?;
                self.u16()?;
                self.string()?;
                self.raw(33)?;
                Body::Other
            }
            "CThumbnail" => {
                self.pointer(Some("CCamera"))?;
                self.pointer(Some("CDib"))?;
                Body::Other
            }
            "CComponentDefinition" => {
                self.raw(22)?;
                let layers = self.count()?;
                let mut got = 0;
                while got < layers {
                    let p = self.pointer(Some("CLayer"))?;
                    if p != 0 {
                        got += 1;
                    }
                }
                if self.version == 19 && layers > 0 && self.tag()? == 0 {
                    self.u16()?;
                }
                self.reference()?;
                self.u32()?;
                let items = self.list()?;
                let rel = self.count()?;
                for _ in 0..rel {
                    self.pointer(Some("CRelationship"))?;
                }
                self.u16()?;
                let guid = self.raw(16)?.iter().map(|v| format!("{v:02x}")).collect();
                let name = self.string()?;
                self.string()?;
                self.string()?;
                self.u32()?;
                let start = self.pos;
                let thumb = self
                    .classes
                    .iter()
                    .find(|(_, n)| n.as_str() == "CThumbnail")
                    .map(|(s, _)| *s);
                let offset = (0..96)
                    .find(|i| {
                        self.bytes
                            .get(start + i..start + i + 2)
                            .is_some_and(|b| b == [255, 255])
                            && self.bytes.get(start + i + 6..start + i + 16) == Some(b"CThumbnail")
                            || thumb.is_some_and(|s| {
                                self.bytes.get(start + i..start + i + 2).is_some_and(|b| {
                                    u16::from_le_bytes([b[0], b[1]]) == 0x8000 | s as u16
                                })
                            })
                    })
                    .ok_or("definition thumbnail missing")?;
                let gap = self.raw(offset)?;
                let group = gap.len() >= 4 && gap[gap.len() - 4..] == [1, 0, 0, 0];
                self.pointer(Some("CThumbnail"))?;
                Body::Definition {
                    items,
                    name,
                    guid,
                    group,
                }
            }
            "CComponentInstance" | "CGroup" | "CImage" => {
                n.draw = self.draw()?;
                let image = name == "CImage";
                let definition = self.pointer(Some("CComponentDefinition"))?;
                let transform = self.floats()?;
                let name = self.string()?;
                self.raw(16)?;
                Body::Instance {
                    definition,
                    transform,
                    name,
                    image,
                }
            }
            "CRelationship" => {
                self.reference()?;
                self.reference()?;
                Body::Other
            }
            "CSkFont" => {
                let family = self.string()?;
                let bold = self.u8()? != 0;
                let italic = self.u8()? != 0;
                let size_pt = self.u32()?;
                let model_height = self.u8()? != 0;
                let height_m = self.f64()? * 0.0254;
                Body::Font(openskp::Font {
                    family,
                    bold,
                    italic,
                    size_pt,
                    model_height,
                    height_m,
                })
            }
            "CText" => {
                n.draw = self.draw()?;
                let font = self.pointer(Some("CSkFont"))?;
                let screen_position = self.floats()?;
                let anchor = self.connection()?;
                let leader_offset = self.floats::<3>()?;
                let view_direction = self.floats()?;
                let leader = match self.u32()? {
                    0 => openskp::Leader::None,
                    1 => openskp::Leader::ViewBased,
                    2 => openskp::Leader::Pushpin,
                    _ => return Err("unknown text leader".into()),
                };
                self.raw(6)?;
                let arrow = self.u32()?;
                let leader_hidden = self.u8()? == 0;
                let content = self.string()?;
                self.raw(5)?;
                Body::Text(
                    Box::new(openskp::Text {
                        screen_position,
                        anchor,
                        leader_offset: if leader == openskp::Leader::Pushpin {
                            leader_offset.map(|v| v * 0.0254)
                        } else {
                            leader_offset
                        },
                        view_direction,
                        leader,
                        arrow,
                        leader_hidden,
                        content,
                        ..Default::default()
                    }),
                    font,
                )
            }
            "CDimensionLinear" => {
                n.draw = self.draw()?;
                let text_override = self.string()?;
                let font = self.pointer(Some("CSkFont"))?;
                let aligned = self.u8()? != 0;
                let arrow = self.u32()?;
                let start = self.connection()?;
                let end = self.connection()?;
                let normal = self.floats()?;
                let x_axis = self.floats()?;
                let variant = self.u32()?;
                let offset_m = self.f64()? * 0.0254;
                let undecoded = self.f64()?;
                let text_position = self.u32()?;
                Body::Dimension(
                    Box::new(openskp::Dimension {
                        pid: 0,
                        def_index: None,
                        text_override,
                        start,
                        end,
                        normal,
                        x_axis,
                        variant,
                        offset_m,
                        undecoded,
                        text_position,
                        aligned,
                        arrow,
                        font: None,
                        layer: n.draw.layer,
                        material: (n.draw.material != 0).then_some(n.draw.material),
                        hidden: n.draw.hidden,
                        casts_shadows: true,
                        receives_shadows: true,
                    }),
                    font,
                )
            }
            "CDimensionRadial" => {
                n.draw = self.draw()?;
                let text_override = self.string()?;
                let font = self.pointer(Some("CSkFont"))?;
                let aligned = self.u8()? != 0;
                let arrow = self.u32()?;
                let edge = self.entity_reference()?;
                let leader = self.floats()?;
                let diameter = self.u8()? != 0;
                Body::Radial(
                    Box::new(openskp::RadialDimension {
                        text_override,
                        aligned,
                        arrow,
                        edge: (edge != 0).then_some(u32::try_from(edge)?),
                        leader,
                        diameter,
                        ..Default::default()
                    }),
                    font,
                )
            }
            "CPolyline3d" => {
                n.draw = self.draw()?;
                let count = self.count()?;
                let mut points = Vec::new();
                for _ in 0..count {
                    points.push(self.floats::<3>()?.map(|v| v * 0.0254));
                }
                Body::Polyline(points)
            }
            "CConstructionLine" => {
                n.draw = self.draw()?;
                let values = self.floats()?;
                self.raw(4)?;
                Body::Guide(values)
            }
            "CConstructionPoint" => {
                n.draw = self.draw()?;
                let p = self.floats()?;
                self.floats::<3>()?;
                self.u8()?;
                Body::Point(p)
            }
            "CSectionPlane" => {
                n.draw = self.draw()?;
                let start = self.pos;
                let first = self.f64()?;
                self.pos = start;
                if !first.is_finite() || first.abs() > 1.0001 {
                    self.pointer(None)?;
                }
                let plane = self.floats()?;
                let (name, label) = if self.peek(3) == [255, 254, 255] {
                    (self.string()?, self.string()?)
                } else {
                    (String::new(), String::new())
                };
                Body::Section(plane, name, label)
            }
            _ => return Err(format!("unsupported classic object {name}").into()),
        };
        Ok(())
    }
    // Connection references may point forward. Only new objects allocate map
    // slots; ordinary references must not be rejected before the walk reaches them.
    fn entity_reference(&mut self) -> Result<usize> {
        let tag = self.tag()?;
        if tag == 65535
            || (tag != 32767 && tag & 32768 != 0)
            || (tag == 32767
                && self.bytes.get(self.pos + 2..self.pos + 6).is_some_and(|b| {
                    u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0x80000000 != 0
                }))
        {
            self.pointer(None)
        } else {
            self.reference()
        }
    }
    fn connection(&mut self) -> Result<openskp::Anchor> {
        let kind = self.u32()?;
        self.u32()?;
        let point = self.floats::<3>()?;
        let entity = self.entity_reference()?;
        let second = self.entity_reference()?;
        let mut paths = [Vec::new(), Vec::new()];
        for path in &mut paths {
            let count = self.count()?;
            if count > 64 {
                return Err("annotation instance path exceeds limit".into());
            }
            for _ in 0..count {
                path.push(u32::try_from(self.entity_reference()?)?);
            }
        }
        Ok(openskp::Anchor {
            kind,
            point_m: if kind == 5 {
                point
            } else {
                point.map(|v| v * 0.0254)
            },
            entity: (entity != 0).then_some(u32::try_from(entity)?),
            second: (second != 0).then_some(u32::try_from(second)?),
            path: paths[0].clone(),
            second_path: paths[1].clone(),
        })
    }
    fn resolve_anchor(&self, anchor: &mut openskp::Anchor) -> Result<()> {
        let pid = |slot: u32| -> Result<u32> {
            let node = self
                .objects
                .get(&(slot as usize))
                .ok_or("unresolved annotation reference")?;
            if node.pid == 0 {
                return Err("annotation entity has no persistent id".into());
            }
            Ok(node.pid)
        };
        anchor.entity = anchor.entity.map(pid).transpose()?;
        anchor.second = anchor.second.map(pid).transpose()?;
        for step in anchor.path.iter_mut().chain(anchor.second_path.iter_mut()) {
            *step = pid(*step)?;
        }
        Ok(())
    }
    fn font_index(&self, model: &mut Model, slot: usize) -> Result<Option<usize>> {
        if slot == 0 {
            return Ok(None);
        }
        let Some(Body::Font(font)) = self.objects.get(&slot).map(|n| &n.body) else {
            return Err("font reference missing".into());
        };
        let i = if let Some(i) = model.fonts.iter().position(|f| f == font) {
            i
        } else {
            let i = model.fonts.len();
            model.fonts.push(font.clone());
            i
        };
        Ok(Some(i))
    }
    fn reference(&mut self) -> Result<usize> {
        let tag = self.u16()?;
        if tag == 32767 {
            Ok(self.u32()? as usize)
        } else if tag & 32768 == 0 {
            Ok(tag as usize)
        } else {
            Err("expected existing reference".into())
        }
    }
    fn prefix(&mut self, materials: usize) -> Result<(Vec<usize>, Vec<usize>, usize)> {
        let mut mats = Vec::new();
        for _ in 0..materials {
            mats.push(self.pointer(Some("CMaterial"))?);
        }
        self.u32()?;
        self.u8()?;
        let count = self.count()?;
        let mut layers = Vec::new();
        while layers.len() < count {
            let tag = self.tag()?;
            if tag == 0 {
                self.u16()?;
                continue;
            }
            if tag & 32768 == 0 && tag != 32767 {
                break;
            }
            layers.push(self.pointer(Some("CLayer"))?);
        }
        while self.tag()? == 0 {
            self.u16()?;
        }
        let anchor = self.pointer(Some("CLayer"))?;
        Ok((mats, layers, anchor))
    }
    fn dictionary(&self, attrs: usize) -> Vec<openskp::Dictionary> {
        match self.objects.get(&attrs).map(|n| &n.body) {
            Some(Body::Attrs(items)) => items
                .iter()
                .filter_map(|i| match self.objects.get(i).map(|n| &n.body) {
                    Some(Body::Dict(d)) => Some(d.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }
    fn attrs(&self, model: &mut Model, node: &Node, owner: openskp::Owner) {
        let dictionaries = self.dictionary(node.attrs);
        if !dictionaries.is_empty() {
            model.dictionaries.push(openskp::Attributes {
                owner,
                dictionaries,
            });
        }
    }
    fn run(
        &self,
        model: &mut Model,
        items: &[usize],
        definition: Option<usize>,
    ) -> Result<GeometryRun> {
        let mut run = GeometryRun {
            def_index: definition,
            ..Default::default()
        };
        let ri = model.geometry.len();
        let mut edges = BTreeSet::new();
        let mut faces = BTreeSet::new();
        for i in items {
            match self.objects.get(i).map(|n| &n.body) {
                Some(Body::Edge(..)) => {
                    edges.insert(*i);
                }
                Some(Body::Face(_, loops, _)) => {
                    faces.insert(*i);
                    for l in loops {
                        if let Some(Body::Loop(uses)) = self.objects.get(l).map(|n| &n.body) {
                            for u in uses {
                                if let Some(Body::Use(e, _)) = self.objects.get(u).map(|n| &n.body)
                                {
                                    edges.insert(*e);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let mut vertices = BTreeSet::new();
        for e in &edges {
            if let Some(Body::Edge(a, b, _)) = self.objects.get(e).map(|n| &n.body) {
                vertices.insert(*a);
                vertices.insert(*b);
            }
        }
        let mut vi = BTreeMap::new();
        for id in vertices {
            let n = self.objects.get(&id).ok_or("missing vertex")?;
            let Body::Vertex(p) = n.body else {
                return Err("reference is not a vertex".into());
            };
            vi.insert(id, run.mesh.vertices.len() as u32);
            run.mesh.vertices.push(p.map(|v| v * 0.0254));
            run.mesh.vertex_pids.push(n.pid);
            run.mesh.vertices_in.push(p);
        }
        let mut curves = BTreeMap::new();
        for id in edges {
            let n = self.objects.get(&id).ok_or("missing edge")?;
            let Body::Edge(a, b, c) = n.body else {
                return Err("reference is not an edge".into());
            };
            let curve = if c == 0 {
                None
            } else if let Some(i) = curves.get(&c) {
                Some(*i)
            } else {
                let source = self.objects.get(&c).ok_or("missing curve")?;
                let Body::Curve(arc) = &source.body else {
                    return Err("curve reference has wrong type".into());
                };
                let i = run.curves.len() as u32;
                run.curves.push(openskp::Curve {
                    pid: source.pid,
                    members: 0,
                    arc: arc.clone(),
                });
                self.attrs(model, source, openskp::Owner::Curve(ri, i as usize));
                curves.insert(c, i);
                Some(i)
            };
            if let Some(c) = curve {
                run.curves[c as usize].members += 1;
            }
            let ei = run.mesh.edges.len();
            run.mesh.edges.push(MeshEdge {
                pid: n.pid,
                v0: *vi.get(&a).ok_or("missing edge vertex")?,
                v1: *vi.get(&b).ok_or("missing edge vertex")?,
                soft: n.draw.soft,
                smooth: n.draw.smooth,
                hidden: n.draw.hidden,
                layer: n.draw.layer,
                material: (n.draw.material != 0).then_some(n.draw.material),
                curve,
                ..Default::default()
            });
            self.attrs(model, n, openskp::Owner::Edge(ri, ei));
        }
        for id in faces {
            let n = self.objects.get(&id).ok_or("missing face")?;
            let Body::Face(plane, loops, back) = &n.body else {
                return Err("invalid face".into());
            };
            let mut rings = Vec::new();
            for l in loops {
                let Some(Body::Loop(uses)) = self.objects.get(l).map(|n| &n.body) else {
                    return Err("missing face loop".into());
                };
                let mut ring = Vec::new();
                for u in uses {
                    let Some(Body::Use(e, reverse)) = self.objects.get(u).map(|n| &n.body) else {
                        return Err("missing edge use".into());
                    };
                    let Some(Body::Edge(a, b, _)) = self.objects.get(e).map(|n| &n.body) else {
                        return Err("missing loop edge".into());
                    };
                    ring.push(
                        *vi.get(if *reverse { b } else { a })
                            .ok_or("missing loop vertex")?,
                    );
                }
                rings.push(ring);
            }
            if rings.is_empty() {
                return Err("face has no boundary".into());
            }
            let texture = match self.objects.get(&n.attrs).map(|n| &n.body) {
                Some(Body::Attrs(items)) => {
                    items
                        .iter()
                        .find_map(|i| match self.objects.get(i).map(|n| &n.body) {
                            Some(Body::Texture(t)) => Some(t.clone()),
                            _ => None,
                        })
                }
                _ => None,
            };
            let fi = run.mesh.faces.len();
            run.mesh.faces.push(MeshFace {
                pid: n.pid,
                outer: rings.remove(0),
                holes: rings,
                normal: plane[..3].try_into()?,
                front_material: (n.draw.material != 0).then_some(n.draw.material),
                back_material: (*back != 0).then_some(*back),
                hidden: n.draw.hidden,
                layer: n.draw.layer,
                texture,
                ..Default::default()
            });
            self.attrs(model, n, openskp::Owner::Face(ri, fi));
        }
        for id in items {
            let Some(n) = self.objects.get(id) else {
                continue;
            };
            match &n.body {
                Body::Instance {
                    definition,
                    transform,
                    name,
                    image,
                } => {
                    if *image {
                        self.attrs(model, n, openskp::Owner::Image(ri, run.images.len()));
                        run.images.push(openskp::ImagePlacement {
                            pid: n.pid,
                            defref: *definition as u32,
                            transform: *transform,
                            hidden: n.draw.hidden,
                            layer: n.draw.layer,
                            casts_shadows: false,
                            receives_shadows: true,
                        });
                    } else {
                        model.instances.push(openskp::Instance {
                            pid: n.pid,
                            definition: self.objects.get(definition).and_then(|n| {
                                if let Body::Definition { name, .. } = &n.body {
                                    Some(name.clone())
                                } else {
                                    None
                                }
                            }),
                            defref: *definition as u32,
                            offset: usize::MAX,
                            translation_m: [
                                transform[9] * 0.0254,
                                transform[10] * 0.0254,
                                transform[11] * 0.0254,
                            ],
                            transform: *transform,
                            material: Some(n.draw.material),
                            name: Some(name.clone()),
                            is_group: Some(self.objects.get(definition).is_some_and(|n| {
                                matches!(n.body, Body::Definition { group: true, .. })
                            })),
                            hidden: Some(n.draw.hidden),
                            layer: Some(n.draw.layer),
                            slot: None,
                            guid: String::new(),
                            casts_shadows: Some(true),
                            receives_shadows: Some(true),
                            locked: Some(false),
                        });
                        self.attrs(model, n, openskp::Owner::Instance(ri, run.placed.len()));
                        run.placed.push(openskp::PlacedInstance {
                            pid: n.pid,
                            defref: *definition as u32,
                            transform: *transform,
                            material: n.draw.material,
                            layer: n.draw.layer,
                            hidden: n.draw.hidden,
                            is_group: self.objects.get(definition).is_some_and(|n| {
                                matches!(n.body, Body::Definition { group: true, .. })
                            }),
                            ..Default::default()
                        });
                    }
                }
                Body::Text(source, font) => {
                    let mut text = *source.clone();
                    text.pid = n.pid;
                    text.def_index = definition;
                    text.font = self.font_index(model, *font)?;
                    text.hidden = n.draw.hidden;
                    text.layer = n.draw.layer;
                    text.material = (n.draw.material != 0).then_some(n.draw.material);
                    self.resolve_anchor(&mut text.anchor)?;
                    self.attrs(model, n, openskp::Owner::Text(model.texts.len()));
                    model.texts.push(text);
                    run.topology.texts += 1;
                }
                Body::Dimension(source, font) => {
                    let mut dim = *source.clone();
                    dim.pid = n.pid;
                    dim.def_index = definition;
                    dim.font = self.font_index(model, *font)?;
                    self.resolve_anchor(&mut dim.start)?;
                    self.resolve_anchor(&mut dim.end)?;
                    self.attrs(model, n, openskp::Owner::Dimension(model.dimensions.len()));
                    model.dimensions.push(dim);
                    run.topology.dimensions += 1;
                }
                Body::Radial(source, font) => {
                    let mut dim = *source.clone();
                    dim.pid = n.pid;
                    dim.def_index = definition;
                    dim.font = self.font_index(model, *font)?;
                    dim.hidden = n.draw.hidden;
                    dim.layer = n.draw.layer;
                    dim.material = (n.draw.material != 0).then_some(n.draw.material);
                    dim.edge = dim
                        .edge
                        .map(|slot| {
                            self.objects
                                .get(&(slot as usize))
                                .map(|n| n.pid)
                                .ok_or("radial dimension edge missing")
                        })
                        .transpose()?;
                    self.attrs(
                        model,
                        n,
                        openskp::Owner::RadialDimension(model.radial_dimensions.len()),
                    );
                    model.radial_dimensions.push(dim);
                    run.topology.radial_dimensions += 1;
                }
                Body::Guide(v) => {
                    self.attrs(model, n, openskp::Owner::Guide(model.guides.len()));
                    model.guides.push(openskp::Guide {
                        def_index: definition,
                        pid: n.pid,
                        point_m: [v[0] * 0.0254, v[1] * 0.0254, v[2] * 0.0254],
                        direction: [v[3], v[4], v[5]],
                        bounds: [v[6], v[7]],
                        layer: n.draw.layer,
                        material: (n.draw.material != 0).then_some(n.draw.material),
                        hidden: n.draw.hidden,
                        casts_shadows: true,
                        receives_shadows: true,
                    });
                }
                Body::Polyline(points) => {
                    self.attrs(model, n, openskp::Owner::Polyline(ri, run.polylines.len()));
                    run.polylines.push(openskp::Polyline {
                        pid: n.pid,
                        points_m: points.clone(),
                        layer: n.draw.layer,
                        material: (n.draw.material != 0).then_some(n.draw.material),
                        hidden: n.draw.hidden,
                        ..Default::default()
                    });
                }
                Body::Point(p) => {
                    self.attrs(
                        model,
                        n,
                        openskp::Owner::GuidePoint(ri, run.guide_points.len()),
                    );
                    run.guide_points.push(openskp::GuidePoint {
                        pid: n.pid,
                        point_m: p.map(|v| v * 0.0254),
                        hidden: n.draw.hidden,
                        layer: n.draw.layer,
                        ..Default::default()
                    });
                }
                Body::Section(plane, name, symbol) => {
                    self.attrs(
                        model,
                        n,
                        openskp::Owner::SectionPlane(ri, run.sections.len()),
                    );
                    run.sections.push(openskp::SectionPlane {
                        pid: n.pid,
                        plane: *plane,
                        name: name.clone(),
                        symbol: symbol.clone(),
                        layer: n.draw.layer,
                        hidden: n.draw.hidden,
                        material: None,
                        casts_shadows: true,
                        receives_shadows: true,
                    });
                }
                _ => {}
            }
        }
        run.topology.vertices = run.mesh.vertices.len();
        run.topology.edges = run.mesh.edges.len();
        run.topology.faces = run.mesh.faces.len();
        run.topology.curves = run.curves.len();
        run.topology.polylines = run.polylines.len();
        run.topology.images_placed = run.images.len();
        Ok(run)
    }
}
fn class_at(data: &[u8], name: &str) -> Option<usize> {
    data.windows(name.len() + 6).position(|w| {
        w[..2] == [255, 255]
            && usize::from(u16::from_le_bytes([w[4], w[5]])) == name.len()
            && &w[6..] == name.as_bytes()
    })
}
/// Tries a structurally calibrated material/layer prefix, never a patched header.
pub(super) fn read(data: &[u8], mut model: Model) -> Result<Model> {
    let (text, _) = openskp::header_info(data).ok_or("missing classic version")?;
    let version = text
        .trim_start_matches('{')
        .split('.')
        .next()
        .ok_or("invalid version")?
        .parse::<u16>()?;
    if !(17..=20).contains(&version) {
        return Err("classic fallback requires release 17–20".into());
    }
    let (start, materials) = if let Some(p) = class_at(data, "CMaterial") {
        let b = data
            .get(p.saturating_sub(4)..p)
            .ok_or("missing material count")?;
        (p, u32::from_le_bytes(b.try_into()?) as usize)
    } else {
        (
            class_at(data, "CLayer")
                .ok_or("missing layer list")?
                .checked_sub(9)
                .ok_or("missing layer prefix")?,
            0,
        )
    };
    let boot_base = 1 << 20;
    let mut boot = Reader::new(data, start, boot_base, version);
    let bases = if materials >= 2 {
        boot.pointer(Some("CMaterial"))?;
        vec![(boot.tag()? & 32767) as usize]
    } else {
        let (_, layers, anchor) = boot.prefix(materials)?;
        layers
            .iter()
            .filter_map(|l| anchor.checked_sub(l - boot_base))
            .filter(|b| *b > 0 && *b < boot_base)
            .collect::<Vec<_>>()
    };
    let mut error = String::from("no valid classic object-map base");
    let mut farthest = 0;
    for base in bases {
        let mut reader = Reader::new(data, start, base, version);
        let attempt = (|| -> Result<Model> {
            let (mats, layers, _) = reader.prefix(materials)?;
            let mut defs = reader.count()?;
            let definition_next = reader.next_class_is("CComponentDefinition");
            let root_count = if defs > 0 && !definition_next {
                let n = defs;
                defs = 0;
                Some(n)
            } else {
                None
            };
            for _ in 0..defs {
                reader.pointer(Some("CComponentDefinition"))?;
            }
            // The declared count can be followed by additional definition
            // records. They share the archive's object map and must be read
            // before interpreting the following root entity count.
            while reader.next_class_is("CComponentDefinition") {
                reader.pointer(Some("CComponentDefinition"))?;
            }
            let roots = if let Some(count) = root_count {
                reader.items(count)?
            } else {
                reader.list()?
            };
            model.geometry.clear();
            model.definitions.clear();
            model.definition_links.clear();
            model.layers.clear();
            model.layer_links.clear();
            model.materials.clear();
            model.material_links.clear();
            model.instances.clear();
            // Global dictionaries were decoded before the geometry manager.
            // Only geometry-owned indices need rebuilding for this walk.
            model
                .dictionaries
                .retain(|a| matches!(a.owner, openskp::Owner::Model));
            model.guides.clear();
            model.texts.clear();
            model.dimensions.clear();
            model.radial_dimensions.clear();
            for (i, id) in mats.iter().enumerate() {
                let node = reader.objects.get(id).ok_or("missing material")?;
                let Body::Material(m) = &node.body else {
                    return Err("material expected".into());
                };
                model.materials.push(m.clone());
                model.material_links.push((u16::try_from(*id)?, i));
                reader.attrs(&mut model, node, openskp::Owner::Material(i));
            }
            for (i, id) in layers.iter().enumerate() {
                let Some(Body::Layer(l)) = reader.objects.get(id).map(|n| &n.body) else {
                    return Err("layer expected".into());
                };
                model.layers.push(l.clone());
                model.layer_links.push((u16::try_from(*id)?, i));
                if let Some(node) = reader.objects.get(id) {
                    reader.attrs(&mut model, node, openskp::Owner::Layer(i));
                }
            }
            let image_definitions = reader
                .objects
                .values()
                .filter_map(|node| match node.body {
                    Body::Instance {
                        definition,
                        image: true,
                        ..
                    } => Some(definition),
                    _ => None,
                })
                .collect::<BTreeSet<_>>();
            for (id, node) in &reader.objects {
                if let Body::Definition {
                    items: _,
                    name,
                    guid,
                    group,
                } = &node.body
                {
                    let i = model.definitions.len();
                    model.definitions.push(openskp::Definition {
                        name: name.clone(),
                        guid: guid.clone(),
                        map_index: Some(*id),
                        kind: if image_definitions.contains(id) {
                            openskp::DefinitionKind::Image
                        } else if *group {
                            openskp::DefinitionKind::Group
                        } else {
                            openskp::DefinitionKind::Component
                        },
                        ..Default::default()
                    });
                    model.definition_links.push((u32::try_from(*id)?, i));
                    reader.attrs(&mut model, node, openskp::Owner::Definition(i));
                }
            }
            let defs = reader
                .objects
                .iter()
                .filter_map(|(i, n)| {
                    if let Body::Definition { items, .. } = &n.body {
                        Some((*i, items))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            for (id, items) in defs {
                let run = reader.run(&mut model, items, Some(id))?;
                model.geometry.push(run);
            }
            let run = reader.run(&mut model, &roots, None)?;
            model.geometry.push(run);
            model.diagnostics = vec![openskp::Diagnostic::ContinuousWalk { base }];
            Ok(model.clone())
        })();
        match attempt {
            Ok(m) => return Ok(m),
            Err(e) => {
                if reader.pos >= farthest {
                    farthest = reader.pos;
                    error = e.to_string();
                }
            }
        }
    }
    Err(format!("classic {text}: {error}").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_lookahead_handles_new_short_and_extended_class_tags() {
        let mut declaration = vec![255, 255, 0, 0, 20, 0];
        declaration.extend_from_slice(b"CComponentDefinition");
        for (bytes, slot, expected) in [
            (declaration, 1, true),
            (vec![0x23, 0x81], 0x123, true),
            (vec![255, 127, 0x23, 0x81, 0, 0x80], 0x8123, true),
            (vec![255, 127, 0x23, 0x81, 0, 0], 0x8123, false),
            (vec![0x23, 0x01], 0x123, false),
            (vec![255, 127, 0x23], 0x8123, false),
            (vec![255], 1, false),
        ] {
            let mut reader = Reader::new(&bytes, 0, 1, 19);
            reader.classes.insert(slot, "CComponentDefinition".into());
            assert_eq!(reader.next_class_is("CComponentDefinition"), expected);
            assert!(!reader.next_class_is("CLayer"));
            assert_eq!(reader.pos, 0);
        }
    }
}
