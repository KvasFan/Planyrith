//! Keeps non-manifold or inconsistently oriented source faces in private groups.
use super::Result;
use glam::DAffine3;
use planyrith_core::{
    commands::Transaction,
    document::{ComponentDefinition, ContainerRef, EntityRecord, Group},
    geom::mesh::{DirectedEdgeIndex, IndexedMesh},
    ids::{EdgeId, FaceId},
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Placement {
    pub container: ContainerRef,
    pub edges: BTreeMap<usize, EdgeId>,
    pub faces: BTreeMap<usize, FaceId>,
}

pub(super) fn insert(
    tx: &mut Transaction<'_>,
    parent: ContainerRef,
    indexed: &IndexedMesh,
    source_hash: &[u8; 32],
) -> Result<Vec<Placement>> {
    let mut batches: Vec<(Vec<usize>, BTreeSet<DirectedEdgeIndex>)> = Vec::new();
    let mut all_face_edges = BTreeSet::new();
    for (i, face) in indexed.faces.iter().enumerate() {
        let uses = face
            .loops
            .iter()
            .flatten()
            .copied()
            .collect::<BTreeSet<_>>();
        all_face_edges.extend(uses.iter().map(|e| e.index() as usize));
        if let Some((faces, occupied)) = batches
            .iter_mut()
            .find(|(_, occupied)| occupied.is_disjoint(&uses))
        {
            faces.push(i);
            occupied.extend(uses);
        } else {
            if batches.len() >= 4096 {
                return Err("SKP container requires more than 4096 surface groups".into());
            }
            batches.push((vec![i], uses));
        }
    }
    if batches.len() <= 1 {
        let ids = tx
            .with_mesh(parent, |m, t, d| m.insert_disjoint(indexed, t, d))?
            .ok_or("overlapping geometry runs")?;
        return Ok(vec![Placement {
            container: parent,
            edges: ids.edges.into_iter().enumerate().collect(),
            faces: ids.faces.into_iter().enumerate().collect(),
        }]);
    }
    let used_vertices = indexed
        .edges
        .iter()
        .flat_map(|e| [e.a as usize, e.b as usize])
        .collect::<BTreeSet<_>>();
    let mut placements = Vec::new();
    for (part, (faces, uses)) in batches.into_iter().enumerate() {
        let mut edges = uses
            .iter()
            .map(|e| e.index() as usize)
            .collect::<BTreeSet<_>>();
        if part == 0 {
            edges.extend((0..indexed.edges.len()).filter(|i| !all_face_edges.contains(i)));
        }
        let edges = edges.into_iter().collect::<Vec<_>>();
        let mut vertices = edges
            .iter()
            .flat_map(|i| [indexed.edges[*i].a as usize, indexed.edges[*i].b as usize])
            .collect::<BTreeSet<_>>();
        if part == 0 {
            vertices.extend((0..indexed.vertices.len()).filter(|i| !used_vertices.contains(i)));
        }
        let vertices = vertices.into_iter().collect::<Vec<_>>();
        let vertex_indices = vertices
            .iter()
            .enumerate()
            .map(|(local, original)| (*original, local as u32))
            .collect::<BTreeMap<_, _>>();
        let edge_indices = edges
            .iter()
            .enumerate()
            .map(|(local, original)| (*original, local as u32))
            .collect::<BTreeMap<_, _>>();
        let mut piece = IndexedMesh {
            vertices: vertices.iter().map(|i| indexed.vertices[*i]).collect(),
            ..Default::default()
        };
        piece.edges = edges
            .iter()
            .map(|i| {
                let mut e = indexed.edges[*i];
                e.a = vertex_indices[&(e.a as usize)];
                e.b = vertex_indices[&(e.b as usize)];
                e
            })
            .collect();
        piece.faces = faces
            .iter()
            .map(|i| {
                let mut f = indexed.faces[*i].clone();
                for edge in f.loops.iter_mut().flatten() {
                    let local = edge_indices[&(edge.index() as usize)];
                    *edge = if edge.is_reversed() {
                        DirectedEdgeIndex::reversed(local)
                    } else {
                        DirectedEdgeIndex::forward(local)
                    };
                }
                f
            })
            .collect();
        let hash =
            blake3::hash(format!("SKP-surfaces:{source_hash:?}:{parent:?}:{part}").as_bytes());
        let mut uid = [0; 16];
        uid.copy_from_slice(&hash.as_bytes()[..16]);
        let name = format!("Imported surfaces {}", part + 1);
        let mut definition = ComponentDefinition::new(uuid::Uuid::from_bytes(uid), &name);
        definition.is_internal_group = true;
        let definition = tx.insert_definition(definition)?;
        tx.insert_entity(
            parent,
            EntityRecord::Group(Group {
                definition,
                transform: DAffine3::IDENTITY,
                name,
            }),
        )?;
        let container = ContainerRef::Definition(definition);
        let ids = tx
            .with_mesh(container, |m, t, d| m.insert_disjoint(&piece, t, d))?
            .ok_or("overlapping surface groups")?;
        placements.push(Placement {
            container,
            edges: edges.into_iter().zip(ids.edges).collect(),
            faces: faces.into_iter().zip(ids.faces).collect(),
        });
    }
    Ok(placements)
}
