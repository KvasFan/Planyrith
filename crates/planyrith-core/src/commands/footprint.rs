//! Implements docs/08-cli-specification.md §3.1 (per-person undo): what an
//! undo step touched, so that an older step can be taken back under newer
//! ones only when none of them touched the same things.
//!
//! A footprint is conservative. Geometry counts as the points its deltas
//! name (vertices, edge ends, face loops) plus the edges and faces by id;
//! two steps that share a point within the merge tolerance, an element, an
//! entity, a definition, a material, a tag, a scene, a style or a sheet
//! overlap. A step that adds, removes or renames a component definition
//! overlaps every step that changes something inside it, and one that adds,
//! removes or edits a material every step that paints with it.

use std::collections::{BTreeMap, BTreeSet};

use glam::DVec3;

use crate::commands::transaction::{AttrTarget, Delta, MeshAliases, UndoEntry};
use crate::document::{ContainerRef, EntityRecord};
use crate::geom::{FaceSides, MeshDelta};
use crate::ids::{DefinitionId, EntityId, MaterialId};

/// What one or more undo steps touched.
#[derive(Debug, Default)]
pub(crate) struct Footprint {
    /// Named things (entities, elements, resources), canonical strings.
    keys: BTreeSet<String>,
    /// Definitions added, removed or renamed as a whole.
    wholes: BTreeSet<DefinitionId>,
    /// Definitions whose contents changed.
    insides: BTreeSet<DefinitionId>,
    /// Materials added, removed or edited.
    materials: BTreeSet<MaterialId>,
    /// Materials painted or unpainted.
    painted: BTreeSet<MaterialId>,
    /// Points named by geometry deltas, per container.
    points: BTreeMap<ContainerRef, Vec<DVec3>>,
}

fn cell(p: DVec3, size: f64) -> [i64; 3] {
    // Coordinates are bounded by `Tolerances::MAX_EXTENT`, so the cells fit.
    [
        (p.x / size).floor() as i64,
        (p.y / size).floor() as i64,
        (p.z / size).floor() as i64,
    ]
}

