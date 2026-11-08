//! Implements docs/05-modeling-tools.md §4.20 `loft`: a solid group through
//! two or more face profiles in order (docs/03 §4.17).

use glam::DAffine3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::{definition_uuid, targets_of, tool_steps};
use crate::commands::params::target_container;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ComponentDefinition, Document, Entities, EntityRecord, Group, Query};
use crate::geom::loft::loft;
use crate::ids::{EntityId, FaceId, Pid};

/// `loft` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LoftParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The profiles in order: each query names one face of the edit context.
    pub profiles: Vec<Query>,
    /// Keep the profile faces (by default they give way to the solid).
    #[serde(default)]
    pub keep_profiles: bool,
}

/// `loft` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LoftOutput {
    /// The new group holding the solid.
    pub group: Pid,
    /// Faces of the solid.
    pub faces: usize,
    /// Its volume, cubic millimeters.
    pub volume_mm3: f64,
}

/// The `loft` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Loft;

impl Command for Loft {
    type Params = LoftParams;
    type Output = LoftOutput;

    fn name(&self) -> &'static str {
        "loft"
    }

    fn validate(&self, _: &Document, p: &LoftParams) -> Result<(), CommandError> {
        if p.profiles.len() < 2 {
            return Err(CommandError::invalid_param(
                "profiles",
                "give two or more profiles",
            ));
        }
        Ok(())
    }

    fn apply(&self, tx: &mut Transaction<'_>, p: LoftParams) -> Result<LoftOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let mut faces: Vec<FaceId> = Vec::new();
        for (i, q) in p.profiles.iter().enumerate() {
            let found: Vec<FaceId> = targets_of(tx, &target, "profiles", q)?
                .into_iter()
                .filter_map(|id| match id {
                    EntityId::Face(f) => Some(f),
                    _ => None,
                })
                .collect();
            match found.as_slice() {
                [f] if !faces.contains(f) => faces.push(*f),
                _ => {
                    return Err(CommandError::invalid_param(
                        "profiles",
                        format!(
                            "profile {} must name exactly one face, not used before",
                            i + 1
                        ),
                    ));
                }
            }
        }
        let (loops, sides) = {
            let mesh = tx
                .document()
                .container(target.container)
                .ok_or_else(|| CommandError::Internal("context missing".into()))?
                .mesh();
            let mut loops = Vec::with_capacity(faces.len());
            for &f in &faces {
                if mesh.face(f).is_some_and(|x| !x.holes.is_empty()) {
                    return Err(CommandError::NotApplicable(
                        "profiles with holes are not supported; loft their outlines".into(),
                    ));
                }
                loops.push(
                    mesh.face_vertices(f)?
                        .into_iter()
                        .map(|v| mesh.position(v))
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
            let sides = mesh
                .face(faces[0])
                .map(|x| x.sides.clone())
                .unwrap_or_default();
            (loops, sides)
        };
        let solid = loft(&loops, sides, tx.tolerances())?;
        let volume_mm3 = solid.volume()?;
        let face_count = solid.face_count();
        if !p.keep_profiles {
            tx.with_mesh(target.container, |m, tol, d| {
                for &f in &faces {
                    if m.face(f).is_some() {
                        m.erase_face(f, true, tol, d)?;
                    }
                }
                Ok(())
            })?;
        }
        let name = "Loft".to_string();
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
        Ok(LoftOutput {
            group,
            faces: face_count,
            volume_mm3,
        })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "loft".into(),
            label: "Loft".into(),
            category: CommandCategory::Edit,
            view_only: false,
            steps: tool_steps(&[("Pick the profile faces in order", "")]),
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
    fn a_square_and_a_circle_above_it_make_a_solid() {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        e.execute_erased(
            "rectangle",
            json!({ "corner_a": [-50, -50, 0], "corner_b": [50, 50, 0] }),
        )
        .unwrap();
        e.execute_erased(
            "circle",
            json!({ "center": [0, 0, 200], "radius": 30, "normal": [0, 0, 1], "segments": 16 }),
        )
        .unwrap();
        let out = e
            .execute_erased(
                "loft",
                json!({ "profiles": ["face & plane:xy@0", "face & plane:xy@200"] }),
            )
            .unwrap();
        let volume = out["volume_mm3"].as_f64().unwrap();
        // Between the prism of the circle and that of the square.
        assert!(
            volume > 200.0 * 2800.0 && volume < 200.0 * 10_000.0,
            "{volume}"
        );
        let check = e.execute_erased("solid-check", json!({})).unwrap();
        assert_eq!(check["printable"], true, "{check}");
        let err = e
            .execute_erased("loft", json!({ "profiles": ["face", "face"] }))
            .unwrap_err();
        assert!(err.to_string().contains("profiles"), "{err}");
    }
}
