//! Implements docs/05-modeling-tools.md §8.3 `mass-properties`, §8.4
//! `interference-check` and §8.5 `cut-list`: measurements of the visible
//! objects that change nothing (milestone 1.8).
//!
//! Volumes, centers and inertias come from the divergence theorem: each
//! closed object's surface is triangulated and every triangle, with a
//! reference point near the object, bounds a tetrahedron whose exact
//! integrals are summed (signed, so holes and hollows count right).

use std::collections::BTreeMap;

use glam::{DAffine3, DMat3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::solid_check::objects;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::selection::occurrence_name;
use crate::document::{ContainerRef, Document, FrontOrBack, InstancePath, Query};
use crate::geom::boolean::{BooleanOp, boolean};
use crate::geom::solid_check::diagnose;
use crate::geom::tessellate::tessellate_face;
use crate::geom::{GeometryError, HalfEdgeMesh};
use crate::io::render_mesh::DrawItem;
use crate::math::Aabb;
use crate::tolerance::Tolerances;

/// Cubic millimeters in a cubic meter.
const MM3_PER_M3: f64 = 1e9;

/// Exact integrals of a closed surface's interior.
#[derive(Debug, Clone, Copy, Default)]
struct Integrals {
    /// Volume.
    v: f64,
    /// First moments (∫x, ∫y, ∫z).
    first: DVec3,
    /// ∫x², ∫y², ∫z².
    sq: DVec3,
    /// ∫xy, ∫yz, ∫zx.
    cross: DVec3,
    /// Surface area.
    area: f64,
}

impl Integrals {
    /// Adds the tetrahedron (0, a, b, c), points relative to the reference.
    fn add(&mut self, a: DVec3, b: DVec3, c: DVec3) {
        let v = a.dot(b.cross(c)) / 6.0;
        self.v += v;
        self.first += v * (a + b + c) / 4.0;
        let sq = |i: usize| {
            let (x, y, z) = (a[i], b[i], c[i]);
            x * x + y * y + z * z + x * y + x * z + y * z
        };
        self.sq += v / 10.0 * DVec3::new(sq(0), sq(1), sq(2));
        let cr = |i: usize, j: usize| {
            2.0 * (a[i] * a[j] + b[i] * b[j] + c[i] * c[j])
                + a[i] * b[j]
                + b[i] * a[j]
                + a[i] * c[j]
                + c[i] * a[j]
                + b[i] * c[j]
                + c[i] * b[j]
        };
        self.cross += v / 20.0 * DVec3::new(cr(0, 1), cr(1, 2), cr(2, 0));
        self.area += 0.5 * (b - a).cross(c - a).length();
    }
}

/// Mass properties of one object or of all of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MassProperties {
    /// The occurrence (empty for the total).
    pub path: InstancePath,
    /// Its name ("Total" for the total).
    pub name: String,
    /// Volume, mm³.
    pub volume_mm3: f64,
    /// Surface area, mm².
    pub area_mm2: f64,
    /// Center of the volume (or of the mass, for the total when every mass
    /// is known), world millimeters.
    pub center_mm: [f64; 3],
    /// Density used, kg/m³.
    pub density_kg_m3: Option<f64>,
    /// Mass, kg.
    pub mass_kg: Option<f64>,
    /// Inertia tensor about the center, world axes, kg·mm².
    pub inertia_kg_mm2: Option<[[f64; 3]; 3]>,
    /// Principal moments of inertia, smallest first, kg·mm².
    pub principal_kg_mm2: Option<[f64; 3]>,
}

