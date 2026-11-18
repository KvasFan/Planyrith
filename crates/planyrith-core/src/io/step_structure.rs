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
//!
//! Dimensions and anchored notes travel as graphical annotations: one
//! draughting model per product with a callout of polylines and text per
//! annotation (milestone 1.39, docs/09 Decisions 2026-11-16). Each
//! dimension also travels as data: a semantic dimension with its nominal
//! value and plus/minus tolerance, tied to its callout; data-only
//! dimensions and geometric tolerances read back as notes (milestone 1.45).

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
    let mut shapes: BTreeMap<ContainerRef, usize> = BTreeMap::new();
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
        shapes.insert(*c, pds);
    }
    // Dimensions and notes as graphical annotations (milestone 1.39).
    let mut styles: Option<(usize, usize, usize)> = None;
    for (c, (_, sr, _)) in &written {
        let callouts = callouts_of(doc, *c);
        if callouts.is_empty() {
            continue;
        }
        let (curve, text, font) = *styles.get_or_insert_with(|| {
            let colour = o.add("COLOUR_RGB('',0.,0.,0.)");
            let line = o.add("DRAUGHTING_PRE_DEFINED_CURVE_FONT('continuous')");
            let cs = o.add(format!(
                "CURVE_STYLE('',#{line},POSITIVE_LENGTH_MEASURE(0.35),#{colour})"
            ));
            let curve = o.add(format!("PRESENTATION_STYLE_ASSIGNMENT((#{cs}))"));
            let tsf = o.add(format!("TEXT_STYLE_FOR_DEFINED_FONT(#{colour})"));
            let ts = o.add(format!("TEXT_STYLE('',#{tsf})"));
            let text = o.add(format!("PRESENTATION_STYLE_ASSIGNMENT((#{ts}))"));
            let font = o.add("DRAUGHTING_PRE_DEFINED_TEXT_FONT('ISO 3098')");
            (curve, text, font)
        });
        let mut items = Vec::new();
        let mut semantic = Vec::new();
        for co in callouts {
            let mut parts = Vec::new();
            for line in &co.lines {
                let pts: Vec<String> = line
                    .iter()
                    .map(|p| format!("#{}", point(&mut o, *p)))
                    .collect();
                let pl = o.add(format!("POLYLINE('',({}))", pts.join(",")));
                parts.push(o.add(format!("ANNOTATION_CURVE_OCCURRENCE('',(#{curve}),#{pl})")));
            }
            if let Some((t, at)) = &co.text {
                let place = placement(&mut o, *at, DVec3::Z, DVec3::X);
                let tl = o.add(format!(
                    "TEXT_LITERAL('',{},#{place},'baseline centre',#{font})",
                    q(t)
                ));
                parts.push(o.add(format!("ANNOTATION_TEXT_OCCURRENCE('',(#{text}),#{tl})")));
            }
            let list = parts
                .iter()
                .map(|i| format!("#{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let callout = o.add(format!("DRAUGHTING_CALLOUT({},({list}))", q(co.kind)));
            items.push(callout);
            if let Some(sd) = &co.semantic {
                semantic.push((sd.clone(), callout));
            }
        }
        let origin = placement(&mut o, DVec3::ZERO, DVec3::Z, DVec3::X);
        let list = items
            .iter()
            .chain(std::iter::once(&origin))
            .map(|i| format!("#{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let dm = o.add(format!("DRAUGHTING_MODEL('annotations',({list}),#{ctx})"));
        o.add(format!(
            "REPRESENTATION_RELATIONSHIP('annotations','',#{dm},#{sr})"
        ));
        // The dimensions as data, tied to their presentation (milestone
        // 1.45).
        let Some(&pds) = shapes.get(c) else { continue };
        for (sd, callout) in semantic {
            let aspects: Vec<usize> = sd
                .points
                .iter()
                .map(|p| {
                    let sa = o.add(format!("SHAPE_ASPECT('','',#{pds},.T.)"));
                    let at = point(&mut o, *p);
                    o.add(format!(
                        "GEOMETRIC_ITEM_SPECIFIC_USAGE('','',#{sa},#{sr},#{at})"
                    ));
                    sa
                })
                .collect();
            let dim = match aspects.as_slice() {
                [a, b] => o.add(format!("DIMENSIONAL_LOCATION({},'',#{a},#{b})", q(sd.kind))),
                [a] => o.add(format!("DIMENSIONAL_SIZE(#{a},{})", q(sd.kind))),
                _ => continue,
            };
            let measure = |o: &mut Out, v: f64, role: &str| {
                o.add(format!(
                    "(LENGTH_MEASURE_WITH_UNIT()MEASURE_REPRESENTATION_ITEM()MEASURE_WITH_UNIT(LENGTH_MEASURE({}),#{len})REPRESENTATION_ITEM({}))",
                    r(v),
                    q(role)
                ))
            };
            let nominal = measure(&mut o, sd.value, "nominal value");
            let sdr = o.add(format!(
                "SHAPE_DIMENSION_REPRESENTATION('',(#{nominal}),#{ctx})"
            ));
            o.add(format!(
                "DIMENSIONAL_CHARACTERISTIC_REPRESENTATION(#{dim},#{sdr})"
            ));
            if let Some((lower, upper)) = sd.tolerance {
                let lo = measure(&mut o, lower, "lower limit");
                let hi = measure(&mut o, upper, "upper limit");
                let tv = o.add(format!("TOLERANCE_VALUE(#{lo},#{hi})"));
                o.add(format!("PLUS_MINUS_TOLERANCE(#{tv},#{dim})"));
            }
            o.add(format!(
                "DRAUGHTING_MODEL_ITEM_ASSOCIATION('','',#{dim},#{dm},#{callout})"
            ));
        }
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

/// One graphical annotation: what it is, its polylines and its text with
/// where the text stands (container coordinates, mm).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Callout {
    /// `linear dimension`, `aligned dimension`, `radius dimension`,
    /// `diameter dimension` or `note`.
    pub kind: &'static str,
    /// Polylines: a linear dimension's two extension lines then its
    /// dimension line; a radial dimension's or note's leader.
    pub lines: Vec<Vec<DVec3>>,
    /// The text and where it stands.
    pub text: Option<(String, DVec3)>,
    /// The dimension as data (milestone 1.45); `None` for a note.
    pub semantic: Option<SemanticDimension>,
}

/// A dimension as data (milestone 1.45): what it measures, its nominal
/// value and tolerance, and the points it is measured between.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticDimension {
    /// `linear distance`, `radius` or `diameter`.
    pub kind: &'static str,
    /// The nominal value, mm.
    pub value: f64,
    /// Lower and upper deviations, mm.
    pub tolerance: Option<(f64, f64)>,
    /// The measured points: two for a distance, the point on the curve for
    /// a radius or diameter.
    pub points: Vec<DVec3>,
}

/// The deviations a dimension's text states after its measurement: `±t`,
/// or `+u` and `-l` (`+0.2/-0.1`), in display units.
pub fn tolerance_of(text: &str) -> Option<(f64, f64)> {
    let tail = text.split_once("<>").map_or(text, |(_, t)| t);
    let number = |rest: &str| -> Option<f64> {
        let n: String = rest
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
            .collect();
        n.replace(',', ".").parse::<f64>().ok()
    };
    if let Some((_, rest)) = tail.split_once('±') {
        let t = number(rest)?;
        return Some((-t, t));
    }
    let (mut upper, mut lower) = (None, None);
    let chars: Vec<(usize, char)> = tail.char_indices().collect();
    for (k, &(i, c)) in chars.iter().enumerate() {
        let before = k.checked_sub(1).map(|j| chars[j].1);
        let starts = before.is_none_or(|b| b.is_whitespace() || b == '/' || b == '(');
        if !starts {
            continue;
        }
        let rest = &tail[i + c.len_utf8()..];
        match c {
            '+' if upper.is_none() => upper = number(rest),
            '-' | '−' if lower.is_none() => lower = number(rest).map(|v| -v),
            _ => {}
        }
    }
    (upper.is_some() || lower.is_some()).then(|| (lower.unwrap_or(0.0), upper.unwrap_or(0.0)))
}

/// The dimensions and anchored notes of container `c` as callouts
/// (screen-only notes have no place in the model and are left out).
pub fn callouts_of(doc: &Document, c: ContainerRef) -> Vec<Callout> {
    use crate::document::annotations::{anchor_position, dimension_view};
    use crate::document::{Dimension, RadialKind};
    let Some(entities) = doc.container(c) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (_, dim) in entities.dimensions() {
        let v = dimension_view(doc, c, dim);
        if v.detached {
            continue;
        }
        let [a, b] = v.measured;
        let [l0, l1] = v.line;
        let (style, text_override) = match dim {
            Dimension::Linear {
                style,
                text_override,
                ..
            }
            | Dimension::Radial {
                style,
                text_override,
                ..
            } => (style, text_override),
        };
        let unit = style.units.unwrap_or(doc.info.display_units).millimeters();
        let tolerance = text_override
            .as_deref()
            .and_then(tolerance_of)
            .map(|(l, u)| (l * unit, u * unit));
        out.push(match dim {
            Dimension::Linear { direction, .. } => Callout {
                kind: if direction.is_some() {
                    "linear dimension"
                } else {
                    "aligned dimension"
                },
                lines: vec![vec![a, l0], vec![b, l1], vec![l0, l1]],
                text: Some((v.text.clone(), v.text_at)),
                semantic: Some(SemanticDimension {
                    kind: "linear distance",
                    value: v.value,
                    tolerance,
                    points: vec![a, b],
                }),
            },
            Dimension::Radial { kind, .. } => Callout {
                kind: match kind {
                    RadialKind::Radius => "radius dimension",
                    RadialKind::Diameter => "diameter dimension",
                },
                lines: vec![vec![l0, l1]],
                text: Some((v.text.clone(), v.text_at)),
                semantic: Some(SemanticDimension {
                    kind: match kind {
                        RadialKind::Radius => "radius",
                        RadialKind::Diameter => "diameter",
                    },
                    value: v.value,
                    tolerance,
                    points: vec![b],
                }),
            },
        });
    }
    for (_, t) in entities.texts() {
        let Some(at) = t.anchor.as_ref().and_then(|a| anchor_position(doc, c, a)) else {
            continue;
        };
        let label = at
            + t.layout
                .as_ref()
                .and_then(|l| l.world_offset)
                .unwrap_or(DVec3::ZERO);
        out.push(Callout {
            kind: "note",
            lines: if label.distance(at) > 1e-9 {
                vec![vec![at, label]]
            } else {
                Vec::new()
            },
            text: Some((t.text.clone(), label)),
            semantic: None,
        });
    }
    out
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
    /// Graphical annotations: kind, polylines and text (milestone 1.39).
    pub annotations: Vec<ReadCallout>,
    /// Dimensions and geometric tolerances given only as data, as note
    /// texts with the point they apply to (milestone 1.45).
    pub notes: Vec<(String, DVec3)>,
}

/// A graphical annotation read back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReadCallout {
    /// The callout's name (its kind when Planyrith wrote it).
    pub kind: String,
    /// Its polylines, mm.
    pub lines: Vec<Vec<DVec3>>,
    /// Its text and where it stands, mm.
    pub text: Option<(String, DVec3)>,
    /// The nominal value of the dimension it presents, mm (milestone 1.45).
    pub nominal: Option<f64>,
    /// That dimension's lower and upper deviations, mm.
    pub tolerance: Option<(f64, f64)>,
}

/// A structure read back.
#[derive(Debug, Clone, Default)]
pub struct ReadModel {
    /// Products by their definition's entity number.
    pub products: BTreeMap<u64, ReadProduct>,
    /// Usages: parent, child, placement (mm), name.
    pub usages: Vec<(u64, u64, DAffine3, String)>,
    /// Dimensions and tolerances given as data that neither a presentation
    /// nor a point in the geometry places.
    pub unplaced: usize,
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
    // Graphical annotations of the products' draughting models.
    let mut callout_at: BTreeMap<u64, (u64, usize)> = BTreeMap::new();
    for (id, i) in &data {
        if i.kind != "DRAUGHTING_MODEL" {
            continue;
        }
        let Some(pd) = rep_owner.get(id).copied() else {
            continue;
        };
        let mut found = Vec::new();
        let before = model.products.get(&pd).map_or(0, |p| p.annotations.len());
        for item in i.args.get(1).and_then(Value::as_list).unwrap_or_default() {
            let Ok(dc) = rd.get(item) else { continue };
            if dc.kind != "DRAUGHTING_CALLOUT" {
                continue;
            }
            let mut callout = ReadCallout {
                kind: dc
                    .args
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                ..Default::default()
            };
            for part in dc.args.get(1).and_then(Value::as_list).unwrap_or_default() {
                let Ok(occ) = rd.get(part) else { continue };
                let Ok(geom) = rd.get(occ.args.get(2).unwrap_or(&Value::Null)) else {
                    continue;
                };
                match (occ.kind.as_str(), geom.kind.as_str()) {
                    ("ANNOTATION_CURVE_OCCURRENCE", "POLYLINE") => {
                        let pts: Vec<DVec3> = geom
                            .args
                            .get(1)
                            .and_then(Value::as_list)
                            .unwrap_or_default()
                            .iter()
                            .filter_map(|p| rd.point(p).ok())
                            .collect();
                        if pts.len() >= 2 {
                            callout.lines.push(pts);
                        }
                    }
                    ("ANNOTATION_TEXT_OCCURRENCE", "TEXT_LITERAL") => {
                        let t = geom.args.get(1).and_then(Value::as_str).unwrap_or_default();
                        if let Ok(at) = rd.placement(geom.args.get(2).unwrap_or(&Value::Null)) {
                            callout.text = Some((t.to_string(), at.translation));
                        }
                    }
                    _ => {}
                }
            }
            if !callout.lines.is_empty() || callout.text.is_some() {
                if let Some(cid) = item.as_ref_id() {
                    callout_at.insert(cid, (pd, before + found.len()));
                }
                found.push(callout);
            }
        }
        if let Some(p) = model.products.get_mut(&pd) {
            p.annotations.extend(found);
        }
    }
    semantic(&rd, &data, &callout_at, &mut model);
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

/// Dimensions and geometric tolerances given as data (milestone 1.45): a
/// dimension presented by a callout lends it its nominal value and
/// tolerance; one without a presentation, and a geometric tolerance without
/// one, becomes a note at the point its shape aspect names.
fn semantic(
    rd: &Reader<'_>,
    data: &BTreeMap<u64, Instance>,
    callout_at: &BTreeMap<u64, (u64, usize)>,
    model: &mut ReadModel,
) {
    // Presentations of semantic items, and the measures of dimensions.
    let mut presented: BTreeMap<u64, u64> = BTreeMap::new();
    let mut values: BTreeMap<u64, f64> = BTreeMap::new();
    let mut tolerances: BTreeMap<u64, (f64, f64)> = BTreeMap::new();
    let mut usages: BTreeMap<u64, u64> = BTreeMap::new();
    let measure = |v: &Value| -> Option<f64> {
        let m = rd.get(v).ok()?;
        let args = m
            .part("MEASURE_WITH_UNIT")
            .or_else(|| m.part("LENGTH_MEASURE_WITH_UNIT"))?;
        args.first()?.as_f64().map(|x| x * rd.unit)
    };
    for i in data.values() {
        let id = |k: usize| i.args.get(k).and_then(Value::as_ref_id);
        match i.kind.as_str() {
            "DRAUGHTING_MODEL_ITEM_ASSOCIATION" => {
                if let (Some(def), Some(item)) = (id(2), id(4)) {
                    presented.insert(def, item);
                }
            }
            "DIMENSIONAL_CHARACTERISTIC_REPRESENTATION" => {
                if let (Some(dim), Ok(rep)) = (id(0), rd.get(i.args.get(1).unwrap_or(&Value::Null)))
                    && let Some(v) = rep
                        .args
                        .get(1)
                        .and_then(Value::as_list)
                        .unwrap_or_default()
                        .iter()
                        .find_map(&measure)
                {
                    values.insert(dim, v);
                }
            }
            "PLUS_MINUS_TOLERANCE" => {
                if let (Ok(tv), Some(dim)) = (rd.get(i.args.first().unwrap_or(&Value::Null)), id(1))
                    && tv.kind == "TOLERANCE_VALUE"
                    && let (Some(lo), Some(hi)) = (
                        tv.args.first().and_then(measure),
                        tv.args.get(1).and_then(measure),
                    )
                {
                    tolerances.insert(dim, (lo, hi));
                }
            }
            "GEOMETRIC_ITEM_SPECIFIC_USAGE" => {
                if let (Some(sa), Some(item)) = (id(2), id(4)) {
                    usages.entry(sa).or_insert(item);
                }
            }
            _ => {}
        }
    }
    // The point a shape aspect names, and the product it belongs to.
    let place = |sa: u64| -> Option<(u64, DVec3)> {
        let aspect = data.get(&sa)?;
        let pds = rd.get(aspect.args.get(2)?).ok()?;
        let pd = pds.args.get(2)?.as_ref_id()?;
        let item = data.get(usages.get(&sa)?)?;
        let at = match item.kind.as_str() {
            "CARTESIAN_POINT" => rd.point(&Value::Ref(*usages.get(&sa)?)).ok()?,
            "VERTEX_POINT" => rd.point(item.args.get(1)?).ok()?,
            "EDGE_CURVE" => {
                (rd.vertex(item.args.get(1)?).ok()? + rd.vertex(item.args.get(2)?).ok()?) * 0.5
            }
            "ADVANCED_FACE" | "FACE_SURFACE" => {
                let loops = rd.face(item).ok()?;
                let outer = loops.first()?;
                outer.iter().copied().sum::<DVec3>() / outer.len().max(1) as f64
            }
            _ => return None,
        };
        Some((pd, at))
    };
    let number = |v: f64| {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    for (id, i) in data {
        let (text, aspect) = if i.kind.starts_with("DIMENSIONAL_LOCATION")
            || i.kind.starts_with("DIMENSIONAL_SIZE")
        {
            let size = i.kind.starts_with("DIMENSIONAL_SIZE");
            if let Some(&callout) = presented.get(id)
                && let Some(&(pd, k)) = callout_at.get(&callout)
                && let Some(c) = model
                    .products
                    .get_mut(&pd)
                    .and_then(|p| p.annotations.get_mut(k))
            {
                c.nominal = values.get(id).copied();
                c.tolerance = tolerances.get(id).copied();
                continue;
            }
            let name = i
                .args
                .get(if size { 1 } else { 0 })
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut text = match name {
                "radius" => "R".to_string(),
                "diameter" => "Ø".to_string(),
                _ => String::new(),
            };
            if let Some(v) = values.get(id) {
                text.push_str(&number(*v));
            }
            match tolerances.get(id) {
                Some((lo, hi)) if (lo + hi).abs() < 1e-12 => {
                    text.push_str(&format!(" ±{}", number(*hi)))
                }
                Some((lo, hi)) => text.push_str(&format!(" +{}/-{}", number(*hi), number(-*lo))),
                None => {}
            }
            let aspect = i
                .args
                .get(if size { 0 } else { 2 })
                .and_then(Value::as_ref_id);
            (text, aspect)
        } else if let Some((kind, args)) = geometric_tolerance(i) {
            if presented.contains_key(id) {
                continue;
            }
            let mut text = format!(
                "{} {}",
                kind,
                args.get(2).and_then(measure).map_or(String::new(), number)
            );
            for datum in datums(rd, i) {
                text.push_str(&format!(" | {datum}"));
            }
            (text, args.get(3).and_then(Value::as_ref_id))
        } else {
            continue;
        };
        match aspect.and_then(place) {
            Some((pd, at)) if model.products.contains_key(&pd) => {
                if let Some(p) = model.products.get_mut(&pd) {
                    p.notes.push((text.trim().to_string(), at));
                }
            }
            _ => model.unplaced += 1,
        }
    }
}

/// A geometric tolerance's kind in words and its `GEOMETRIC_TOLERANCE`
/// arguments (name, description, magnitude, toleranced shape aspect).
fn geometric_tolerance(i: &Instance) -> Option<(String, &[Value])> {
    let mut kinds = std::iter::once(i.kind.as_str()).chain(i.parts.iter().map(|(k, _)| k.as_str()));
    let kind = kinds.rfind(|k| {
        k.ends_with("_TOLERANCE")
            && !k.starts_with("GEOMETRIC_TOLERANCE")
            && *k != "PLUS_MINUS_TOLERANCE"
    })?;
    let args = i.part("GEOMETRIC_TOLERANCE").unwrap_or(&i.args);
    let words = kind
        .trim_end_matches("_TOLERANCE")
        .replace('_', " ")
        .to_lowercase();
    Some((words, args))
}

/// The datum letters a geometric tolerance refers to, in order.
fn datums(rd: &Reader<'_>, i: &Instance) -> Vec<String> {
    let Some(refs) = i
        .part("GEOMETRIC_TOLERANCE_WITH_DATUM_REFERENCE")
        .and_then(|a| a.first())
        .and_then(Value::as_list)
    else {
        return Vec::new();
    };
    let letter = |v: &Value| -> Option<String> {
        let d = rd.get(v).ok()?;
        (d.kind == "DATUM").then(|| d.args.get(4)?.as_str().map(str::to_string))?
    };
    let mut out = Vec::new();
    for r in refs {
        let Ok(system) = rd.get(r) else { continue };
        match system.kind.as_str() {
            "DATUM_SYSTEM" => {
                for c in system
                    .args
                    .get(4)
                    .and_then(Value::as_list)
                    .unwrap_or_default()
                {
                    if let Ok(compartment) = rd.get(c)
                        && let Some(l) = compartment.args.get(4).and_then(letter)
                    {
                        out.push(l);
                    }
                }
            }
            "DATUM_REFERENCE" => {
                out.extend(system.args.get(1).and_then(letter));
            }
            _ => {}
        }
    }
    out
}

/// The dimension or note a callout read back stands for, attached to the
/// geometry of `c` where its points meet it (milestone 1.39). Linear and
/// aligned dimensions come back as dimensions, radial ones as radial
/// dimensions on a curve when one is there; anything else with a text as a
/// note (with a leader from its first polyline's start).
fn annotation_record(
    doc: &Document,
    c: ContainerRef,
    callout: &ReadCallout,
) -> Option<crate::document::EntityRecord> {
    use crate::document::annotations::{curve_of, dimension_text, find_anchor};
    use crate::document::{DimStyle, Dimension, EntityRecord, RadialKind, Text, TextLayout};
    let style = DimStyle::default();
    let text = callout.text.clone();
    match callout.kind.as_str() {
        "linear dimension" | "aligned dimension" if callout.lines.len() >= 3 => {
            let (a, l0) = (callout.lines[0][0], callout.lines[0][1]);
            let (b, l1) = (callout.lines[1][0], *callout.lines[1].last()?);
            let direction = (callout.kind == "linear dimension")
                .then(|| (l1 - l0).try_normalize())
                .flatten();
            let value = match direction {
                Some(d) => (b - a).dot(d).abs(),
                None => (b - a).length(),
            };
            let measured = dimension_text(doc, &style, value, "", None);
            let text_override = with_tolerance(
                doc,
                text.map(|t| t.0).filter(|t| *t != measured),
                callout.tolerance,
            );
            return Some(EntityRecord::Dimension(Dimension::Linear {
                start: find_anchor(doc, c, a),
                end: find_anchor(doc, c, b),
                offset: l0 - a,
                direction,
                style,
                text_override,
            }));
        }
        "radius dimension" | "diameter dimension" if !callout.lines.is_empty() => {
            let (p, end) = (callout.lines[0][0], *callout.lines[0].last()?);
            let anchor = find_anchor(doc, c, p);
            if let Some((_, radius)) = curve_of(doc, c, &anchor) {
                let kind = if callout.kind == "radius dimension" {
                    RadialKind::Radius
                } else {
                    RadialKind::Diameter
                };
                let value = if kind == RadialKind::Radius {
                    radius
                } else {
                    2.0 * radius
                };
                let prefix = if kind == RadialKind::Radius {
                    "R"
                } else {
                    "Ø"
                };
                let measured = dimension_text(doc, &style, value, prefix, None);
                return Some(EntityRecord::Dimension(Dimension::Radial {
                    curve: anchor,
                    kind,
                    leader: end - p,
                    style,
                    text_override: with_tolerance(
                        doc,
                        text.map(|t| t.0).filter(|t| *t != measured),
                        callout.tolerance,
                    ),
                }));
            }
        }
        _ => {}
    }
    let (content, label) = text?;
    let at = callout.lines.first().map_or(label, |l| l[0]);
    Some(EntityRecord::Text(Text {
        layout: Some(TextLayout {
            world_offset: Some(label - at),
            leader_visible: label.distance(at) > 1e-9,
            ..Default::default()
        }),
        anchor: Some(find_anchor(doc, c, at)),
        text: content,
        screen_offset: glam::DVec2::ZERO,
    }))
}

/// A dimension's text override with the tolerance given as data added
/// after the measurement, unless the text already states one (milestone
/// 1.45).
fn with_tolerance(
    doc: &Document,
    text_override: Option<String>,
    tolerance: Option<(f64, f64)>,
) -> Option<String> {
    let Some((lower, upper)) = tolerance else {
        return text_override;
    };
    if text_override.as_deref().and_then(tolerance_of).is_some() {
        return text_override;
    }
    let unit = doc.info.display_units.millimeters();
    let number = |v: f64| {
        let s = format!("{:.4}", v / unit);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    let tol = if (lower + upper).abs() < 1e-9 {
        format!("±{}", number(upper))
    } else {
        format!("+{}/-{}", number(upper), number(-lower))
    };
    Some(match text_override {
        Some(t) => format!("{t} {tol}"),
        None => format!("<> {tol}"),
    })
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
        for callout in &product.annotations {
            if let Some(record) =
                annotation_record(tx.document(), ContainerRef::Definition(d), callout)
            {
                tx.insert_entity(ContainerRef::Definition(d), record)
                    .map_err(err)?;
            }
        }
        // Dimensions and tolerances given only as data (milestone 1.45).
        for (text, at) in &product.notes {
            let anchor = crate::document::annotations::find_anchor(
                tx.document(),
                ContainerRef::Definition(d),
                *at,
            );
            let record = EntityRecord::Text(crate::document::Text {
                layout: None,
                anchor: Some(anchor),
                text: text.clone(),
                screen_offset: glam::DVec2::ZERO,
            });
            tx.insert_entity(ContainerRef::Definition(d), record)
                .map_err(err)?;
        }
    }
    if model.unplaced > 0 {
        report.warnings.push(crate::diagnostics::Warning::new(
            crate::diagnostics::WarningCode::W2004,
            format!(
                "{} STEP dimensions or tolerances given as data name no point of the geometry",
                model.unplaced
            ),
        ));
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
    fn dimensions_and_notes_travel_as_annotations() {
        use crate::document::annotations::dimension_view;
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        for (name, params) in [
            (
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [400, 200, 0] }),
            ),
            (
                "push-pull",
                json!({ "face": { "Query": "face" }, "distance": 100 }),
            ),
            ("circle", json!({ "center": [600, 0, 0], "radius": 50 })),
            (
                "dimension",
                json!({ "kind": "radius", "curve": { "Query": "edge & last" },
                                 "offset": [80, 80, 0] }),
            ),
            (
                "dimension",
                json!({ "kind": "linear", "start": [0, 0, 0], "end": [400, 200, 0],
                                 "offset": [0, -80, 0], "direction": [1, 0, 0] }),
            ),
            (
                "dimension",
                json!({ "start": [0, 0, 100], "end": [400, 200, 100],
                                 "offset": [0, 0, 50], "text_override": "<> diagonal" }),
            ),
            (
                "text",
                json!({ "at": [400, 200, 100], "text": "Corner",
                            "layout": { "world_offset": [100, 100, 100], "leader_visible": true } }),
            ),
        ] {
            e.execute_erased(name, params).unwrap();
        }
        let doc = e.document();
        let views: Vec<(f64, String)> = doc
            .model
            .dimensions()
            .map(|(_, d)| {
                let v = dimension_view(doc, ContainerRef::Model, d);
                (v.value, v.text)
            })
            .collect();
        assert_eq!(views.len(), 3);
        let (text, _) = write(doc, &ExportScope::default(), "a.step").unwrap();
        assert_eq!(text.matches("DRAUGHTING_CALLOUT(").count(), 4, "{text}");
        assert!(text.contains(" diagonal'"), "overrides travel as shown");
        let model = read(&text).unwrap();
        let root = model
            .products
            .values()
            .find(|p| !p.annotations.is_empty())
            .unwrap();
        let kinds: Vec<&str> = root.annotations.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "radius dimension",
                "linear dimension",
                "aligned dimension",
                "note"
            ]
        );

        // Imported, they are dimensions and a note again, measuring alike.
        let mut back = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        let storage = Arc::new(crate::io::MemoryStorage::new());
        back.set_storage(storage.clone());
        crate::io::Storage::write(
            storage.as_ref(),
            &crate::io::StorageKey::new("a.step"),
            text.as_bytes(),
        )
        .unwrap();
        back.execute_erased("import", json!({ "path": "a.step" }))
            .unwrap();
        let doc = back.document();
        let (d, def) = doc
            .definitions
            .iter()
            .find(|(_, def)| def.entities.dimensions().next().is_some())
            .unwrap();
        let c = ContainerRef::Definition(d);
        let got: Vec<(f64, String)> = def
            .entities
            .dimensions()
            .map(|(_, dim)| {
                let v = dimension_view(doc, c, dim);
                assert!(!v.detached);
                (v.value, v.text)
            })
            .collect();
        // The circle's edges come back straight (STEP from Planyrith writes
        // segments), so the radius returns as a note with its label.
        assert_eq!(got.len(), 2);
        for ((a, ta), (b, tb)) in views[1..].iter().zip(&got) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
            assert_eq!(ta, tb);
        }
        let notes: Vec<&crate::document::Text> = def.entities.texts().map(|(_, t)| t).collect();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, views[0].1);
        assert_eq!(notes[1].text, "Corner");
        let offset = notes[1].layout.as_ref().unwrap().world_offset.unwrap();
        assert!(offset.distance(DVec3::splat(100.0)) < 1e-9, "{offset}");
    }

    #[test]
    fn tolerances_read_from_dimension_texts() {
        assert_eq!(tolerance_of("<> ±0.1"), Some((-0.1, 0.1)));
        assert_eq!(tolerance_of("<> +0.2/-0.1"), Some((-0.1, 0.2)));
        assert_eq!(tolerance_of("25 +0,2 -0,05"), Some((-0.05, 0.2)));
        assert_eq!(tolerance_of("<> -0.3"), Some((-0.3, 0.0)));
        assert_eq!(tolerance_of("<> diagonal"), None);
        assert_eq!(tolerance_of("3-4 bolts"), None);
    }

    /// Milestone 1.45: dimensions also travel as data, tied to their
    /// presentation, with their tolerances.
    #[test]
    fn dimensions_travel_as_data_with_tolerances() {
        use crate::document::annotations::dimension_view;
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        for (name, params) in [
            (
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [400, 200, 0] }),
            ),
            (
                "push-pull",
                json!({ "face": { "Query": "face" }, "distance": 100 }),
            ),
            (
                "dimension",
                json!({ "kind": "linear", "start": [0, 0, 0], "end": [400, 200, 0],
                        "offset": [0, -80, 0], "direction": [1, 0, 0],
                        "text_override": "<> ±0.1" }),
            ),
            (
                "dimension",
                json!({ "start": [0, 0, 100], "end": [400, 0, 100], "offset": [0, 0, 50] }),
            ),
        ] {
            e.execute_erased(name, params).unwrap();
        }
        let (text, _) = write(e.document(), &ExportScope::default(), "a.step").unwrap();
        assert_eq!(
            text.matches("DIMENSIONAL_LOCATION('linear distance'")
                .count(),
            2
        );
        assert_eq!(text.matches("PLUS_MINUS_TOLERANCE(").count(), 1);
        assert_eq!(
            text.matches("DRAUGHTING_MODEL_ITEM_ASSOCIATION(").count(),
            2
        );
        assert_eq!(text.matches("GEOMETRIC_ITEM_SPECIFIC_USAGE(").count(), 4);
        let model = read(&text).unwrap();
        assert_eq!(model.unplaced, 0);
        let root = model
            .products
            .values()
            .find(|p| !p.annotations.is_empty())
            .unwrap();
        let nominal: Vec<Option<f64>> = root.annotations.iter().map(|c| c.nominal).collect();
        let tolerance: Vec<_> = root.annotations.iter().map(|c| c.tolerance).collect();
        assert_eq!(nominal, [Some(400.0), Some(400.0)]);
        assert_eq!(tolerance, [Some((-0.1, 0.1)), None]);
        assert!(root.notes.is_empty(), "presented dimensions are not notes");
        // A tolerance given only as data reaches the imported dimension.
        let stripped = text.replace(r"'400.0mm \X2\00B1\X0\0.1'", "'400.0mm'");
        assert!(stripped != text, "the shown tolerance is removed");
        let mut back = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        let storage = Arc::new(crate::io::MemoryStorage::new());
        back.set_storage(storage.clone());
        crate::io::Storage::write(
            storage.as_ref(),
            &crate::io::StorageKey::new("a.step"),
            stripped.as_bytes(),
        )
        .unwrap();
        back.execute_erased("import", json!({ "path": "a.step" }))
            .unwrap();
        let doc = back.document();
        let (d, def) = doc
            .definitions
            .iter()
            .find(|(_, def)| def.entities.dimensions().next().is_some())
            .unwrap();
        let texts: Vec<String> = def
            .entities
            .dimensions()
            .map(|(_, dim)| dimension_view(doc, ContainerRef::Definition(d), dim).text)
            .collect();
        assert_eq!(texts, ["400.0mm ±0.1", "400.0mm"]);
    }

    /// Milestone 1.45: dimensions and geometric tolerances that another
    /// writer gives only as data become notes where they apply.
    #[test]
    fn tolerances_given_only_as_data_become_notes() {
        let base = include_str!("../../tests/fixtures/step/box.step");
        let measure = |id: u32, v: &str, role: &str| {
            format!(
                "#{id}=(LENGTH_MEASURE_WITH_UNIT()MEASURE_REPRESENTATION_ITEM()MEASURE_WITH_UNIT(LENGTH_MEASURE({v}),#12)REPRESENTATION_ITEM('{role}'));\n"
            )
        };
        let mut extra = String::new();
        extra += "#1001=SHAPE_ASPECT('top','',#4,.T.);\n";
        extra += "#1002=GEOMETRIC_ITEM_SPECIFIC_USAGE('','',#1001,#10,#18);\n";
        extra += &measure(1003, "0.05", "magnitude");
        extra += "#1004=DATUM('','',#4,.F.,'A');\n";
        extra += "#1005=DATUM_REFERENCE_COMPARTMENT('','',#4,.F.,#1004,$);\n";
        extra += "#1006=DATUM_SYSTEM('','',#4,.F.,(#1005));\n";
        extra += "#1007=(GEOMETRIC_TOLERANCE('pos','',#1003,#1001)GEOMETRIC_TOLERANCE_WITH_DATUM_REFERENCE((#1006))POSITION_TOLERANCE());\n";
        extra += "#1008=FLATNESS_TOLERANCE('flat','',#1003,#1001);\n";
        extra += "#1009=SHAPE_ASPECT('','',#4,.T.);\n";
        extra += "#1010=DIMENSIONAL_SIZE(#1001,'diameter');\n";
        extra += "#1011=SHAPE_DIMENSION_REPRESENTATION('',(#1012),#11);\n";
        extra += &measure(1012, "12.5", "nominal value");
        extra += "#1013=DIMENSIONAL_CHARACTERISTIC_REPRESENTATION(#1010,#1011);\n";
        extra += "#1014=TOLERANCE_VALUE(#1015,#1016);\n";
        extra += &measure(1015, "-0.1", "lower limit");
        extra += &measure(1016, "0.2", "upper limit");
        extra += "#1017=PLUS_MINUS_TOLERANCE(#1014,#1010);\n";
        extra += "#1018=FLATNESS_TOLERANCE('','',#1003,#1009);\n";
        let at = base.rfind("ENDSEC;").unwrap();
        let text = format!("{}{extra}{}", &base[..at], &base[at..]);
        let model = read(&text).unwrap();
        assert_eq!(model.unplaced, 1, "the aspect without a point");
        let notes: Vec<&str> = model
            .products
            .values()
            .flat_map(|p| p.notes.iter().map(|(t, _)| t.as_str()))
            .collect();
        assert_eq!(
            notes,
            ["position 0.05 | A", "flatness 0.05", "Ø12.5 +0.2/-0.1"]
        );
        let mut back = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        let storage = Arc::new(crate::io::MemoryStorage::new());
        back.set_storage(storage.clone());
        crate::io::Storage::write(
            storage.as_ref(),
            &crate::io::StorageKey::new("b.step"),
            text.as_bytes(),
        )
        .unwrap();
        let out = back
            .execute_erased("import", json!({ "path": "b.step" }))
            .unwrap();
        assert!(out.to_string().contains("W2004"), "{out}");
        let doc = back.document();
        let texts: Vec<String> = doc
            .definitions
            .iter()
            .flat_map(|(_, def)| {
                def.entities
                    .texts()
                    .map(|(_, t)| t.text.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(texts.len(), 3);
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
