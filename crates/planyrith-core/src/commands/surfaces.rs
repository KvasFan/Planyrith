//! Implements docs/05-modeling-tools.md §8.14 (surfaces, milestone 1.15):
//! `surface-fill`, `surface-extend`, `surface-trim` and `surface-knit`.
//!
//! A surface is an open shell of faces, usually a group; its patches are
//! made of flat pieces whose shared edges are soft and smooth, so it shades
//! as one curved surface (docs/03 §4.21).

use std::collections::{BTreeMap, BTreeSet};

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{SideRows, place_geometry, remove_container_entity, targets_of};
use crate::commands::edit::transform::chain;
use crate::commands::fabrication::new_group;
use crate::commands::params::{PointSchema, Target, VectorSchema, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ContainerRef, Document, Query};
use crate::geom::section::face_cut;
use crate::geom::surface::{FillMethod, edge_uses, fill, indexed, pieces, soften};
use crate::geom::tessellate::tessellate_face;
use crate::geom::{EdgeFlags, FaceSides, HalfEdgeMesh, NoDeltas};
use crate::ids::{EdgeId, EntityId, FaceId, Pid, VertexId};
use crate::math::Plane;
use crate::tolerance::Tolerances;
use crate::units::Length;

fn meta(name: &str, label: &str) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        label: label.into(),
        category: CommandCategory::Solid,
        view_only: false,
        steps: Vec::new(),
    }
}

/// Faces a query names, by the container holding them.
pub(crate) struct FaceSet {
    /// The group, or `None` for faces of the edit context.
    pub entity: Option<EntityId>,
    /// Where the faces are.
    pub container: ContainerRef,
    /// Container to edit-context coordinates.
    pub to_target: DAffine3,
    /// The faces.
    pub faces: Vec<FaceId>,
}

/// The faces `q` names: faces of the edit context, and every face of named
/// groups (and of component instances when `instances`).
pub(crate) fn face_sets(
    tx: &Transaction<'_>,
    target: &Target,
    param: &str,
    q: &Query,
    instances: bool,
) -> Result<Vec<FaceSet>, CommandError> {
    let ids = targets_of(tx, target, param, q)?;
    let doc = tx.document();
    let here = doc
        .container(target.container)
        .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?;
    let mut own = Vec::new();
    let mut sets = Vec::new();
    for id in ids {
        match id {
            EntityId::Face(f) => own.push(f),
            EntityId::Group(_) | EntityId::Instance(_) => {
                if matches!(id, EntityId::Instance(_)) && !instances {
                    continue;
                }
                let Some(d) = here.definition_of(id) else {
                    continue;
                };
                let container = ContainerRef::Definition(d);
                let faces: Vec<FaceId> = doc
                    .container(container)
                    .map(|e| e.mesh().faces().map(|(f, _)| f).collect())
                    .unwrap_or_default();
                sets.push(FaceSet {
                    entity: Some(id),
                    container,
                    to_target: here.transform_of(id).unwrap_or(DAffine3::IDENTITY),
                    faces,
                });
            }
            _ => {}
        }
    }
    if !own.is_empty() {
        sets.insert(
            0,
            FaceSet {
                entity: None,
                container: target.container,
                to_target: DAffine3::IDENTITY,
                faces: own,
            },
        );
    }
    if sets.iter().all(|s| s.faces.is_empty()) {
        return Err(CommandError::invalid_param(
            param,
            "name faces, or groups holding faces",
        ));
    }
    Ok(sets)
}

/// A face's loops (outer first), in its container's coordinates.
pub(crate) fn rings(mesh: &HalfEdgeMesh, f: FaceId) -> Result<Vec<Vec<DVec3>>, CommandError> {
    let face = mesh.face_ref(f)?;
    let mut out = vec![mesh.loop_positions(face.outer)?];
    for h in &face.holes {
        out.push(mesh.loop_positions(*h)?);
    }
    Ok(out)
}

