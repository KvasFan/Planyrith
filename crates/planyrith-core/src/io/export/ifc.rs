//! Implements docs/09-file-formats.md §5 IFC export: an IFC4 file (the
//! exchange structure's text encoding) with a project, a site, a building
//! and one storey holding one element per top-level group, component
//! instance and the model's loose faces; geometry as triangulated face
//! sets in world millimeters, one per material, each with its surface
//! color (docs/09 Decisions, IFC).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write;

use glam::DVec3;

use crate::document::selection::occurrence_name;
use crate::document::{Document, FrontOrBack, InstancePath};
use crate::ids::MaterialId;
use crate::io::export::scene::collect;
use crate::io::export::{ExportFormat, ExportOptions, ExportReport, ExportScope, Exporter};
use crate::io::{IoError, Progress};

/// The IFC exporter.
#[derive(Debug, Default, Clone, Copy)]
pub struct IfcExporter;

/// The characters of IFC's compressed GUIDs, in value order.
const GUID_CHARS: &[u8; 64] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_$";

/// A 22-character IFC GUID from 128 bits: two bits, then 21 groups of six.
fn guid(bits: [u8; 16]) -> String {
    let n = u128::from_be_bytes(bits);
    let mut out = String::with_capacity(22);
    out.push(GUID_CHARS[(n >> 126) as usize] as char);
    for k in (0..21).rev() {
        out.push(GUID_CHARS[((n >> (6 * k)) & 63) as usize] as char);
    }
    out
}

/// A stable GUID for `what` (the same model exports the same ids).
fn stable_guid(what: &str) -> String {
    let hash = blake3::hash(format!("planyrith ifc\0{what}").as_bytes());
    let mut bits = [0u8; 16];
    bits.copy_from_slice(&hash.as_bytes()[..16]);
    guid(bits)
}

/// A string literal: quotes doubled, backslashes doubled, other than
/// printable ASCII as `\X2\…\X0\` UTF-16 code units.
fn text(s: &str) -> String {
    let mut out = String::from("'");
    let mut wide: Vec<u16> = Vec::new();
    let flush = |out: &mut String, wide: &mut Vec<u16>| {
        if !wide.is_empty() {
            out.push_str("\\X2\\");
            for u in wide.drain(..) {
                let _ = write!(out, "{u:04X}");
            }
            out.push_str("\\X0\\");
        }
    };
    for c in s.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut out, &mut wide);
            match c {
                '\'' => out.push_str("''"),
                '\\' => out.push_str("\\\\"),
                c => out.push(c),
            }
        } else {
            let mut buf = [0u16; 2];
            wide.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut out, &mut wide);
    out.push('\'');
    out
}

/// A real: always with a decimal point.
fn real(v: f64) -> String {
    let s = format!("{}", v + 0.0);
    if s.contains(['.', 'e', 'E']) {
        s
    } else {
        format!("{s}.")
    }
}

/// Numbered entity lines.
struct Data {
    lines: String,
    next: u64,
}

impl Data {
    fn add(&mut self, entity: impl AsRef<str>) -> u64 {
        let id = self.next;
        self.next += 1;
        let _ = writeln!(self.lines, "#{id}={};", entity.as_ref());
        id
    }
}

/// Welded points (with their index by position bits) and triangles.
type Part = (Vec<DVec3>, BTreeMap<[u64; 3], usize>, Vec<[usize; 3]>);

/// One element's geometry: per material, welded points and triangles.
#[derive(Default)]
struct Element {
    name: String,
    key: String,
    /// Its IFC class (`IfcBuildingElementProxy` unless told otherwise).
    class: &'static str,
    /// Its storey's name (`Level 0` unless told otherwise).
    storey: String,
    /// Every container in it is a closed solid (its volume means something).
    closed: bool,
    /// Its tag's name, if any.
    tag: Option<String>,
    /// Further `ifc` attributes, written as properties.
    properties: Vec<(String, crate::document::Value)>,
    /// Per occurrence and material.
    parts: BTreeMap<(usize, MaterialId), Part>,
}

