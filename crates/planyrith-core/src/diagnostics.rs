//! Implements docs/04-document-model.md §13 (validate/repair), the statistics
//! of docs/08 §2 (`info`), and the warning codes of docs/08 §9 / docs/01 §7
//! shared by command outputs.
//!
//! Owner key: `later` (validation needs `mesh::validate` and `document`).

#![warn(unused_variables, dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::{
    Anchor, ContainerRef, Dimension, Document, Entities, EntityCounts, ImageRef,
};
use crate::geom::NoDeltas;
use crate::ids::{EntityId, FaceId, Pid};
use crate::math::transform::is_invertible;

/// Stable warning codes (docs/08 §9).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum WarningCode {
    /// Faces merged automatically.
    W2001,
    /// Vertices merged by tolerance.
    W2002,
    /// Detached dimension anchors.
    W2003,
    /// Unsupported import entity skipped.
    W2004,
    /// Texture downscaled.
    W2005,
    /// A damaged or unusable part was dropped while loading (journal,
    /// a part whose name cannot be written back, a part the manifest lists
    /// but the container lacks).
    W2006,
}

impl WarningCode {
    /// The code as printed (`"W2001"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            WarningCode::W2001 => "W2001",
            WarningCode::W2002 => "W2002",
            WarningCode::W2003 => "W2003",
            WarningCode::W2004 => "W2004",
            WarningCode::W2005 => "W2005",
            WarningCode::W2006 => "W2006",
        }
    }
}

/// A non-fatal note attached to a command output or report (docs/01 §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Warning {
    /// Stable code.
    pub code: WarningCode,
    /// Human-readable message.
    pub message: String,
}

impl Warning {
    /// Builds a warning.
    pub fn new(code: WarningCode, message: impl Into<String>) -> Self {
        Warning {
            code,
            message: message.into(),
        }
    }
}

/// Severity of a validation problem.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Cosmetic or recoverable.
    Warning,
    /// The document violates an invariant; `repair` may fix it.
    Error,
}

/// Kinds of problems found by [`validate`] (docs/04 §13).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ProblemKind {
    /// A mesh invariant of docs/03 §3.1 is violated.
    MeshInvariant,
    /// Two vertices closer than `tolerances.length`.
    NearVertices,
    /// Face with area below `length²`.
    DegenerateFace,
    /// Dimension/text anchor pointing at a missing entity (`W2003`).
    DanglingAnchor,
    /// Image in the store with no material or image entity using it.
    UnreferencedImage,
    /// A definition contains itself directly or indirectly.
    CyclicNesting,
    /// Nesting deeper than the hard limit of docs/04 §14.
    NestingTooDeep,
    /// Two tags share a name.
    DuplicateTagName,
    /// Scene refers to a deleted tag/style.
    SceneDanglingReference,
    /// Group/instance/image transform is not invertible.
    NonInvertibleTransform,
    /// `next_pid` is not above every pid in use.
    PidCounter,
    /// Pid bound twice or missing for an entity in the order list.
    PidBookkeeping,
    /// Definition `usage_count` differs from the actual count.
    UsageCount,
    /// A reference to a definition, material, tag or image that does not
    /// exist, or a definition missing from (or doubled in) the definition order.
    DanglingReference,
    /// The document tolerances are inconsistent (docs/03 §2).
    InvalidTolerances,
}

/// One problem (docs/04 §13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Problem {
    /// Kind.
    pub kind: ProblemKind,
    /// Severity.
    pub severity: Severity,
    /// Container involved, if any.
    pub container: Option<ContainerRef>,
    /// Entity involved, if any.
    pub entity: Option<Pid>,
    /// Detail text.
    pub message: String,
}

/// Output of [`validate`] (docs/04 §13; exit code 3 when `has_errors`, docs/08 §8).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    /// Problems in deterministic order (container, then pid, then kind).
    pub problems: Vec<Problem>,
}

impl Report {
    /// `true` when no problems were found.
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }

    /// `true` when at least one problem has `Severity::Error`.
    pub fn has_errors(&self) -> bool {
        self.problems.iter().any(|p| p.severity == Severity::Error)
    }
}

