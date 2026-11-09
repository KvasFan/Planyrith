//! Implements docs/05-modeling-tools.md §4.22 `revolve`: a solid group swept
//! by a face turning about an axis (docs/03 §4.19).

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{definition_uuid, targets_of};
use crate::commands::params::{PointSchema, VectorSchema, check_point, target_container};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ComponentDefinition, Document, Entities, EntityRecord, Group, Query};
use crate::geom::revolve::revolve;
use crate::ids::{EntityId, Pid};
use crate::units::Angle;

fn full_turn() -> Angle {
    Angle::radians(std::f64::consts::TAU)
}

fn segments() -> u32 {
    24
}

/// `revolve` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RevolveParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The profile: one face of the edit context.
    pub profile: Query,
    /// An edge of the edit context as the axis (instead of `axis_point`
    /// and `axis`).
    #[serde(default)]
    pub axis_edge: Option<Query>,
    /// A point of the axis.
    #[serde(default)]
    #[schemars(with = "Option<PointSchema>")]
    pub axis_point: Option<DVec3>,
    /// The axis direction; the profile turns counter-clockwise about it.
    #[serde(default)]
    #[schemars(with = "Option<VectorSchema>")]
    pub axis: Option<DVec3>,
    /// How far to turn (a full turn by default).
    #[serde(default = "full_turn")]
    pub angle: Angle,
    /// Straight pieces per full turn.
    #[serde(default = "segments")]
    pub segments: u32,
    /// Keep the profile face (by default it gives way to the solid).
    #[serde(default)]
    pub keep_profile: bool,
}

/// `revolve` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RevolveOutput {
    /// The new group holding the solid.
    pub group: Pid,
    /// Faces of the solid.
    pub faces: usize,
    /// Its volume, cubic millimeters.
    pub volume_mm3: f64,
}

/// The `revolve` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Revolve;

impl Command for Revolve {
    type Params = RevolveParams;
    type Output = RevolveOutput;

