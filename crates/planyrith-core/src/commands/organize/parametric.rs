//! Implements docs/05-modeling-tools.md §5.14 parametric components:
//! `parameter-set` (a component's parameter), `instance-parameter-set` (an
//! instance's own value), `part-rule-set` (a part's position, size or
//! visibility as a formula) and `parametric-update` (evaluate and apply).
//! Parameters and rules are attributes (docs/04 §9.1): `parameters` on the
//! definition and on instances, `rules` on parts; formulas follow
//! `crate::params`.

use std::collections::BTreeMap;

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::common::targets_of;
use crate::commands::organize::components::{MakeUnique, MakeUniqueParams};
use crate::commands::params::target_container;
use crate::commands::transaction::AttrTarget;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ContainerRef, Document, EntityView, Query, Value};
use crate::ids::{DefinitionId, EntityId};
use crate::params::{FormulaError, Names, Parameters, evaluate};

/// Dictionary of parameters (definitions and instances).
pub const PARAMETERS: &str = "parameters";
/// Dictionary of a part's rules.
pub const RULES: &str = "rules";
/// Properties a rule may set.
pub const PROPERTIES: [&str; 7] = ["x", "y", "z", "size_x", "size_y", "size_z", "visible"];

fn formula_error(e: FormulaError) -> CommandError {
    CommandError::invalid_param("formula", e.0)
}

fn check_name(name: &str) -> Result<(), CommandError> {
    let ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !["min", "max", "round", "floor", "ceil", "abs"].contains(&name);
    if ok {
        Ok(())
    } else {
        Err(CommandError::invalid_param(
            "name",
            "a parameter name is letters, digits and `_`, not a function's name",
        ))
    }
}

/// A formula that reads: numbers, units, names and functions in place.
fn check_formula(formula: &str) -> Result<(), CommandError> {
    struct Any;
    impl Names for Any {
        fn value(&mut self, _: &str) -> Result<f64, FormulaError> {
            Ok(1.0)
        }
    }
    match evaluate(formula, &mut Any) {
        Ok(_) => Ok(()),
        // Division by a name standing for 1 never fails; a literal zero
        // divisor is still a mistake.
        Err(e) => Err(formula_error(e)),
    }
}

/// The formulas stored in dictionary `dict` of `attrs`.
fn formulas(
    attrs: Option<&crate::document::AttributeDictionaries>,
    dict: &str,
) -> BTreeMap<String, String> {
    attrs
        .and_then(|a| a.dictionary(dict))
        .map(|d| {
            d.iter()
                .filter_map(|(k, v)| match v {
                    Value::Str(s) => Some((k.clone(), s.clone())),
                    Value::Float(f) => Some((k.clone(), format!("{f}"))),
                    Value::Int(i) => Some((k.clone(), i.to_string())),
                    Value::Length(l) => Some((k.clone(), format!("{}", l.as_mm()))),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

// ----- parameter-set ----------------------------------------------------------------------

/// `parameter-set` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ParameterSetParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The component (definition name).
    pub component: String,
    /// Parameter name (letters, digits, `_`).
    pub name: String,
    /// Its formula, e.g. `600`, `60cm`, `width - 2 * board`; absent with
    /// `remove`.
    #[serde(default)]
    pub formula: Option<String>,
    /// Remove the parameter.
    #[serde(default)]
    pub remove: bool,
}

/// What a parameter command set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ParameterOutput {
    /// Entities or definitions changed.
    pub changed: usize,
}

fn definition_named(doc: &Document, name: &str) -> Result<DefinitionId, CommandError> {
    crate::commands::organize::components::component_named(doc, name).map_err(|_| {
        CommandError::invalid_param("component", format!("no component named `{name}`"))
    })
}

fn check_value(name: &str, formula: Option<&String>, remove: bool) -> Result<(), CommandError> {
    check_name(name)?;
    match (formula, remove) {
        (Some(f), false) => check_formula(f),
        (None, true) => Ok(()),
        _ => Err(CommandError::invalid_param(
            "formula",
            "give a formula, or remove",
        )),
    }
}

/// The `parameter-set` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct ParameterSet;

impl Command for ParameterSet {
    type Params = ParameterSetParams;
    type Output = ParameterOutput;

    fn name(&self) -> &'static str {
        "parameter-set"
    }

    fn validate(&self, doc: &Document, p: &ParameterSetParams) -> Result<(), CommandError> {
        definition_named(doc, &p.component)?;
        check_value(&p.name, p.formula.as_ref(), p.remove)
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ParameterSetParams,
    ) -> Result<ParameterOutput, CommandError> {
        let id = definition_named(tx.document(), &p.component)?;
        tx.set_attribute(
            AttrTarget::Definition(id),
            PARAMETERS,
            &p.name,
            p.formula.map(Value::Str).filter(|_| !p.remove),
        )?;
        Ok(ParameterOutput { changed: 1 })
    }

    fn meta(&self) -> CommandMeta {
        meta("parameter-set", "Set Component Parameter")
    }
}

fn meta(name: &str, label: &str) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        label: label.into(),
        category: CommandCategory::Organize,
        view_only: false,
        steps: Vec::new(),
    }
}