/// Which fixes [`repair`] may apply (docs/08 §2 `--fix`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub struct RepairFixes {
    /// Merge vertices within tolerance.
    pub merge_vertices: bool,
    /// Remove faces with area below `length²`.
    pub degenerate_faces: bool,
    /// Delete dangling anchors, scene references, unreferenced images.
    pub dangling_refs: bool,
}

impl RepairFixes {
    /// Every fix enabled (`--fix all`).
    pub const ALL: RepairFixes = RepairFixes {
        merge_vertices: true,
        degenerate_faces: true,
        dangling_refs: true,
    };
}

/// Output of [`repair`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct RepairReport {
    /// Problems that were fixed.
    pub fixed: Vec<Problem>,
    /// What is still wrong.
    pub remaining: Report,
    /// Warnings produced while fixing (`W2001`, `W2002`).
    pub warnings: Vec<Warning>,
}

/// Document statistics for `planyrith info` (docs/08 §2).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Statistics {
    /// Counts in the model container.
    pub model: EntityCounts,
    /// Counts summed over all definitions.
    pub definitions_total: EntityCounts,
    /// Number of definitions.
    pub definitions: usize,
    /// Number of materials.
    pub materials: usize,
    /// Number of tags.
    pub tags: usize,
    /// Number of scenes.
    pub scenes: usize,
    /// Number of styles.
    pub styles: usize,
    /// Number of images.
    pub images: usize,
}

