//! Implements docs/09-file-formats.md §5 USD import (milestone 1.43): text
//! layers (`.usda`) and USDZ packages holding them.
//!
//! A small reader of the text syntax takes the stage's metadata
//! (`upAxis`, `metersPerUnit`, `defaultPrim`), prims (`def`, `over`,
//! `class`) with their attributes, relationships, references and
//! children. Meshes (`points`, `faceVertexCounts`, `faceVertexIndices`,
//! `orientation`, `primvars:st`, `primvars:displayColor`) are placed by
//! their transforms (`xformOp:*` in `xformOpOrder`); preview-surface
//! materials bound by `material:binding` give colors, opacity, roughness,
//! metalness and textures. A prim referencing another (in the same layer,
//! or another text layer beside it or in the package) places the same
//! meshes again, so they come back as components. Binary layers are
//! refused with a clear message; invisible prims and `class` prims are not
//! drawn.

use std::collections::BTreeMap;
use std::io::Read;

use glam::{DAffine3, DMat3, DMat4, DVec2, DVec3, DVec4};

use crate::commands::Transaction;
use crate::commands::params::Target;
use crate::diagnostics::{Warning, WarningCode};
use crate::document::Rgba8;
use crate::io::import::build::{
    ImportedMaterial, ImportedMesh, ImportedNode, ImportedScene, build_scene,
};
use crate::io::import::{
    Confidence, ImportAs, ImportFormat, ImportOptions, ImportReport, Importer,
};
use crate::io::{IoError, Progress, Storage, StorageKey};
use crate::units::LengthUnit;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "usd".into(),
        reason: reason.into(),
    }
}

// ----- the text syntax --------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Num(f64),
    Path(String),
    Asset(String),
    Punct(char),
}

fn lex(text: &str) -> Result<Vec<Tok>, IoError> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '"' || c == '\'' {
            // Triple-quoted strings too.
            let triple = i + 2 < chars.len() && chars[i + 1] == c && chars[i + 2] == c;
            let mut s = String::new();
            i += if triple { 3 } else { 1 };
            loop {
                if i >= chars.len() {
                    return Err(corrupt("a string does not end"));
                }
                if triple {
                    if i + 2 < chars.len()
                        && chars[i] == c
                        && chars[i + 1] == c
                        && chars[i + 2] == c
                    {
                        i += 3;
                        break;
                    }
                } else if chars[i] == c {
                    i += 1;
                    break;
                }
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                    s.push(match chars[i] {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                } else {
                    s.push(chars[i]);
                }
                i += 1;
            }
            out.push(Tok::Str(s));
        } else if c == '<' {
            let start = i + 1;
            while i < chars.len() && chars[i] != '>' {
                i += 1;
            }
            out.push(Tok::Path(chars[start..i.min(chars.len())].iter().collect()));
            i += 1;
        } else if c == '@' {
            let triple = i + 2 < chars.len() && chars[i + 1] == '@' && chars[i + 2] == '@';
            let skip = if triple { 3 } else { 1 };
            let start = i + skip;
            i = start;
            while i < chars.len() && chars[i] != '@' {
                i += 1;
            }
            out.push(Tok::Asset(
                chars[start..i.min(chars.len())].iter().collect(),
            ));
            i += skip;
        } else if c.is_ascii_digit()
            || ((c == '-' || c == '+' || c == '.')
                && chars
                    .get(i + 1)
                    .is_some_and(|d| d.is_ascii_digit() || *d == '.'))
        {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '.' | '-' | '+'))
            {
                // A sign only right after an exponent.
                if matches!(chars[i], '-' | '+') && !matches!(chars[i - 1], 'e' | 'E') {
                    break;
                }
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            out.push(Tok::Num(
                s.parse()
                    .map_err(|_| corrupt(format!("`{s}` is not a number")))?,
            ));
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | ':' | '.' | '[' | ']'))
            {
                // `float3[]` keeps its brackets; `[` elsewhere is punctuation.
                if chars[i] == '[' && chars.get(i + 1) != Some(&']') {
                    break;
                }
                if chars[i] == ']' && chars[i - 1] != '[' {
                    break;
                }
                i += 1;
            }
            out.push(Tok::Word(chars[start..i].iter().collect()));
        } else {
            out.push(Tok::Punct(c));
            i += 1;
        }
    }
    Ok(out)
}

/// A value of the text syntax.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Num(f64),
    Str(String),
    Word(String),
    Path(String),
    Asset(String),
    List(Vec<Val>),
    Tuple(Vec<Val>),
    Dict(Vec<(String, Val)>),
    None,
}

impl Val {
    fn num(&self) -> Option<f64> {
        match self {
            Val::Num(n) => Some(*n),
            _ => None,
        }
    }

    fn text(&self) -> Option<&str> {
        match self {
            Val::Str(s) | Val::Word(s) => Some(s),
            _ => None,
        }
    }

    fn items(&self) -> &[Val] {
        match self {
            Val::List(v) | Val::Tuple(v) => v,
            _ => &[],
        }
    }

    fn vec3(&self) -> Option<DVec3> {
        let v = self.items();
        Some(DVec3::new(
            v.first()?.num()?,
            v.get(1)?.num()?,
            v.get(2)?.num()?,
        ))
    }