// ----- instance-parameter-set --------------------------------------------------------------

/// `instance-parameter-set` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InstanceParameterSetParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Component instances in the edit context.
    pub targets: Query,
    /// Parameter name.
    pub name: String,
    /// The instance's own formula; absent with `remove` (back to the
    /// component's).
    #[serde(default)]
    pub formula: Option<String>,
    /// Remove the instance's own value.
    #[serde(default)]
    pub remove: bool,
}

/// The `instance-parameter-set` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct InstanceParameterSet;

impl Command for InstanceParameterSet {
    type Params = InstanceParameterSetParams;
    type Output = ParameterOutput;

    fn name(&self) -> &'static str {
        "instance-parameter-set"
    }

    fn validate(&self, _: &Document, p: &InstanceParameterSetParams) -> Result<(), CommandError> {
        check_value(&p.name, p.formula.as_ref(), p.remove)
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: InstanceParameterSetParams,
    ) -> Result<ParameterOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let ids: Vec<EntityId> = targets_of(tx, &target, "targets", &p.targets)?
            .into_iter()
            .filter(|id| matches!(id, EntityId::Instance(_)))
            .collect();
        if ids.is_empty() {
            return Err(CommandError::invalid_param(
                "targets",
                "select component instances",
            ));
        }
        for &id in &ids {
            tx.set_attribute(
                AttrTarget::Entity {
                    container: target.container,
                    id,
                },
                PARAMETERS,
                &p.name,
                p.formula.clone().map(Value::Str).filter(|_| !p.remove),
            )?;
        }
        Ok(ParameterOutput { changed: ids.len() })
    }

    fn meta(&self) -> CommandMeta {
        meta("instance-parameter-set", "Set Instance Parameter")
    }
}

// ----- part-rule-set -----------------------------------------------------------------------

/// `part-rule-set` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PartRuleSetParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Groups or instances inside the component open for editing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<Query>,
    /// Instead of `targets`: the component (definition name)…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    /// …and the name of its parts to set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// `x`, `y`, `z` (the part's lowest corner in the component), `size_x`,
    /// `size_y`, `size_z` (its extent along its own axes) or `visible` (0
    /// hides it).
    pub property: String,
    /// The formula, in the component's parameters; absent with `remove`.
    #[serde(default)]
    pub formula: Option<String>,
    /// Remove the rule.
    #[serde(default)]
    pub remove: bool,
}

/// The groups and instances of `container` called `name` (an unnamed
/// instance goes by its component's name).
fn parts_named(doc: &Document, container: ContainerRef, name: &str) -> Vec<EntityId> {
    let Some(entities) = doc.container(container) else {
        return Vec::new();
    };
    entities
        .order()
        .iter()
        .copied()
        .filter(|&id| match entities.view(id) {
            Some(EntityView::Group(g)) => g.name == name,
            Some(EntityView::Instance(i)) => match &i.name {
                Some(n) => n == name,
                None => doc.definition(i.definition).is_some_and(|d| d.name == name),
            },
            _ => false,
        })
        .collect()
}

/// The `part-rule-set` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct PartRuleSet;

impl Command for PartRuleSet {
    type Params = PartRuleSetParams;
    type Output = ParameterOutput;