/// Runs every check of docs/04 §13 without mutating.
pub fn validate(doc: &Document) -> Report {
    let mut problems = Vec::new();
    let tol = *doc.tolerances();
    let mut max_pid = 0u64;
    let mut seen_pids: BTreeMap<Pid, ContainerRef> = BTreeMap::new();

    for c in doc.containers() {
        let Some(entities) = doc.container(c) else {
            continue;
        };
        let mut push =
            |kind: ProblemKind, severity: Severity, entity: Option<Pid>, message: String| {
                problems.push(Problem {
                    kind,
                    severity,
                    container: Some(c),
                    entity,
                    message,
                });
            };
        for v in entities.mesh().validate(&tol) {
            let kind = match v.invariant {
                3 => ProblemKind::NearVertices,
                2 if v.detail.contains("zero area") => ProblemKind::DegenerateFace,
                _ => ProblemKind::MeshInvariant,
            };
            push(kind, Severity::Error, None, v.detail);
        }
        // Pid bookkeeping: every ordered entity exists and has a pid, every
        // edge/face has one, pids are unique in the document.
        for id in entities.iter() {
            let pid = entities.pid_of(id);
            if !entities.contains(id) || pid.is_none() {
                push(
                    ProblemKind::PidBookkeeping,
                    Severity::Error,
                    pid,
                    format!("{id} is listed in the order but missing or unbound"),
                );
            }
            if let Some(pid) = pid {
                max_pid = max_pid.max(pid.0);
                if let Some(other) = seen_pids.insert(pid, c) {
                    push(
                        ProblemKind::PidBookkeeping,
                        Severity::Error,
                        Some(pid),
                        format!("{pid} is bound in {other:?} and {c:?}"),
                    );
                }
            }
        }
        let unbound = entities
            .mesh()
            .edges()
            .map(|(e, _)| EntityId::Edge(e))
            .chain(entities.mesh().faces().map(|(f, _)| EntityId::Face(f)))
            .filter(|id| entities.pid_of(*id).is_none())
            .count();
        if unbound > 0 {
            push(
                ProblemKind::PidBookkeeping,
                Severity::Warning,
                None,
                format!("{unbound} edges/faces have no pid"),
            );
        }
        // Transforms.
        let transforms = entities
            .groups()
            .map(|(g, x)| (EntityId::Group(g), x.transform))
            .chain(
                entities
                    .instances()
                    .map(|(i, x)| (EntityId::Instance(i), x.transform)),
            )
            .chain(
                entities
                    .images()
                    .map(|(i, x)| (EntityId::Image(i), x.transform)),
            );
        let mut singular = Vec::new();
        for (id, t) in transforms {
            if !is_invertible(&t, &tol) {
                singular.push((entities.pid_of(id), id));
            }
        }
        for (pid, id) in singular {
            push(
                ProblemKind::NonInvertibleTransform,
                Severity::Error,
                pid,
                format!("{id} has a non-invertible transform"),
            );
        }
        // Anchors of dimensions and texts.
        let mut anchors: Vec<(EntityId, &Anchor)> = Vec::new();
        for (d, dim) in entities.dimensions() {
            match dim {
                Dimension::Linear { start, end, .. } => {
                    anchors.push((EntityId::Dimension(d), start));
                    anchors.push((EntityId::Dimension(d), end));
                }
                Dimension::Radial { curve, .. } => anchors.push((EntityId::Dimension(d), curve)),
            }
        }
        for (t, text) in entities.texts() {
            if let Some(a) = &text.anchor {
                anchors.push((EntityId::Text(t), a));
            }
        }
        let dangling: Vec<(Option<Pid>, EntityId)> = anchors
            .into_iter()
            .filter(|(_, a)| crate::document::annotations::anchor_position(doc, c, a).is_none())
            .map(|(id, _)| (entities.pid_of(id), id))
            .collect();
        for (pid, id) in dangling {
            push(
                ProblemKind::DanglingAnchor,
                Severity::Warning,
                pid,
                format!("{id} has a detached anchor (W2003)"),
            );
        }
        for (pid, message) in dangling_references(doc, entities) {
            push(
                ProblemKind::DanglingReference,
                Severity::Error,
                pid,
                message,
            );
        }
    }

    let mut doc_problem = |kind: ProblemKind, severity: Severity, message: String| {
        problems.push(Problem {
            kind,
            severity,
            container: None,
            entity: None,
            message,
        });
    };
    if let Err(e) = doc.tolerances().validate() {
        doc_problem(
            ProblemKind::InvalidTolerances,
            Severity::Error,
            format!("document tolerances: {e}"),
        );
    }
    let mut ordered = BTreeSet::new();
    for &d in &doc.definition_order {
        if doc.definition(d).is_none() || !ordered.insert(d) {
            doc_problem(
                ProblemKind::DanglingReference,
                Severity::Error,
                format!("the definition order lists {d} that is missing or doubled"),
            );
        }
    }
    for (d, def) in doc.definitions.iter() {
        if !ordered.contains(&d) {
            doc_problem(
                ProblemKind::DanglingReference,
                Severity::Error,
                format!(
                    "definition `{}` is not in the definition order (its contents are not checked)",
                    def.name
                ),
            );
        }
    }
    for (_, m) in doc.materials.iter() {
        if let Some(t) = &m.texture
            && doc.images.get(t.image).is_none()
        {
            doc_problem(
                ProblemKind::DanglingReference,
                Severity::Error,
                format!("material `{}` uses a texture image that is missing", m.name),
            );
        }
    }
    // Cyclic or too deep nesting anywhere (also in definitions not reachable
    // from the model).
    if let Err(err) = doc.nesting_depth() {
        let kind = match err {
            crate::document::HierarchyError::TooDeep(_) => ProblemKind::NestingTooDeep,
            _ => ProblemKind::CyclicNesting,
        };
        doc_problem(kind, Severity::Error, err.to_string());
    }
    let counts = doc.usage_counts();
    for (d, def) in doc.definitions.iter() {
        let actual = counts.get(&d).copied().unwrap_or(0);
        if def.usage_count != actual {
            doc_problem(
                ProblemKind::UsageCount,
                Severity::Warning,
                format!(
                    "definition `{}` records {} uses but has {actual}",
                    def.name, def.usage_count
                ),
            );
        }
    }
    let mut names = BTreeSet::new();
    for (_, tag) in doc.tags.iter() {
        if !names.insert(tag.name.clone()) {
            doc_problem(
                ProblemKind::DuplicateTagName,
                Severity::Error,
                format!("tag name `{}` is used twice", tag.name),
            );
        }
    }
    for (id, scene) in doc.scenes.iter() {
        let missing_tag = scene
            .tag_visibility
            .as_ref()
            .is_some_and(|m| m.keys().any(|t| doc.tags.get(*t).is_none()));
        let missing_style = scene.style.is_some_and(|s| doc.styles.get(s).is_none());
        if missing_tag || missing_style {
            doc_problem(
                ProblemKind::SceneDanglingReference,
                Severity::Warning,
                format!(
                    "scene `{}` ({id}) refers to a deleted tag or style",
                    scene.name
                ),
            );
        }
    }
    for (image, _) in doc.images.iter() {
        if !image_used(doc, image) {
            doc_problem(
                ProblemKind::UnreferencedImage,
                Severity::Warning,
                format!("image {} is not used", image.to_hex()),
            );
        }
    }
    if max_pid >= doc.info.next_pid.next_raw() {
        doc_problem(
            ProblemKind::PidCounter,
            Severity::Error,
            format!(
                "next_pid {} is not above the largest pid in use ({max_pid})",
                doc.info.next_pid.next_raw()
            ),
        );
    }
    Report { problems }
}