/// The eigenvalues of a symmetric 3×3 matrix, ascending (Jacobi rotations).
fn eigenvalues(m: DMat3) -> [f64; 3] {
    let mut a = [
        [m.x_axis.x, m.y_axis.x, m.z_axis.x],
        [m.x_axis.y, m.y_axis.y, m.z_axis.y],
        [m.x_axis.z, m.y_axis.z, m.z_axis.z],
    ];
    for _ in 0..64 {
        let mut off = 0.0;
        for (i, j) in [(0, 1), (0, 2), (1, 2)] {
            off += a[i][j] * a[i][j];
        }
        let scale = a[0][0].abs() + a[1][1].abs() + a[2][2].abs();
        if off <= 1e-30 * scale.max(1e-300) * scale.max(1e-300) {
            break;
        }
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q].abs() < 1e-300 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let t = if theta == 0.0 { 1.0 } else { t };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            let mut b = a;
            for k in 0..3 {
                b[k][p] = c * a[k][p] - s * a[k][q];
                b[k][q] = s * a[k][p] + c * a[k][q];
            }
            let mut d = b;
            for k in 0..3 {
                d[p][k] = c * b[p][k] - s * b[q][k];
                d[q][k] = s * b[p][k] + c * b[q][k];
            }
            a = d;
        }
    }
    let mut e = [a[0][0], a[1][1], a[2][2]];
    e.sort_by(f64::total_cmp);
    e
}

/// The inertia tensor about the origin from the integrals, per unit density.
fn inertia(i: &Integrals) -> DMat3 {
    let (xx, yy, zz) = (i.sq.x, i.sq.y, i.sq.z);
    let (xy, yz, zx) = (i.cross.x, i.cross.y, i.cross.z);
    DMat3::from_cols(
        DVec3::new(yy + zz, -xy, -zx),
        DVec3::new(-xy, xx + zz, -yz),
        DVec3::new(-zx, -yz, xx + yy),
    )
}

/// `I` moved from the origin to `c` for mass `m` (parallel axes).
fn about(i: DMat3, m: f64, c: DVec3) -> DMat3 {
    let shift = DMat3::from_diagonal(DVec3::splat(c.length_squared()))
        - DMat3::from_cols(c * c.x, c * c.y, c * c.z);
    i - shift * m
}

/// The matrix by rows (`-0` written as `0`).
fn rows(m: DMat3) -> [[f64; 3]; 3] {
    [
        [m.x_axis.x, m.y_axis.x, m.z_axis.x],
        [m.x_axis.y, m.y_axis.y, m.z_axis.y],
        [m.x_axis.z, m.y_axis.z, m.z_axis.z],
    ]
    .map(|row| row.map(|x| x + 0.0))
}

/// The integrals of a closed mesh placed by `world`, about `reference`,
/// with the area its faces cover per front material.
fn integrate(
    mesh: &HalfEdgeMesh,
    world: &DAffine3,
    reference: DVec3,
    tol: &Tolerances,
) -> Result<(Integrals, BTreeMap<Option<crate::ids::MaterialId>, f64>), GeometryError> {
    let flip = world.matrix3.determinant() < 0.0;
    let mut out = Integrals::default();
    let mut by_material = BTreeMap::new();
    for (f, face) in mesh.faces() {
        let t = tessellate_face(mesh, f, tol)?;
        let before = out.area;
        for tri in &t.triangles {
            let p = |k: usize| world.transform_point3(t.positions[tri[k] as usize]) - reference;
            let (a, b, c) = if flip {
                (p(0), p(2), p(1))
            } else {
                (p(0), p(1), p(2))
            };
            out.add(a, b, c);
        }
        *by_material.entry(face.sides.material_front).or_insert(0.0) += out.area - before;
    }
    Ok((out, by_material))
}

/// The density of the material covering most of the surface among those
/// with one (faces with the default material take the occurrence's
/// override), else `fallback`.
fn density_of(
    doc: &Document,
    item: &DrawItem,
    by_material: &BTreeMap<Option<crate::ids::MaterialId>, f64>,
    fallback: Option<f64>,
) -> Option<f64> {
    let mut best: Option<(f64, f64)> = None;
    for (&m, &area) in by_material {
        let id = m.or(item.material_override);
        let id = doc.materials.resolve(id, FrontOrBack::Front);
        if let Some(d) = doc.materials.get(id).and_then(|m| m.density)
            && best.is_none_or(|(a, _)| area > a)
        {
            best = Some((area, d));
        }
    }
    best.map(|(_, d)| d).or(fallback)
}

