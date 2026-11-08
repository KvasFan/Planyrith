//! Implements docs/05-modeling-tools.md §4.17 `fillet` and §4.18 `chamfer`:
//! edges of the solid in the edit context rounded or cut flat
//! (docs/03 §4.15). The container's geometry is replaced by the result in
//! one undo step.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{FaceCopy, redraw_geometry, targets_of, tool_steps};
use crate::commands::params::target_container;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Document, Query};
use crate::geom::rounding::{Rounding, round_edges};
use crate::geom::{EdgeFlags, GeometryError};
use crate::ids::{EdgeId, EntityId, FaceId};
use crate::units::Length;

fn eight() -> u32 {
    8
}

/// `fillet` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FilletParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Edges of the solid in the edit context.
    pub edges: Query,
    /// Radius of the rounding.
    pub radius: Length,
    /// Straight pieces of each arc, 1–64 (8).
    #[serde(default = "eight")]
    pub segments: u32,
}

/// `chamfer` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChamferParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Edges of the solid in the edit context.
    pub edges: Query,
    /// Distance of the cut along both faces from the edge.
    pub distance: Length,
}

/// `fillet` / `chamfer` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RoundingOutput {
    /// Edges rounded.
    pub rounded: usize,
    /// Faces of the solid afterwards.
    pub faces: usize,
    /// Its volume, cubic millimeters.
    pub volume_mm3: f64,
}

fn positive(name: &str, length: Length) -> Result<(), CommandError> {
    if length.as_mm() > 0.0 && length.as_mm().is_finite() {
        Ok(())
    } else {
        Err(CommandError::OutOfRange {
            name: name.into(),
            reason: "must be a positive length".into(),
        })
    }
}

/// Rounds the selected edges and puts the result in place of the
/// container's geometry.
fn apply(
    tx: &mut Transaction<'_>,
    common: &CommonParams,
    query: &Query,
    shape: Rounding,
) -> Result<RoundingOutput, CommandError> {
    let target = target_container(tx, common)?;
    let edges: Vec<EdgeId> = targets_of(tx, &target, "edges", query)?
        .into_iter()
        .filter_map(|id| match id {
            EntityId::Edge(e) => Some(e),
            _ => None,
        })
        .collect();
    if edges.is_empty() {
        return Err(CommandError::invalid_param(
            "edges",
            "select at least one edge",
        ));
    }
    let mesh = tx
        .document()
        .container(target.container)
        .ok_or_else(|| CommandError::Internal("context missing".into()))?
        .mesh()
        .clone();
    let out = round_edges(&mesh, &edges, shape, tx.tolerances())?;
    let rounded = out.mesh;
    let volume_mm3 = rounded.volume()?;
    // The old geometry goes; the result is drawn in its place.
    tx.with_mesh(target.container, |m, tol, d| {
        m.transact(tol, d, |m, log| {
            let faces: Vec<FaceId> = m.faces().map(|(f, _)| f).collect();
            for f in faces {
                m.erase_face_inner(f, true, log)?;
            }
            let edges: Vec<EdgeId> = m.edges().map(|(e, _)| e).collect();
            for e in edges {
                if m.edge(e).is_some() {
                    m.erase_edge_inner(e, tol, log)?;
                }
            }
            Ok::<_, GeometryError>(())
        })
    })?;
    let mut segments = Vec::with_capacity(rounded.edge_count());
    for (e, edge) in rounded.edges() {
        let s = rounded.edge_segment(e)?;
        let flags = if edge.flags.contains(EdgeFlags::SOFT) {
            EdgeFlags::SOFT | EdgeFlags::SMOOTH
        } else {
            EdgeFlags::NONE
        };
        segments.push((s.a, s.b, flags));
    }
    let mut faces: Vec<FaceCopy> = Vec::with_capacity(rounded.face_count());
    for (f, face) in rounded.faces() {
        let outer = rounded
            .face_vertices(f)?
            .into_iter()
            .map(|v| rounded.position(v))
            .collect::<Result<Vec<_>, _>>()?;
        faces.push((outer, face.plane, face.sides.clone()));
    }
    redraw_geometry(tx, target.container, &segments, &faces)?;
    let faces = tx
        .document()
        .container(target.container)
        .map_or(0, |c| c.mesh().face_count());
    Ok(RoundingOutput {
        rounded: out.rounded,
        faces,
        volume_mm3,
    })
}

/// The `fillet` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Fillet;

impl Command for Fillet {
    type Params = FilletParams;
    type Output = RoundingOutput;