/// `true` when a material texture or an image entity's material uses `image`.
/// `true` when a material texture, a watermark or a definition thumbnail
/// uses the image.
pub(crate) fn image_used(doc: &Document, image: ImageRef) -> bool {
    doc.materials
        .iter()
        .any(|(_, m)| m.texture.as_ref().is_some_and(|t| t.image == image))
        || doc
            .styles
            .iter()
            .any(|(_, s)| s.watermarks.iter().any(|w| w.image == image))
        || doc
            .definitions
            .iter()
            .any(|(_, d)| d.thumbnail == Some(image))
}

/// References of one container's entities to things that do not exist:
/// definitions of groups/instances, face and override materials, tags.
fn dangling_references(doc: &Document, entities: &Entities) -> Vec<(Option<Pid>, String)> {
    let mut out = Vec::new();
    for (g, x) in entities.groups() {
        if doc.definition(x.definition).is_none() {
            let id = EntityId::Group(g);
            out.push((
                entities.pid_of(id),
                format!("{id} names a missing definition"),
            ));
        }
    }
    for (i, x) in entities.instances() {
        let id = EntityId::Instance(i);
        if doc.definition(x.definition).is_none() {
            out.push((
                entities.pid_of(id),
                format!("{id} names a missing definition"),
            ));
        }
        if x.material_override
            .is_some_and(|m| doc.materials.get(m).is_none())
        {
            out.push((
                entities.pid_of(id),
                format!("{id} overrides with a missing material"),
            ));
        }
    }
    for (f, face) in entities.mesh().faces() {
        let sides = [face.sides.material_front, face.sides.material_back];
        if sides
            .into_iter()
            .flatten()
            .any(|m| doc.materials.get(m).is_none())
        {
            let id = EntityId::Face(f);
            out.push((entities.pid_of(id), format!("{id} uses a missing material")));
        }
    }
    for id in entities.iter() {
        if entities
            .tag_of(id)
            .is_some_and(|t| doc.tags.get(t).is_none())
        {
            out.push((entities.pid_of(id), format!("{id} has a missing tag")));
        }
    }
    out
}