impl Footprint {
    /// The footprint of `entries`, with edge and face ids resolved to the
    /// ids those elements have now.
    pub(crate) fn of<'a>(
        entries: impl IntoIterator<Item = &'a UndoEntry>,
        aliases: &MeshAliases,
    ) -> Footprint {
        let mut f = Footprint::default();
        for entry in entries {
            for tx in &entry.transactions {
                for delta in &tx.deltas {
                    f.add(delta, aliases);
                }
            }
        }
        f
    }

    /// `true` when the two footprints share anything; points match within
    /// `merge` millimeters.
    pub(crate) fn overlaps(&self, other: &Footprint, merge: f64) -> bool {
        if self.keys.intersection(&other.keys).next().is_some() {
            return true;
        }
        let whole_vs_inside = |a: &Footprint, b: &Footprint| {
            a.wholes
                .iter()
                .any(|d| b.wholes.contains(d) || b.insides.contains(d))
                || a.materials
                    .iter()
                    .any(|m| b.materials.contains(m) || b.painted.contains(m))
        };
        if whole_vs_inside(self, other) || whole_vs_inside(other, self) {
            return true;
        }
        let size = merge.max(f64::MIN_POSITIVE) * 2.0;
        for (container, mine) in &self.points {
            let Some(theirs) = other.points.get(container) else {
                continue;
            };
            let mut grid: BTreeMap<[i64; 3], Vec<DVec3>> = BTreeMap::new();
            for p in theirs {
                grid.entry(cell(*p, size)).or_default().push(*p);
            }
            for p in mine {
                let [x, y, z] = cell(*p, size);
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            if grid
                                .get(&[x + dx, y + dy, z + dz])
                                .is_some_and(|near| near.iter().any(|q| q.distance(*p) <= merge))
                            {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

    fn key(&mut self, key: String) {
        self.keys.insert(key);
    }

    fn container(&mut self, container: ContainerRef) {
        if let ContainerRef::Definition(d) = container {
            self.insides.insert(d);
        }
    }

    fn entity(&mut self, container: ContainerRef, id: EntityId, aliases: &MeshAliases) {
        self.container(container);
        let id = match (id, aliases.get(&container)) {
            (EntityId::Edge(e), Some(a)) => EntityId::Edge(a.edge(e)),
            (EntityId::Face(f), Some(a)) => EntityId::Face(a.face(f)),
            _ => id,
        };
        self.key(format!("entity {container:?} {id:?}"));
    }

    fn sides(&mut self, sides: &FaceSides) {
        self.painted.extend(
            [sides.material_front, sides.material_back]
                .into_iter()
                .flatten(),
        );
    }

    /// What an entity record refers to: a group or an instance uses its
    /// definition (like a change inside it), an image or an instance its
    /// material.
    fn record(&mut self, record: &EntityRecord) {
        match record {
            EntityRecord::Group(g) => {
                self.insides.insert(g.definition);
            }
            EntityRecord::Instance(i) => {
                self.insides.insert(i.definition);
                self.painted.extend(i.material_override);
            }
            EntityRecord::Image(image) => {
                self.painted.insert(image.material);
            }
            _ => {}
        }
    }

    fn points(&mut self, container: ContainerRef, points: impl IntoIterator<Item = DVec3>) {
        self.points.entry(container).or_default().extend(points);
    }

    fn add(&mut self, delta: &Delta, aliases: &MeshAliases) {
        match delta {
            Delta::Mesh { container, delta } => self.mesh(*container, delta, aliases),
            Delta::PidBound { container, id, .. }
            | Delta::PidUnbound { container, id, .. }
            | Delta::TagChanged { container, id, .. }
            | Delta::HiddenChanged { container, id, .. }
            | Delta::LockedChanged { container, id, .. }
            | Delta::TransformChanged { container, id, .. }
            | Delta::EntityChanged { container, id, .. }
            | Delta::EntityInserted { container, id, .. }
            | Delta::EntityRemoved { container, id, .. } => {
                self.entity(*container, *id, aliases);
                match delta {
                    Delta::EntityChanged { from, to, .. } => {
                        self.record(from);
                        self.record(to);
                    }
                    Delta::EntityInserted { record, .. } | Delta::EntityRemoved { record, .. } => {
                        self.record(record);
                    }
                    _ => {}
                }
            }
            Delta::AttributeChanged {
                target, dict, key, ..
            } => {
                let target = match *target {
                    AttrTarget::Entity { container, id } => {
                        self.entity(container, id, aliases);
                        format!("{container:?} {id:?}")
                    }
                    other => format!("{other:?}"),
                };
                self.key(format!("attribute {target} {dict} {key}"));
            }
            Delta::DefinitionInserted { id, .. }
            | Delta::DefinitionRemoved { id, .. }
            | Delta::DefinitionChanged { id, .. } => {
                self.wholes.insert(*id);
            }
            Delta::MaterialInserted { id, .. }
            | Delta::MaterialRemoved { id, .. }
            | Delta::MaterialChanged { id, .. } => {
                self.materials.insert(*id);
            }
            Delta::ImageInserted { id, .. } | Delta::ImageRemoved { id, .. } => {
                self.key(format!("image {id:?}"));
            }
            Delta::TagInserted { id, .. }
            | Delta::TagRemoved { id, .. }
            | Delta::TagUpdated { id, .. } => self.key(format!("tag {id:?}")),
            Delta::TagFolderInserted { id, .. }
            | Delta::TagFolderRemoved { id, .. }
            | Delta::TagFolderChanged { id, .. } => self.key(format!("tag folder {id:?}")),
            // Scenes are ordered: any two scene changes overlap.
            Delta::SceneInserted { .. }
            | Delta::SceneRemoved { .. }
            | Delta::SceneChanged { .. }
            | Delta::SceneMoved { .. } => self.key("scenes".into()),
            Delta::StyleInserted { id, .. }
            | Delta::StyleRemoved { id, .. }
            | Delta::StyleChanged { id, .. } => self.key(format!("style {id:?}")),
            Delta::ActiveStyleChanged { .. } => self.key("active style".into()),
            Delta::InfoChanged { .. } => self.key("model info".into()),
            Delta::RenderingChanged { .. } => self.key("rendering".into()),
            Delta::SheetPageName { .. }
            | Delta::SheetSettings { .. }
            | Delta::SheetPageOrder { .. }
            | Delta::SheetPaper { .. }
            | Delta::SheetPage { .. }
            | Delta::SheetItem { .. }
            | Delta::SheetLayers { .. } => self.key("sheets".into()),
        }
    }

    fn mesh(&mut self, container: ContainerRef, delta: &MeshDelta, aliases: &MeshAliases) {
        self.container(container);
        let ids = aliases.get(&container);
        let face = |f| ids.map_or(f, |a| a.face(f));
        let edge = |e| ids.map_or(e, |a| a.edge(e));
        match delta {
            MeshDelta::VertexAdded { position, .. } | MeshDelta::VertexRemoved { position, .. } => {
                self.points(container, [*position]);
            }
            MeshDelta::VertexMoved { from, to, .. } => self.points(container, [*from, *to]),
            MeshDelta::VerticesMoved { moves } => {
                self.points(container, moves.iter().flat_map(|(_, a, b)| [*a, *b]));
            }
            MeshDelta::EdgeAdded { id, a, b, .. } | MeshDelta::EdgeRemoved { id, a, b, .. } => {
                self.points(container, [*a, *b]);
                self.key(format!("edge {container:?} {:?}", edge(*id)));
            }
            MeshDelta::EdgeFlagsChanged { id, .. } => {
                self.key(format!("edge {container:?} {:?}", edge(*id)));
            }
            MeshDelta::FaceAdded { id, snapshot } | MeshDelta::FaceRemoved { id, snapshot } => {
                self.points(
                    container,
                    snapshot
                        .outer
                        .iter()
                        .chain(snapshot.holes.iter().flatten())
                        .copied(),
                );
                self.sides(&snapshot.sides);
                self.key(format!("face {container:?} {:?}", face(*id)));
            }
            MeshDelta::FaceSidesChanged { id, from, to } => {
                self.sides(from);
                self.sides(to);
                self.key(format!("face {container:?} {:?}", face(*id)));
            }
            MeshDelta::FaceSplit { old, new } => {
                for f in [*old, new[0], new[1]] {
                    self.key(format!("face {container:?} {:?}", face(f)));
                }
            }
            MeshDelta::FacesMerged { kept, removed } => {
                for f in [*kept, *removed] {
                    self.key(format!("face {container:?} {:?}", face(f)));
                }
            }
            MeshDelta::CurveAdded { id, members, .. }
            | MeshDelta::CurveRemoved { id, members, .. } => {
                self.key(format!("curve {container:?} {id:?}"));
                for e in members {
                    self.key(format!("edge {container:?} {:?}", edge(*e)));
                }
            }
        }
    }
}