/// `mass-properties` parameters (docs/05 §8.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MassPropertiesParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Only the objects these entities belong to (every visible closed
    /// object by default).
    #[serde(default)]
    pub targets: Option<Query>,
    /// Include hidden entities and tags.
    #[serde(default)]
    pub include_hidden: bool,
    /// Density, kg/m³, for objects whose materials give none.
    #[serde(default)]
    pub density: Option<f64>,
}

/// `mass-properties` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MassPropertiesOutput {
    /// Each closed object.
    pub objects: Vec<MassProperties>,
    /// All of them together.
    pub total: MassProperties,
    /// Objects left out because they are not closed solids.
    pub skipped: Vec<String>,
}

/// The `mass-properties` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct MassPropertiesCommand;

impl Command for MassPropertiesCommand {
    type Params = MassPropertiesParams;
    type Output = MassPropertiesOutput;

    fn name(&self) -> &'static str {
        "mass-properties"
    }

    fn validate(&self, _: &Document, p: &MassPropertiesParams) -> Result<(), CommandError> {
        if p.density.is_some_and(|d| !(d.is_finite() && d > 0.0)) {
            return Err(CommandError::OutOfRange {
                name: "density".into(),
                reason: "must be a positive number of kg/m³".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MassPropertiesParams,
    ) -> Result<MassPropertiesOutput, CommandError> {
        let items = objects(tx, &p.common, p.targets.as_ref(), p.include_hidden)?;
        let doc = tx.document();
        let tol = *tx.tolerances();
        let mut solid: BTreeMap<ContainerRef, bool> = BTreeMap::new();
        let mut out = Vec::new();
        let mut skipped = Vec::new();
        // Sums for the total, about the world origin.
        let mut all = Integrals::default();
        let (mut mass_sum, mut first_mass, mut inertia_sum) = (0.0, DVec3::ZERO, DMat3::ZERO);
        let mut every_mass = true;
        for item in &items {
            let Some(entities) = doc.container(item.container) else {
                continue;
            };
            let mesh = entities.mesh();
            let closed = match solid.get(&item.container) {
                Some(c) => *c,
                None => {
                    let c = diagnose(mesh, &tol)?.is_printable();
                    solid.insert(item.container, c);
                    c
                }
            };
            let name = occurrence_name(doc, &item.path);
            if !closed {
                skipped.push(name);
                continue;
            }
            let mut bb = Aabb::EMPTY;
            for (_, v) in mesh.vertices() {
                bb = bb.including(item.world.transform_point3(v.position));
            }
            let reference = bb.center();
            let (i, by_material) = integrate(mesh, &item.world, reference, &tol)?;
            if i.v <= 0.0 {
                skipped.push(name);
                continue;
            }
            let centroid = i.first / i.v;
            let density = density_of(doc, item, &by_material, p.density);
            let mass = density.map(|d| d * i.v / MM3_PER_M3);
            let tensor = mass.map(|m| about(inertia(&i) * (m / i.v), m, centroid));
            // Totals about the origin.
            all.v += i.v;
            all.area += i.area;
            all.first += i.first + reference * i.v;
            match (mass, tensor) {
                (Some(m), Some(t)) => {
                    let c = reference + centroid;
                    mass_sum += m;
                    first_mass += c * m;
                    // Back from the center to the origin.
                    inertia_sum += t
                        + (DMat3::from_diagonal(DVec3::splat(c.length_squared()))
                            - DMat3::from_cols(c * c.x, c * c.y, c * c.z))
                            * m;
                }
                _ => every_mass = false,
            }
            out.push(MassProperties {
                path: item.path.clone(),
                name,
                volume_mm3: i.v,
                area_mm2: i.area,
                center_mm: (reference + centroid).to_array(),
                density_kg_m3: density,
                mass_kg: mass,
                inertia_kg_mm2: tensor.map(rows),
                principal_kg_mm2: tensor.map(eigenvalues),
            });
        }
        let total = if out.is_empty() {
            MassProperties {
                path: InstancePath::default(),
                name: "Total".into(),
                volume_mm3: 0.0,
                area_mm2: 0.0,
                center_mm: [0.0; 3],
                density_kg_m3: None,
                mass_kg: None,
                inertia_kg_mm2: None,
                principal_kg_mm2: None,
            }
        } else {
            let known = every_mass && mass_sum > 0.0;
            let center = if known {
                first_mass / mass_sum
            } else {
                all.first / all.v
            };
            let tensor = known.then(|| about(inertia_sum, mass_sum, center));
            MassProperties {
                path: InstancePath::default(),
                name: "Total".into(),
                volume_mm3: all.v,
                area_mm2: all.area,
                center_mm: center.to_array(),
                density_kg_m3: known.then(|| mass_sum * MM3_PER_M3 / all.v),
                mass_kg: known.then_some(mass_sum),
                inertia_kg_mm2: tensor.map(rows),
                principal_kg_mm2: tensor.map(eigenvalues),
            }
        };
        Ok(MassPropertiesOutput {
            objects: out,
            total,
            skipped,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "mass-properties".into(),
            label: "Mass Properties".into(),
            category: CommandCategory::Solid,
            view_only: true,
            steps: Vec::new(),
        }
    }
}

// ----- interference ---------------------------------------------------------------------

/// `interference-check` parameters (docs/05 §8.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InterferenceParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Only the objects these entities belong to (every visible closed
    /// object by default).
    #[serde(default)]
    pub targets: Option<Query>,
    /// Include hidden entities and tags.
    #[serde(default)]
    pub include_hidden: bool,
}

/// Two objects that overlap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Interference {
    /// The first object.
    pub a: InstancePath,
    /// Its name.
    pub a_name: String,
    /// The second object.
    pub b: InstancePath,
    /// Its name.
    pub b_name: String,
    /// The volume they share, mm³.
    pub volume_mm3: f64,
    /// World bounds of the shared volume.
    pub bounds_mm: [[f64; 3]; 2],
}

/// `interference-check` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InterferenceOutput {
    /// Overlapping pairs, in the order of the objects.
    pub interferences: Vec<Interference>,
    /// Closed objects checked.
    pub checked: usize,
    /// Pairs whose overlap could not be computed (their names).
    pub undecided: Vec<[String; 2]>,
}

