//! Implements docs/10 §2.2 attribute helpers through the command registry.

use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{Document, Query, Value as Attribute};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) fn attribute(value: Value) -> Attribute {
    match value {
        Value::Null => Attribute::Str(String::new()),
        Value::Bool(v) => Attribute::Bool(v),
        Value::Number(v) => {
            if let Some(i) = v.as_i64() {
                Attribute::Int(i)
            } else {
                Attribute::Float(v.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(v) => Attribute::Str(v),
        Value::Array(v) => Attribute::Array(v.into_iter().map(attribute).collect()),
        Value::Object(v) => Attribute::Map(v.into_iter().map(|(k, v)| (k, attribute(v))).collect()),
    }
}
pub(super) fn json(value: &Attribute) -> Value {
    match value {
        Attribute::Bool(v) => serde_json::json!(v),
        Attribute::Int(v) => serde_json::json!(v),
        Attribute::Float(v) => serde_json::json!(v),
        Attribute::Str(v) => serde_json::json!(v),
        Attribute::Length(v) => serde_json::json!(v.as_mm()),
        Attribute::Bytes(v) => serde_json::json!(v),
        Attribute::Vec3(v) => serde_json::json!(v.to_array()),
        Attribute::Color(v) => serde_json::json!(v),
        Attribute::EntityRef(v) => serde_json::json!(v.0),
        Attribute::Array(v) => Value::Array(v.iter().map(json).collect()),
        Attribute::Map(v) => Value::Object(v.iter().map(|(k, v)| (k.clone(), json(v))).collect()),
    }
}
/// Set one namespaced entity attribute; JSON null removes it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AttributeParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Entities in the current edit context.
    pub targets: Query,
    /// Dictionary namespace.
    pub dictionary: String,
    /// Attribute name.
    pub key: String,
    /// JSON attribute value, null to remove.
    pub value: Value,
}
/// Registry command used by scripts and available to every frontend.
#[derive(Debug, Default, Clone, Copy)]
pub struct AttributeSet;
impl Command for AttributeSet {
    type Params = AttributeParams;
    type Output = usize;
    fn name(&self) -> &'static str {
        "attribute-set"
    }
    fn validate(&self, _: &Document, p: &AttributeParams) -> Result<(), CommandError> {
        if p.dictionary.is_empty() || p.key.is_empty() {
            return Err(CommandError::invalid_param(
                "dictionary",
                "dictionary and key must be nonempty",
            ));
        }
        Ok(())
    }
    fn apply(&self, tx: &mut Transaction<'_>, p: AttributeParams) -> Result<usize, CommandError> {
        let target = crate::commands::params::target_container(tx, &p.common)?;
        let ids = crate::commands::common::targets_of(tx, &target, "targets", &p.targets)?;
        let value = if p.value.is_null() {
            None
        } else {
            Some(attribute(p.value))
        };
        for &id in &ids {
            tx.set_attribute(
                crate::commands::transaction::AttrTarget::Entity {
                    container: target.container,
                    id,
                },
                &p.dictionary,
                &p.key,
                value.clone(),
            )?;
        }
        Ok(ids.len())
    }
    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "attribute-set".into(),
            label: "Set attribute".into(),
            category: CommandCategory::Edit,
            view_only: false,
            steps: Vec::new(),
        }
    }
}
