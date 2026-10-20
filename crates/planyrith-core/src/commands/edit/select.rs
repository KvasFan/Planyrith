//! Implements docs/05-modeling-tools.md §4.1 `select` (a view command: it
//! changes the engine selection, not the document).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::params::select;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, ToolStep, Transaction,
};
use crate::document::{Document, Query, SelectMode};
use crate::inference::InferenceKind;

/// `select` parameters (docs/05 §4.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SelectParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// Selection query (docs/08 §5).
    pub query: Query,
    /// How the matches combine with the current selection.
    #[serde(default)]
    pub mode: SelectMode,
}

/// `select` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SelectOutput {
    /// Size of the selection afterwards.
    pub count: usize,
    /// Items per entity kind (kebab-case kind names).
    pub kinds: BTreeMap<String, usize>,
}

/// The `select` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct Select;

impl Command for Select {
    type Params = SelectParams;
    type Output = SelectOutput;

    fn name(&self) -> &'static str {
        "select"
    }

    fn validate(&self, _doc: &Document, p: &SelectParams) -> Result<(), CommandError> {
        p.query
            .parse()
            .map(|_| ())
            .map_err(|e| CommandError::invalid_param("query", e.to_string()))
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SelectParams,
    ) -> Result<SelectOutput, CommandError> {
        let matches = select(tx, &p.query)?;
        let mut selection = tx.query_env().selection.clone();
        selection.apply(p.mode, &matches);
        let mut kinds = BTreeMap::new();
        for path in selection.iter() {
            if let Ok(r) = tx.document().resolve(path) {
                *kinds.entry(r.entity.kind().to_string()).or_insert(0) += 1;
            }
        }
        let count = selection.len();
        tx.set_selection(selection);
        Ok(SelectOutput { count, kinds })
    }

    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: "select".into(),
            category: CommandCategory::Edit,
            label: "Select".into(),
            view_only: true,
            steps: vec![ToolStep {
                prompt: "Click to select (Shift toggles, Ctrl adds)".into(),
                value_field: String::new(),
                inference: vec![InferenceKind::OnFace, InferenceKind::OnEdge],
            }],
        }
    }
}