/// The `interference-check` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct InterferenceCheck;

impl Command for InterferenceCheck {
    type Params = InterferenceParams;
    type Output = InterferenceOutput;

    fn name(&self) -> &'static str {
        "interference-check"
    }

    fn validate(&self, _: &Document, _: &InterferenceParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: InterferenceParams,
    ) -> Result<InterferenceOutput, CommandError> {
        let items = objects(tx, &p.common, p.targets.as_ref(), p.include_hidden)?;
        let doc = tx.document();
        let tol = *tx.tolerances();
        let mut solids: Vec<(&DrawItem, &HalfEdgeMesh, Aabb)> = Vec::new();
        let mut closed: BTreeMap<ContainerRef, bool> = BTreeMap::new();
        for item in &items {
            let Some(entities) = doc.container(item.container) else {
                continue;
            };
            let mesh = entities.mesh();
            let ok = match closed.get(&item.container) {
                Some(c) => *c,
                None => {
                    let c = diagnose(mesh, &tol)?.is_printable();
                    closed.insert(item.container, c);
                    c
                }
            };
            if !ok {
                continue;
            }
            let mut bb = Aabb::EMPTY;
            for (_, v) in mesh.vertices() {
                bb = bb.including(item.world.transform_point3(v.position));
            }
            solids.push((item, mesh, bb));
        }
        let mut out = InterferenceOutput {
            interferences: Vec::new(),
            checked: solids.len(),
            undecided: Vec::new(),
        };
        for i in 0..solids.len() {
            for j in i + 1..solids.len() {
                let (a, ma, ba) = solids[i];
                let (b, mb, bb) = solids[j];
                let Some(overlap) = ba.intersection(&bb) else {
                    continue;
                };
                if overlap.is_empty() || (overlap.max - overlap.min).min_element() <= tol.length {
                    continue;
                }
                let names = || [occurrence_name(doc, &a.path), occurrence_name(doc, &b.path)];
                match boolean(BooleanOp::Intersect, ma, &a.world, mb, &b.world, &tol) {
                    Ok(r) => {
                        let mut volume = 0.0;
                        let mut bounds = Aabb::EMPTY;
                        for s in &r.solids {
                            volume += s.volume().unwrap_or(0.0).abs();
                            for (_, v) in s.vertices() {
                                bounds = bounds.including(v.position);
                            }
                        }
                        // Touching faces share no volume.
                        if volume > tol.length * tol.length * tol.length * 1e3 && !bounds.is_empty()
                        {
                            let [a_name, b_name] = names();
                            out.interferences.push(Interference {
                                a: a.path.clone(),
                                a_name,
                                b: b.path.clone(),
                                b_name,
                                volume_mm3: volume,
                                bounds_mm: [bounds.min.to_array(), bounds.max.to_array()],
                            });
                        }
                    }
                    Err(_) => out.undecided.push(names()),
                }
            }
        }
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "interference-check".into(),
            label: "Check Interference".into(),
            category: CommandCategory::Solid,
            view_only: true,
            steps: Vec::new(),
        }
    }
}