    fn name(&self) -> &'static str {
        "fillet"
    }

    fn validate(&self, _: &Document, p: &FilletParams) -> Result<(), CommandError> {
        positive("radius", p.radius)?;
        if !(1..=64).contains(&p.segments) {
            return Err(CommandError::OutOfRange {
                name: "segments".into(),
                reason: "must be between 1 and 64".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: FilletParams,
    ) -> Result<RoundingOutput, CommandError> {
        apply(
            tx,
            &p.common,
            &p.edges,
            Rounding::Fillet {
                radius: p.radius.as_mm(),
                segments: p.segments,
            },
        )
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "fillet".into(),
            label: "Fillet".into(),
            category: CommandCategory::Edit,
            view_only: false,
            steps: tool_steps(&[("Pick the edges to round", "type the radius")]),
        }
    }
}

/// The `chamfer` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Chamfer;

impl Command for Chamfer {
    type Params = ChamferParams;
    type Output = RoundingOutput;

    fn name(&self) -> &'static str {
        "chamfer"
    }

    fn validate(&self, _: &Document, p: &ChamferParams) -> Result<(), CommandError> {
        positive("distance", p.distance)
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ChamferParams,
    ) -> Result<RoundingOutput, CommandError> {
        apply(
            tx,
            &p.common,
            &p.edges,
            Rounding::Chamfer {
                distance: p.distance.as_mm(),
            },
        )
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "chamfer".into(),
            label: "Chamfer".into(),
            category: CommandCategory::Edit,
            view_only: false,
            steps: tool_steps(&[("Pick the edges to cut", "type the distance")]),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn run(e: &mut Engine, name: &str, params: Value) -> Value {
        e.execute_erased(name, params)
            .unwrap_or_else(|err| panic!("{name}: {err}"))
    }

    /// A 100 × 50 × 40 box at the model root.
    fn boxed() -> Engine {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [100, 50, 0] }),
        );
        run(
            &mut e,
            "push-pull",
            json!({ "face": { "Query": "face & last" }, "distance": 40 }),
        );
        e
    }

    #[test]
    fn filleting_the_long_edges_rounds_them_in_one_undo_step() {
        let mut e = boxed();
        let out = run(
            &mut e,
            "fillet",
            json!({ "edges": "edge & length=100", "radius": 10, "segments": 4 }),
        );
        assert_eq!(out["rounded"], 4);
        assert_eq!(out["faces"], 6 + 4 * 4);
        let polygon = 0.5 * 4.0 * 100.0 * libm::sin(std::f64::consts::FRAC_PI_2 / 4.0);
        let expected = 200_000.0 - 4.0 * (100.0 - polygon) * 100.0;
        let volume = out["volume_mm3"].as_f64().unwrap();
        assert!((volume - expected).abs() < 1e-6, "{volume} vs {expected}");
        let check = run(&mut e, "solid-check", json!({}));
        assert_eq!(check["printable"], true, "{check}");
        assert!(crate::diagnostics::validate(e.document()).is_clean());
        e.undo().unwrap();
        let mesh = e
            .document()
            .container(crate::document::ContainerRef::Model)
            .unwrap()
            .mesh();
        assert_eq!(mesh.face_count(), 6);
    }

    #[test]
    fn a_chamfer_on_a_corner_pair_and_bad_input() {
        let mut e = boxed();
        let out = run(
            &mut e,
            "chamfer",
            json!({ "edges": "edge & length=40", "distance": 5 }),
        );
        assert_eq!(out["rounded"], 4);
        assert_eq!(out["faces"], 10);
        for (params, why) in [
            (json!({ "edges": "face", "distance": 5 }), "edges"),
            (json!({ "edges": "edge", "distance": 0 }), "distance"),
        ] {
            let err = e.execute_erased("chamfer", params).unwrap_err();
            assert!(err.to_string().contains(why), "{err}");
        }
        let err = e
            .execute_erased(
                "fillet",
                json!({ "edges": "edge", "radius": 5, "segments": 0 }),
            )
            .unwrap_err();
        assert!(err.to_string().contains("segments"), "{err}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod corner_tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use serde_json::json;
    use std::sync::Arc;

    /// Every edge of a box: the roundings meet at the corners and the
    /// result is still a printable solid, smaller than the box.
    #[test]
    fn all_twelve_edges_of_a_box() {
        for (name, params) in [
            (
                "fillet",
                json!({ "edges": "edge", "radius": 5, "segments": 3 }),
            ),
            ("chamfer", json!({ "edges": "edge", "distance": 5 })),
        ] {
            let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
            e.execute_erased(
                "rectangle",
                json!({ "corner_a": [0, 0, 0], "corner_b": [100, 50, 0] }),
            )
            .unwrap();
            e.execute_erased(
                "push-pull",
                json!({ "face": { "Query": "face & last" }, "distance": 40 }),
            )
            .unwrap();
            let out = e.execute_erased(name, params).unwrap();
            assert_eq!(out["rounded"], 12, "{name}");
            let volume = out["volume_mm3"].as_f64().unwrap();
            assert!(volume < 200_000.0 && volume > 190_000.0, "{name}: {volume}");
            let check = e.execute_erased("solid-check", json!({})).unwrap();
            assert_eq!(check["printable"], true, "{name}: {check}");
        }
    }
}
