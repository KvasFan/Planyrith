//! Implements docs/09-file-formats.md §5 STEP with product structure
//! (milestone 1.17): a writer and a reader of the exchange structure
//! (ISO 10303-21) for the automotive design schema (AP214), without the
//! B-rep bridge.
//!
//! Every container exported becomes a product (its name, its part number
//! as the product's id, its description, its properties as user-defined
//! attributes); the model is the top product. Each group or component
//! occurrence is a next-assembly-usage with its placement. Planar faces
//! bounded by straight edges are written exactly: closed shells as
//! manifold solids, others as surface models. A placement that is not a
//! rigid motion is applied to the geometry, flattening that branch.
//!
//! The reader takes the same structure back, with planar faces bounded by
//! lines and circular arcs (sampled); anything else is reported as
//! unsupported so the caller can fall back to a curved-surface reader.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use glam::{DAffine3, DMat3, DVec3};

use crate::document::{ContainerRef, Document, InstancePath};
use crate::geom::FaceSides;
use crate::geom::HalfEdgeMesh;
use crate::geom::boolean::{Piece, stitch};
use crate::io::IoError;
use crate::io::export::{ExportScope, scene};
use crate::io::import::step21::{Instance, Value, parse};
use crate::tolerance::Tolerances;

fn corrupt(reason: impl Into<String>) -> IoError {
    IoError::Corrupt {
        part: "step".into(),
        reason: reason.into(),
    }
}

// ----- writing --------------------------------------------------------------------------

/// Entity text with sequential ids.
struct Out {
    lines: Vec<String>,
}

impl Out {
    fn add(&mut self, entity: impl Into<String>) -> usize {
        self.lines.push(entity.into());
        self.lines.len()
    }
}

/// A STEP string literal.
fn q(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' => out.push_str("''"),
            '\\' => out.push_str("\\\\"),
            c if c.is_ascii() && !c.is_ascii_control() => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                out.push_str("\\X2\\");
                for u in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "{:04X}", u);
                }
                out.push_str("\\X0\\");
            }
        }
    }
    out.push('\'');
    out
}

/// A real in STEP form.
fn r(v: f64) -> String {
    let v = if v.abs() < 1e-12 { 0.0 } else { v };
    let s = format!("{v:.9}");
    // `{:.9}` always has a point, which STEP reals need.
    s.trim_end_matches('0').to_string()
}

fn point(o: &mut Out, p: DVec3) -> usize {
    o.add(format!(
        "CARTESIAN_POINT('',({},{},{}))",
        r(p.x),
        r(p.y),
        r(p.z)
    ))
}

fn direction(o: &mut Out, d: DVec3) -> usize {
    let d = d.normalize_or_zero();
    o.add(format!("DIRECTION('',({},{},{}))", r(d.x), r(d.y), r(d.z)))
}

fn placement(o: &mut Out, origin: DVec3, z: DVec3, x: DVec3) -> usize {
    let p = point(o, origin);
    let a = direction(o, z);
    let b = direction(o, x);
    o.add(format!("AXIS2_PLACEMENT_3D('',#{p},#{a},#{b})"))
}

fn rigid(t: &DAffine3) -> bool {
    let m = t.matrix3;
    let mtm = m.transpose() * m;
    (mtm - DMat3::IDENTITY)
        .to_cols_array()
        .iter()
        .all(|v| v.abs() < 1e-9)
        && m.determinant() > 0.0
}