// ----- cut list -------------------------------------------------------------------------

/// `cut-list` parameters (docs/05 §8.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CutListParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Only the parts these entities belong to (every visible group and
    /// component with faces by default).
    #[serde(default)]
    pub targets: Option<Query>,
    /// Include hidden entities and tags.
    #[serde(default)]
    pub include_hidden: bool,
}

/// One line of the cut list: identical parts together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CutListRow {
    /// The part's name.
    pub name: String,
    /// How many.
    pub count: u32,
    /// Its longest side, mm.
    pub length_mm: f64,
    /// The middle side, mm.
    pub width_mm: f64,
    /// The shortest side, mm.
    pub thickness_mm: f64,
    /// The material covering most of it.
    pub material: String,
    /// Volume of one, mm³ (closed parts only).
    pub volume_mm3: Option<f64>,
}

/// `cut-list` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CutListOutput {
    /// Rows, longest parts first.
    pub rows: Vec<CutListRow>,
    /// Parts in all.
    pub parts: u32,
}

/// The `cut-list` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct CutList;

impl Command for CutList {
    type Params = CutListParams;
    type Output = CutListOutput;

    fn name(&self) -> &'static str {
        "cut-list"
    }

    fn validate(&self, _: &Document, _: &CutListParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: CutListParams,
    ) -> Result<CutListOutput, CommandError> {
        let items = objects(tx, &p.common, p.targets.as_ref(), p.include_hidden)?;
        let doc = tx.document();
        let tol = *tx.tolerances();
        // (name, rounded sizes, material) → row.
        let mut rows: BTreeMap<(String, [i64; 3], String), CutListRow> = BTreeMap::new();
        for item in items.iter().filter(|i| !i.path.is_root()) {
            let Some(entities) = doc.container(item.container) else {
                continue;
            };
            let mesh = entities.mesh();
            // The part's own axes: its contents' bounds, stretched by the
            // placement's scale along each axis.
            let mut lo = DVec3::splat(f64::INFINITY);
            let mut hi = DVec3::splat(f64::NEG_INFINITY);
            for (_, v) in mesh.vertices() {
                lo = lo.min(v.position);
                hi = hi.max(v.position);
            }
            if !lo.x.is_finite() {
                continue;
            }
            let m = item.world.matrix3;
            let size =
                (hi - lo) * DVec3::new(m.x_axis.length(), m.y_axis.length(), m.z_axis.length());
            let mut sides = size.to_array();
            sides.sort_by(|a, b| b.total_cmp(a));
            let mut area_by: BTreeMap<String, f64> = BTreeMap::new();
            for (f, face) in mesh.faces() {
                let id = doc.materials.resolve(
                    face.sides.material_front.or(item.material_override),
                    FrontOrBack::Front,
                );
                let name = doc
                    .materials
                    .get(id)
                    .map(|m| m.name.clone())
                    .unwrap_or_default();
                *area_by.entry(name).or_insert(0.0) += mesh.face_area(f).unwrap_or(0.0);
            }
            let material = area_by
                .iter()
                .max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(a.0)))
                .map(|(n, _)| n.clone())
                .unwrap_or_default();
            let volume = diagnose(mesh, &tol)?
                .is_printable()
                .then(|| mesh.volume().ok())
                .flatten()
                .map(|v| v.abs() * m.determinant().abs());
            let name = occurrence_name(doc, &item.path);
            // Sizes compared to a tenth of a millimeter.
            let key_sides = sides.map(|s| (s * 10.0).round() as i64);
            let row = rows
                .entry((name.clone(), key_sides, material.clone()))
                .or_insert(CutListRow {
                    name,
                    count: 0,
                    length_mm: sides[0],
                    width_mm: sides[1],
                    thickness_mm: sides[2],
                    material,
                    volume_mm3: volume,
                });
            row.count += 1;
        }
        let mut rows: Vec<CutListRow> = rows.into_values().collect();
        rows.sort_by(|a, b| {
            b.length_mm
                .total_cmp(&a.length_mm)
                .then(b.width_mm.total_cmp(&a.width_mm))
                .then(a.name.cmp(&b.name))
        });
        let parts = rows.iter().map(|r| r.count).sum();
        Ok(CutListOutput { rows, parts })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "cut-list".into(),
            label: "Cut List".into(),
            category: CommandCategory::Solid,
            view_only: true,
            steps: Vec::new(),
        }
    }
}

