//! Implements docs/10 §2 script evaluation through the command transaction.

use super::{ScriptControl, ScriptLimits, ScriptOutput, plan, replay};
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommandRegistry, CommonParams, Engine,
    Transaction,
};
use crate::document::Document;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

fn args() -> Value {
    serde_json::json!({})
}
/// Typed script invocation, also used by batch/autosave/RPC.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScriptParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Rhai source. Bare lengths are millimeters and angles radians.
    pub source: String,
    /// Arguments exposed as the constant `args` map.
    #[serde(default = "args")]
    pub args: Value,
}
/// Evaluate a sandboxed script as one atomic command.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScriptEval;
impl Command for ScriptEval {
    type Params = ScriptParams;
    type Output = ScriptOutput;
    fn name(&self) -> &'static str {
        "script-eval"
    }
    fn validate(&self, _: &Document, p: &ScriptParams) -> Result<(), CommandError> {
        if !p.args.is_object() || p.source.len() > 4 * 1024 * 1024 {
            return Err(CommandError::invalid_param(
                "source",
                "source must be at most 4 MiB and args must be an object",
            ));
        }
        Ok(())
    }
    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: ScriptParams,
    ) -> Result<ScriptOutput, CommandError> {
        let mut registry = CommandRegistry::with_builtins();
        for plugin in super::plugins::bundled()? {
            plugin.register(&mut registry)?;
        }
        let registry = Arc::new(registry);
        let mut engine = Engine::new(tx.document().fork_for_preview(), registry.clone());
        engine.set_selection(tx.query_env().selection.clone());
        engine.set_last_created(tx.query_env().last.to_vec());
        engine.set_context(crate::commands::params::command_context(tx, &p.common))?;
        let plan = plan(
            engine,
            &p.source,
            &p.args,
            None,
            ScriptLimits::default(),
            ScriptControl::default(),
        )?;
        replay(tx, &registry, plan.actions)?;
        Ok(plan.output)
    }
    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "script-eval".into(),
            label: "Script".into(),
            category: CommandCategory::Edit,
            view_only: false,
            steps: Vec::new(),
        }
    }
}