    fn vec2(&self) -> Option<DVec2> {
        let v = self.items();
        Some(DVec2::new(v.first()?.num()?, v.get(1)?.num()?))
    }
}

/// A prim of a layer.
#[derive(Debug, Clone, Default)]
struct Prim {
    specifier: String,
    kind: String,
    name: String,
    /// Attribute values by name (`xformOp:translate`, `points`…).
    attrs: BTreeMap<String, Val>,
    /// Attribute connections (`inputs:diffuseColor.connect`).
    connections: BTreeMap<String, String>,
    /// Relationship targets.
    rels: BTreeMap<String, Vec<String>>,
    /// Referenced prims: an optional layer and a prim path.
    references: Vec<(Option<String>, Option<String>)>,
    /// Metadata of interest (`instanceable`, `kind`…).
    meta: BTreeMap<String, Val>,
    children: Vec<Prim>,
}

/// A parsed layer.
#[derive(Debug, Clone, Default)]
struct Layer {
    meta: BTreeMap<String, Val>,
    prims: Vec<Prim>,
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
}

const SPECIFIERS: [&str; 3] = ["def", "over", "class"];

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.at).cloned();
        self.at += 1;
        t
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Punct(c)) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), IoError> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(corrupt(format!("expected `{c}` near token {}", self.at)))
        }
    }

    fn value(&mut self) -> Result<Val, IoError> {
        Ok(match self.next() {
            Some(Tok::Num(n)) => Val::Num(n),
            Some(Tok::Str(s)) => Val::Str(s),
            Some(Tok::Path(p)) => Val::Path(p),
            Some(Tok::Asset(a)) => Val::Asset(a),
            Some(Tok::Word(w)) if w == "None" => Val::None,
            Some(Tok::Word(w)) => Val::Word(w),
            Some(Tok::Punct('[')) => {
                let mut items = Vec::new();
                while !self.eat(']') {
                    items.push(self.value()?);
                    self.eat(',');
                }
                Val::List(items)
            }
            Some(Tok::Punct('(')) => {
                let mut items = Vec::new();
                while !self.eat(')') {
                    items.push(self.value()?);
                    self.eat(',');
                }
                Val::Tuple(items)
            }
            Some(Tok::Punct('{')) => {
                // A dictionary: `type key = value` entries.
                let mut items = Vec::new();
                while !self.eat('}') {
                    let mut words = Vec::new();
                    while let Some(Tok::Word(w)) | Some(Tok::Str(w)) = self.peek().cloned() {
                        words.push(w);
                        self.at += 1;
                    }
                    if self.eat('=') {
                        let v = self.value()?;
                        items.push((words.last().cloned().unwrap_or_default(), v));
                    } else if self.next().is_none() {
                        return Err(corrupt("a dictionary does not end"));
                    }
                    self.eat(';');
                }
                Val::Dict(items)
            }
            other => return Err(corrupt(format!("unexpected {other:?}"))),
        })
    }

    /// `( key = value … )` after a prim's name or an attribute.
    fn metadata(&mut self) -> Result<BTreeMap<String, Val>, IoError> {
        let mut out = BTreeMap::new();
        if !self.eat('(') {
            return Ok(out);
        }
        while !self.eat(')') {
            match self.next() {
                Some(Tok::Str(_)) => {} // a documentation string
                Some(Tok::Word(w)) => {
                    let (key, list_op) = if matches!(
                        w.as_str(),
                        "prepend" | "append" | "add" | "delete" | "reorder"
                    ) {
                        match self.next() {
                            Some(Tok::Word(k)) => (k, true),
                            other => return Err(corrupt(format!("metadata: {other:?}"))),
                        }
                    } else {
                        (w, false)
                    };
                    let _ = list_op;
                    if self.eat('=') {
                        let v = self.value()?;
                        out.insert(key, v);
                    }
                }
                Some(Tok::Punct(';' | ',')) => {}
                None => return Err(corrupt("metadata does not end")),
                Some(_) => {}
            }
        }
        Ok(out)
    }

    fn prim(&mut self, specifier: String) -> Result<Prim, IoError> {
        let mut prim = Prim {
            specifier,
            ..Default::default()
        };
        if let Some(Tok::Word(kind)) = self.peek().cloned() {
            prim.kind = kind;
            self.at += 1;
        }
        match self.next() {
            Some(Tok::Str(name)) => prim.name = name,
            other => return Err(corrupt(format!("a prim needs a name, not {other:?}"))),
        }
        prim.meta = self.metadata()?;
        for key in ["references", "payload", "inherits"] {
            if let Some(v) = prim.meta.get(key) {
                let list = match v {
                    Val::List(items) => items.clone(),
                    other => vec![other.clone()],
                };
                for item in list {
                    let r = match item {
                        Val::Path(p) => (None, Some(p)),
                        Val::Asset(a) => (Some(a), None),
                        _ => continue,
                    };
                    prim.references.push(r);
                }
            }
        }
        // `@file@</Prim>` reads as an asset then a path: join them.
        self.expect('{')?;
        loop {
            match self.peek().cloned() {
                Some(Tok::Punct('}')) => {
                    self.at += 1;
                    break;
                }
                Some(Tok::Word(w)) if SPECIFIERS.contains(&w.as_str()) => {
                    self.at += 1;
                    let child = self.prim(w)?;
                    prim.children.push(child);
                }
                Some(Tok::Word(w)) if w == "variantSet" => {
                    // Variant sets are skipped whole.
                    self.at += 1;
                    self.next();
                    self.eat('=');
                    self.skip_block()?;
                }
                Some(Tok::Word(_)) => self.property(&mut prim)?,
                Some(Tok::Punct(';')) => self.at += 1,
                None => return Err(corrupt("a prim does not end")),
                Some(other) => return Err(corrupt(format!("unexpected {other:?} in a prim"))),
            }
        }
        Ok(prim)
    }

    fn skip_block(&mut self) -> Result<(), IoError> {
        self.expect('{')?;
        let mut depth = 1;
        while depth > 0 {
            match self.next() {
                Some(Tok::Punct('{')) => depth += 1,
                Some(Tok::Punct('}')) => depth -= 1,
                None => return Err(corrupt("a block does not end")),
                _ => {}
            }
        }
        Ok(())
    }

    /// An attribute or relationship.
    fn property(&mut self, prim: &mut Prim) -> Result<(), IoError> {
        let mut words = Vec::new();
        while let Some(Tok::Word(w)) = self.peek().cloned() {
            words.push(w);
            self.at += 1;
        }
        let name = words.last().cloned().unwrap_or_default();
        let is_rel = words.iter().any(|w| w == "rel");
        if self.eat('=') {
            let v = self.value()?;
            if is_rel {
                let targets = match v {
                    Val::Path(p) => vec![p],
                    Val::List(items) => items
                        .into_iter()
                        .filter_map(|i| match i {
                            Val::Path(p) => Some(p),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                prim.rels.insert(name, targets);
            } else if let Some(base) = name.strip_suffix(".connect") {
                if let Val::Path(p) = v {
                    prim.connections.insert(base.to_string(), p);
                }
            } else if let Some(base) = name.strip_suffix(".timeSamples") {
                // The first sample stands for the attribute.
                if let Val::Dict(samples) = v
                    && let Some((_, first)) = samples.into_iter().next()
                {
                    prim.attrs.insert(base.to_string(), first);
                }
            } else {
                prim.attrs.insert(name, v);
            }
        } else if self.peek() == Some(&Tok::Punct('{')) {
            // `float3 xformOp:translate.timeSamples = { … }` without `=`
            // forms and other blocks are skipped.
            self.skip_block()?;
        }
        let _ = self.metadata()?;
        Ok(())
    }

    fn layer(&mut self) -> Result<Layer, IoError> {
        let mut layer = Layer {
            meta: self.metadata()?,
            ..Default::default()
        };
        while let Some(t) = self.next() {
            match t {
                Tok::Word(w) if SPECIFIERS.contains(&w.as_str()) => {
                    let p = self.prim(w)?;
                    layer.prims.push(p);
                }
                Tok::Punct(';') => {}
                other => return Err(corrupt(format!("unexpected {other:?} at the top"))),
            }
        }
        Ok(layer)
    }
}

fn parse(text: &str) -> Result<Layer, IoError> {
    let first = text.lines().next().unwrap_or_default();
    if !first.trim_start().starts_with("#usda") {
        return Err(IoError::UnsupportedFormat(
            "not a text USD layer (it must start with #usda)".into(),
        ));
    }
    Parser {
        toks: lex(text)?,
        at: 0,
    }
    .layer()
}

// ----- composition ------------------------------------------------------------------------

/// The prim at `path` (`/A/B`) of a layer.
fn find<'l>(layer: &'l Layer, path: &str) -> Option<&'l Prim> {
    let mut parts = path
        .trim_start_matches('/')
        .split('/')
        .filter(|p| !p.is_empty());
    let first = parts.next()?;
    let mut prim = layer.prims.iter().find(|p| p.name == first)?;
    for part in parts {
        prim = prim.children.iter().find(|p| p.name == part)?;
    }
    Some(prim)
}

