//! Implements docs/05-modeling-tools.md §8, solid tools on groups/components.

use crate::commands::common::{
    definition_uuid, refuse_locked, remove_container_entity, targets_of, tool_steps,
};
use crate::commands::params::target_container;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{
    ComponentDefinition, ComponentInstance, Document, Entities, EntityRecord, Group, Query,
};
use crate::geom::boolean::{BooleanOp, boolean};
use crate::geom::{GeometryError, HalfEdgeMesh};
use crate::ids::{EntityId, Pid};
use glam::DAffine3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parameters shared by the six solid operations.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SolidParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// First solid; the cutter for subtract/trim.
    #[serde(default)]
    pub a: Option<Query>,
    /// Second solid; the object cut by the first for subtract/trim.
    #[serde(default)]
    pub b: Option<Query>,
    /// Two or more solids for union/outer-shell (alternative to a/b).
    #[serde(default)]
    pub targets: Option<Query>,
    /// Keep a component result when both inputs share a component definition.
    #[serde(default)]
    pub keep_component: bool,
}
/// Solid operation result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SolidOutput {
    /// Result groups or components; trim also returns the preserved cutter.
    pub result: Vec<Pid>,
    /// Total volume of the result, cubic millimeters.
    pub volume_mm3: f64,
    /// Whether the B-rep fallback was used.
    pub fallback_used: bool,
}

fn validate(p: &SolidParams, op: BooleanOp) -> Result<(), CommandError> {
    if p.targets.is_some() {
        if !matches!(op, BooleanOp::Union | BooleanOp::OuterShell) || p.a.is_some() || p.b.is_some()
        {
            return Err(CommandError::invalid_param(
                "targets",
                "only union/outer-shell accept targets instead of a and b",
            ));
        }
    } else if p.a.is_none() || p.b.is_none() {
        return Err(CommandError::invalid_param(
            "a",
            "give a and b, or targets for union/outer-shell",
        ));
    }
    Ok(())
}