/// Applies the enabled fixes and re-validates (docs/04 §13 `repair`). Repairs
/// work on the file outside the undo history; pids of re-created mesh
/// elements are re-bound afterwards.
pub fn repair(doc: &mut Document, fixes: &RepairFixes) -> RepairReport {
    let before = validate(doc);
    let mut warnings = Vec::new();
    let tol = *doc.tolerances();
    // Mesh fixes run in a transaction so re-created edges and faces keep
    // their pids, order slots, tags, hidden state and attributes (the same
    // identity rules as commands, docs/03 Decisions); the deltas are not kept.
    {
        let mut tx = crate::commands::Transaction::new(doc, "Repair");
        for c in tx.document().containers() {
            if fixes.merge_vertices {
                let merged = tx.with_mesh(c, |mesh, tol, d| Ok(merge_near_vertices(mesh, tol, d)));
                if let Ok(merged) = merged
                    && merged > 0
                {
                    warnings.push(Warning::new(
                        WarningCode::W2002,
                        format!("{merged} vertex pairs merged in {c:?}"),
                    ));
                }
            }
            if fixes.degenerate_faces {
                let degenerate: Vec<FaceId> = tx
                    .document()
                    .container(c)
                    .map(|e| {
                        e.mesh()
                            .faces()
                            .map(|(f, _)| f)
                            .filter(|f| e.mesh().face_area(*f).is_ok_and(|a| a <= tol.length_sq))
                            .collect()
                    })
                    .unwrap_or_default();
                for f in degenerate {
                    let _ = tx.with_mesh(c, |mesh, tol, d| mesh.erase_face(f, false, tol, d));
                }
            }
        }
        let _ = tx.commit();
    }
    for c in doc.containers() {
        let mut next_pid = doc.info.next_pid;
        if let Some(entities) = doc.container_mut(c) {
            // Anything a damaged file left unbound gets a pid.
            entities.sync_mesh_pids(&mut next_pid);
        }
        doc.info.next_pid = next_pid;
    }
    if fixes.dangling_refs {
        let tags: BTreeSet<_> = doc.tags.iter().map(|(t, _)| t).collect();
        let styles: BTreeSet<_> = doc.styles.iter().map(|(s, _)| s).collect();
        let scene_ids: Vec<_> = doc.scenes.iter().map(|(id, _)| id).collect();
        for id in scene_ids {
            let Some(mut scene) = doc.scenes.get(id).cloned() else {
                continue;
            };
            if let Some(map) = &mut scene.tag_visibility {
                map.retain(|t, _| tags.contains(t));
            }
            if scene.style.is_some_and(|s| !styles.contains(&s)) {
                scene.style = None;
            }
            doc.scenes.replace(id, scene);
        }
        let unused: Vec<ImageRef> = doc
            .images
            .iter()
            .map(|(r, _)| r)
            .filter(|r| !image_used(doc, *r))
            .collect();
        for r in unused {
            doc.images.remove(r);
        }
        repair_references(doc, &mut warnings);
    }
    // The usage counts are a cache: always recomputed.
    doc.refresh_usage_counts();
    let mut max_pid = 0;
    for c in doc.containers() {
        if let Some(entities) = doc.container(c) {
            for id in entities.iter() {
                if let Some(p) = entities.pid_of(id) {
                    max_pid = max_pid.max(p.0);
                }
            }
        }
    }
    if max_pid >= doc.info.next_pid.next_raw() {
        doc.info.next_pid.observe(Pid(max_pid));
    }
    let remaining = validate(doc);
    let fixed = before
        .problems
        .into_iter()
        .filter(|p| !remaining.problems.contains(p))
        .collect();
    RepairReport {
        fixed,
        remaining,
        warnings,
    }
}