/// Degrees to a rotation about one axis.
fn rotation(axis: usize, degrees: f64) -> DMat3 {
    let r = degrees.to_radians();
    match axis {
        0 => DMat3::from_rotation_x(r),
        1 => DMat3::from_rotation_y(r),
        _ => DMat3::from_rotation_z(r),
    }
}

/// A prim's local transform from its transform operations.
fn local(prim: &Prim) -> DAffine3 {
    let order: Vec<String> = match prim.attrs.get("xformOpOrder") {
        Some(Val::List(items)) => items
            .iter()
            .filter_map(|v| v.text().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let mut t = DAffine3::IDENTITY;
    for op in order {
        let (op, invert) = match op.strip_prefix("!invert!") {
            Some(o) => (o.to_string(), true),
            None => (op, false),
        };
        if op == "!resetXformStack!" {
            t = DAffine3::IDENTITY;
            continue;
        }
        let Some(v) = prim.attrs.get(&op) else {
            continue;
        };
        let kind = op
            .trim_start_matches("xformOp:")
            .split(':')
            .next()
            .unwrap_or_default();
        let m = match kind {
            "translate" => v.vec3().map(DAffine3::from_translation),
            "scale" => v
                .vec3()
                .or_else(|| v.num().map(DVec3::splat))
                .map(DAffine3::from_scale),
            "rotateX" => v.num().map(|a| DAffine3::from_mat3(rotation(0, a))),
            "rotateY" => v.num().map(|a| DAffine3::from_mat3(rotation(1, a))),
            "rotateZ" => v.num().map(|a| DAffine3::from_mat3(rotation(2, a))),
            k if k.starts_with("rotate") && k.len() == 9 => v.vec3().map(|a| {
                // `rotateXYZ`: about the first axis first.
                let order: Vec<usize> = k[6..]
                    .chars()
                    .map(|c| match c {
                        'X' => 0,
                        'Y' => 1,
                        _ => 2,
                    })
                    .collect();
                let mut m = DMat3::IDENTITY;
                for &axis in &order {
                    m = rotation(axis, a[axis]) * m;
                }
                DAffine3::from_mat3(m)
            }),
            "orient" => {
                let q = v.items();
                (q.len() == 4).then(|| {
                    let n = |i: usize| q[i].num().unwrap_or(0.0);
                    DAffine3::from_quat(glam::DQuat::from_xyzw(n(1), n(2), n(3), n(0)).normalize())
                })
            }
            "transform" => {
                // Row vectors: the matrix's rows are the images of the axes.
                let rows: Vec<DVec4> = v
                    .items()
                    .iter()
                    .filter_map(|r| {
                        let r = r.items();
                        Some(DVec4::new(
                            r.first()?.num()?,
                            r.get(1)?.num()?,
                            r.get(2)?.num()?,
                            r.get(3)?.num()?,
                        ))
                    })
                    .collect();
                (rows.len() == 4).then(|| {
                    let m = DMat4::from_cols(rows[0], rows[1], rows[2], rows[3]);
                    DAffine3::from_mat4(m)
                })
            }
            _ => None,
        };
        if let Some(m) = m {
            t *= if invert { m.inverse() } else { m };
        }
    }
    t
}

/// Linear RGB to an sRGB channel.
fn srgb(c: f64) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * libm::pow(c, 1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// Reads layers and textures: the package's entries or the storage
/// beside the source.
struct Files<'a> {
    package: BTreeMap<String, Vec<u8>>,
    storage: Option<&'a dyn Storage>,
    source: String,
}

impl Files<'_> {
    fn read(&self, name: &str) -> Option<Vec<u8>> {
        let name = name.trim_start_matches("./");
        if let Some(b) = self.package.get(name) {
            return Some(b.clone());
        }
        let key = crate::io::import::sibling(&self.source, name);
        self.storage?.read(&StorageKey::new(&key)).ok()
    }
}

/// What reading the stage gathers.
struct Gather<'a> {
    files: &'a Files<'a>,
    scene: ImportedScene,
    /// Meshes made from a (layer, prim path), shared by references.
    made: BTreeMap<(String, String), Vec<usize>>,
    materials: BTreeMap<(String, String), Option<usize>>,
    layers: BTreeMap<String, Layer>,
    warnings: Vec<String>,
}