    fn name(&self) -> &'static str {
        "part-rule-set"
    }

    fn validate(&self, doc: &Document, p: &PartRuleSetParams) -> Result<(), CommandError> {
        match (&p.targets, &p.component, &p.part) {
            (Some(_), None, None) => {}
            (None, Some(c), Some(_)) => {
                definition_named(doc, c)?;
            }
            _ => {
                return Err(CommandError::invalid_param(
                    "targets",
                    "give targets (inside the open component), or component and part",
                ));
            }
        }
        if !PROPERTIES.contains(&p.property.as_str()) {
            return Err(CommandError::invalid_param(
                "property",
                format!("one of {}", PROPERTIES.join(", ")),
            ));
        }
        match (&p.formula, p.remove) {
            (Some(f), false) => check_formula(f),
            (None, true) => Ok(()),
            _ => Err(CommandError::invalid_param(
                "formula",
                "give a formula, or remove",
            )),
        }
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: PartRuleSetParams,
    ) -> Result<ParameterOutput, CommandError> {
        let (container, ids) = if let (Some(component), Some(part)) = (&p.component, &p.part) {
            let container = ContainerRef::Definition(definition_named(tx.document(), component)?);
            (container, parts_named(tx.document(), container, part))
        } else {
            let target = target_container(tx, &p.common)?;
            if !matches!(target.container, ContainerRef::Definition(_)) {
                return Err(CommandError::NotApplicable(
                    "rules belong to parts of a component: open the component first".into(),
                ));
            }
            let query = p
                .targets
                .as_ref()
                .ok_or_else(|| CommandError::invalid_param("targets", "nothing to set"))?;
            let ids: Vec<EntityId> = targets_of(tx, &target, "targets", query)?
                .into_iter()
                .filter(|id| matches!(id, EntityId::Group(_) | EntityId::Instance(_)))
                .collect();
            (target.container, ids)
        };
        if ids.is_empty() {
            return Err(CommandError::invalid_param(
                "targets",
                "select groups or instances inside the component",
            ));
        }
        for &id in &ids {
            tx.set_attribute(
                AttrTarget::Entity { container, id },
                RULES,
                &p.property,
                p.formula.clone().map(Value::Str).filter(|_| !p.remove),
            )?;
        }
        Ok(ParameterOutput { changed: ids.len() })
    }

    fn meta(&self) -> CommandMeta {
        meta("part-rule-set", "Set Part Rule")
    }
}

// ----- parametric-update -------------------------------------------------------------------

/// `parametric-update` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ParametricUpdateParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Component instances in the edit context (every instance of a
    /// parametric component when absent).
    #[serde(default)]
    pub targets: Option<Query>,
}

/// `parametric-update` output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ParametricUpdateOutput {
    /// Instances updated.
    pub instances: usize,
    /// Parts moved, resized, shown or hidden.
    pub parts: usize,
    /// Instances that got a component of their own (their values differ).
    pub made_unique: usize,
}

/// The `parametric-update` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct ParametricUpdate;