/// The area enclosed by a loop of points, mm².
pub(crate) fn ring_area(ring: &[DVec3]) -> f64 {
    let mut a = DVec3::ZERO;
    for (p, q) in ring.iter().zip(ring.iter().cycle().skip(1)) {
        a += p.cross(*q);
    }
    0.5 * a.length()
}

/// The area of a face given by its loops, mm².
pub(crate) fn rings_area(rings: &[Vec<DVec3>]) -> f64 {
    rings
        .iter()
        .enumerate()
        .map(|(i, r)| if i == 0 { ring_area(r) } else { -ring_area(r) })
        .sum()
}

/// A point inside face `f`: the center of its largest triangle.
pub(crate) fn inside_point(
    mesh: &HalfEdgeMesh,
    f: FaceId,
    tol: &Tolerances,
) -> Result<DVec3, CommandError> {
    let t = tessellate_face(mesh, f, tol)?;
    let mut best: Option<(f64, DVec3)> = None;
    for tri in &t.triangles {
        let [a, b, c] = tri.map(|i| t.positions[i as usize]);
        let area = (b - a).cross(c - a).length();
        if best.is_none_or(|(ba, _)| area > ba) {
            best = Some((area, (a + b + c) / 3.0));
        }
    }
    best.map(|(_, p)| p)
        .ok_or_else(|| CommandError::Internal("a face without triangles".into()))
}

/// Edges bordering one face only.
fn open_edges(mesh: &HalfEdgeMesh) -> Result<usize, CommandError> {
    let mut n = 0;
    for (e, _) in mesh.edges() {
        let [a, b] = mesh.edge_faces(e)?;
        if a.is_some() != b.is_some() {
            n += 1;
        }
    }
    Ok(n)
}

/// What a surface command gave.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SurfaceOutput {
    /// The surface group made.
    pub surface: Option<Pid>,
    /// Faces made.
    pub faces: usize,
    /// Their area, mm².
    pub area_mm2: f64,
    /// Edges of the result bordering one face only (its open boundary).
    pub open_edges: usize,
    /// The result closes a volume (`surface-knit`).
    #[serde(default)]
    pub closed: bool,
    /// The enclosed volume when closed, mm³.
    #[serde(default)]
    pub volume_mm3: Option<f64>,
}

// ----- surface-fill ---------------------------------------------------------------------

fn eight() -> u32 {
    8
}

fn surface_name() -> String {
    "Surface".into()
}

/// `surface-fill` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SurfaceFillParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The boundary: edges of the edit context forming one closed chain.
    pub boundary: Query,
    /// Steps along each side (8, at most 200).
    #[serde(default = "eight")]
    pub divisions: u32,
    /// How the boundary is filled.
    #[serde(default)]
    pub method: FillMethod,
    /// The new group's name.
    #[serde(default = "surface_name")]
    pub name: String,
}

/// The `surface-fill` command: a smooth patch spanning a closed boundary.
#[derive(Debug, Default, Clone, Copy)]
pub struct SurfaceFill;

impl Command for SurfaceFill {
    type Params = SurfaceFillParams;
    type Output = SurfaceOutput;