/// The building element classes written, with the attributes that follow
/// `Tag` in each (all IFC4 element classes share the first eight).
const CLASSES: [(&str, &str); 14] = [
    ("IfcBuildingElementProxy", ".NOTDEFINED."),
    ("IfcWall", ".NOTDEFINED."),
    ("IfcSlab", ".NOTDEFINED."),
    ("IfcRoof", ".NOTDEFINED."),
    ("IfcColumn", ".NOTDEFINED."),
    ("IfcBeam", ".NOTDEFINED."),
    ("IfcStair", ".NOTDEFINED."),
    ("IfcRailing", ".NOTDEFINED."),
    ("IfcCovering", ".NOTDEFINED."),
    ("IfcPlate", ".NOTDEFINED."),
    ("IfcMember", ".NOTDEFINED."),
    ("IfcFurniture", ".NOTDEFINED."),
    ("IfcDoor", "$,$,.NOTDEFINED.,.NOTDEFINED.,$"),
    ("IfcWindow", "$,$,.NOTDEFINED.,.NOTDEFINED.,$"),
];

/// The class a tag name stands for: its words (English or Italian,
/// singular or plural) against the classes (docs/09 Decisions, IFC).
fn class_of_tag(name: &str) -> Option<&'static str> {
    let words: Vec<String> = name
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect();
    let table: [(&[&str], &'static str); 13] = [
        (
            &["wall", "walls", "muro", "muri", "parete", "pareti"],
            "IfcWall",
        ),
        (
            &[
                "slab",
                "slabs",
                "floor",
                "floors",
                "solaio",
                "solai",
                "pavimento",
                "pavimenti",
            ],
            "IfcSlab",
        ),
        (
            &["roof", "roofs", "tetto", "tetti", "copertura", "coperture"],
            "IfcRoof",
        ),
        (
            &[
                "column", "columns", "pillar", "pillars", "colonna", "colonne", "pilastro",
                "pilastri",
            ],
            "IfcColumn",
        ),
        (&["beam", "beams", "trave", "travi"], "IfcBeam"),
        (
            &["stair", "stairs", "staircase", "scala", "scale"],
            "IfcStair",
        ),
        (
            &[
                "railing",
                "railings",
                "ringhiera",
                "ringhiere",
                "parapetto",
                "parapetti",
            ],
            "IfcRailing",
        ),
        (
            &[
                "ceiling",
                "ceilings",
                "covering",
                "coverings",
                "controsoffitto",
                "rivestimento",
                "rivestimenti",
            ],
            "IfcCovering",
        ),
        (&["plate", "plates", "lastra", "lastre"], "IfcPlate"),
        (&["member", "members", "elemento"], "IfcMember"),
        (
            &[
                "furniture",
                "furnishing",
                "arredo",
                "arredi",
                "mobile",
                "mobili",
            ],
            "IfcFurniture",
        ),
        (&["door", "doors", "porta", "porte"], "IfcDoor"),
        (&["window", "windows", "finestra", "finestre"], "IfcWindow"),
    ];
    table
        .iter()
        .find(|(keys, _)| words.iter().any(|w| keys.contains(&w.as_str())))
        .map(|(_, class)| *class)
}

/// The entities holding the group or instance a path ends at.
fn entity_at<'d>(
    doc: &'d Document,
    path: &InstancePath,
) -> Option<(&'d crate::document::Entities, crate::ids::EntityId)> {
    let last = *path.steps.last()?;
    let (container, _) = doc.resolve_container(&path.parent()?).ok()?;
    let entities = doc.container(container)?;
    Some((entities, entities.key_to_id(last)?))
}