/// The extent of a part's geometry in its own coordinates: its mesh and,
/// one level down, its groups and instances.
fn part_bounds(doc: &Document, definition: DefinitionId) -> Option<(DVec3, DVec3)> {
    let entities = doc.definition(definition).map(|d| &d.entities)?;
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for (_, v) in entities.mesh().vertices() {
        lo = lo.min(v.position);
        hi = hi.max(v.position);
    }
    for &id in entities.order() {
        let (Some(d), Some(t)) = (entities.definition_of(id), entities.transform_of(id)) else {
            continue;
        };
        if let Some(inner) = doc.definition(d) {
            for (_, v) in inner.entities.mesh().vertices() {
                let p = t.transform_point3(v.position);
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
    }
    lo.is_finite().then_some((lo, hi))
}

/// Applies the rules of the parts of `definition` with parameter values
/// `values`; returns the parts changed.
fn apply_rules(
    tx: &mut Transaction<'_>,
    definition: DefinitionId,
    values: &BTreeMap<String, String>,
) -> Result<usize, CommandError> {
    let container = ContainerRef::Definition(definition);
    let parts: Vec<(EntityId, BTreeMap<String, String>, DAffine3, DefinitionId)> = {
        let Some(entities) = tx.document().container(container) else {
            return Ok(0);
        };
        entities
            .order()
            .iter()
            .filter_map(|&id| {
                let rules = formulas(entities.attrs(id), RULES);
                let t = entities.transform_of(id)?;
                let d = entities.definition_of(id)?;
                (!rules.is_empty()).then_some((id, rules, t, d))
            })
            .collect()
    };
    let mut changed = 0;
    for (id, rules, current, part_definition) in parts {
        let mut names = Parameters::new(values);
        let mut value = |property: &str| -> Result<Option<f64>, CommandError> {
            rules
                .get(property)
                .map(|f| evaluate(f, &mut names).map_err(formula_error))
                .transpose()
        };
        let mut t = current;
        if let Some((lo, hi)) = part_bounds(tx.document(), part_definition) {
            let size = hi - lo;
            for (k, property) in ["size_x", "size_y", "size_z"].into_iter().enumerate() {
                if let Some(s) = value(property)? {
                    if s <= 0.0 {
                        return Err(CommandError::OutOfRange {
                            name: property.into(),
                            reason: format!("evaluates to {s}; sizes are positive"),
                        });
                    }
                    if size[k] > 1e-9 {
                        let axis = t.matrix3.col(k).normalize_or_zero();
                        *t.matrix3.col_mut(k) = axis * (s / size[k]);
                    }
                }
            }
            // The lowest corner of the part's box, after sizing.
            let corners = (0..8).map(|i| {
                let p = DVec3::new(
                    if i & 1 == 0 { lo.x } else { hi.x },
                    if i & 2 == 0 { lo.y } else { hi.y },
                    if i & 4 == 0 { lo.z } else { hi.z },
                );
                t.transform_point3(p)
            });
            let low = corners.fold(DVec3::splat(f64::INFINITY), DVec3::min);
            for (k, property) in ["x", "y", "z"].into_iter().enumerate() {
                if let Some(target) = value(property)? {
                    t.translation[k] += target - low[k];
                }
            }
        }
        if t != current {
            tx.set_transform(container, id, t)?;
            changed += 1;
        }
        if let Some(v) = value("visible")? {
            let hidden = v == 0.0;
            let was = tx
                .document()
                .container(container)
                .is_some_and(|e| e.is_hidden(id));
            if was != hidden {
                tx.set_hidden(container, id, hidden)?;
                changed += 1;
            }
        }
    }
    Ok(changed)
}

impl Command for ParametricUpdate {
    type Params = ParametricUpdateParams;
    type Output = ParametricUpdateOutput;

    fn name(&self) -> &'static str {
        "parametric-update"
    }

    fn validate(&self, _: &Document, _: &ParametricUpdateParams) -> Result<(), CommandError> {
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ParametricUpdateParams,
    ) -> Result<ParametricUpdateOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let instances: Vec<EntityId> = match &p.targets {
            Some(q) => targets_of(tx, &target, "targets", q)?,
            None => tx
                .document()
                .container(target.container)
                .map(|e| e.order().to_vec())
                .unwrap_or_default(),
        }
        .into_iter()
        .filter(|id| matches!(id, EntityId::Instance(_)))
        .collect();
        let mut out = ParametricUpdateOutput {
            instances: 0,
            parts: 0,
            made_unique: 0,
        };
        // Definitions already brought to their component's own values.
        let mut done: Vec<DefinitionId> = Vec::new();
        for id in instances {
            let (definition, own, pid) = {
                let Some(entities) = tx.document().container(target.container) else {
                    continue;
                };
                let Some(EntityView::Instance(i)) = entities.view(id) else {
                    continue;
                };
                (
                    i.definition,
                    formulas(entities.attrs(id), PARAMETERS),
                    entities.pid_of(id),
                )
            };
            let defaults = formulas(
                tx.document().definition(definition).map(|d| &d.attrs),
                PARAMETERS,
            );
            if defaults.is_empty() && own.is_empty() {
                continue;
            }
            let mut values = defaults.clone();
            values.extend(own.clone());
            Parameters::new(&values).all().map_err(formula_error)?;
            let mut definition = definition;
            // Own values need a component of their own when others share it.
            if !own.is_empty() {
                let shared = tx
                    .document()
                    .containers()
                    .into_iter()
                    .filter_map(|c| tx.document().container(c))
                    .flat_map(|e| e.instances().map(|(_, x)| x.definition).collect::<Vec<_>>())
                    .filter(|d| *d == definition)
                    .count()
                    > 1;
                if shared && let Some(pid) = pid {
                    MakeUnique.apply(
                        tx,
                        MakeUniqueParams {
                            common: CommonParams {
                                context: p.common.context.clone(),
                                dry_run: false,
                            },
                            targets: Query::new(format!("@{}", pid.0)),
                        },
                    )?;
                    out.made_unique += 1;
                    if let Some(EntityView::Instance(i)) = tx
                        .document()
                        .container(target.container)
                        .and_then(|e| e.view(id))
                    {
                        definition = i.definition;
                    }
                }
            } else if done.contains(&definition) {
                out.instances += 1;
                continue;
            }
            out.parts += apply_rules(tx, definition, &values)?;
            out.instances += 1;
            if own.is_empty() {
                done.push(definition);
            }
        }
        Ok(out)
    }

    fn meta(&self) -> CommandMeta {
        meta("parametric-update", "Update Parametric Components")
    }
}

