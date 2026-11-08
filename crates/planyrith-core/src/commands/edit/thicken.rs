//! Implements docs/05-modeling-tools.md §4.19 `thicken`: faces of the edit
//! context made into a solid group of a given thickness (docs/03 §4.16).

use std::collections::BTreeSet;

use glam::DAffine3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{definition_uuid, targets_of, tool_steps};
use crate::commands::params::target_container;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ComponentDefinition, Document, Entities, EntityRecord, Group, Query};
use crate::geom::thicken::thicken;
use crate::ids::{EntityId, FaceId, Pid};
use crate::units::Length;

/// `thicken` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ThickenParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Faces of the surface, in the edit context.
    pub faces: Query,
    /// Thickness along the faces' fronts; negative: behind them.
    pub thickness: Length,
    /// Half the thickness each way.
    #[serde(default)]
    pub centered: bool,
    /// Keep the surface's faces (by default they give way to the solid).
    #[serde(default)]
    pub keep_surface: bool,
}

/// `thicken` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ThickenOutput {
    /// The new group holding the solid.
    pub group: Pid,
    /// Faces of the solid.
    pub faces: usize,
    /// Its volume, cubic millimeters.
    pub volume_mm3: f64,
}

/// The `thicken` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Thicken;

impl Command for Thicken {
    type Params = ThickenParams;
    type Output = ThickenOutput;

    fn name(&self) -> &'static str {
        "thicken"
    }

    fn validate(&self, _: &Document, p: &ThickenParams) -> Result<(), CommandError> {
        let t = p.thickness.as_mm();
        if t == 0.0 || !t.is_finite() {
            return Err(CommandError::OutOfRange {
                name: "thickness".into(),
                reason: "must be a non-zero length".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ThickenParams,
    ) -> Result<ThickenOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let faces: BTreeSet<FaceId> = targets_of(tx, &target, "faces", &p.faces)?
            .into_iter()
            .filter_map(|id| match id {
                EntityId::Face(f) => Some(f),
                _ => None,
            })
            .collect();
        if faces.is_empty() {
            return Err(CommandError::invalid_param(
                "faces",
                "select at least one face",
            ));
        }
        let solid = {
            let entities = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("context missing".into()))?;
            thicken(
                entities.mesh(),
                &faces,
                p.thickness.as_mm(),
                p.centered,
                tx.tolerances(),
            )?
        };
        let volume_mm3 = solid.volume()?;
        let face_count = solid.face_count();
        if !p.keep_surface {
            tx.with_mesh(target.container, |m, tol, d| {
                for &f in &faces {
                    if m.face(f).is_some() {
                        m.erase_face(f, true, tol, d)?;
                    }
                }
                Ok(())
            })?;
        }
        let name = "Thickened".to_string();
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
        Ok(ThickenOutput {
            group,
            faces: face_count,
            volume_mm3,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "thicken".into(),
            label: "Thicken".into(),
            category: CommandCategory::Edit,
            view_only: false,
            steps: tool_steps(&[("Pick the faces to thicken", "type the thickness")]),
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
    fn a_rectangle_becomes_a_printable_slab_and_undo_restores_it() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        e.execute_erased(
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [100, 50, 0] }),
        )
        .unwrap();
        let out = e
            .execute_erased("thicken", json!({ "faces": "face", "thickness": 3 }))
            .unwrap();
        assert_eq!(out["faces"], 6);
        assert!((out["volume_mm3"].as_f64().unwrap() - 15_000.0).abs() < 1e-6);
        let check = e.execute_erased("solid-check", json!({})).unwrap();
        assert_eq!(check["printable"], true, "{check}");
        assert_eq!(
            check["objects"].as_array().unwrap().len(),
            1,
            "the sheet is gone"
        );
        e.undo().unwrap();
        let check = e.execute_erased("solid-check", json!({})).unwrap();
        assert_eq!(check["objects"][0]["name"], "Model");
        assert!(
            e.execute_erased("thicken", json!({ "faces": "edge", "thickness": 3 }))
                .is_err()
        );
        assert!(
            e.execute_erased("thicken", json!({ "faces": "face", "thickness": 0 }))
                .is_err()
        );
    }
}