/// Repairs dangling references (docs/04 §13): faces and overrides fall back
/// to the default material, entities with a missing tag become untagged,
/// groups and instances of a missing definition are removed, the
/// definition order lists every definition once, and textures whose image is
/// missing are dropped.
fn repair_references(doc: &mut Document, warnings: &mut Vec<Warning>) {
    let mut fixed = 0usize;
    // The definition order.
    let mut seen = BTreeSet::new();
    let defs: BTreeSet<_> = doc.definitions.iter().map(|(d, _)| d).collect();
    let before = doc.definition_order.len();
    doc.definition_order
        .retain(|d| defs.contains(d) && seen.insert(*d));
    fixed += before - doc.definition_order.len();
    for d in defs {
        if !seen.contains(&d) {
            doc.definition_order.push(d);
            fixed += 1;
        }
    }
    // Textures.
    let materials: Vec<_> = doc.materials.iter().map(|(id, _)| id).collect();
    for id in materials {
        let Some(m) = doc.materials.get(id) else {
            continue;
        };
        if let Some(t) = &m.texture
            && doc.images.get(t.image).is_none()
        {
            let mut m = m.clone();
            m.texture = None;
            if doc.materials.replace(id, m).is_ok() {
                fixed += 1;
            }
        }
    }
    // Entities.
    let materials: BTreeSet<_> = doc.materials.iter().map(|(id, _)| id).collect();
    let tags: BTreeSet<_> = doc.tags.iter().map(|(id, _)| id).collect();
    let definitions: BTreeSet<_> = doc.definitions.iter().map(|(d, _)| d).collect();
    for c in doc.containers() {
        let Some(entities) = doc.container_mut(c) else {
            continue;
        };
        let faces: Vec<(FaceId, crate::geom::FaceSides)> = entities
            .mesh()
            .faces()
            .map(|(f, x)| (f, x.sides.clone()))
            .collect();
        for (f, mut sides) in faces {
            let mut changed = false;
            for m in [&mut sides.material_front, &mut sides.material_back] {
                if m.is_some_and(|m| !materials.contains(&m)) {
                    *m = None;
                    changed = true;
                }
            }
            if changed
                && entities
                    .mesh_mut()
                    .set_face_sides(f, sides, &mut NoDeltas)
                    .is_ok()
            {
                fixed += 1;
            }
        }
        let ids: Vec<EntityId> = entities.iter().collect();
        for id in ids {
            if entities.tag_of(id).is_some_and(|t| !tags.contains(&t))
                && entities.set_tag(id, None).is_ok()
            {
                fixed += 1;
            }
            let missing_definition = entities
                .definition_of(id)
                .is_some_and(|d| !definitions.contains(&d));
            if missing_definition && entities.remove(id).is_ok() {
                fixed += 1;
                continue;
            }
            if let Some(crate::document::EntityView::Instance(i)) = entities.view(id)
                && i.material_override.is_some_and(|m| !materials.contains(&m))
            {
                let mut i = i.clone();
                i.material_override = None;
                if entities
                    .replace(id, crate::document::EntityRecord::Instance(i))
                    .is_ok()
                {
                    fixed += 1;
                }
            }
        }
    }
    if fixed > 0 {
        warnings.push(Warning::new(
            WarningCode::W2006,
            format!("{fixed} dangling reference(s) repaired"),
        ));
    }
}

/// Merges vertex pairs closer than the tolerance by moving one onto the
/// other (the kernel's merge-on-move welds them). Returns the merge count.
fn merge_near_vertices(
    mesh: &mut crate::geom::HalfEdgeMesh,
    tol: &crate::tolerance::Tolerances,
    d: &mut dyn crate::geom::DeltaSink,
) -> usize {
    let mut merged = 0;
    // Bounded: each successful merge removes a vertex.
    for _ in 0..mesh.vertex_count() {
        let mut points: Vec<_> = mesh.vertices().map(|(v, x)| (x.position, v)).collect();
        points.sort_by(|a, b| a.0.x.total_cmp(&b.0.x).then(a.1.cmp(&b.1)));
        let mut pair = None;
        'scan: for i in 0..points.len() {
            for j in i + 1..points.len() {
                if points[j].0.x - points[i].0.x > tol.length {
                    break;
                }
                if tol.same_point(points[i].0, points[j].0) {
                    pair = Some((points[i], points[j]));
                    break 'scan;
                }
            }
        }
        let Some(((keep, _), (p, v))) = pair else {
            break;
        };
        let t = glam::DAffine3::from_translation(keep - p);
        let moved =
            mesh.transform_vertices(&[v], &t, crate::geom::TransformOptions::default(), tol, d);
        match moved {
            Ok(r) if r.merged_vertices > 0 => merged += 1,
            _ => break,
        }
    }
    merged
}