impl Gather<'_> {
    fn layer(&mut self, name: &str) -> Option<Layer> {
        if let Some(l) = self.layers.get(name) {
            return Some(l.clone());
        }
        let bytes = self.files.read(name)?;
        let text = String::from_utf8(bytes).ok()?;
        match parse(&text) {
            Ok(l) => {
                self.layers.insert(name.to_string(), l.clone());
                Some(l)
            }
            Err(e) => {
                self.warnings.push(format!("layer {name}: {e}"));
                None
            }
        }
    }

    /// The material bound to `prim` (in `layer_name`).
    fn material(&mut self, layer_name: &str, layer: &Layer, prim: &Prim) -> Option<usize> {
        let path = prim.rels.get("material:binding")?.first()?.clone();
        let key = (layer_name.to_string(), path.clone());
        if let Some(m) = self.materials.get(&key) {
            return *m;
        }
        let made = find(layer, &path).map(|mat| {
            let mut m = ImportedMaterial {
                name: mat.name.clone(),
                color: Rgba8 {
                    r: 200,
                    g: 200,
                    b: 200,
                    a: 255,
                },
                opacity: 1.0,
                ..Default::default()
            };
            // The surface shader: the one the material's output connects to,
            // else any preview surface among its children.
            let surface = mat
                .connections
                .get("outputs:surface")
                .and_then(|p| find(layer, p.split('.').next().unwrap_or_default()))
                .or_else(|| {
                    mat.children.iter().find(|c| {
                        c.attrs.get("info:id").and_then(Val::text) == Some("UsdPreviewSurface")
                    })
                });
            if let Some(s) = surface {
                if let Some(c) = s.attrs.get("inputs:diffuseColor").and_then(Val::vec3) {
                    m.color = Rgba8 {
                        r: srgb(c.x),
                        g: srgb(c.y),
                        b: srgb(c.z),
                        a: 255,
                    };
                }
                if let Some(o) = s.attrs.get("inputs:opacity").and_then(Val::num) {
                    m.opacity = o.clamp(0.0, 1.0) as f32;
                }
                m.roughness = s
                    .attrs
                    .get("inputs:roughness")
                    .and_then(Val::num)
                    .map(|v| v as f32);
                m.metallic = s
                    .attrs
                    .get("inputs:metallic")
                    .and_then(Val::num)
                    .map(|v| v as f32);
                if let Some(tex) = s
                    .connections
                    .get("inputs:diffuseColor")
                    .and_then(|p| find(layer, p.split('.').next().unwrap_or_default()))
                    && let Some(Val::Asset(file)) = tex.attrs.get("inputs:file")
                {
                    m.texture = self.files.read(file);
                    if m.texture.is_none() {
                        self.warnings.push(format!("texture {file} was not found"));
                    }
                }
            }
            self.scene.materials.push(m);
            self.scene.materials.len() - 1
        });
        self.materials.insert(key, made);
        made
    }

    /// A mesh prim's triangles, or `None` without geometry.
    fn mesh(&mut self, layer_name: &str, layer: &Layer, prim: &Prim) -> Option<ImportedMesh> {
        let points: Vec<DVec3> = prim
            .attrs
            .get("points")?
            .items()
            .iter()
            .filter_map(Val::vec3)
            .collect();
        let counts: Vec<usize> = prim
            .attrs
            .get("faceVertexCounts")?
            .items()
            .iter()
            .filter_map(|v| v.num().map(|n| n as usize))
            .collect();
        let indices: Vec<usize> = prim
            .attrs
            .get("faceVertexIndices")?
            .items()
            .iter()
            .filter_map(|v| v.num().map(|n| n as usize))
            .collect();
        let left = prim.attrs.get("orientation").and_then(Val::text) == Some("leftHanded");
        // Texture coordinates, per point or per face corner.
        let st: Vec<DVec2> = prim
            .attrs
            .get("primvars:st")
            .or_else(|| prim.attrs.get("primvars:UVMap"))
            .map(|v| v.items().iter().filter_map(Val::vec2).collect())
            .unwrap_or_default();
        let st_indices: Vec<usize> = prim
            .attrs
            .get("primvars:st:indices")
            .map(|v| {
                v.items()
                    .iter()
                    .filter_map(|n| n.num().map(|n| n as usize))
                    .collect()
            })
            .unwrap_or_default();
        let per_corner = !st.is_empty()
            && (st.len() == indices.len() || st_indices.len() == indices.len())
            && st.len() != points.len();
        let mut material = self.material(layer_name, layer, prim);
        if material.is_none()
            && let Some(c) = prim
                .attrs
                .get("primvars:displayColor")
                .and_then(|v| v.items().first().and_then(Val::vec3).or_else(|| v.vec3()))
        {
            self.scene.materials.push(ImportedMaterial {
                name: format!("{} color", prim.name),
                color: Rgba8 {
                    r: srgb(c.x),
                    g: srgb(c.y),
                    b: srgb(c.z),
                    a: 255,
                },
                opacity: 1.0,
                ..Default::default()
            });
            material = Some(self.scene.materials.len() - 1);
        }
        let mut mesh = ImportedMesh {
            name: prim.name.clone(),
            ..Default::default()
        };
        let mut corner = 0;
        for &n in &counts {
            if corner + n > indices.len() {
                break;
            }
            let face: Vec<usize> = indices[corner..corner + n].to_vec();
            let corners: Vec<usize> = (corner..corner + n).collect();
            corner += n;
            if n < 3 || face.iter().any(|i| *i >= points.len()) {
                continue;
            }
            // Each corner a position of its own when coordinates are per
            // corner; positions weld again when the mesh is built.
            let ids: Vec<u32> = face
                .iter()
                .zip(&corners)
                .map(|(&p, &c)| {
                    mesh.positions.push(points[p]);
                    if !st.is_empty() {
                        let uv = if per_corner {
                            st.get(st_indices.get(c).copied().unwrap_or(c)).copied()
                        } else {
                            st.get(st_indices.get(p).copied().unwrap_or(p)).copied()
                        };
                        mesh.uvs.push(uv.unwrap_or_default());
                    }
                    (mesh.positions.len() - 1) as u32
                })
                .collect();
            for tri in triangulate(&face.iter().map(|i| points[*i]).collect::<Vec<_>>()) {
                let mut t = [ids[tri[0]], ids[tri[1]], ids[tri[2]]];
                if left {
                    t.swap(1, 2);
                }
                mesh.triangles.push(t);
                mesh.materials.push(material);
            }
        }
        (!mesh.triangles.is_empty()).then_some(mesh)
    }

    /// Walks a prim (and what it references), placing its meshes.
    fn walk(
        &mut self,
        layer_name: &str,
        layer: &Layer,
        prim: &Prim,
        path: &str,
        parent: DAffine3,
        depth: usize,
    ) {
        if depth > 64 || prim.specifier == "class" {
            return;
        }
        if prim.attrs.get("visibility").and_then(Val::text) == Some("invisible") {
            return;
        }
        let mut transform = local(prim);
        // A referenced prim brings its meshes and children; this prim's own
        // transform wins when it has one.
        for (asset, target) in prim.references.clone() {
            let (ref_layer_name, ref_layer) = match &asset {
                Some(a) => match self.layer(a) {
                    Some(l) => (a.clone(), l),
                    None => {
                        self.warnings.push(format!("reference {a} was not found"));
                        continue;
                    }
                },
                None => (layer_name.to_string(), layer.clone()),
            };
            let target = target.or_else(|| {
                ref_layer
                    .meta
                    .get("defaultPrim")
                    .and_then(Val::text)
                    .map(|n| format!("/{n}"))
            });
            let Some(target) = target else { continue };
            let Some(source) = find(&ref_layer, &target).cloned() else {
                self.warnings
                    .push(format!("reference {target} was not found"));
                continue;
            };
            if !prim.attrs.contains_key("xformOpOrder") {
                transform = local(&source);
            }
            let world = parent * transform;
            self.place(
                &ref_layer_name,
                &ref_layer,
                &source,
                &target,
                world,
                depth + 1,
            );
        }
        let world = parent * transform;
        self.place(layer_name, layer, prim, path, world, depth);
    }

    /// Places a prim's own mesh (once made, shared) and walks its children.
    fn place(
        &mut self,
        layer_name: &str,
        layer: &Layer,
        prim: &Prim,
        path: &str,
        world: DAffine3,
        depth: usize,
    ) {
        if prim.kind == "Mesh" {
            let key = (layer_name.to_string(), path.to_string());
            let meshes = match self.made.get(&key) {
                Some(m) => m.clone(),
                None => {
                    let made: Vec<usize> = self
                        .mesh(layer_name, layer, prim)
                        .map(|m| {
                            self.scene.meshes.push(m);
                            vec![self.scene.meshes.len() - 1]
                        })
                        .unwrap_or_default();
                    self.made.insert(key, made.clone());
                    made
                }
            };
            for mesh in meshes {
                self.scene.nodes.push(ImportedNode {
                    name: prim.name.clone(),
                    transform: world,
                    mesh,
                });
            }
        }
        for child in &prim.children {
            let child_path = format!("{path}/{}", child.name);
            // Materials and shaders are not drawn.
            if matches!(child.kind.as_str(), "Material" | "Shader") {
                continue;
            }
            self.walk(layer_name, layer, child, &child_path, world, depth + 1);
        }
    }
}