/// Registers the measuring commands.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(MassPropertiesCommand);
    registry.register(InterferenceCheck);
    registry.register(CutList);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::commands::{CommandRegistry, Engine};
    use serde_json::json;
    use std::sync::Arc;

    fn engine() -> Engine {
        Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()))
    }

    fn block(e: &mut Engine, at: [f64; 3], size: [f64; 3], name: &str) {
        e.execute_erased(
            "rectangle",
            json!({ "corner_a": at, "corner_b": [at[0] + size[0], at[1] + size[1], at[2]] }),
        )
        .unwrap();
        e.execute_erased(
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": size[2] }),
        )
        .unwrap();
        e.execute_erased(
            "make-group",
            json!({ "targets": "edge & !group | face & !group", "name": name }),
        )
        .unwrap();
    }

    #[test]
    fn eigenvalues_of_a_rotated_diagonal() {
        let r = DMat3::from_rotation_z(0.3) * DMat3::from_rotation_x(0.7);
        let m = r * DMat3::from_diagonal(DVec3::new(1.0, 5.0, 9.0)) * r.transpose();
        let e = eigenvalues(m);
        for (got, want) in e.iter().zip([1.0, 5.0, 9.0]) {
            assert!((got - want).abs() < 1e-9, "{e:?}");
        }
    }

    #[test]
    fn a_steel_block_has_the_textbook_mass_and_inertia() {
        let mut e = engine();
        e.execute_erased(
            "material-create",
            json!({ "name": "Steel", "color": "#888888", "density": 7850 }),
        )
        .unwrap();
        block(&mut e, [100.0, 0.0, 0.0], [200.0, 100.0, 50.0], "Block");
        e.execute_erased(
            "material-apply",
            json!({ "targets": "group", "material": "Steel" }),
        )
        .unwrap();
        let out = e.execute_erased("mass-properties", json!({})).unwrap();
        let o = &out["objects"][0];
        let v = 200.0 * 100.0 * 50.0;
        assert!((o["volume_mm3"].as_f64().unwrap() - v).abs() < 1e-6);
        let area = 2.0 * (200.0 * 100.0 + 200.0 * 50.0 + 100.0 * 50.0);
        assert!((o["area_mm2"].as_f64().unwrap() - area).abs() < 1e-6);
        let c = o["center_mm"].as_array().unwrap();
        assert!((c[0].as_f64().unwrap() - 200.0).abs() < 1e-9);
        assert!((c[1].as_f64().unwrap() - 50.0).abs() < 1e-9);
        assert!((c[2].as_f64().unwrap() - 25.0).abs() < 1e-9);
        let m = 7850.0 * v / 1e9;
        assert!((o["mass_kg"].as_f64().unwrap() - m).abs() < 1e-12);
        // A box: Ixx = m (b² + c²) / 12.
        let i = o["inertia_kg_mm2"].as_array().unwrap();
        let ixx = i[0][0].as_f64().unwrap();
        assert!((ixx - m * (100.0f64.powi(2) + 50.0f64.powi(2)) / 12.0).abs() < 1e-6);
        assert!(i[0][1].as_f64().unwrap().abs() < 1e-6);
        // Without a density, a default can be given.
        let mut e = engine();
        block(&mut e, [0.0, 0.0, 0.0], [10.0, 10.0, 10.0], "Cube");
        let out = e.execute_erased("mass-properties", json!({})).unwrap();
        assert!(out["objects"][0]["mass_kg"].is_null());
        let out = e
            .execute_erased("mass-properties", json!({ "density": 1000 }))
            .unwrap();
        assert!((out["total"]["mass_kg"].as_f64().unwrap() - 0.001).abs() < 1e-12);
    }

    #[test]
    fn overlapping_blocks_interfere_and_touching_ones_do_not() {
        let mut e = engine();
        block(&mut e, [0.0, 0.0, 0.0], [100.0, 100.0, 100.0], "A");
        block(&mut e, [50.0, 50.0, 50.0], [100.0, 100.0, 100.0], "B");
        block(&mut e, [300.0, 0.0, 0.0], [100.0, 100.0, 100.0], "C");
        block(&mut e, [400.0, 0.0, 0.0], [100.0, 100.0, 100.0], "D");
        let out = e.execute_erased("interference-check", json!({})).unwrap();
        let list = out["interferences"].as_array().unwrap();
        assert_eq!(list.len(), 1, "{out}");
        assert!((list[0]["volume_mm3"].as_f64().unwrap() - 125_000.0).abs() < 1e-3);
        assert_eq!(out["checked"], 4);
    }

    #[test]
    fn identical_boards_share_a_row_of_the_cut_list() {
        let mut e = engine();
        block(&mut e, [0.0, 0.0, 0.0], [800.0, 18.0, 400.0], "Side");
        e.execute_erased(
            "pattern",
            json!({ "targets": "group", "kind": "linear", "count": 2, "spacing": 600 }),
        )
        .unwrap();
        block(&mut e, [0.0, 0.0, 500.0], [600.0, 300.0, 18.0], "Shelf");
        let out = e.execute_erased("cut-list", json!({})).unwrap();
        let rows = out["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{out}");
        assert_eq!(rows[0]["name"], "Side");
        assert_eq!(rows[0]["count"], 2);
        assert!((rows[0]["length_mm"].as_f64().unwrap() - 800.0).abs() < 1e-9);
        assert!((rows[0]["thickness_mm"].as_f64().unwrap() - 18.0).abs() < 1e-9);
        assert_eq!(out["parts"], 3);
    }
}