/// Counts for `planyrith info`.
pub fn statistics(doc: &Document) -> Statistics {
    let mut definitions_total = EntityCounts::default();
    for (_, def) in doc.definitions.iter() {
        let c = def.entities.counts();
        definitions_total.vertices += c.vertices;
        definitions_total.edges += c.edges;
        definitions_total.faces += c.faces;
        definitions_total.groups += c.groups;
        definitions_total.instances += c.instances;
        definitions_total.guides += c.guides;
        definitions_total.dimensions += c.dimensions;
        definitions_total.texts += c.texts;
        definitions_total.images += c.images;
        definitions_total.section_planes += c.section_planes;
    }
    Statistics {
        model: doc.model.counts(),
        definitions_total,
        definitions: doc.definitions.len(),
        materials: doc.materials.len(),
        tags: doc.tags.len(),
        scenes: doc.scenes.len(),
        styles: doc.styles.len(),
        images: doc.images.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_flags() {
        let mut r = Report::default();
        assert!(r.is_clean() && !r.has_errors());
        r.problems.push(Problem {
            kind: ProblemKind::DanglingAnchor,
            severity: Severity::Warning,
            container: None,
            entity: Some(Pid(3)),
            message: "detached".into(),
        });
        assert!(!r.is_clean() && !r.has_errors());
        assert_eq!(WarningCode::W2003.as_str(), "W2003");
    }

    #[test]
    fn engine_built_documents_validate_clean_and_counts_add_up() {
        use crate::commands::{CommandRegistry, Engine};
        let mut e = Engine::new(
            Document::new(),
            std::sync::Arc::new(CommandRegistry::with_builtins()),
        );
        let r = e
            .execute_erased(
                "rectangle",
                serde_json::json!({ "corner_a": [0,0,0], "corner_b": [1000,500,0] }),
            )
            .unwrap();
        e.execute_erased(
            "push-pull",
            serde_json::json!({ "face": { "Pid": r["face"] }, "distance": 300.0 }),
        )
        .unwrap();
        let doc = e.document();
        let report = validate(doc);
        assert!(report.is_clean(), "{report:#?}");
        let stats = statistics(doc);
        assert_eq!(stats.model.faces, 6);
        assert_eq!(stats.model.edges, 12);
        assert_eq!(stats.materials, 2);
    }

    #[test]
    fn repair_merges_near_vertices_and_rebinds_pids() {
        let mut doc = Document::new();
        // Two edges whose ends are 1e-4 mm apart, written without merging.
        let indexed = crate::geom::IndexedMesh {
            vertices: vec![
                glam::DVec3::ZERO,
                glam::DVec3::X * 10.0,
                glam::DVec3::new(10.0001, 0.0, 0.0),
                glam::DVec3::new(10.0, 10.0, 0.0),
            ],
            edges: vec![
                crate::geom::mesh::IndexedEdge {
                    a: 0,
                    b: 1,
                    flags: Default::default(),
                    curve: None,
                },
                crate::geom::mesh::IndexedEdge {
                    a: 2,
                    b: 3,
                    flags: Default::default(),
                    curve: None,
                },
            ],
            faces: vec![],
            curves: vec![],
        };
        // from_indexed validates, so build the bad mesh element by element.
        let mesh = HalfEdgeMeshProbe::build(&indexed);
        *doc.model.mesh_mut() = mesh;
        let report = validate(&doc);
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.kind == ProblemKind::NearVertices)
        );
        let fixed = repair(&mut doc, &RepairFixes::ALL);
        assert!(fixed.remaining.is_clean(), "{:#?}", fixed.remaining);
        assert_eq!(doc.model.mesh().vertex_count(), 3);
        assert_eq!(doc.model.len(), 2, "both edges bound to pids");
        assert_eq!(fixed.warnings.len(), 1);
    }

    /// Builds a mesh from indexed data without validation (test helper).
    struct HalfEdgeMeshProbe;

    impl HalfEdgeMeshProbe {
        fn build(indexed: &crate::geom::IndexedMesh) -> crate::geom::HalfEdgeMesh {
            let mut mesh = crate::geom::HalfEdgeMesh::new();
            let mut ids = Vec::new();
            for &p in &indexed.vertices {
                ids.push(mesh.new_vertex(p, &mut NoDeltas));
            }
            for e in &indexed.edges {
                mesh.new_edge(ids[e.a as usize], ids[e.b as usize], e.flags, &mut NoDeltas)
                    .unwrap();
            }
            mesh
        }
    }
}