/// Triangles of a polygon (indices into `points`): ear clipping in its
/// best-fitting plane.
fn triangulate(points: &[DVec3]) -> Vec<[usize; 3]> {
    if points.len() == 3 {
        return vec![[0, 1, 2]];
    }
    let mut n = DVec3::ZERO;
    for (a, b) in points.iter().zip(points.iter().cycle().skip(1)) {
        n += a.cross(*b);
    }
    let n = n.normalize_or_zero();
    if n == DVec3::ZERO {
        return Vec::new();
    }
    let u = if n.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let u = (u - n * u.dot(n)).normalize();
    let v = n.cross(u);
    let data: Vec<f64> = points.iter().flat_map(|p| [p.dot(u), p.dot(v)]).collect();
    earcutr::earcut(&data, &[], 2)
        .map(|t| {
            t.chunks(3)
                .filter(|c| c.len() == 3)
                .map(|c| [c[0], c[1], c[2]])
                .collect()
        })
        .unwrap_or_default()
}

/// The meters per unit as one of the document's units, or a factor to
/// millimeters.
fn units(meters: f64) -> (Option<LengthUnit>, f64) {
    for u in [
        LengthUnit::Millimeter,
        LengthUnit::Centimeter,
        LengthUnit::Meter,
        LengthUnit::Inch,
        LengthUnit::Foot,
    ] {
        if (u.millimeters() / 1000.0 - meters).abs() <= 1e-9 * meters.abs().max(1e-12) {
            return (Some(u), 1.0);
        }
    }
    (Some(LengthUnit::Millimeter), meters * 1000.0)
}