fn apply(
    tx: &mut Transaction<'_>,
    p: SolidParams,
    op: BooleanOp,
) -> Result<SolidOutput, CommandError> {
    let target = target_container(tx, &p.common)?;
    let ids = if let Some(q) = &p.targets {
        targets_of(tx, &target, "targets", q)?
    } else {
        let mut ids = Vec::new();
        for (param, q) in [("a", &p.a), ("b", &p.b)] {
            let q = q
                .as_ref()
                .ok_or_else(|| CommandError::invalid_param(param, "missing"))?;
            let found = targets_of(tx, &target, param, q)?;
            if found.len() != 1 {
                return Err(CommandError::invalid_param(
                    param,
                    "must select exactly one solid group or instance",
                ));
            }
            ids.extend(found);
        }
        ids
    };
    if ids.len() < 2
        || ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != ids.len()
    {
        return Err(CommandError::invalid_param(
            "targets",
            "select at least two distinct solids",
        ));
    }
    refuse_locked(tx, target.container, &ids)?;
    let entities = tx
        .document()
        .container(target.container)
        .ok_or_else(|| CommandError::Internal("context missing".into()))?;
    let mut inputs = Vec::new();
    let mut definitions = Vec::new();
    for &id in &ids {
        let def = entities
            .definition_of(id)
            .and_then(|d| tx.document().definition(d))
            .ok_or_else(|| {
                CommandError::NotApplicable(
                    "solid tools require groups or component instances".into(),
                )
            })?;
        if !def.entities.groups().collect::<Vec<_>>().is_empty()
            || !def.entities.instances().collect::<Vec<_>>().is_empty()
            || !def.entities.mesh().is_solid()
        {
            return Err(CommandError::NotApplicable("solid must contain only a closed mesh, with no wire edges, open boundaries or nested instances".into()));
        }
        definitions.push(entities.definition_of(id));
        let transform = entities
            .transform_of(id)
            .ok_or_else(|| CommandError::Internal("solid transform missing".into()))?;
        let (indexed, _) = def.entities.mesh().to_indexed();
        let transformed = crate::geom::mesh::transform_indexed(&indexed, &transform);
        inputs.push(HalfEdgeMesh::from_indexed(&transformed, tx.tolerances())?);
    }
    let keep_component = p.keep_component
        && ids.iter().all(|id| matches!(id, EntityId::Instance(_)))
        && definitions.iter().all(|d| *d == definitions[0]);
    let tol = *tx.tolerances();
    let mut fallback_used = false;
    let mut output = if matches!(op, BooleanOp::Union | BooleanOp::OuterShell) {
        let mut merged = inputs[0].clone();
        for b in &inputs[1..] {
            let result = boolean(
                op,
                &merged,
                &DAffine3::IDENTITY,
                b,
                &DAffine3::IDENTITY,
                &tol,
            )?;
            fallback_used |= result.fallback_used;
            merged =
                result.solids.into_iter().next().ok_or_else(|| {
                    GeometryError::NonRobust("union produced an empty result".into())
                })?;
        }
        vec![merged]
    } else {
        let operation = if op == BooleanOp::Trim {
            BooleanOp::Subtract
        } else {
            op
        };
        // User order is cutter then target; the kernel takes minuend first.
        let (a, b) = if matches!(op, BooleanOp::Subtract | BooleanOp::Trim) {
            (&inputs[1], &inputs[0])
        } else {
            (&inputs[0], &inputs[1])
        };
        let result = boolean(
            operation,
            a,
            &DAffine3::IDENTITY,
            b,
            &DAffine3::IDENTITY,
            &tol,
        )?;
        fallback_used = result.fallback_used;
        result.solids
    };
    let mut volume_mm3 = output
        .iter()
        .try_fold(0.0, |v, m| m.volume().map(|w| v + w))?;
    let mut result = Vec::new();
    if op == BooleanOp::Trim {
        let pid = entities
            .pid_of(ids[0])
            .ok_or_else(|| CommandError::Internal("cutter pid missing".into()))?;
        result.push(pid);
        volume_mm3 += inputs[0].volume()?;
    }
    for (i, mesh) in output.drain(..).enumerate() {
        let name = format!("Solid result {}", i + 1);
        let entities = Entities::from_mesh(mesh, || tx.allocate_pid());
        let uuid = definition_uuid(tx.document(), &name, &entities);
        let mut def = ComponentDefinition::new(uuid, name.clone());
        def.is_internal_group = !keep_component;
        def.entities = entities;
        let definition = tx.insert_definition(def)?;
        let record = if keep_component {
            EntityRecord::Instance(ComponentInstance {
                definition,
                transform: DAffine3::IDENTITY,
                name: None,
                material_override: None,
                glued_to: None,
                dynamic_attrs: Default::default(),
            })
        } else {
            EntityRecord::Group(Group {
                definition,
                transform: DAffine3::IDENTITY,
                name,
            })
        };
        let (_, pid) = tx.insert_entity(target.container, record)?;
        result.push(pid);
    }
    for (i, id) in ids.into_iter().enumerate() {
        if op != BooleanOp::Trim || i != 0 {
            remove_container_entity(tx, target.container, id)?;
        }
    }
    Ok(SolidOutput {
        result,
        volume_mm3,
        fallback_used,
    })
}

macro_rules! solid_command {
    ($ty:ident,$name:literal,$label:literal,$op:ident) => {
        #[doc=$label]
        #[derive(Debug, Default, Clone, Copy)]
        pub struct $ty;
        impl Command for $ty {
            type Params = SolidParams;
            type Output = SolidOutput;
            fn name(&self) -> &'static str {
                $name
            }
            fn validate(&self, _: &Document, p: &SolidParams) -> Result<(), CommandError> {
                validate(p, BooleanOp::$op)
            }
            fn apply(
                &self,
                tx: &mut Transaction<'_>,
                p: SolidParams,
            ) -> Result<SolidOutput, CommandError> {
                apply(tx, p, BooleanOp::$op)
            }
            fn meta(&self) -> CommandMeta {
                CommandMeta {
                    name: $name.into(),
                    label: $label.into(),
                    category: CommandCategory::Solid,
                    view_only: false,
                    steps: tool_steps(&[
                        ("Pick the first solid (cutter)", ""),
                        ("Pick the second solid (target)", ""),
                    ]),
                }
            }
        }
    };
}
solid_command!(SolidUnion, "solid-union", "Solid Union", Union);
solid_command!(SolidSubtract, "solid-subtract", "Solid Subtract", Subtract);
solid_command!(
    SolidIntersect,
    "solid-intersect",
    "Solid Intersect",
    Intersect
);
solid_command!(SolidTrim, "solid-trim", "Solid Trim", Trim);
solid_command!(SolidSplit, "solid-split", "Solid Split", Split);
solid_command!(OuterShell, "outer-shell", "Outer Shell", OuterShell);

/// Registers the solid tools.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(SolidUnion);
    registry.register(SolidSubtract);
    registry.register(SolidIntersect);
    registry.register(SolidTrim);
    registry.register(SolidSplit);
    registry.register(OuterShell);
}