/// The class of a top-level entity: an `ifc`/`class` attribute naming one
/// of [`CLASSES`] (with or without the `Ifc` prefix, any case), else its
/// tag's name, else the proxy.
fn class_of(doc: &Document, path: &InstancePath) -> &'static str {
    let proxy = CLASSES[0].0;
    let Some((model, id)) = entity_at(doc, path) else {
        return proxy;
    };
    if let Some(crate::document::Value::Str(text)) =
        model.attrs(id).and_then(|a| a.get("ifc", "class"))
    {
        let wanted = text.trim().to_lowercase();
        let wanted = wanted.strip_prefix("ifc").unwrap_or(&wanted).to_string();
        if let Some((class, _)) = CLASSES
            .iter()
            .find(|(c, _)| c[3..].to_lowercase() == wanted)
        {
            return class;
        }
    }
    model
        .tag_of(id)
        .and_then(|t| doc.tags.get(t))
        .and_then(|t| class_of_tag(&t.name))
        .unwrap_or(proxy)
}

/// The tag name and the `ifc` attributes other than `class` and `storey`
/// of a top-level entity.
fn tag_and_properties(
    doc: &Document,
    path: &InstancePath,
) -> (Option<String>, Vec<(String, crate::document::Value)>) {
    let Some((model, id)) = entity_at(doc, path) else {
        return (None, Vec::new());
    };
    let tag = model
        .tag_of(id)
        .and_then(|t| doc.tags.get(t))
        .map(|t| t.name.clone());
    let properties = model
        .attrs(id)
        .and_then(|a| a.dictionary("ifc"))
        .map(|d| {
            d.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "class" | "storey"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    (tag, properties)
}

/// A property's value as an IFC measure, if it has one.
fn property_value(value: &crate::document::Value) -> Option<String> {
    use crate::document::Value;
    Some(match value {
        Value::Bool(b) => format!("IFCBOOLEAN(.{}.)", if *b { "T" } else { "F" }),
        Value::Int(i) => format!("IFCINTEGER({i})"),
        Value::Float(f) if f.is_finite() => format!("IFCREAL({})", real(*f)),
        Value::Length(l) => format!("IFCLENGTHMEASURE({})", real(l.as_mm())),
        Value::Str(label) => format!("IFCLABEL({})", text(label)),
        _ => return None,
    })
}

/// The storey of a top-level entity: its `ifc`/`storey` attribute, else
/// `Level 0`.
fn storey_of(doc: &Document, path: &InstancePath) -> String {
    let named = || {
        let (model, id) = entity_at(doc, path)?;
        match model.attrs(id)?.get("ifc", "storey")? {
            crate::document::Value::Str(text) if !text.trim().is_empty() => {
                Some(text.trim().to_string())
            }
            _ => None,
        }
    };
    named().unwrap_or_else(|| DEFAULT_STOREY.to_string())
}

/// The storey of elements that name none.
const DEFAULT_STOREY: &str = "Level 0";

fn elements(doc: &Document, scope: &ExportScope) -> Vec<Element> {
    let scene = collect(doc, scope);
    let mut order: Vec<InstancePath> = Vec::new();
    let mut by_path: BTreeMap<InstancePath, Element> = BTreeMap::new();
    for (index, occ) in scene.occurrences.iter().enumerate() {
        let Some(mesh) = scene.mesh(occ.container) else {
            continue;
        };
        if mesh.faces.is_empty() {
            continue;
        }
        let element_path = element_path(doc, &occ.path);
        let element = by_path.entry(element_path.clone()).or_insert_with(|| {
            order.push(element_path.clone());
            let (tag, properties) = tag_and_properties(doc, &element_path);
            Element {
                name: occurrence_name(doc, &element_path),
                key: if element_path.is_root() {
                    "model".into()
                } else {
                    element_path
                        .steps
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("/")
                },
                class: class_of(doc, &element_path),
                storey: storey_of(doc, &element_path),
                closed: true,
                tag,
                properties,
                parts: BTreeMap::new(),
            }
        });
        element.closed &= doc
            .container(occ.container)
            .is_some_and(|c| c.mesh().is_solid());
        let flip = occ.world.matrix3.determinant() < 0.0;
        for face in &mesh.faces {
            let material = doc
                .materials
                .resolve(face.material.or(occ.material_override), FrontOrBack::Front);
            let (points, weld, triangles) = element.parts.entry((index, material)).or_default();
            let ids: Vec<usize> = face
                .positions
                .iter()
                .map(|p| {
                    let q = occ.world.transform_point3(*p) + DVec3::ZERO;
                    *weld
                        .entry([q.x.to_bits(), q.y.to_bits(), q.z.to_bits()])
                        .or_insert_with(|| {
                            points.push(q);
                            points.len() - 1
                        })
                })
                .collect();
            for t in &face.triangles {
                let [a, b, c] = t.map(|i| ids[i as usize]);
                triangles.push(if flip { [a, c, b] } else { [a, b, c] });
            }
        }
    }
    order
        .into_iter()
        .filter_map(|k| by_path.remove(&k))
        .collect()
}

/// The element an occurrence belongs to: the first group or instance on
/// its path with `ifc` attributes (an imported building's elements sit in
/// the import's group), else the top-level one.
fn element_path(doc: &Document, path: &InstancePath) -> InstancePath {
    let mut prefix = InstancePath::root();
    for &step in &path.steps {
        prefix = prefix.child(step);
        if entity_at(doc, &prefix)
            .and_then(|(e, id)| e.attrs(id))
            .is_some_and(|a| a.dictionary("ifc").is_some())
        {
            return prefix;
        }
    }
    path.steps
        .first()
        .map_or(InstancePath::root(), |&k| InstancePath::root().child(k))
}

/// The element's quantities (its volume, when closed) and properties
/// (tag, main material, further `ifc` attributes).
fn write_properties(d: &mut Data, doc: &Document, element: &Element, product: u64) {
    let key = &element.key;
    if element.closed {
        // Signed tetrahedra from the origin, mm³ → m³.
        let mm3: f64 = element
            .parts
            .values()
            .flat_map(|(points, _, triangles)| {
                triangles
                    .iter()
                    .map(move |[a, b, c]| points[*a].dot(points[*b].cross(points[*c])) / 6.0)
            })
            .sum();
        if mm3 > 0.0 {
            let volume = d.add(format!(
                "IFCQUANTITYVOLUME('GrossVolume',$,$,{},$)",
                real(mm3 / 1e9)
            ));
            let set = d.add(format!(
                "IFCELEMENTQUANTITY({},$,'BaseQuantities',$,$,(#{volume}))",
                text(&stable_guid(&format!("quantities {key}")))
            ));
            d.add(format!(
                "IFCRELDEFINESBYPROPERTIES({},$,$,$,(#{product}),#{set})",
                text(&stable_guid(&format!("quantities rel {key}")))
            ));
        }
    }
    let mut values: Vec<(String, String)> = Vec::new();
    if let Some(tag) = &element.tag {
        values.push(("Tag".into(), format!("IFCLABEL({})", text(tag))));
    }
    // The material covering the most triangles.
    let mut counts: BTreeMap<MaterialId, usize> = BTreeMap::new();
    for ((_, m), (_, _, t)) in &element.parts {
        *counts.entry(*m).or_default() += t.len();
    }
    if let Some(name) = counts
        .iter()
        .max_by_key(|(_, n)| **n)
        .and_then(|(m, _)| doc.materials.get(*m))
        .map(|m| m.name.clone())
    {
        values.push(("Material".into(), format!("IFCLABEL({})", text(&name))));
    }
    for (k, v) in &element.properties {
        if let Some(value) = property_value(v) {
            values.push((k.clone(), value));
        }
    }
    if values.is_empty() {
        return;
    }
    let ids: Vec<String> = values
        .iter()
        .map(|(name, value)| {
            format!(
                "#{}",
                d.add(format!(
                    "IFCPROPERTYSINGLEVALUE({},$,{value},$)",
                    text(name)
                ))
            )
        })
        .collect();
    let set = d.add(format!(
        "IFCPROPERTYSET({},$,'Planyrith',$,({}))",
        text(&stable_guid(&format!("properties {key}"))),
        ids.join(",")
    ));
    d.add(format!(
        "IFCRELDEFINESBYPROPERTIES({},$,$,$,(#{product}),#{set})",
        text(&stable_guid(&format!("properties rel {key}")))
    ));
}

/// The whole file.
fn file(doc: &Document, elements: &[Element], name: &str) -> (String, usize) {
    let mut d = Data {
        lines: String::new(),
        next: 1,
    };
    let version = env!("CARGO_PKG_VERSION");
    let org = d.add("IFCORGANIZATION($,'Planyrith',$,$,$)");
    let _app = d.add(format!(
        "IFCAPPLICATION(#{org},'{version}','Planyrith','Planyrith')"
    ));
    let mm = d.add("IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.)");
    let area = d.add("IFCSIUNIT(*,.AREAUNIT.,$,.SQUARE_METRE.)");
    let volume = d.add("IFCSIUNIT(*,.VOLUMEUNIT.,$,.CUBIC_METRE.)");
    let angle = d.add("IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.)");
    let units = d.add(format!(
        "IFCUNITASSIGNMENT((#{mm},#{area},#{volume},#{angle}))"
    ));
    let origin = d.add("IFCCARTESIANPOINT((0.,0.,0.))");
    let up = d.add("IFCDIRECTION((0.,0.,1.))");
    let east = d.add("IFCDIRECTION((1.,0.,0.))");
    let axes = d.add(format!("IFCAXIS2PLACEMENT3D(#{origin},#{up},#{east})"));
    let context = d.add(format!(
        "IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-05,#{axes},$)"
    ));
    let body = d.add(format!(
        "IFCGEOMETRICREPRESENTATIONSUBCONTEXT('Body','Model',*,*,*,*,#{context},$,.MODEL_VIEW.,$)"
    ));
    let project = d.add(format!(
        "IFCPROJECT({},$,{},$,$,$,$,(#{context}),#{units})",
        text(&stable_guid("project")),
        text(name)
    ));
    let site_place = d.add(format!("IFCLOCALPLACEMENT($,#{axes})"));
    let site = d.add(format!(
        "IFCSITE({},$,'Site',$,$,#{site_place},$,$,.ELEMENT.,$,$,$,$,$)",
        text(&stable_guid("site"))
    ));
    let building_place = d.add(format!("IFCLOCALPLACEMENT(#{site_place},#{axes})"));
    let building = d.add(format!(
        "IFCBUILDING({},$,'Building',$,$,#{building_place},$,$,.ELEMENT.,$,$,$)",
        text(&stable_guid("building"))
    ));
    // Storeys by name, lowest first; each at its elements' lowest point
    // (the default one at 0 when it holds nothing lower).
    let mut levels: BTreeMap<&str, f64> = BTreeMap::new();
    for e in elements {
        let low = e
            .parts
            .values()
            .flat_map(|(points, ..)| points.iter().map(|p| p.z))
            .fold(f64::INFINITY, f64::min);
        let entry = levels.entry(e.storey.as_str()).or_insert(f64::INFINITY);
        *entry = entry.min(low);
    }
    if levels.is_empty() {
        levels.insert(DEFAULT_STOREY, 0.0);
    }
    let mut ordered: Vec<(&str, f64)> = levels
        .into_iter()
        .map(|(n, z)| (n, if z.is_finite() { z } else { 0.0 }))
        .collect();
    ordered.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(b.0)));
    let mut storeys: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for (name, z) in &ordered {
        let point = d.add(format!("IFCCARTESIANPOINT((0.,0.,{}))", real(*z)));
        let level_axes = d.add(format!("IFCAXIS2PLACEMENT3D(#{point},#{up},#{east})"));
        let place = d.add(format!(
            "IFCLOCALPLACEMENT(#{building_place},#{level_axes})"
        ));
        let storey = d.add(format!(
            "IFCBUILDINGSTOREY({},$,{},$,$,#{place},$,$,.ELEMENT.,{})",
            text(&stable_guid(&format!("storey {name}"))),
            text(name),
            real(*z)
        ));
        storeys.insert(name, (storey, place));
    }
    let storey_list: Vec<String> = ordered
        .iter()
        .filter_map(|(n, _)| storeys.get(n).map(|(s, _)| format!("#{s}")))
        .collect();
    d.add(format!(
        "IFCRELAGGREGATES({},$,$,$,#{project},(#{site}))",
        text(&stable_guid("project-site"))
    ));
    d.add(format!(
        "IFCRELAGGREGATES({},$,$,$,#{site},(#{building}))",
        text(&stable_guid("site-building"))
    ));
    d.add(format!(
        "IFCRELAGGREGATES({},$,$,$,#{building},({}))",
        text(&stable_guid("building-storey")),
        storey_list.join(",")
    ));
    // One surface style per material.
    let mut styles: BTreeMap<MaterialId, u64> = BTreeMap::new();
    let mut products: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut triangles_total = 0;
    for element in elements {
        let mut items = Vec::new();
        for ((_, material), (points, _, triangles)) in &element.parts {
            if triangles.is_empty() {
                continue;
            }
            triangles_total += triangles.len();
            let coords: Vec<String> = points
                .iter()
                .map(|p| format!("({},{},{})", real(p.x), real(p.y), real(p.z)))
                .collect();
            let list = d.add(format!("IFCCARTESIANPOINTLIST3D(({}),$)", coords.join(",")));
            let index: Vec<String> = triangles
                .iter()
                .map(|[a, b, c]| format!("({},{},{})", a + 1, b + 1, c + 1))
                .collect();
            let set = d.add(format!(
                "IFCTRIANGULATEDFACESET(#{list},$,$,({}),$)",
                index.join(",")
            ));
            let style = match styles.get(material) {
                Some(&s) => s,
                None => {
                    let m = doc.materials.get(*material);
                    let (rgb, opacity, label) = m
                        .map_or(([255, 255, 255], 1.0, "Default".to_string()), |m| {
                            ([m.color.r, m.color.g, m.color.b], m.opacity, m.name.clone())
                        });
                    let c = |v: u8| real(f64::from(v) / 255.0);
                    let colour = d.add(format!(
                        "IFCCOLOURRGB($,{},{},{})",
                        c(rgb[0]),
                        c(rgb[1]),
                        c(rgb[2])
                    ));
                    let shading = d.add(format!(
                        "IFCSURFACESTYLESHADING(#{colour},{})",
                        real(f64::from(1.0 - opacity.clamp(0.0, 1.0)))
                    ));
                    let s = d.add(format!(
                        "IFCSURFACESTYLE({},.BOTH.,(#{shading}))",
                        text(&label)
                    ));
                    styles.insert(*material, s);
                    s
                }
            };
            d.add(format!("IFCSTYLEDITEM(#{set},(#{style}),$)"));
            items.push(format!("#{set}"));
        }
        if items.is_empty() {
            continue;
        }
        let shape = d.add(format!(
            "IFCSHAPEREPRESENTATION(#{body},'Body','Tessellation',({}))",
            items.join(",")
        ));
        let definition = d.add(format!("IFCPRODUCTDEFINITIONSHAPE($,$,(#{shape}))"));
        let Some(&(_, storey_place)) = storeys.get(element.storey.as_str()) else {
            continue;
        };
        // Element points are in world millimeters: the placement undoes
        // the storey's elevation.
        let level = ordered
            .iter()
            .find(|(n, _)| *n == element.storey)
            .map_or(0.0, |(_, z)| *z);
        let back = d.add(format!("IFCCARTESIANPOINT((0.,0.,{}))", real(-level)));
        let back_axes = d.add(format!("IFCAXIS2PLACEMENT3D(#{back},#{up},#{east})"));
        let place = d.add(format!("IFCLOCALPLACEMENT(#{storey_place},#{back_axes})"));
        let tail = CLASSES
            .iter()
            .find(|(c, _)| *c == element.class)
            .map_or(".NOTDEFINED.", |(_, t)| *t);
        let product = d.add(format!(
            "{}({},$,{},$,$,#{place},#{definition},$,{tail})",
            element.class.to_uppercase(),
            text(&stable_guid(&format!("element {}", element.key))),
            text(&element.name)
        ));
        products
            .entry(element.storey.as_str())
            .or_default()
            .push(format!("#{product}"));
        write_properties(&mut d, doc, element, product);
    }
    for (name, list) in &products {
        if let Some(&(storey, _)) = storeys.get(name) {
            d.add(format!(
                "IFCRELCONTAINEDINSPATIALSTRUCTURE({},$,$,$,({}),#{storey})",
                text(&stable_guid(&format!("contained {name}"))),
                list.join(",")
            ));
        }
    }
    let mut out = String::new();
    let _ = writeln!(out, "ISO-10303-21;");
    let _ = writeln!(out, "HEADER;");
    let _ = writeln!(
        out,
        "FILE_DESCRIPTION(('ViewDefinition [ReferenceView_V1.2]'),'2;1');"
    );
    let _ = writeln!(
        out,
        "FILE_NAME({},'1970-01-01T00:00:00',(''),(''),'Planyrith {version}','Planyrith','');",
        text(name)
    );
    let _ = writeln!(out, "FILE_SCHEMA(('IFC4'));");
    let _ = writeln!(out, "ENDSEC;");
    let _ = writeln!(out, "DATA;");
    out.push_str(&d.lines);
    let _ = writeln!(out, "ENDSEC;");
    let _ = writeln!(out, "END-ISO-10303-21;");
    (out, triangles_total)
}