/// Reads a text layer (and what it references) into a scene.
fn read_stage(
    text: &str,
    files: &Files<'_>,
    name: String,
) -> Result<(ImportedScene, Vec<String>), IoError> {
    let root = parse(text)?;
    let mut g = Gather {
        files,
        scene: ImportedScene {
            name,
            ..Default::default()
        },
        made: BTreeMap::new(),
        materials: BTreeMap::new(),
        layers: BTreeMap::new(),
        warnings: Vec::new(),
    };
    let up = root
        .meta
        .get("upAxis")
        .and_then(Val::text)
        .unwrap_or("Y")
        .to_string();
    g.scene.y_up = up != "Z";
    let meters = root
        .meta
        .get("metersPerUnit")
        .and_then(Val::num)
        .unwrap_or(0.01);
    let (unit, factor) = units(meters);
    g.scene.units = unit;
    let pre = DAffine3::from_scale(DVec3::splat(factor));
    for prim in &root.prims {
        let path = format!("/{}", prim.name);
        g.walk("", &root, prim, &path, pre, 0);
    }
    if g.scene.nodes.is_empty() {
        return Err(IoError::UnsupportedFormat(
            "the USD stage holds no visible meshes".into(),
        ));
    }
    Ok((g.scene, g.warnings))
}

/// The entries of a USDZ package, in order.
fn unpack(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, IoError> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| corrupt(format!("not a USDZ package: {e}")))?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| corrupt(e.to_string()))?;
        if f.is_dir() {
            continue;
        }
        let name = f.name().map_err(|e| corrupt(e.to_string()))?.to_string();
        let mut data = Vec::new();
        f.by_ref()
            .take(256 * 1024 * 1024)
            .read_to_end(&mut data)
            .map_err(|e| corrupt(e.to_string()))?;
        out.push((name, data));
    }
    Ok(out)
}

