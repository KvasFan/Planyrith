//! Implements the document-facing side of docs/03-geometry-kernel.md §6: the
//! [`InstanceProvider`] trait through which [`super::DocumentIndex`] learns the
//! container occurrences of a document (instance paths, world transforms,
//! visibility) and borrows each container's mesh, plus its implementation for
//! [`Document`]. Keeping the index behind a trait limits its dependency on the
//! document model to `EntityPath`/`InstancePath`, `ContainerRef` and `DAffine3`
//! and lets tests index synthetic scenes without a full document.
//!
//! Visibility rules follow docs/06 §8 (hidden entities, tag visibility, active
//! section planes cut picking).
//!
//! Owner key: `spatial`.

use glam::DAffine3;

use crate::document::{ContainerRef, Document, Entities, EntityKey, InstancePath, Tags};
use crate::geom::HalfEdgeMesh;
use crate::ids::EntityId;
use crate::ids::TagId;
use crate::math::Plane;
use crate::tolerance::Tolerances;

/// One occurrence of a container in the instance tree (docs/04 §12): the model
/// root (`path.is_root()`) or a group/instance reached through `path`.
#[derive(Debug, Clone, PartialEq)]
pub struct Occurrence {
    /// Path from the model root to this occurrence.
    pub path: InstancePath,
    /// The container the occurrence opens.
    pub container: ContainerRef,
    /// Container-to-world transform (product of the step transforms).
    pub world: DAffine3,
    /// `true` when any step on the path is hidden or carries a hidden tag
    /// (docs/04 §5 visibility rule); hidden occurrences are skipped by picking
    /// unless [`super::HitFilter::include_hidden`] is set.
    pub hidden: bool,
}

/// What [`super::DocumentIndex`] needs from a scene (docs/03 §6 "per document").
///
/// Implementations must be deterministic: [`InstanceProvider::occurrences`]
/// returns the same order for the same document state (parents before their
/// children, children in container order), because that order is the leaf
/// order of the instance BVH and the tie-breaker of broad-phase queries.
pub trait InstanceProvider {
    /// Document tolerances (docs/03 §2) used to expand leaf boxes.
    fn tolerances(&self) -> &Tolerances;

    /// Every container occurrence reachable from the model root, the root
    /// first, in depth-first container order.
    fn occurrences(&self) -> Vec<Occurrence>;

    /// The mesh of a container, `None` when the container no longer exists.
    fn mesh(&self, container: ContainerRef) -> Option<&HalfEdgeMesh>;

    /// `true` when an edge or face is hidden by its own flag or by a hidden tag
    /// (docs/06 §8 "picking respects hidden entities, tag visibility").
    fn is_entity_hidden(&self, _container: ContainerRef, _entity: EntityId) -> bool {
        false
    }

    /// Active section planes of a container in the container's local frame;
    /// geometry on the `+normal` side is cut away and ignored by picking when
    /// [`super::HitFilter::respect_sections`] is set (docs/06 §8, docs/04 §3.8).
    fn active_section_planes(&self, _container: ContainerRef) -> Vec<Plane> {
        Vec::new()
    }
}

/// Effective tag visibility: the tag and every ancestor folder are visible
/// (docs/04 §5). Local stand-in for `Tags::is_visible` until the `document`
/// owner implements it; an unknown tag counts as visible.
fn tag_visible(tags: &Tags, id: TagId) -> bool {
    let Some(tag) = tags.get(id) else {
        return true;
    };
    if !tag.visible {
        return false;
    }
    let mut folder = tag.folder;
    let mut depth = 0;
    while let Some(f) = folder.and_then(|f| tags.folder(f)) {
        if !f.visible {
            return false;
        }
        folder = f.parent;
        depth += 1;
        if depth > Entities::MAX_DEPTH {
            // A folder cycle is a document defect (docs/04 §13), not a crash.
            break;
        }
    }
    true
}

/// `true` when the entity is hidden itself or through its tag (docs/04 §5).
fn entity_hidden_in(doc: &Document, entities: &Entities, id: EntityId) -> bool {
    if entities.is_hidden(id) {
        return true;
    }
    let tag = entities.tag_of(id).unwrap_or_else(|| doc.tags.untagged());
    !tag_visible(&doc.tags, tag)
}

/// Depth-first walk over groups and instances in container order.
fn collect_occurrences(
    doc: &Document,
    path: &InstancePath,
    container: ContainerRef,
    world: DAffine3,
    hidden: bool,
    out: &mut Vec<Occurrence>,
) {
    let Some(entities) = doc.container(container) else {
        return;
    };
    out.push(Occurrence {
        path: path.clone(),
        container,
        world,
        hidden,
    });
    if path.depth() >= Entities::MAX_DEPTH {
        // Nesting limit (docs/04 §14); also stops self-referencing definitions.
        return;
    }
    for id in entities.iter().filter(|id| id.is_container_instance()) {
        let (Some(def), Some(local)) = (entities.definition_of(id), entities.transform_of(id))
        else {
            continue;
        };
        if doc.definition(def).is_none() {
            continue;
        }
        let child_hidden = hidden || entity_hidden_in(doc, entities, id);
        collect_occurrences(
            doc,
            &path.child(EntityKey::Id(id)),
            ContainerRef::Definition(def),
            world * local,
            child_hidden,
            out,
        );
    }
}

impl InstanceProvider for Document {
    fn tolerances(&self) -> &Tolerances {
        Document::tolerances(self)
    }

    fn occurrences(&self) -> Vec<Occurrence> {
        let mut out = Vec::new();
        collect_occurrences(
            self,
            &InstancePath::root(),
            ContainerRef::Model,
            DAffine3::IDENTITY,
            false,
            &mut out,
        );
        out
    }

    fn mesh(&self, container: ContainerRef) -> Option<&HalfEdgeMesh> {
        self.container(container).map(Entities::mesh)
    }

    fn is_entity_hidden(&self, container: ContainerRef, entity: EntityId) -> bool {
        self.container(container)
            .is_some_and(|entities| entity_hidden_in(self, entities, entity))
    }

    fn active_section_planes(&self, container: ContainerRef) -> Vec<Plane> {
        if !self.rendering.section.cuts_visible {
            return Vec::new();
        }
        self.container(container)
            .map(|entities| {
                entities
                    .section_planes()
                    .filter(|(_, s)| s.active)
                    .map(|(_, s)| s.plane)
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_visibility_walks_folders() {
        let doc = Document::new();
        assert!(tag_visible(&doc.tags, doc.tags.untagged()));
        assert!(tag_visible(&doc.tags, TagId::default()));
    }

    #[test]
    fn empty_document_has_one_root_occurrence() {
        let doc = Document::new();
        let occ = InstanceProvider::occurrences(&doc);
        assert_eq!(occ.len(), 1);
        assert!(occ[0].path.is_root());
        assert_eq!(occ[0].container, ContainerRef::Model);
        assert_eq!(occ[0].world, DAffine3::IDENTITY);
        assert!(!occ[0].hidden);
        assert!(InstanceProvider::mesh(&doc, ContainerRef::Model).is_some());
        assert!(InstanceProvider::active_section_planes(&doc, ContainerRef::Model).is_empty());
    }
}