/// The faces of `mesh` (each its loops, outer first) transformed by `t`,
/// split into connected shells, each with whether it is closed.
fn shells(mesh: &HalfEdgeMesh, t: &DAffine3) -> Vec<(Vec<Vec<Vec<DVec3>>>, bool)> {
    let faces: Vec<_> = mesh.faces().map(|(f, _)| f).collect();
    let index: BTreeMap<_, usize> = faces.iter().enumerate().map(|(i, f)| (*f, i)).collect();
    let mut seen = vec![false; faces.len()];
    let flip = t.matrix3.determinant() < 0.0;
    let mut out = Vec::new();
    for start in 0..faces.len() {
        if seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let mut members = Vec::new();
        let mut closed = true;
        while let Some(i) = stack.pop() {
            members.push(i);
            let Ok(face) = mesh.face_ref(faces[i]) else {
                continue;
            };
            let mut loops = vec![face.outer];
            loops.extend(face.holes.iter().copied());
            for l in loops {
                for h in mesh.loop_half_edges(l).unwrap_or_default() {
                    let Some(he) = mesh.half_edge(h) else {
                        continue;
                    };
                    match mesh.half_edge(he.twin).and_then(|tw| tw.face) {
                        Some(g) => {
                            if let Some(&j) = index.get(&g)
                                && !seen[j]
                            {
                                seen[j] = true;
                                stack.push(j);
                            }
                        }
                        None => closed = false,
                    }
                }
            }
        }
        members.sort_unstable();
        let rings = members
            .into_iter()
            .filter_map(|i| {
                let face = mesh.face_ref(faces[i]).ok()?;
                let mut loops = vec![face.outer];
                loops.extend(face.holes.iter().copied());
                Some(
                    loops
                        .into_iter()
                        .filter_map(|l| mesh.loop_positions(l).ok())
                        .map(|ring| {
                            let mut ring: Vec<DVec3> =
                                ring.into_iter().map(|p| t.transform_point3(p)).collect();
                            if flip {
                                ring.reverse();
                            }
                            ring
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        out.push((rings, closed));
    }
    out
}

/// Writes one shell's faces; returns the shell's id.
fn write_shell(o: &mut Out, faces: &[Vec<Vec<DVec3>>], closed: bool) -> usize {
    let mut vertices: BTreeMap<[i64; 3], usize> = BTreeMap::new();
    let mut edges: BTreeMap<([i64; 3], [i64; 3]), usize> = BTreeMap::new();
    let key = |p: DVec3| {
        [
            (p.x * 1e6).round() as i64,
            (p.y * 1e6).round() as i64,
            (p.z * 1e6).round() as i64,
        ]
    };
    let mut face_ids = Vec::new();
    for rings in faces {
        let Some(outer) = rings.first() else { continue };
        if outer.len() < 3 {
            continue;
        }
        let mut n = DVec3::ZERO;
        for (a, b) in outer.iter().zip(outer.iter().cycle().skip(1)) {
            n += a.cross(*b);
        }
        let n = n.normalize_or_zero();
        let x = (outer[1] - outer[0]).normalize_or_zero();
        let x = (x - n * x.dot(n)).normalize_or_zero();
        let axis = placement(
            o,
            outer[0],
            n,
            if x == DVec3::ZERO {
                n.any_orthonormal_vector()
            } else {
                x
            },
        );
        let plane = o.add(format!("PLANE('',#{axis})"));
        let mut bounds = Vec::new();
        for (li, ring) in rings.iter().enumerate() {
            let mut oriented = Vec::new();
            for (a, b) in ring.iter().zip(ring.iter().cycle().skip(1)) {
                let (ka, kb) = (key(*a), key(*b));
                if ka == kb {
                    continue;
                }
                let mut vertex = |p: DVec3, k: [i64; 3], o: &mut Out| {
                    *vertices.entry(k).or_insert_with(|| {
                        let pt = point(o, p);
                        o.add(format!("VERTEX_POINT('',#{pt})"))
                    })
                };
                let va = vertex(*a, ka, o);
                let vb = vertex(*b, kb, o);
                let (edge, forward) = match edges.get(&(kb, ka)) {
                    Some(e) => (*e, false),
                    None => {
                        let e = *edges.entry((ka, kb)).or_insert_with(|| {
                            let start = point(o, *a);
                            let dir = direction(o, *b - *a);
                            let vector = o.add(format!("VECTOR('',#{dir},{})", r(a.distance(*b))));
                            let line = o.add(format!("LINE('',#{start},#{vector})"));
                            o.add(format!("EDGE_CURVE('',#{va},#{vb},#{line},.T.)"))
                        });
                        (e, true)
                    }
                };
                oriented.push(o.add(format!(
                    "ORIENTED_EDGE('',*,*,#{edge},{})",
                    if forward { ".T." } else { ".F." }
                )));
            }
            if oriented.len() < 3 {
                continue;
            }
            let list = oriented
                .iter()
                .map(|i| format!("#{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let lp = o.add(format!("EDGE_LOOP('',({list}))"));
            bounds.push(o.add(format!(
                "{}('',#{lp},.T.)",
                if li == 0 {
                    "FACE_OUTER_BOUND"
                } else {
                    "FACE_BOUND"
                }
            )));
        }
        if bounds.is_empty() {
            continue;
        }
        let list = bounds
            .iter()
            .map(|i| format!("#{i}"))
            .collect::<Vec<_>>()
            .join(",");
        face_ids.push(o.add(format!("ADVANCED_FACE('',({list}),#{plane},.T.)")));
    }
    let list = face_ids
        .iter()
        .map(|i| format!("#{i}"))
        .collect::<Vec<_>>()
        .join(",");
    o.add(format!(
        "{}('',({list}))",
        if closed { "CLOSED_SHELL" } else { "OPEN_SHELL" }
    ))
}

/// What a product carries.
struct ProductData {
    name: String,
    id: String,
    description: String,
    properties: BTreeMap<String, String>,
    /// Shells in the product's own frame.
    shells: Vec<(Vec<Vec<Vec<DVec3>>>, bool)>,
}

fn product_data(doc: &Document, c: ContainerRef) -> ProductData {
    let (name, description, attrs) = match c {
        ContainerRef::Model => (
            if doc.info.title.is_empty() {
                "Model".to_string()
            } else {
                doc.info.title.clone()
            },
            doc.info.description.clone(),
            &doc.attrs,
        ),
        ContainerRef::Definition(d) => match doc.definition(d) {
            Some(def) => (def.name.clone(), def.description.clone(), &def.attrs),
            None => (String::new(), String::new(), &doc.attrs),
        },
    };
    let properties: BTreeMap<String, String> = attrs
        .dictionary(crate::commands::product::PROPERTIES)
        .map(|d| {
            d.iter()
                .filter_map(|(k, v)| match v {
                    crate::document::Value::Str(s) => Some((k.clone(), s.clone())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let id = properties
        .get(crate::commands::product::PART_NUMBER)
        .cloned()
        .unwrap_or_else(|| name.clone());
    ProductData {
        name,
        id,
        description,
        properties,
        shells: Vec::new(),
    }
}

/// The exchange structure of the document's visible model, with its
/// product structure.
pub fn write(
    doc: &Document,
    scope: &ExportScope,
    file_name: &str,
) -> Result<(String, usize), IoError> {
    let sc = scene::collect(doc, scope);
    // Nodes of the tree: occurrence paths (the model is the empty path).
    let mut nodes: BTreeMap<Vec<String>, (InstancePath, ContainerRef, DAffine3)> = BTreeMap::new();
    let key_of = |p: &InstancePath| p.steps.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>();
    nodes.insert(
        Vec::new(),
        (
            InstancePath::root(),
            ContainerRef::Model,
            DAffine3::IDENTITY,
        ),
    );
    for occ in &sc.occurrences {
        let mut path = occ.path.clone();
        loop {
            let k = key_of(&path);
            if let std::collections::btree_map::Entry::Vacant(slot) = nodes.entry(k) {
                let (c, w) = doc
                    .resolve_container(&path)
                    .map_err(|_| corrupt("an occurrence vanished"))?;
                slot.insert((path.clone(), c, w));
            }
            match path.parent() {
                Some(p) => path = p,
                None => break,
            }
        }
    }
    // Products by container; a branch under a non-rigid placement is
    // flattened into the node that has it.
    let mut products: BTreeMap<ContainerRef, ProductData> = BTreeMap::new();
    let mut usages: BTreeMap<(ContainerRef, String), (ContainerRef, DAffine3, String)> =
        BTreeMap::new();
    let mut flattened: Vec<Vec<String>> = Vec::new();
    let mesh_of = |c: ContainerRef| sc.mesh(c).is_some();
    for (k, (path, c, w)) in &nodes {
        if flattened
            .iter()
            .any(|f| k.len() > f.len() && k.starts_with(f))
        {
            continue;
        }
        let parent = match path.parent() {
            Some(p) => nodes.get(&key_of(&p)).cloned(),
            None => None,
        };
        let local = parent.as_ref().map_or(*w, |(_, _, pw)| pw.inverse() * *w);
        if let Some((_, pc, _)) = &parent
            && !rigid(&local)
        {
            // Bake this subtree into the parent product's geometry.
            flattened.push(k.clone());
            let entry = products
                .entry(*pc)
                .or_insert_with(|| product_data(doc, *pc));
            for (k2, (_, c2, w2)) in &nodes {
                if k2.starts_with(k)
                    && mesh_of(*c2)
                    && let Some(entities) = doc.container(*c2)
                {
                    let to_parent = parent.as_ref().map_or(*w2, |(_, _, pw)| pw.inverse() * *w2);
                    entry.shells.extend(shells(entities.mesh(), &to_parent));
                }
            }
            continue;
        }
        products.entry(*c).or_insert_with(|| {
            let mut d = product_data(doc, *c);
            if mesh_of(*c)
                && let Some(entities) = doc.container(*c)
            {
                d.shells = shells(entities.mesh(), &DAffine3::IDENTITY);
            }
            d
        });
        if let (Some((_, pc, _)), Some(step)) = (&parent, path.steps.last()) {
            let name = doc
                .resolve_container(&path.parent().unwrap_or_default())
                .ok()
                .and_then(|(pcr, _)| {
                    let ents = doc.container(pcr)?;
                    let id = ents.key_to_id(*step)?;
                    ents.pid_of(id).map(|p| p.0.to_string())
                })
                .unwrap_or_default();
            usages.insert((*pc, format!("{step:?}")), (*c, local, name));
        }
    }
    let mut o = Out { lines: Vec::new() };
    let app = o.add("APPLICATION_CONTEXT('automotive design')");
    o.add(format!(
        "APPLICATION_PROTOCOL_DEFINITION('international standard','automotive_design',2000,#{app})"
    ));
    let pctx = o.add(format!("PRODUCT_CONTEXT('',#{app},'mechanical')"));
    let pdctx = o.add(format!(
        "PRODUCT_DEFINITION_CONTEXT('part definition',#{app},'design')"
    ));
    let len = o.add("(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.))");
    let ang = o.add("(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.))");
    let sol = o.add("(NAMED_UNIT(*)SI_UNIT($,.STERADIAN.)SOLID_ANGLE_UNIT())");
    let unc = o.add(format!(
        "UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-03),#{len},'distance_accuracy_value','confusion accuracy')"
    ));
    let ctx = o.add(format!(
        "(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{unc}))GLOBAL_UNIT_ASSIGNED_CONTEXT((#{len},#{ang},#{sol}))REPRESENTATION_CONTEXT('Context','3D'))"
    ));
    let revision = crate::commands::product::revisions_of(doc)
        .last()
        .map(|r| r.label.clone())
        .unwrap_or_default();
    // Products, in container order.
    let mut written: BTreeMap<ContainerRef, (usize, usize, usize)> = BTreeMap::new();
    let mut solids = 0;
    for (c, data) in &products {
        let p = o.add(format!(
            "PRODUCT({},{},{},(#{pctx}))",
            q(&data.id),
            q(&data.name),
            q(&data.description)
        ));
        o.add(format!("PRODUCT_RELATED_PRODUCT_CATEGORY('part',$,(#{p}))"));
        let rev = if *c == ContainerRef::Model {
            revision.as_str()
        } else {
            ""
        };
        let pf = o.add(format!("PRODUCT_DEFINITION_FORMATION({},'',#{p})", q(rev)));
        let pd = o.add(format!("PRODUCT_DEFINITION('design','',#{pf},#{pdctx})"));
        let pds = o.add(format!("PRODUCT_DEFINITION_SHAPE('','',#{pd})"));
        let origin = placement(&mut o, DVec3::ZERO, DVec3::Z, DVec3::X);
        let sr = o.add(format!(
            "SHAPE_REPRESENTATION({},(#{origin}),#{ctx})",
            q(&data.name)
        ));
        o.add(format!("SHAPE_DEFINITION_REPRESENTATION(#{pds},#{sr})"));
        let mut solid_items = Vec::new();
        let mut surface_items = Vec::new();
        for (faces, closed) in &data.shells {
            let shell = write_shell(&mut o, faces, *closed);
            if *closed {
                solid_items.push(o.add(format!("MANIFOLD_SOLID_BREP('',#{shell})")));
            } else {
                surface_items.push(o.add(format!("SHELL_BASED_SURFACE_MODEL('',(#{shell}))")));
            }
        }
        solids += solid_items.len() + surface_items.len();
        for (kind, items) in [
            ("ADVANCED_BREP_SHAPE_REPRESENTATION", &solid_items),
            ("MANIFOLD_SURFACE_SHAPE_REPRESENTATION", &surface_items),
        ] {
            if items.is_empty() {
                continue;
            }
            let o2 = placement(&mut o, DVec3::ZERO, DVec3::Z, DVec3::X);
            let list = std::iter::once(o2)
                .chain(items.iter().copied())
                .map(|i| format!("#{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let rep = o.add(format!("{kind}({},({list}),#{ctx})", q(&data.name)));
            o.add(format!(
                "SHAPE_REPRESENTATION_RELATIONSHIP('','',#{sr},#{rep})"
            ));
        }
        // Properties as user-defined attributes.
        for (k, v) in &data.properties {
            if k == crate::commands::product::PART_NUMBER {
                continue;
            }
            let pdef = o.add(format!("PROPERTY_DEFINITION({},'',#{pd})", q(k)));
            let item = o.add(format!(
                "DESCRIPTIVE_REPRESENTATION_ITEM({},{})",
                q(k),
                q(v)
            ));
            let rep = o.add(format!("REPRESENTATION('',(#{item}),#{ctx})"));
            o.add(format!(
                "PROPERTY_DEFINITION_REPRESENTATION(#{pdef},#{rep})"
            ));
        }
        written.insert(*c, (pd, sr, origin));
    }
    for ((parent, step), (child, t, name)) in &usages {
        let (Some((ppd, psr, _)), Some((cpd, csr, corigin))) =
            (written.get(parent), written.get(child))
        else {
            continue;
        };
        let nauo = o.add(format!(
            "NEXT_ASSEMBLY_USAGE_OCCURRENCE({},{},'',#{ppd},#{cpd},$)",
            q(step),
            q(name)
        ));
        let pds = o.add(format!("PRODUCT_DEFINITION_SHAPE('','',#{nauo})"));
        let placed = placement(&mut o, t.translation, t.matrix3.z_axis, t.matrix3.x_axis);
        let idt = o.add(format!(
            "ITEM_DEFINED_TRANSFORMATION('','',#{corigin},#{placed})"
        ));
        let rel = o.add(format!(
            "(REPRESENTATION_RELATIONSHIP('','',#{csr},#{psr})REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(#{idt})SHAPE_REPRESENTATION_RELATIONSHIP())"
        ));
        o.add(format!(
            "CONTEXT_DEPENDENT_SHAPE_REPRESENTATION(#{rel},#{pds})"
        ));
    }
    if solids == 0 {
        return Err(IoError::UnsupportedFormat(
            "nothing visible to export".into(),
        ));
    }
    let mut text = String::from("ISO-10303-21;\nHEADER;\n");
    text.push_str("FILE_DESCRIPTION(('Planyrith model'),'2;1');\n");
    let _ = writeln!(
        text,
        "FILE_NAME({},'1970-01-01T00:00:00',(''),(''),'Planyrith','Planyrith','');",
        q(file_name)
    );
    text.push_str(
        "FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));\nENDSEC;\nDATA;\n",
    );
    for (i, line) in o.lines.iter().enumerate() {
        let _ = writeln!(text, "#{}={};", i + 1, line);
    }
    text.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
    Ok((text, solids))
}

// ----- reading --------------------------------------------------------------------------

/// A product read back.
#[derive(Debug, Clone, Default)]
pub struct ReadProduct {
    /// Its name.
    pub name: String,
    /// Its id (part number).
    pub id: String,
    /// Its description.
    pub description: String,
    /// User-defined attributes.
    pub properties: BTreeMap<String, String>,
    /// Its own faces: shells of faces (loops, outer first), in mm.
    pub shells: Vec<Vec<Vec<Vec<DVec3>>>>,
}

/// A structure read back.
#[derive(Debug, Clone, Default)]
pub struct ReadModel {
    /// Products by their definition's entity number.
    pub products: BTreeMap<u64, ReadProduct>,
    /// Usages: parent, child, placement (mm), name.
    pub usages: Vec<(u64, u64, DAffine3, String)>,
}

impl ReadModel {
    /// Products used by no other product, in entity order.
    pub fn roots(&self) -> Vec<u64> {
        self.products
            .keys()
            .copied()
            .filter(|p| !self.usages.iter().any(|(_, c, _, _)| c == p))
            .collect()
    }
}

struct Reader<'a> {
    data: &'a BTreeMap<u64, Instance>,
    unit: f64,
}

fn unsupported(what: &str) -> IoError {
    IoError::UnsupportedFormat(format!("STEP {what} is not read without the B-rep bridge"))
}

impl Reader<'_> {
    fn get(&self, v: &Value) -> Result<&Instance, IoError> {
        let id = v
            .as_ref_id()
            .ok_or_else(|| corrupt("a reference was expected"))?;
        self.data
            .get(&id)
            .ok_or_else(|| corrupt(format!("#{id} is missing")))
    }

    fn arg<'b>(&self, i: &'b Instance, n: usize) -> Result<&'b Value, IoError> {
        i.args
            .get(n)
            .ok_or_else(|| corrupt(format!("{} lacks an argument", i.kind)))
    }

    fn point(&self, v: &Value) -> Result<DVec3, IoError> {
        let i = self.get(v)?;
        if i.kind != "CARTESIAN_POINT" {
            return Err(corrupt("a point was expected"));
        }
        let c = self
            .arg(i, 1)?
            .as_list()
            .ok_or_else(|| corrupt("bad point"))?;
        let f = |k: usize| c.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        Ok(DVec3::new(f(0), f(1), f(2)) * self.unit)
    }

    fn direction(&self, v: &Value) -> Result<DVec3, IoError> {
        if matches!(v, Value::Null) {
            return Ok(DVec3::ZERO);
        }
        let i = self.get(v)?;
        let c = self
            .arg(i, 1)?
            .as_list()
            .ok_or_else(|| corrupt("bad direction"))?;
        let f = |k: usize| c.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        Ok(DVec3::new(f(0), f(1), f(2)).normalize_or_zero())
    }

    /// An axis placement as a transform.
    fn placement(&self, v: &Value) -> Result<DAffine3, IoError> {
        let i = self.get(v)?;
        if i.kind != "AXIS2_PLACEMENT_3D" {
            return Err(unsupported(&i.kind.to_lowercase()));
        }
        let o = self.point(self.arg(i, 1)?)?;
        let mut z = self.direction(self.arg(i, 2)?)?;
        if z == DVec3::ZERO {
            z = DVec3::Z;
        }
        let mut x = self.direction(i.args.get(3).unwrap_or(&Value::Null))?;
        x = (x - z * x.dot(z)).normalize_or_zero();
        if x == DVec3::ZERO {
            x = z.any_orthonormal_vector();
        }
        let y = z.cross(x);
        Ok(DAffine3::from_mat3_translation(
            DMat3::from_cols(x, y, z),
            o,
        ))
    }

    fn vertex(&self, v: &Value) -> Result<DVec3, IoError> {
        let i = self.get(v)?;
        self.point(self.arg(i, 1)?)
    }

    /// The points of one oriented edge from its start (its end excluded).
    fn edge_points(&self, oe: &Instance) -> Result<Vec<DVec3>, IoError> {
        let ec = self.get(self.arg(oe, 3)?)?;
        let forward = self.arg(oe, 4)?.as_enum() != Some("F");
        if ec.kind != "EDGE_CURVE" {
            return Err(corrupt("an edge curve was expected"));
        }
        let (a, b) = (
            self.vertex(self.arg(ec, 1)?)?,
            self.vertex(self.arg(ec, 2)?)?,
        );
        let same = self.arg(ec, 4)?.as_enum() != Some("F");
        let curve = self.get(self.arg(ec, 3)?)?;
        let mut pts = match curve.kind.as_str() {
            "LINE" | "POLYLINE" => vec![a],
            "CIRCLE" => {
                let frame = self.placement(self.arg(curve, 1)?)?;
                let radius = self.arg(curve, 2)?.as_f64().unwrap_or(0.0) * self.unit;
                let inv = frame.inverse();
                let angle = |p: DVec3| {
                    let l = inv.transform_point3(p);
                    libm::atan2(l.y, l.x)
                };
                let (mut t0, mut t1) = (angle(a), angle(b));
                if !same {
                    std::mem::swap(&mut t0, &mut t1);
                }
                let mut sweep = t1 - t0;
                while sweep <= 1e-9 {
                    sweep += std::f64::consts::TAU;
                }
                let steps = ((sweep / std::f64::consts::TAU * 24.0).ceil() as usize).max(2);
                let mut ps: Vec<DVec3> = (0..steps)
                    .map(|k| {
                        let t = t0 + sweep * k as f64 / steps as f64;
                        frame.transform_point3(DVec3::new(
                            radius * libm::cos(t),
                            radius * libm::sin(t),
                            0.0,
                        ))
                    })
                    .collect();
                if !same {
                    // Points from b to a; we need a to b.
                    ps.push(frame.transform_point3(DVec3::new(
                        radius * libm::cos(t1),
                        radius * libm::sin(t1),
                        0.0,
                    )));
                    ps.reverse();
                    ps.pop();
                }
                ps
            }
            other => return Err(unsupported(&format!("curve {}", other.to_lowercase()))),
        };
        if !forward {
            // The edge runs from b to a.
            let mut all = pts.clone();
            all.push(b);
            all.reverse();
            all.pop();
            pts = all;
        }
        Ok(pts)
    }

    fn face(&self, f: &Instance) -> Result<Vec<Vec<DVec3>>, IoError> {
        if f.kind != "ADVANCED_FACE" && f.kind != "FACE_SURFACE" {
            return Err(unsupported(&format!("face {}", f.kind.to_lowercase())));
        }
        let surface = self.get(self.arg(f, 2)?)?;
        if surface.kind != "PLANE" {
            return Err(unsupported(&format!(
                "surface {}",
                surface.kind.to_lowercase()
            )));
        }
        let same = self.arg(f, 3)?.as_enum() != Some("F");
        let mut outer: Option<Vec<DVec3>> = None;
        let mut holes = Vec::new();
        for b in self.arg(f, 1)?.as_list().unwrap_or_default() {
            let bound = self.get(b)?;
            let lp = self.get(self.arg(bound, 1)?)?;
            let orient = self.arg(bound, 2)?.as_enum() != Some("F");
            let mut ring = Vec::new();
            match lp.kind.as_str() {
                "EDGE_LOOP" => {
                    for oe in self.arg(lp, 1)?.as_list().unwrap_or_default() {
                        ring.extend(self.edge_points(self.get(oe)?)?);
                    }
                }
                "POLY_LOOP" => {
                    for p in self.arg(lp, 1)?.as_list().unwrap_or_default() {
                        ring.push(self.point(p)?);
                    }
                }
                other => return Err(unsupported(&format!("loop {}", other.to_lowercase()))),
            }
            if orient != same {
                ring.reverse();
            }
            if bound.kind == "FACE_OUTER_BOUND" && outer.is_none() {
                outer = Some(ring);
            } else {
                holes.push(ring);
            }
        }
        // Without a marked outer bound, the largest loop is outer.
        let mut loops = match outer {
            Some(o) => vec![o],
            None => {
                holes.sort_by(|a, b| area(b).total_cmp(&area(a)));
                if holes.is_empty() {
                    return Ok(Vec::new());
                }
                vec![holes.remove(0)]
            }
        };
        loops.extend(holes);
        Ok(loops)
    }

    fn shell(&self, s: &Instance) -> Result<Vec<Vec<Vec<DVec3>>>, IoError> {
        if s.kind == "ORIENTED_CLOSED_SHELL" || s.kind == "ORIENTED_OPEN_SHELL" {
            let mut faces = self.shell(self.get(self.arg(s, 2)?)?)?;
            if self.arg(s, 3)?.as_enum() == Some("F") {
                for f in &mut faces {
                    for ring in f.iter_mut() {
                        ring.reverse();
                    }
                }
            }
            return Ok(faces);
        }
        let mut faces = Vec::new();
        for f in self.arg(s, 1)?.as_list().unwrap_or_default() {
            let loops = self.face(self.get(f)?)?;
            if !loops.is_empty() {
                faces.push(loops);
            }
        }
        Ok(faces)
    }

    /// The shells of a representation's items.
    fn items(&self, rep: &Instance, out: &mut Vec<Vec<Vec<Vec<DVec3>>>>) -> Result<(), IoError> {
        for item in self.arg(rep, 1)?.as_list().unwrap_or_default() {
            let i = self.get(item)?;
            match i.kind.as_str() {
                "MANIFOLD_SOLID_BREP" | "FACETED_BREP" => {
                    out.push(self.shell(self.get(self.arg(i, 1)?)?)?)
                }
                "BREP_WITH_VOIDS" => {
                    out.push(self.shell(self.get(self.arg(i, 1)?)?)?);
                    for v in self.arg(i, 2)?.as_list().unwrap_or_default() {
                        out.push(self.shell(self.get(v)?)?);
                    }
                }
                "SHELL_BASED_SURFACE_MODEL" => {
                    for s in self.arg(i, 1)?.as_list().unwrap_or_default() {
                        out.push(self.shell(self.get(s)?)?);
                    }
                }
                "AXIS2_PLACEMENT_3D" => {}
                "MAPPED_ITEM" => return Err(unsupported("mapped item")),
                _ => {}
            }
        }
        Ok(())
    }
}

fn area(ring: &[DVec3]) -> f64 {
    let mut a = DVec3::ZERO;
    for (p, q) in ring.iter().zip(ring.iter().cycle().skip(1)) {
        a += p.cross(*q);
    }
    a.length()
}

/// Reads a structure written by any AP203/AP214/AP242 exporter, as far as
/// planar faces with straight or circular edges go.
pub fn read(text: &str) -> Result<ReadModel, IoError> {
    let data = parse(text)?;
    let unit = if text.contains(".MILLI.,.METRE.") || text.contains(".MILLI., .METRE.") {
        1.0
    } else if text.contains("'INCH'") {
        25.4
    } else if text.contains("SI_UNIT($,.METRE.)") {
        1000.0
    } else {
        1.0
    };
    let rd = Reader { data: &data, unit };
    let mut model = ReadModel::default();
    // Products and their definitions.
    let mut rep_of: BTreeMap<u64, u64> = BTreeMap::new(); // product definition → shape representation
    for (id, i) in &data {
        if i.kind == "PRODUCT_DEFINITION" {
            let pf = rd.get(rd.arg(i, 2)?)?;
            let p = rd.get(rd.arg(pf, 2)?)?;
            model.products.insert(
                *id,
                ReadProduct {
                    id: rd.arg(p, 0)?.as_str().unwrap_or_default().to_string(),
                    name: rd.arg(p, 1)?.as_str().unwrap_or_default().to_string(),
                    description: rd.arg(p, 2)?.as_str().unwrap_or_default().to_string(),
                    ..Default::default()
                },
            );
        }
    }
    // Shapes of products, and shapes of usages.
    let mut pds_target: BTreeMap<u64, u64> = BTreeMap::new();
    for (id, i) in &data {
        if i.kind == "PRODUCT_DEFINITION_SHAPE"
            && let Some(t) = i.args.get(2).and_then(Value::as_ref_id)
        {
            pds_target.insert(*id, t);
        }
    }
    for i in data.values() {
        if i.kind == "SHAPE_DEFINITION_REPRESENTATION"
            && let (Some(pds), Some(rep)) = (
                i.args.first().and_then(Value::as_ref_id),
                i.args.get(1).and_then(Value::as_ref_id),
            )
            && let Some(pd) = pds_target.get(&pds)
            && model.products.contains_key(pd)
        {
            rep_of.insert(*pd, rep);
        }
    }
    // Geometry: each product's representation and those related to it
    // without a transformation.
    let mut related: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for i in data.values() {
        if (i.kind == "SHAPE_REPRESENTATION_RELATIONSHIP"
            || i.kind == "REPRESENTATION_RELATIONSHIP")
            && i.part("REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION")
                .is_none()
            && let (Some(a), Some(b)) = (
                i.args.get(2).and_then(Value::as_ref_id),
                i.args.get(3).and_then(Value::as_ref_id),
            )
        {
            related.entry(a).or_default().push(b);
            related.entry(b).or_default().push(a);
        }
    }
    let mut rep_owner: BTreeMap<u64, u64> = BTreeMap::new();
    for (pd, rep) in &rep_of {
        rep_owner.insert(*rep, *pd);
        let mut stack = vec![*rep];
        let mut seen = vec![*rep];
        let mut shells = Vec::new();
        while let Some(r) = stack.pop() {
            if let Some(inst) = data.get(&r) {
                rd.items(inst, &mut shells)?;
            }
            for n in related.get(&r).cloned().unwrap_or_default() {
                if !seen.contains(&n) {
                    seen.push(n);
                    rep_owner.insert(n, *pd);
                    stack.push(n);
                }
            }
        }
        if let Some(p) = model.products.get_mut(pd) {
            p.shells = shells;
        }
    }
    // Properties.
    for i in data.values() {
        if i.kind == "PROPERTY_DEFINITION_REPRESENTATION"
            && let (Ok(pdef), Some(rep)) = (
                rd.get(i.args.first().unwrap_or(&Value::Null)),
                i.args
                    .get(1)
                    .and_then(Value::as_ref_id)
                    .and_then(|r| data.get(&r)),
            )
            && let Some(pd) = pdef.args.get(2).and_then(Value::as_ref_id)
            && let Some(product) = model.products.get_mut(&pd)
        {
            for item in rep.args.get(1).and_then(Value::as_list).unwrap_or_default() {
                if let Ok(it) = rd.get(item)
                    && it.kind == "DESCRIPTIVE_REPRESENTATION_ITEM"
                {
                    let k = it.args.first().and_then(Value::as_str).unwrap_or_default();
                    let v = it.args.get(1).and_then(Value::as_str).unwrap_or_default();
                    if !k.is_empty() {
                        product.properties.insert(k.to_string(), v.to_string());
                    }
                }
            }
        }
    }
    // Usages with their placements.
    for i in data.values() {
        if i.kind != "CONTEXT_DEPENDENT_SHAPE_REPRESENTATION" {
            continue;
        }
        let rel = rd.get(rd.arg(i, 0)?)?;
        let pds = rd.get(rd.arg(i, 1)?)?;
        let nauo = rd.get(rd.arg(pds, 2)?)?;
        if nauo.kind != "NEXT_ASSEMBLY_USAGE_OCCURRENCE" {
            continue;
        }
        let (Some(parent), Some(child)) = (
            nauo.args.get(3).and_then(Value::as_ref_id),
            nauo.args.get(4).and_then(Value::as_ref_id),
        ) else {
            continue;
        };
        let name = nauo
            .args
            .get(1)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let t = match rel.part("REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION") {
            Some(args) => {
                let idt = rd.get(args.first().unwrap_or(&Value::Null))?;
                if idt.kind != "ITEM_DEFINED_TRANSFORMATION" {
                    return Err(unsupported("transformation operator"));
                }
                let a = rd.placement(rd.arg(idt, 2)?)?;
                let b = rd.placement(rd.arg(idt, 3)?)?;
                b * a.inverse()
            }
            None => DAffine3::IDENTITY,
        };
        model.usages.push((parent, child, t, name));
    }
    if model.products.is_empty() {
        // Geometry without a product structure: one product of it all.
        let mut product = ReadProduct::default();
        for (id, i) in &data {
            if matches!(
                i.kind.as_str(),
                "MANIFOLD_SOLID_BREP"
                    | "FACETED_BREP"
                    | "BREP_WITH_VOIDS"
                    | "SHELL_BASED_SURFACE_MODEL"
            ) {
                let holder = Instance {
                    kind: "REPRESENTATION".into(),
                    args: vec![
                        Value::Str(String::new()),
                        Value::List(vec![Value::Ref(*id)]),
                    ],
                    parts: Vec::new(),
                };
                rd.items(&holder, &mut product.shells)?;
            }
        }
        model.products.insert(0, product);
    }
    if model.products.values().all(|p| p.shells.is_empty()) {
        return Err(unsupported("geometry"));
    }
    Ok(model)
}

/// A shell's faces as a mesh.
pub fn shell_mesh(faces: &[Vec<Vec<DVec3>>], tol: &Tolerances) -> Result<HalfEdgeMesh, IoError> {
    let pieces: Vec<Piece> = faces
        .iter()
        .map(|loops| Piece {
            loops: loops.clone(),
            sides: FaceSides::default(),
        })
        .collect();
    stitch(&pieces, tol).map_err(|e| corrupt(e.to_string()))
}

// ----- exporter and importer -----------------------------------------------------------

/// STEP export with product structure (docs/09 §5).
#[derive(Debug, Default, Clone, Copy)]
pub struct StructuredStepExporter;

impl crate::io::export::Exporter for StructuredStepExporter {
    fn format(&self) -> crate::io::export::ExportFormat {
        crate::io::export::ExportFormat::Step
    }

    fn export(
        &self,
        doc: &Document,
        scope: &ExportScope,
        _opts: &crate::io::export::ExportOptions,
        dst: &mut dyn std::io::Write,
        progress: &dyn crate::io::Progress,
    ) -> Result<crate::io::export::ExportReport, IoError> {
        progress.check_cancelled()?;
        let (text, solids) = write(doc, scope, "model.step")?;
        dst.write_all(text.as_bytes())
            .map_err(|e| IoError::storage("step", e.to_string()))?;
        Ok(crate::io::export::ExportReport {
            bytes_written: text.len() as u64,
            entities: solids,
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}

/// STEP import with product structure; files with curved geometry go to
/// the B-rep bridge when the build has it (docs/09 §5).
#[derive(Debug, Default, Clone, Copy)]
pub struct StructuredStepImporter;

impl crate::io::import::Importer for StructuredStepImporter {
    fn format(&self) -> crate::io::import::ImportFormat {
        crate::io::import::ImportFormat::Step
    }

    fn probe(&self, bytes: &[u8], name: &str) -> crate::io::import::Confidence {
        use crate::io::import::Confidence;
        if bytes.starts_with(b"ISO-10303-21;")
            && !bytes.windows(4).any(|w| w == b"IFC2" || w == b"IFC4")
        {
            Confidence::CERTAIN
        } else if name.ends_with(".step") || name.ends_with(".stp") {
            Confidence::EXTENSION
        } else {
            Confidence::NONE
        }
    }

    fn import(
        &self,
        src: &mut dyn std::io::Read,
        opts: &crate::io::import::ImportOptions,
        tx: &mut crate::commands::Transaction<'_>,
        target: &crate::commands::params::Target,
        progress: &dyn crate::io::Progress,
    ) -> Result<crate::io::import::ImportReport, IoError> {
        use std::io::Read as _;
        let mut source = String::new();
        src.take(64 * 1024 * 1024 + 1)
            .read_to_string(&mut source)
            .map_err(|e| corrupt(e.to_string()))?;
        if source.len() > 64 * 1024 * 1024 {
            return Err(IoError::LimitExceeded("STEP input exceeds 64 MiB".into()));
        }
        match read(&source) {
            Ok(model) => build(&model, opts, tx, target, progress),
            #[cfg(feature = "brep")]
            Err(IoError::UnsupportedFormat(_)) => crate::io::step::StepImporter.import(
                &mut source.as_bytes(),
                opts,
                tx,
                target,
                progress,
            ),
            Err(e) => Err(e),
        }
    }
}

/// Creates the products of `model` as components under the import's root.
fn build(
    model: &ReadModel,
    opts: &crate::io::import::ImportOptions,
    tx: &mut crate::commands::Transaction<'_>,
    target: &crate::commands::params::Target,
    progress: &dyn crate::io::Progress,
) -> Result<crate::io::import::ImportReport, IoError> {
    use crate::document::{
        ComponentDefinition, ComponentInstance, Entities, EntityRecord, Group, Value as AttrValue,
    };
    use crate::io::import::{ImportAs, ImportReport};
    if matches!(opts.import_as, ImportAs::Image | ImportAs::Texture) {
        return Err(IoError::UnsupportedFormat(
            "STEP imports as a group or component".into(),
        ));
    }
    let tol = *tx.tolerances();
    let mut report = ImportReport::default();
    let mut made: BTreeMap<u64, crate::ids::DefinitionId> = BTreeMap::new();
    let err = |e: crate::commands::CommandError| corrupt(e.to_string());
    // Products in an order where children come before parents.
    let mut order: Vec<u64> = Vec::new();
    fn visit(m: &ReadModel, p: u64, depth: usize, order: &mut Vec<u64>) -> Result<(), IoError> {
        if depth > 64 {
            return Err(corrupt("the product structure is too deep or cyclic"));
        }
        for (parent, child, _, _) in &m.usages {
            if *parent == p {
                visit(m, *child, depth + 1, order)?;
            }
        }
        if !order.contains(&p) {
            order.push(p);
        }
        Ok(())
    }
    let roots = model.roots();
    for r in &roots {
        visit(model, *r, 0, &mut order)?;
    }
    for p in order {
        progress.check_cancelled()?;
        let product = &model.products[&p];
        let mut mesh = HalfEdgeMesh::new();
        for shell in &product.shells {
            let piece = shell_mesh(shell, &tol)?;
            report.faces += piece.face_count();
            report.edges += piece.edge_count();
            let (indexed, _) = piece.to_indexed();
            if mesh.is_empty() {
                mesh = piece;
            } else if mesh
                .insert_disjoint(&indexed, &tol, &mut crate::geom::NoDeltas)
                .map_err(|e| corrupt(e.to_string()))?
                .is_none()
            {
                report.skipped += 1;
            }
        }
        let name = if product.name.is_empty() {
            crate::io::import::source_stem(&opts.source, "STEP")
        } else {
            product.name.clone()
        };
        let mut entities = Entities::from_mesh(mesh, || tx.allocate_pid());
        for (parent, child, t, occ_name) in &model.usages {
            if *parent != p {
                continue;
            }
            let Some(d) = made.get(child) else { continue };
            let record = EntityRecord::Instance(ComponentInstance {
                definition: *d,
                transform: *t,
                name: (!occ_name.is_empty() && occ_name.parse::<u64>().is_err())
                    .then(|| occ_name.clone()),
                material_override: None,
                glued_to: None,
                dynamic_attrs: Default::default(),
            });
            entities
                .insert(record, tx.allocate_pid())
                .map_err(|e| corrupt(e.to_string()))?;
        }
        let uuid = crate::commands::common::definition_uuid(tx.document(), &name, &entities);
        let mut def = ComponentDefinition::new(uuid, name.clone());
        def.description = product.description.clone();
        def.entities = entities;
        if !product.id.is_empty() && product.id != product.name {
            def.attrs.set(
                crate::commands::product::PROPERTIES,
                crate::commands::product::PART_NUMBER,
                AttrValue::Str(product.id.clone()),
            );
        }
        for (k, v) in &product.properties {
            def.attrs.set(
                crate::commands::product::PROPERTIES,
                k,
                AttrValue::Str(v.clone()),
            );
        }
        let is_root = roots.contains(&p);
        if is_root && matches!(opts.import_as, ImportAs::Group) {
            def.is_internal_group = true;
        }
        if let ImportAs::Component(n) = &opts.import_as
            && is_root
        {
            def.name = n.clone();
        }
        let d = tx.insert_definition(def).map_err(err)?;
        made.insert(p, d);
    }
    let axes = if opts.swap_yz == Some(true) {
        DMat3::from_cols(DVec3::X, -DVec3::Z, DVec3::Y)
    } else {
        DMat3::IDENTITY
    };
    let place =
        target.local * DAffine3::from_translation(opts.at) * DAffine3::from_mat3(axes * opts.scale);
    for r in roots {
        let d = made[&r];
        let name = model.products[&r].name.clone();
        let record = if matches!(opts.import_as, ImportAs::Group) {
            EntityRecord::Group(Group {
                definition: d,
                transform: place,
                name,
            })
        } else {
            EntityRecord::Instance(ComponentInstance {
                definition: d,
                transform: place,
                name: None,
                material_override: None,
                glued_to: None,
                dynamic_attrs: Default::default(),
            })
        };
        let (_, pid) = tx.insert_entity(target.container, record).map_err(err)?;
        report.created.push(pid);
    }
    Ok(report)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;
    use crate::commands::{CommandRegistry, Engine};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn an_assembly_goes_out_and_comes_back() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        e.execute_erased("standard-part", json!({"kind": "hex-nut", "size": "M8"}))
            .unwrap();
        e.execute_erased(
            "standard-part",
            json!({"kind": "hex-nut", "size": "M8", "position": [40, 0, 0], "axis": [1, 0, 0]}),
        )
        .unwrap();
        e.execute_erased(
            "properties-set",
            json!({"target": "instance", "properties": {"vendor": "Acme"}}),
        )
        .unwrap();
        let (text, solids) = write(e.document(), &ExportScope::default(), "nuts.step").unwrap();
        assert_eq!(solids, 1, "one product's solid, used twice");
        assert!(text.contains("NEXT_ASSEMBLY_USAGE_OCCURRENCE"));
        let model = read(&text).unwrap();
        assert_eq!(model.products.len(), 2);
        assert_eq!(model.usages.len(), 2);
        let nut = model
            .products
            .values()
            .find(|p| p.name == "ISO 4032 M8")
            .unwrap();
        assert_eq!(nut.id, "ISO 4032 M8");
        assert_eq!(nut.properties["vendor"], "Acme");
        assert_eq!(nut.properties["standard"], "ISO 4032");
        let tol = Tolerances::default();
        let mesh = shell_mesh(&nut.shells[0], &tol).unwrap();
        assert!(mesh.is_solid());
        let turned = model
            .usages
            .iter()
            .find(|u| u.2.translation.x > 39.0)
            .unwrap();
        assert!((turned.2.matrix3.z_axis - DVec3::X).length() < 1e-9);
        // The same text twice: deterministic, and as the golden file.
        let (again, _) = write(e.document(), &ExportScope::default(), "nuts.step").unwrap();
        assert_eq!(text, again);
        golden("step/nuts-assembly.step", &text);
    }

    /// Compares `text` with the fixture `name` (written when missing or
    /// with `PLANYRITH_UPDATE_GOLDEN`).
    pub(crate) fn golden(name: &str, text: &str) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        if std::env::var_os("PLANYRITH_UPDATE_GOLDEN").is_some() || !path.exists() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        let want = std::fs::read_to_string(&path).unwrap();
        assert!(
            want == text,
            "{} differs (set PLANYRITH_UPDATE_GOLDEN=1 to accept)",
            path.display()
        );
    }

    #[test]
    fn plain_solids_from_other_writers() {
        let tol = Tolerances::default();
        for (text, volume) in [
            (include_str!("../../tests/fixtures/step/box.step"), None),
            (
                include_str!("../../tests/fixtures/step/through-hole.step"),
                None,
            ),
            (
                include_str!("../../tests/fixtures/step/hollow.step"),
                None::<f64>,
            ),
        ] {
            let model = read(text).unwrap();
            let shells: Vec<_> = model
                .products
                .values()
                .flat_map(|p| p.shells.iter())
                .collect();
            assert!(!shells.is_empty());
            let mut total = 0.0;
            for s in &shells {
                let m = shell_mesh(s, &tol).unwrap();
                assert!(m.is_solid(), "{} faces in, {} out", s.len(), m.face_count());
                total += m.volume().unwrap();
            }
            assert!(total > 0.0);
            let _ = volume;
        }
    }
}
