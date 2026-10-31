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

#[derive(Default)]
struct Batch {
    faces: Vec<usize>,
    uses: BTreeSet<DirectedEdgeIndex>,
    edges: BTreeSet<usize>,
    vertices: BTreeSet<usize>,
}

fn assign(
    batches: &mut Vec<Batch>,
    face: Option<usize>,
    uses: BTreeSet<DirectedEdgeIndex>,
    edges: BTreeSet<usize>,
    vertices: BTreeSet<usize>,
    conflicts: &super::conflicts::Conflicts,
) -> Result<()> {
    let candidate = batches.iter().position(|b| {
        b.uses.is_disjoint(&uses)
            && edges
                .iter()
                .all(|e| conflicts.edges[*e].is_disjoint(&b.edges))
            && vertices
                .iter()
                .all(|v| conflicts.vertices[*v].is_disjoint(&b.vertices))
    });
    let index = if let Some(i) = candidate {
        i
    } else {
        if batches.len() >= 4096 {
            return Err("SKP container requires more than 4096 surface groups".into());
        }
        batches.push(Batch::default());
        batches.len() - 1
    };
    let batch = &mut batches[index];
    batch.faces.extend(face);
    batch.uses.extend(uses);
    batch.edges.extend(edges);
    batch.vertices.extend(vertices);
    Ok(())
}

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
    let mut batches = Vec::new();
    let conflicts = super::conflicts::detect(indexed, tx.document().tolerances());
    let mut all_face_edges = BTreeSet::new();
    for (i, face) in indexed.faces.iter().enumerate() {
        let uses = face
            .loops
            .iter()
            .flatten()
            .copied()
            .collect::<BTreeSet<_>>();
        all_face_edges.extend(uses.iter().map(|e| e.index() as usize));
        let edges = uses
            .iter()
            .map(|e| e.index() as usize)
            .collect::<BTreeSet<_>>();
        let vertices = edges
            .iter()
            .flat_map(|i| [indexed.edges[*i].a as usize, indexed.edges[*i].b as usize])
            .collect();
        assign(&mut batches, Some(i), uses, edges, vertices, &conflicts)?;
    }
    for i in 0..indexed.edges.len() {
        if !all_face_edges.contains(&i) {
            let edge = indexed.edges[i];
            assign(
                &mut batches,
                None,
                BTreeSet::new(),
                BTreeSet::from([i]),
                BTreeSet::from([edge.a as usize, edge.b as usize]),
                &conflicts,
            )?;
        }
    }
    let used_vertices = indexed
        .edges
        .iter()
        .flat_map(|e| [e.a as usize, e.b as usize])
        .collect::<BTreeSet<_>>();
    for i in 0..indexed.vertices.len() {
        if !used_vertices.contains(&i) {
            assign(
                &mut batches,
                None,
                BTreeSet::new(),
                BTreeSet::new(),
                BTreeSet::from([i]),
                &conflicts,
            )?;
        }
    }
    if batches.len() <= 1 {
        let mut indexed = indexed.clone();
        super::curves::split_chains(&mut indexed);
        let ids = tx
            .with_mesh(parent, |m, t, d| m.insert_disjoint(&indexed, t, d))?
            .ok_or("overlapping geometry runs")?;
        return Ok(vec![Placement {
            container: parent,
            edges: ids.edges.into_iter().enumerate().collect(),
            faces: ids.faces.into_iter().enumerate().collect(),
        }]);
    }
    let mut placements = Vec::new();
    for (part, batch) in batches.into_iter().enumerate() {
        let faces = batch.faces;
        let edges = batch.edges.into_iter().collect::<Vec<_>>();
        let vertices = batch.vertices.into_iter().collect::<Vec<_>>();
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
            curves: indexed.curves.clone(),
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
        super::curves::split_chains(&mut piece);
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