    fn name(&self) -> &'static str {
        "revolve"
    }

    fn validate(&self, _: &Document, p: &RevolveParams) -> Result<(), CommandError> {
        match (&p.axis_edge, p.axis_point, p.axis) {
            (Some(_), None, None) => {}
            (None, Some(point), Some(axis)) => {
                check_point("axis_point", point)?;
                if axis.try_normalize().is_none() {
                    return Err(CommandError::invalid_param("axis", "must be non-zero"));
                }
            }
            _ => {
                return Err(CommandError::invalid_param(
                    "axis",
                    "give axis_edge, or axis_point and axis",
                ));
            }
        }
        let a = p.angle.as_radians();
        if !(a.is_finite() && a > 0.0 && a <= std::f64::consts::TAU + 1e-9) {
            return Err(CommandError::OutOfRange {
                name: "angle".into(),
                reason: "must be more than 0° and at most 360°".into(),
            });
        }
        if !(3..=720).contains(&p.segments) {
            return Err(CommandError::OutOfRange {
                name: "segments".into(),
                reason: "must be between 3 and 720".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: RevolveParams,
    ) -> Result<RevolveOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let faces: Vec<_> = targets_of(tx, &target, "profile", &p.profile)?
            .into_iter()
            .filter_map(|id| match id {
                EntityId::Face(f) => Some(f),
                _ => None,
            })
            .collect();
        let [face] = faces.as_slice() else {
            return Err(CommandError::invalid_param(
                "profile",
                "must name exactly one face",
            ));
        };
        let face = *face;
        let (origin, axis) = match &p.axis_edge {
            Some(q) => {
                let edges: Vec<_> = targets_of(tx, &target, "axis_edge", q)?
                    .into_iter()
                    .filter_map(|id| match id {
                        EntityId::Edge(e) => Some(e),
                        _ => None,
                    })
                    .collect();
                let [edge] = edges.as_slice() else {
                    return Err(CommandError::invalid_param(
                        "axis_edge",
                        "must name exactly one edge",
                    ));
                };
                let mesh = tx
                    .document()
                    .container(target.container)
                    .ok_or_else(|| CommandError::Internal("context missing".into()))?
                    .mesh();
                let s = mesh.edge_segment(*edge)?;
                (s.a, s.b - s.a)
            }
            None => {
                // Given in world coordinates, like other points.
                let local = target.local;
                (
                    local.transform_point3(p.axis_point.unwrap_or_default()),
                    local.transform_vector3(p.axis.unwrap_or(DVec3::Z)),
                )
            }
        };
        let (loops, sides) = {
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("context missing".into()))?
                .mesh();
            let f = mesh.face_ref(face)?;
            let mut loops = vec![mesh.loop_positions(f.outer)?];
            for &h in &f.holes {
                loops.push(mesh.loop_positions(h)?);
            }
            (loops, f.sides.clone())
        };
        let solid = revolve(
            &loops,
            origin,
            axis,
            p.angle.as_radians(),
            p.segments,
            sides,
            tx.tolerances(),
        )?;
        let volume_mm3 = solid.volume()?;
        let face_count = solid.face_count();
        if !p.keep_profile {
            tx.with_mesh(target.container, |m, tol, d| {
                if m.face(face).is_some() {
                    m.erase_face(face, true, tol, d)?;
                }
                Ok(())
            })?;
        }
        let name = "Revolved".to_string();
        let entities = Entities::from_mesh(solid, || tx.allocate_pid());
        let uuid = definition_uuid(tx.document(), &name, &entities);
        let mut def = ComponentDefinition::new(uuid, name.clone());
        def.is_internal_group = true;
        def.entities = entities;
        let definition = tx.insert_definition(def)?;
        let (_, group) = tx.insert_entity(
            target.container,
            EntityRecord::Group(Group {
                definition,
                transform: DAffine3::IDENTITY,
                name,
            }),
        )?;
        Ok(RevolveOutput {
            group,
            faces: face_count,
            volume_mm3,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "revolve".into(),
            label: "Revolve".into(),
            category: CommandCategory::Edit,
            view_only: false,
            // Collected by Tools › Revolve… (docs/05 §4.22).
            steps: Vec::new(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn a_profile_turns_into_a_printable_ring_and_undo_restores_it() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        e.execute_erased(
            "rectangle",
            json!({ "corner_a": [50, 0, 0], "corner_b": [100, 0, 40] }),
        )
        .unwrap();
        let before = e.document().clone();
        let out = e
            .execute_erased(
                "revolve",
                json!({ "profile": "face", "axis_point": [0, 0, 0], "axis": [0, 0, 1],
                        "segments": 36 }),
            )
            .unwrap();
        // Annulus area × height, as a 36-gon.
        let ring = 0.5
            * 36.0
            * (100.0f64.powi(2) - 50.0f64.powi(2))
            * libm::sin(std::f64::consts::TAU / 36.0);
        let v = out["volume_mm3"].as_f64().unwrap();
        assert!((v - ring * 40.0).abs() < 1e-3, "{v}");
        let check = e.execute_erased("solid-check", json!({})).unwrap();
        assert_eq!(check["printable"], true, "{check}");
        e.undo().unwrap();
        assert_eq!(
            e.document().model.mesh().face_count(),
            before.model.mesh().face_count()
        );

        // About a drawn axis line, a quarter turn (angles are radians in
        // JSON), keeping the profile.
        e.execute_erased("line", json!({ "points": [[0, 0, -10], [0, 0, 100]] }))
            .unwrap();
        let out = e
            .execute_erased(
                "revolve",
                json!({ "profile": "face", "axis_edge": "edge & length>100",
                        "angle": std::f64::consts::FRAC_PI_2, "segments": 36,
                        "keep_profile": true }),
            )
            .unwrap();
        let v = out["volume_mm3"].as_f64().unwrap();
        assert!((v * 4.0 - ring * 40.0).abs() < 1e-3, "{v}");
        assert_eq!(
            e.document().model.mesh().face_count(),
            1,
            "the profile stayed"
        );
    }
}