/// Registers the parametric commands.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(ParameterSet);
    registry.register(InstanceParameterSet);
    registry.register(PartRuleSet);
    registry.register(ParametricUpdate);
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

    /// A cabinet: two side panels and a shelf, as a component.
    fn cabinet() -> Engine {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        for (name, a, b, h) in [
            ("Left", [0, 0, 0], [18, 400, 0], 700),
            ("Right", [582, 0, 0], [600, 400, 0], 700),
            ("Shelf", [18, 0, 300], [582, 400, 300], 18),
        ] {
            run(&mut e, "rectangle", json!({ "corner_a": a, "corner_b": b }));
            run(
                &mut e,
                "push-pull",
                json!({ "face": { "Query": "face & last" }, "distance": h }),
            );
            run(
                &mut e,
                "make-group",
                json!({ "targets": "edge | face", "name": name }),
            );
        }
        run(
            &mut e,
            "make-component",
            json!({ "targets": "group", "name": "Cabinet" }),
        );
        e
    }

    /// The edit context inside the first instance.
    fn inside(e: &mut Engine) -> Value {
        let model = e
            .document()
            .container(crate::document::ContainerRef::Model)
            .unwrap();
        let id = *model
            .order()
            .iter()
            .find(|id| matches!(id, crate::ids::EntityId::Instance(_)))
            .unwrap();
        let pid = model.pid_of(id).unwrap();
        json!(crate::document::EntityPath::in_model(
            crate::document::EntityKey::Pid(pid)
        ))
    }

    #[test]
    fn rules_size_and_place_parts_from_parameters() {
        let mut e = cabinet();
        for (name, formula) in [("width", "800"), ("board", "18mm")] {
            run(
                &mut e,
                "parameter-set",
                json!({ "component": "Cabinet", "name": name, "formula": formula }),
            );
        }
        // One rule inside the open component, one by names.
        let context = inside(&mut e);
        run(
            &mut e,
            "part-rule-set",
            json!({ "context": context, "targets": "group & name:Right",
                    "property": "x", "formula": "width - board" }),
        );
        run(
            &mut e,
            "part-rule-set",
            json!({ "component": "Cabinet", "part": "Shelf",
                    "property": "size_x", "formula": "width - 2 * board" }),
        );
        let out = run(&mut e, "parametric-update", json!({}));
        assert_eq!(out["instances"], 1);
        assert_eq!(out["parts"], 2);
        let check = run(&mut e, "solid-check", json!({}));
        let sizes: Vec<Value> = check["objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| json!([o["name"], o["size_mm"][0]]))
            .collect();
        assert!(sizes.contains(&json!(["Shelf", 764.0])), "{sizes:?}");

        // A copy with its own width gets a component of its own.
        run(
            &mut e,
            "move",
            json!({ "targets": "instance", "delta": [2000, 0, 0], "copy": true }),
        );
        run(
            &mut e,
            "instance-parameter-set",
            json!({ "targets": "instance & last", "name": "width", "formula": "1000" }),
        );
        let out = run(&mut e, "parametric-update", json!({}));
        assert_eq!(out["made_unique"], 1, "{out}");
        let check = run(&mut e, "solid-check", json!({}));
        let shelves: Vec<f64> = check["objects"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["name"] == "Shelf")
            .map(|o| o["size_mm"][0].as_f64().unwrap())
            .collect();
        assert_eq!(shelves.len(), 2);
        assert!(
            shelves.contains(&764.0) && shelves.contains(&964.0),
            "{shelves:?}"
        );
    }

    #[test]
    fn bad_names_formulas_and_places_are_refused() {
        let mut e = cabinet();
        for params in [
            json!({ "component": "Cabinet", "name": "max", "formula": "1" }),
            json!({ "component": "Cabinet", "name": "w", "formula": "1 +" }),
            json!({ "component": "Nope", "name": "w", "formula": "1" }),
            json!({ "component": "Cabinet", "name": "w" }),
        ] {
            assert!(e.execute_erased("parameter-set", params).is_err());
        }
        assert!(
            e.execute_erased(
                "part-rule-set",
                json!({ "targets": "instance", "property": "x", "formula": "1" })
            )
            .is_err(),
            "rules need the component open"
        );
        run(
            &mut e,
            "parameter-set",
            json!({ "component": "Cabinet", "name": "a", "formula": "b" }),
        );
        run(
            &mut e,
            "parameter-set",
            json!({ "component": "Cabinet", "name": "b", "formula": "a" }),
        );
        assert!(e.execute_erased("parametric-update", json!({})).is_err());
    }
}