impl Exporter for IfcExporter {
    fn format(&self) -> ExportFormat {
        ExportFormat::Ifc
    }

    fn export(
        &self,
        doc: &Document,
        scope: &ExportScope,
        opts: &ExportOptions,
        dst: &mut dyn Write,
        progress: &dyn Progress,
    ) -> Result<ExportReport, IoError> {
        let elements = elements(doc, scope);
        progress.report(0.5, "writing the model");
        let name = crate::io::import::source_stem(&opts.destination, "Model");
        let (content, triangles) = file(doc, &elements, &name);
        dst.write_all(content.as_bytes())
            .map_err(|e| IoError::Storage {
                key: "ifc".into(),
                reason: e.to_string(),
            })?;
        Ok(ExportReport {
            bytes_written: content.len() as u64,
            entities: triangles,
            warnings: Vec::new(),
            side_files: Vec::new(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn tags_name_classes_in_two_languages() {
        assert_eq!(class_of_tag("Walls"), Some("IfcWall"));
        assert_eq!(class_of_tag("Muri esterni"), Some("IfcWall"));
        assert_eq!(class_of_tag("Ground floor"), Some("IfcSlab"));
        assert_eq!(class_of_tag("Finestre"), Some("IfcWindow"));
        assert_eq!(class_of_tag("Untagged"), None);
    }

    #[test]
    fn guids_texts_and_reals_follow_the_encoding() {
        assert_eq!(guid([0; 16]), "0000000000000000000000");
        assert_eq!(guid([0xFF; 16]), "3$$$$$$$$$$$$$$$$$$$$$");
        assert_eq!(stable_guid("a"), stable_guid("a"));
        assert_ne!(stable_guid("a"), stable_guid("b"));
        assert_eq!(stable_guid("x").len(), 22);
        assert_eq!(text("O'Brien \\ ok"), "'O''Brien \\\\ ok'");
        assert_eq!(text("Città"), "'Citt\\X2\\00E0\\X0\\'");
        assert_eq!(real(3.0), "3.");
        assert_eq!(real(-0.0), "0.");
        assert_eq!(real(2.5), "2.5");
    }
}
