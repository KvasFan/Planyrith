//! Detects source geometry that needs separate native surfaces (docs/09 §5).
use planyrith_core::{
    geom::mesh::IndexedMesh,
    math::{
        Segment,
        intersect::{SegmentContact, segment_segment},
    },
    tolerance::Tolerances,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Conflicts {
    pub edges: Vec<BTreeSet<usize>>,
    pub vertices: Vec<BTreeSet<usize>>,
}

pub(super) fn detect(mesh: &IndexedMesh, tol: &Tolerances) -> Conflicts {
    let mut result = Conflicts {
        edges: vec![BTreeSet::new(); mesh.edges.len()],
        vertices: vec![BTreeSet::new(); mesh.vertices.len()],
    };
    let mut cells = BTreeMap::<[i64; 3], Vec<usize>>::new();
    for (i, p) in mesh.vertices.iter().enumerate() {
        let cell = p.to_array().map(|x| (x / tol.length).floor() as i64);
        for x in cell[0] - 1..=cell[0] + 1 {
            for y in cell[1] - 1..=cell[1] + 1 {
                for z in cell[2] - 1..=cell[2] + 1 {
                    for &other in cells.get(&[x, y, z]).into_iter().flatten() {
                        if tol.same_point(*p, mesh.vertices[other]) {
                            result.vertices[i].insert(other);
                            result.vertices[other].insert(i);
                        }
                    }
                }
            }
        }
        cells.entry(cell).or_default().push(i);
    }
    let segments = mesh
        .edges
        .iter()
        .map(|e| Segment::new(mesh.vertices[e.a as usize], mesh.vertices[e.b as usize]))
        .collect::<Vec<_>>();
    let boxes = segments
        .iter()
        .map(|s| s.aabb().expanded(tol.length))
        .collect::<Vec<_>>();
    if boxes.len() < 2 {
        return result;
    }
    let mean = boxes
        .iter()
        .map(|b| (b.max - b.min).max_element())
        .sum::<f64>()
        / boxes.len() as f64;
    let size = (mean * 2.0).max(tol.length * 4.0);
    let index = |x: f64| (x / size).floor().clamp(-1e15, 1e15) as i64;
    let ranges = boxes
        .iter()
        .map(|b| (b.min.to_array().map(index), b.max.to_array().map(index)))
        .collect::<Vec<_>>();
    let mut entries = Vec::new();
    let mut large = Vec::new();
    for (i, (lo, hi)) in ranges.iter().enumerate() {
        let count = (0..3).fold(1i64, |n, k| n.saturating_mul(hi[k] - lo[k] + 1));
        if count > 64 {
            large.push(i);
            continue;
        }
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    entries.push(([x, y, z], i));
                }
            }
        }
    }
    entries.sort_unstable();
    let mut test = |i: usize, j: usize| {
        if !boxes[i].intersects(&boxes[j]) {
            return;
        }
        let a = mesh.edges[i];
        let b = mesh.edges[j];
        let crossing = match segment_segment(&segments[i], &segments[j], tol) {
            SegmentContact::None => false,
            SegmentContact::Overlap { .. } => true,
            SegmentContact::Point { point, .. } => ![a.a, a.b]
                .into_iter()
                .filter(|v| *v == b.a || *v == b.b)
                .any(|v| tol.same_point(mesh.vertices[v as usize], point)),
        };
        if crossing {
            result.edges[i].insert(j);
            result.edges[j].insert(i);
        }
    };
    let mut at = 0;
    while at < entries.len() {
        let cell = entries[at].0;
        let end = at + entries[at..].partition_point(|e| e.0 == cell);
        for a in at..end {
            for b in a + 1..end {
                let (i, j) = (entries[a].1, entries[b].1);
                let first = std::array::from_fn(|k| ranges[i].0[k].max(ranges[j].0[k]));
                if cell == first {
                    test(i, j);
                }
            }
        }
        at = end;
    }
    for (at, &i) in large.iter().enumerate() {
        for j in 0..boxes.len() {
            if i != j && (!large[..=at].contains(&j)) {
                test(i, j);
            }
        }
    }
    result
}