/// USD import (text layers and USDZ packages of them).
#[derive(Debug, Default, Clone, Copy)]
pub struct UsdImporter;

impl Importer for UsdImporter {
    fn format(&self) -> ImportFormat {
        ImportFormat::Usd
    }

    fn probe(&self, bytes: &[u8], name: &str) -> Confidence {
        let lower = name.to_ascii_lowercase();
        if bytes.starts_with(b"#usda") {
            Confidence::CERTAIN
        } else if lower.ends_with(".usda") || lower.ends_with(".usdz") || lower.ends_with(".usd") {
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
            return Err(IoError::LimitExceeded("USD input exceeds 512 MiB".into()));
        }
        if matches!(opts.import_as, ImportAs::Image | ImportAs::Texture) {
            return Err(IoError::UnsupportedFormat(
                "USD imports as a group or component".into(),
            ));
        }
        let binary = |b: &[u8]| b.starts_with(b"PXR-USDC");
        let storage = tx.storage();
        let (text, package) = if bytes.starts_with(b"PK") {
            let entries = unpack(&bytes)?;
            let Some((_, first)) = entries
                .iter()
                .find(|(n, _)| n.ends_with(".usda") || n.ends_with(".usd") || n.ends_with(".usdc"))
            else {
                return Err(corrupt("the package holds no USD layer"));
            };
            if binary(first) {
                return Err(IoError::UnsupportedFormat(
                    "the package's layer is binary USD (crate); export it as text (.usda)".into(),
                ));
            }
            let text = String::from_utf8(first.clone())
                .map_err(|_| corrupt("the layer is not UTF-8 text"))?;
            (text, entries.into_iter().collect::<BTreeMap<_, _>>())
        } else if binary(&bytes) {
            return Err(IoError::UnsupportedFormat(
                "binary USD (crate) is not supported; export the stage as text (.usda)".into(),
            ));
        } else {
            (
                String::from_utf8(bytes).map_err(|_| corrupt("the layer is not UTF-8 text"))?,
                BTreeMap::new(),
            )
        };
        progress.check_cancelled()?;
        let files = Files {
            package,
            storage,
            source: opts.source.clone(),
        };
        let name = crate::io::import::source_stem(&opts.source, "USD");
        let (scene, warnings) = read_stage(&text, &files, name)?;
        let mut report = build_scene(tx, target, scene, opts)?;
        for w in warnings {
            report.warnings.push(Warning::new(WarningCode::W2004, w));
        }
        Ok(report)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use crate::io::MemoryStorage;
    use serde_json::json;
    use std::sync::Arc;

    const CUBE: &str = r#"#usda 1.0
(
    defaultPrim = "World"
    metersPerUnit = 0.01
    upAxis = "Z"
)

def Xform "World"
{
    def Xform "Box" (
        kind = "component"
    )
    {
        double3 xformOp:translate = (10, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]

        def Mesh "Cube"
        {
            int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]
            int[] faceVertexIndices = [0, 3, 2, 1, 4, 5, 6, 7, 0, 1, 5, 4, 1, 2, 6, 5, 2, 3, 7, 6, 3, 0, 4, 7]
            point3f[] points = [(0, 0, 0), (10, 0, 0), (10, 10, 0), (0, 10, 0), (0, 0, 10), (10, 0, 10), (10, 10, 10), (0, 10, 10)]
            rel material:binding = </World/Looks/Red>
        }
    }

    def Xform "Copy" (
        prepend references = </World/Box>
    )
    {
        double3 xformOp:translate = (0, 50, 0)
        float3 xformOp:rotateXYZ = (0, 0, 90)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateXYZ"]
    }

    def Scope "Looks"
    {
        def Material "Red"
        {
            token outputs:surface.connect = </World/Looks/Red/Surface.outputs:surface>

            def Shader "Surface"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (1, 0, 0)
                float inputs:roughness = 0.5
                token outputs:surface
            }
        }
    }