    fn name(&self) -> &'static str {
        "surface-fill"
    }

    fn validate(&self, _: &Document, p: &SurfaceFillParams) -> Result<(), CommandError> {
        if !(1..=200).contains(&p.divisions) {
            return Err(CommandError::OutOfRange {
                name: "divisions".into(),
                reason: "must be 1 to 200".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SurfaceFillParams,
    ) -> Result<SurfaceOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let edges: Vec<EdgeId> = targets_of(tx, &target, "boundary", &p.boundary)?
            .into_iter()
            .filter_map(|id| match id {
                EntityId::Edge(e) => Some(e),
                _ => None,
            })
            .collect();
        let closed_chain =
            || CommandError::invalid_param("boundary", "the edges must form one closed chain");
        let Some(first) = edges.first() else {
            return Err(closed_chain());
        };
        let tol = *tx.tolerances();
        let (mut points, closed) = {
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
                .mesh();
            let near = mesh.position(mesh.edge_endpoints(*first)?.0)?;
            chain(mesh, &edges, near).map_err(|_| closed_chain())?
        };
        if !closed {
            return Err(closed_chain());
        }
        points.pop();
        let polys = fill(&points, p.divisions as usize, p.method, &tol)?;
        let area_mm2 = polys.iter().map(|q| ring_area(q)).sum();
        let faces: Vec<_> = polys
            .into_iter()
            .map(|q| (vec![q], FaceSides::default()))
            .collect();
        let mut piece = indexed(&faces, &tol);
        soften(&mut piece, std::f64::consts::PI);
        let mesh = HalfEdgeMesh::from_indexed(&piece, &tol)?;
        let out = SurfaceOutput {
            surface: None,
            faces: mesh.faces().count(),
            area_mm2,
            open_edges: open_edges(&mesh)?,
            closed: false,
            volume_mm3: None,
        };
        let (pid, _) = new_group(tx, &target, &p.name, mesh)?;
        Ok(SurfaceOutput {
            surface: Some(pid),
            ..out
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("surface-fill", "Fill Surface")
    }
}

// ----- surface-extend -------------------------------------------------------------------

/// `surface-extend` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SurfaceExtendParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Open edges of the edit context, each bordering one face.
    pub edges: Query,
    /// How far the surface goes on beyond them, in each face's plane.
    pub distance: Length,
}

/// The `surface-extend` command: strips that continue faces past their
/// open edges, tangent to them.
#[derive(Debug, Default, Clone, Copy)]
pub struct SurfaceExtend;

impl Command for SurfaceExtend {
    type Params = SurfaceExtendParams;
    type Output = SurfaceOutput;

    fn name(&self) -> &'static str {
        "surface-extend"
    }

    fn validate(&self, _: &Document, p: &SurfaceExtendParams) -> Result<(), CommandError> {
        if !(p.distance.as_mm() > 0.0 && p.distance.as_mm().is_finite()) {
            return Err(CommandError::OutOfRange {
                name: "distance".into(),
                reason: "must be positive".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SurfaceExtendParams,
    ) -> Result<SurfaceOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let edges: Vec<EdgeId> = targets_of(tx, &target, "edges", &p.edges)?
            .into_iter()
            .filter_map(|id| match id {
                EntityId::Edge(e) => Some(e),
                _ => None,
            })
            .collect();
        if edges.is_empty() {
            return Err(CommandError::invalid_param("edges", "name open edges"));
        }
        let tol = *tx.tolerances();
        let distance = p.distance.as_mm();
        // Per edge: its ends in the face's loop order, outward direction and
        // the face's sides.
        let mut sides: Vec<(VertexId, VertexId, DVec3, DVec3, DVec3, FaceSides)> = Vec::new();
        let mut outs: BTreeMap<VertexId, Vec<DVec3>> = BTreeMap::new();
        {
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
                .mesh();
            for e in &edges {
                let edge = mesh
                    .edge(*e)
                    .ok_or_else(|| CommandError::Internal("an edge vanished".into()))?;
                let he = mesh
                    .half_edge(edge.he)
                    .ok_or_else(|| CommandError::Internal("a half-edge vanished".into()))?;
                let tw = mesh
                    .half_edge(he.twin)
                    .ok_or_else(|| CommandError::Internal("a half-edge vanished".into()))?;
                let (a, b, f) = match (he.face, tw.face) {
                    (Some(f), None) => (he.origin, tw.origin, f),
                    (None, Some(f)) => (tw.origin, he.origin, f),
                    _ => {
                        return Err(CommandError::invalid_param(
                            "edges",
                            "each edge must border exactly one face",
                        ));
                    }
                };
                let face = mesh.face_ref(f)?;
                let (pa, pb) = (mesh.position(a)?, mesh.position(b)?);
                let out = (pb - pa).cross(face.plane.normal).normalize_or_zero();
                outs.entry(a).or_default().push(out);
                outs.entry(b).or_default().push(out);
                sides.push((a, b, pa, pb, out, face.sides.clone()));
            }
        }
        let reach = |v: VertexId| -> DVec3 {
            let list = outs.get(&v).map(Vec::as_slice).unwrap_or_default();
            let dir = list.iter().copied().sum::<DVec3>().normalize_or_zero();
            let cos = list
                .iter()
                .map(|o| o.dot(dir))
                .fold(1.0_f64, f64::min)
                .max(0.25);
            dir * (distance / cos)
        };
        let mut faces = Vec::new();
        for (a, b, pa, pb, _, s) in &sides {
            for q in pieces(vec![*pb, *pa, *pa + reach(*a), *pb + reach(*b)], &tol) {
                faces.push((vec![q], s.clone()));
            }
        }
        let area_mm2 = faces.iter().map(|(r, _)| rings_area(r)).sum();
        let mut piece = indexed(&faces, &tol);
        soften(&mut piece, 20f64.to_radians());
        let rows = (
            vec![SideRows::default(); piece.edges.len()],
            vec![SideRows::default(); piece.faces.len()],
        );
        place_geometry(tx, target.container, &piece, rows.0, rows.1)?;
        let open = {
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
                .mesh();
            open_edges(mesh)?
        };
        Ok(SurfaceOutput {
            surface: None,
            faces: piece.faces.len(),
            area_mm2,
            open_edges: open,
            closed: false,
            volume_mm3: None,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("surface-extend", "Extend Surface")
    }
}

// ----- surface-trim ---------------------------------------------------------------------

/// `surface-trim` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SurfaceTrimParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The surface: faces of the edit context, or groups.
    pub surface: Query,
    /// A point of the cutting plane.
    #[schemars(with = "PointSchema")]
    pub point: DVec3,
    /// The plane's normal: what lies on the side it points to is removed.
    #[schemars(with = "VectorSchema")]
    pub normal: DVec3,
}

/// What `surface-trim` did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SurfaceTrimOutput {
    /// Faces cut or removed.
    pub trimmed: usize,
    /// Pieces of cut faces kept.
    pub pieces: usize,
    /// Area removed, mm².
    pub removed_mm2: f64,
}

fn on_segment(x: DVec3, a: DVec3, b: DVec3, tol: &Tolerances) -> bool {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 <= tol.length_sq {
        return x.distance(a) <= tol.length;
    }
    let t = ((x - a).dot(ab) / len2).clamp(0.0, 1.0);
    x.distance(a + ab * t) <= tol.length
}

/// The `surface-trim` command: cuts a surface along a plane and removes
/// the part on one side.
#[derive(Debug, Default, Clone, Copy)]
pub struct SurfaceTrim;

impl Command for SurfaceTrim {
    type Params = SurfaceTrimParams;
    type Output = SurfaceTrimOutput;

    fn name(&self) -> &'static str {
        "surface-trim"
    }

    fn validate(&self, _: &Document, p: &SurfaceTrimParams) -> Result<(), CommandError> {
        if p.normal.try_normalize().is_none() {
            return Err(CommandError::invalid_param("normal", "must be non-zero"));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SurfaceTrimParams,
    ) -> Result<SurfaceTrimOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let tol = *tx.tolerances();
        let point = target.local.transform_point3(p.point);
        let normal = target.local.transform_vector3(p.normal);
        let sets = face_sets(tx, &target, "surface", &p.surface, false)?;
        let mut out = SurfaceTrimOutput {
            trimmed: 0,
            pieces: 0,
            removed_mm2: 0.0,
        };
        for set in sets {
            let inv = crate::geom::intersect::invert(&set.to_target)?;
            let n = (set.to_target.matrix3.transpose() * normal).normalize_or_zero();
            let plane = Plane::from_point_normal(inv.transform_point3(point), n);
            let mut erase = Vec::new();
            let mut touched: Vec<EdgeId> = Vec::new();
            let mut soft: Vec<(DVec3, DVec3)> = Vec::new();
            let mut kept: Vec<(Vec<Vec<DVec3>>, FaceSides)> = Vec::new();
            {
                let mesh = tx
                    .document()
                    .container(set.container)
                    .ok_or_else(|| CommandError::Internal("a container vanished".into()))?
                    .mesh();
                for f in &set.faces {
                    let r = rings(mesh, *f)?;
                    let s: Vec<f64> = r
                        .iter()
                        .flatten()
                        .map(|v| plane.signed_distance(*v))
                        .collect();
                    if s.iter().all(|x| *x <= tol.length) {
                        continue;
                    }
                    let face = mesh.face_ref(*f)?;
                    erase.push(*f);
                    let mut loops = vec![face.outer];
                    loops.extend(face.holes.iter().copied());
                    for l in loops {
                        for h in mesh.loop_half_edges(l)? {
                            let Some(he) = mesh.half_edge(h) else {
                                continue;
                            };
                            touched.push(he.edge);
                            if mesh
                                .edge(he.edge)
                                .is_some_and(|e| e.flags.contains(EdgeFlags::SOFT))
                            {
                                let (a, b) = mesh.edge_endpoints(he.edge)?;
                                soft.push((mesh.position(a)?, mesh.position(b)?));
                            }
                        }
                    }
                    let area = rings_area(&r);
                    if s.iter().all(|x| *x >= -tol.length) {
                        out.removed_mm2 += area;
                        continue;
                    }
                    // A copy of the face, cut along the plane.
                    let sides = face.sides.clone();
                    let mut tmp =
                        HalfEdgeMesh::from_indexed(&indexed(&[(r, sides.clone())], &tol), &tol)?;
                    let Some(tf) = tmp.faces().map(|(g, _)| g).next() else {
                        continue;
                    };
                    for (a, b) in face_cut(&tmp, tf, &plane, &tol) {
                        tmp.add_edge(a, b, &tol, &mut NoDeltas)?;
                    }
                    let parts: Vec<FaceId> = tmp.faces().map(|(g, _)| g).collect();
                    let mut left = 0.0;
                    for g in parts {
                        if plane.signed_distance(inside_point(&tmp, g, &tol)?) < 0.0 {
                            let pr = rings(&tmp, g)?;
                            left += rings_area(&pr);
                            kept.push((pr, sides.clone()));
                        }
                    }
                    out.removed_mm2 += area - left;
                }
            }
            if erase.is_empty() {
                continue;
            }
            out.trimmed += erase.len();
            out.pieces += kept.len();
            tx.with_mesh(set.container, |m, tol, d| {
                for f in &erase {
                    if m.face(*f).is_some() {
                        m.erase_face(*f, true, tol, d)?;
                    }
                }
                Ok(())
            })?;
            let mut piece = indexed(&kept, &tol);
            let uses = edge_uses(&piece);
            for (e, u) in piece.edges.iter_mut().zip(&uses) {
                let (a, b) = (piece.vertices[e.a as usize], piece.vertices[e.b as usize]);
                if *u == 2
                    && soft
                        .iter()
                        .any(|(p, q)| on_segment(a, *p, *q, &tol) && on_segment(b, *p, *q, &tol))
                {
                    e.flags = EdgeFlags::SOFT | EdgeFlags::SMOOTH;
                }
            }
            let rows = (
                vec![SideRows::default(); piece.edges.len()],
                vec![SideRows::default(); piece.faces.len()],
            );
            place_geometry(tx, set.container, &piece, rows.0, rows.1)?;
            // Edges that became the surface's border are drawn.
            tx.with_mesh(set.container, |m, _, d| {
                for e in &touched {
                    let Some(edge) = m.edge(*e) else { continue };
                    if edge.flags.bits() == 0 {
                        continue;
                    }
                    let [a, b] = m.edge_faces(*e)?;
                    if a.is_some() != b.is_some() {
                        m.set_edge_flags(*e, EdgeFlags::NONE, d)?;
                    }
                }
                Ok(())
            })?;
        }
        if out.trimmed == 0 {
            return Err(CommandError::NotApplicable(
                "nothing of the surface lies on the plane's normal side".into(),
            ));
        }
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        meta("surface-trim", "Trim Surface")
    }
}

// ----- surface-knit ---------------------------------------------------------------------

fn knit_name() -> String {
    "Knit surface".into()
}

/// `surface-knit` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SurfaceKnitParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Two or more surface groups of the edit context.
    pub surfaces: Query,
    /// The joined group's name.
    #[serde(default = "knit_name")]
    pub name: String,
}

/// The `surface-knit` command: joins surface groups along their common
/// edges into one group; when they close a volume, it is a solid.
#[derive(Debug, Default, Clone, Copy)]
pub struct SurfaceKnit;

impl Command for SurfaceKnit {
    type Params = SurfaceKnitParams;
    type Output = SurfaceOutput;

    fn name(&self) -> &'static str {
        "surface-knit"
    }

    fn validate(&self, _: &Document, _: &SurfaceKnitParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SurfaceKnitParams,
    ) -> Result<SurfaceOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let tol = *tx.tolerances();
        let sets = face_sets(tx, &target, "surfaces", &p.surfaces, false)?;
        if sets.len() < 2 || sets.iter().any(|s| s.entity.is_none()) {
            return Err(CommandError::invalid_param(
                "surfaces",
                "name two or more groups",
            ));
        }
        let q = tol.length.max(1e-6);
        let key = |p: DVec3| {
            [
                (p.x / q).round() as i64,
                (p.y / q).round() as i64,
                (p.z / q).round() as i64,
            ]
        };
        let mut faces: Vec<(Vec<Vec<DVec3>>, FaceSides)> = Vec::new();
        let mut directed: BTreeSet<([i64; 3], [i64; 3])> = BTreeSet::new();
        let mut flagged: Vec<(DVec3, DVec3, EdgeFlags)> = Vec::new();
        for set in &sets {
            let mesh = tx
                .document()
                .container(set.container)
                .ok_or_else(|| CommandError::Internal("a container vanished".into()))?
                .mesh();
            let t = set.to_target;
            let flip = t.matrix3.determinant() < 0.0;
            let mut mine = Vec::new();
            for f in &set.faces {
                let mut r: Vec<Vec<DVec3>> = rings(mesh, *f)?
                    .into_iter()
                    .map(|ring| ring.into_iter().map(|q| t.transform_point3(q)).collect())
                    .collect();
                if flip {
                    for ring in &mut r {
                        ring.reverse();
                    }
                }
                mine.push((r, mesh.face_ref(*f)?.sides.clone()));
            }
            // A surface running along a joined edge the same way as the
            // surfaces before it is turned over, so the shell is oriented.
            let runs_along = mine.iter().flat_map(|(r, _)| r.iter()).any(|ring| {
                ring.iter()
                    .zip(ring.iter().cycle().skip(1))
                    .any(|(a, b)| directed.contains(&(key(*a), key(*b))))
            });
            for (r, _) in &mut mine {
                for ring in r.iter_mut() {
                    if runs_along {
                        ring.reverse();
                    }
                    for (a, b) in ring.iter().zip(ring.iter().cycle().skip(1)) {
                        directed.insert((key(*a), key(*b)));
                    }
                }
            }
            faces.extend(mine);
            for (e, edge) in mesh.edges() {
                if edge.flags.bits() != 0 {
                    let (a, b) = mesh.edge_endpoints(e)?;
                    flagged.push((
                        t.transform_point3(mesh.position(a)?),
                        t.transform_point3(mesh.position(b)?),
                        edge.flags,
                    ));
                }
            }
        }
        let mut piece = indexed(&faces, &tol);
        let at: BTreeMap<[i64; 3], u32> = piece
            .vertices
            .iter()
            .enumerate()
            .map(|(i, v)| (key(*v), i as u32))
            .collect();
        let by_ends: BTreeMap<(u32, u32), usize> = piece
            .edges
            .iter()
            .enumerate()
            .map(|(i, e)| ((e.a, e.b), i))
            .collect();
        for (a, b, flags) in flagged {
            if let (Some(i), Some(j)) = (at.get(&key(a)), at.get(&key(b)))
                && let Some(e) = by_ends.get(&((*i).min(*j), (*i).max(*j)))
            {
                piece.edges[*e].flags = flags;
            }
        }
        let mesh = HalfEdgeMesh::from_indexed(&piece, &tol).map_err(|_| {
            CommandError::NotApplicable(
                "the surfaces do not join into one shell (an edge would border three faces)".into(),
            )
        })?;
        let open = open_edges(&mesh)?;
        let closed = open == 0;
        let volume_mm3 = if closed {
            mesh.volume().ok().map(f64::abs)
        } else {
            None
        };
        let area_mm2 = faces.iter().map(|(r, _)| rings_area(r)).sum();
        let count = mesh.faces().count();
        for set in &sets {
            if let Some(id) = set.entity {
                remove_container_entity(tx, target.container, id)?;
            }
        }
        let (pid, _) = new_group(tx, &target, &p.name, mesh)?;
        Ok(SurfaceOutput {
            surface: Some(pid),
            faces: count,
            area_mm2,
            open_edges: open,
            closed,
            volume_mm3,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("surface-knit", "Knit Surfaces")
    }
}

/// Registers the surface commands.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(SurfaceFill);
    registry.register(SurfaceExtend);
    registry.register(SurfaceTrim);
    registry.register(SurfaceKnit);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn run(e: &mut Engine, name: &str, p: Value) -> Value {
        e.execute_erased(name, p).unwrap()
    }

    fn f(v: &Value) -> f64 {
        v.as_f64().unwrap()
    }

    #[test]
    fn two_saddles_filled_trimmed_and_knit() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        run(
            &mut e,
            "line",
            json!({"points": [[0,0,0],[100,0,50],[100,100,0],[0,100,50]], "close": true}),
        );
        let a = run(
            &mut e,
            "surface-fill",
            json!({"boundary": "edge", "divisions": 6}),
        );
        // Coons quads of a saddle are not flat: two triangles each.
        assert_eq!(a["faces"], 72);
        assert_eq!(a["open_edges"], 24);
        run(&mut e, "erase", json!({"targets": "edge"}));
        run(
            &mut e,
            "line",
            json!({"points": [[100,100,0],[100,0,50],[200,0,0],[200,100,50]], "close": true}),
        );
        run(
            &mut e,
            "surface-fill",
            json!({"boundary": "edge", "divisions": 6}),
        );
        run(&mut e, "erase", json!({"targets": "edge"}));
        let k = run(&mut e, "surface-knit", json!({"surfaces": "group"}));
        assert_eq!(k["faces"], 144);
        assert_eq!(k["open_edges"], 36);
        assert_eq!(k["closed"], false);
        // Trimming at x = 40 keeps the faces left of it and cut pieces.
        let t = run(
            &mut e,
            "surface-trim",
            json!({"surface": "group", "point": [40, 0, 0], "normal": [1, 0, 0]}),
        );
        assert!(t["pieces"].as_u64().unwrap() > 0);
        let total = f(&k["area_mm2"]);
        assert!(f(&t["removed_mm2"]) > 0.5 * total && f(&t["removed_mm2"]) < total);
        // Nothing beyond the plane is left.
        let doc = e.document();
        let max_x = doc
            .containers()
            .into_iter()
            .filter_map(|c| doc.container(c))
            .flat_map(|c| {
                c.mesh()
                    .vertices()
                    .map(|(_, v)| v.position.x)
                    .collect::<Vec<_>>()
            })
            .fold(f64::MIN, f64::max);
        assert!(max_x <= 40.0 + 1e-6, "{max_x}");
    }

    #[test]
    fn a_face_extended_on_all_sides() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [100, 60, 0] }),
        );
        let x = run(
            &mut e,
            "surface-extend",
            json!({"edges": "edge", "distance": 10}),
        );
        assert_eq!(x["faces"], 4);
        assert!((f(&x["area_mm2"]) - (120.0 * 80.0 - 100.0 * 60.0)).abs() < 1e-6);
        assert_eq!(x["open_edges"], 4);
        let err = e.execute_erased("surface-extend", json!({"edges": "edge", "distance": 10}));
        assert!(err.is_err());
    }
}