    def Mesh "Hidden"
    {
        token visibility = "invisible"
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
    }
}
"#;

    fn engine() -> (Engine, Arc<MemoryStorage>) {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        let storage = Arc::new(MemoryStorage::new());
        e.set_storage(storage.clone());
        (e, storage)
    }

    /// Volumes and world bounds of every component placement.
    fn placements(e: &Engine) -> Vec<(f64, DVec3, DVec3)> {
        let doc = e.document();
        let mut out = Vec::new();
        for (_, def) in doc.definitions.iter() {
            for (_, inst) in def.entities.instances() {
                let mesh = doc.definition(inst.definition).unwrap().entities.mesh();
                let (mut lo, mut hi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
                for (_, v) in mesh.vertices() {
                    let p = inst.transform.transform_point3(v.position);
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
                out.push((mesh.volume().unwrap().abs(), lo, hi));
            }
        }
        out.sort_by(|a, b| a.1.y.total_cmp(&b.1.y));
        out
    }

    #[test]
    fn a_text_stage_with_a_reference_and_a_material() {
        let (mut e, storage) = engine();
        storage
            .write(&StorageKey::new("cube.usda"), CUBE.as_bytes())
            .unwrap();
        let out = e
            .execute_erased("import", json!({ "path": "cube.usda" }))
            .unwrap();
        assert_eq!(out["format"], "usd", "{out}");
        // One mesh, placed twice: a component, in centimeters (×10).
        let placed = placements(&e);
        assert_eq!(placed.len(), 2, "{placed:?}");
        for (v, _, _) in &placed {
            assert!((v - 1_000_000.0).abs() < 1e-3, "{v}");
        }
        let close = |a: DVec3, b: [f64; 3]| a.distance(DVec3::from_array(b)) < 1e-6;
        assert!(
            close(placed[0].1, [100.0, 0.0, 0.0]) && close(placed[0].2, [200.0, 100.0, 100.0]),
            "{placed:?}"
        );
        // The copy is turned a quarter about Z at (0, 500).
        assert!(
            close(placed[1].1, [-100.0, 500.0, 0.0]) && close(placed[1].2, [0.0, 600.0, 100.0]),
            "{placed:?}"
        );
        // Red, from the preview surface.
        let red = e
            .document()
            .materials
            .iter()
            .any(|(_, m)| m.name == "Red" && m.color.r == 255 && m.color.g == 0);
        assert!(red);
    }

    #[test]
    fn packages_textures_and_binary_layers() {
        // A package with its texture.
        let png = {
            let img = image::RgbImage::from_pixel(4, 4, image::Rgb([0, 128, 255]));
            let mut out = std::io::Cursor::new(Vec::new());
            img.write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        let layer = CUBE.replace(
            "float inputs:roughness = 0.5",
            "float inputs:roughness = 0.5\n                color3f inputs:diffuseColor.connect = </World/Looks/Red/Tex.outputs:rgb>",
        )
        .replace(
            "            def Shader \"Surface\"",
            "            def Shader \"Tex\"\n            {\n                uniform token info:id = \"UsdUVTexture\"\n                asset inputs:file = @tex.png@\n            }\n\n            def Shader \"Surface\"",
        );
        let package =
            crate::io::export::usd::usdz(&[("scene.usda", layer.as_bytes()), ("tex.png", &png)])
                .unwrap();
        let (mut e, storage) = engine();
        storage
            .write(&StorageKey::new("cube.usdz"), &package)
            .unwrap();
        e.execute_erased("import", json!({ "path": "cube.usdz" }))
            .unwrap();
        let textured = e
            .document()
            .materials
            .iter()
            .any(|(_, m)| m.name == "Red" && m.texture.is_some());
        assert!(textured);
        // A binary layer is refused with a reason.
        let (mut e, storage) = engine();
        storage
            .write(&StorageKey::new("b.usd"), b"PXR-USDC\0\0\0\0")
            .unwrap();
        let err = e
            .execute_erased("import", json!({ "path": "b.usd" }))
            .unwrap_err();
        assert!(err.to_string().contains("binary USD"), "{err}");
    }

    #[test]
    fn exported_stages_come_back() {
        let (mut e, storage) = engine();
        for (name, params) in [
            (
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [100, 60, 0] }),
            ),
            (
                "push-pull",
                json!({ "face": { "Query": "face" }, "distance": 40 }),
            ),
            ("make-group", json!({ "targets": "all", "name": "Block" })),
        ] {
            e.execute_erased(name, params).unwrap();
        }
        e.execute_erased("export", json!({ "path": "b.usda" }))
            .unwrap();
        let text = storage.read(&StorageKey::new("b.usda")).unwrap();
        let (mut back, storage) = engine();
        storage.write(&StorageKey::new("b.usda"), &text).unwrap();
        back.execute_erased("import", json!({ "path": "b.usda" }))
            .unwrap();
        let doc = back.document();
        let volume: f64 = doc
            .definitions
            .iter()
            .map(|(_, d)| d.entities.mesh().volume().unwrap_or(0.0).abs())
            .sum();
        assert!((volume - 240_000.0).abs() < 1e-3, "{volume}");
    }

    #[test]
    fn the_syntax_reads_what_it_should() {
        let toks = lex("float3[] a = [(1, -2.5e1, .5)] # note\nrel b = </X/Y>").unwrap();
        assert!(toks.contains(&Tok::Word("float3[]".into())));
        assert!(toks.contains(&Tok::Num(-25.0)));
        assert!(toks.contains(&Tok::Num(0.5)));
        assert!(toks.contains(&Tok::Path("/X/Y".into())));
        assert!(parse("PXR-USDC").is_err());
        let layer = parse(CUBE).unwrap();
        let cube = find(&layer, "/World/Box/Cube").unwrap();
        assert_eq!(cube.kind, "Mesh");
        assert_eq!(
            cube.rels["material:binding"],
            vec!["/World/Looks/Red".to_string()]
        );
        let copy = find(&layer, "/World/Copy").unwrap();
        assert_eq!(
            copy.references,
            vec![(None, Some("/World/Box".to_string()))]
        );
        let t = local(copy);
        assert!(
            t.transform_point3(DVec3::X)
                .distance(DVec3::new(0.0, 51.0, 0.0))
                < 1e-12
        );
    }
}
