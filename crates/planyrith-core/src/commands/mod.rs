//! Implements docs/01-architecture.md §3 (the command system), §7 (error
//! model) and §8 (events): the `Command` trait, its object-safe erased form,
//! the registry, the `Engine`, and `CommandError` with the stable codes of
//! docs/08 §9. Transactions, deltas and the undo stack are in
//! [`transaction`]; one sub-module per tool category holds the commands of
//! docs/05.
//!
//! Deviation from docs/01 §3.1 (recorded in its Decisions): the trait exposes
//! `fn name(&self) -> &'static str` instead of an associated `const NAME` so
//! that the erased form stays object-safe without a second trait per command;
//! `Engine::execute::<C>` therefore requires `C: Default` (commands are
//! stateless).
//!
//! Owner key: `later` (first consumer: milestone 0.1 commands).

pub mod annotate;
pub(crate) mod common;
pub mod draw;
pub mod edit;
pub mod file;
pub mod organize;
pub mod params;
pub mod transaction;
#[cfg(test)]
mod undo_tests;
pub mod view;

pub use transaction::{
    AttrTarget, CommittedTransaction, Delta, MeshAliases, Transaction, TxView, UndoEntry, UndoStack,
};

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::diagnostics::Warning;
use crate::document::{
    Camera, Document, EditContext, EntityPath, EntityRef, InstancePath, PathError, Query, Selection,
};
use crate::geom::GeometryError;
use crate::ids::{Pid, SceneId};
use crate::inference::InferenceKind;
use crate::io::IoError;

/// Version of the registry/schema contract negotiated by RPC clients (docs/01 §9).
pub const SCHEMA_VERSION: u32 = 1;

/// Command failures (docs/01 §7) with the stable codes of docs/08 §9.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CommandError {
    /// `E1001` — a parameter value is invalid.
    #[error("invalid parameter `{name}`: {reason}")]
    InvalidParam {
        /// Parameter name (kebab-case as in the CLI).
        name: String,
        /// Explanation.
        reason: String,
    },
    /// `E1002` — a parameter is out of range or has a unit mismatch.
    #[error("parameter `{name}` out of range: {reason}")]
    OutOfRange {
        /// Parameter name.
        name: String,
        /// Explanation.
        reason: String,
    },
    /// `E1101` — an entity reference did not resolve.
    #[error("entity not found: {0}")]
    NotFound(EntityRef),
    /// `E1102` — a query matched several entities where one was required.
    #[error("ambiguous selection: `{query}` matched {matched} entities")]
    Ambiguous {
        /// The query.
        query: Query,
        /// How many it matched.
        matched: usize,
    },
    /// `E1201` — the operation does not apply to this input.
    #[error("operation not applicable: {0}")]
    NotApplicable(String),
    /// Geometry failure (`E1301`/`E1302`/…, see [`GeometryError::code`]).
    #[error("geometry error: {0}")]
    Geometry(#[from] GeometryError),
    /// `E1701` — cancelled through a `CancelToken`.
    #[error("cancelled")]
    Cancelled,
    /// I/O failure (`E14xx`/`E1501`, see [`IoError::code`]).
    #[error("io: {0}")]
    Io(#[from] IoError),
    /// `E1601` — script error with position.
    #[error("script error at {line}:{col}: {message}")]
    Script {
        /// 1-based line.
        line: u32,
        /// 1-based column.
        col: u32,
        /// Message.
        message: String,
    },
    /// `E1901` — an internal inconsistency (a bug; never expected on valid input).
    #[error("internal error: {0}")]
    Internal(String),
}

impl CommandError {
    /// Stable machine-readable code (docs/08 §9).
    pub fn code(&self) -> &'static str {
        match self {
            CommandError::InvalidParam { .. } => "E1001",
            CommandError::OutOfRange { .. } => "E1002",
            CommandError::NotFound(_) => "E1101",
            CommandError::Ambiguous { .. } => "E1102",
            CommandError::NotApplicable(_) => "E1201",
            CommandError::Geometry(g) => g.code(),
            CommandError::Cancelled => "E1701",
            CommandError::Io(io) => io.code(),
            CommandError::Script { .. } => "E1601",
            CommandError::Internal(_) => "E1901",
        }
    }

    /// The parameter name for `E1001`/`E1002` (docs/08 §7 `error.param`).
    pub fn param(&self) -> Option<&str> {
        match self {
            CommandError::InvalidParam { name, .. } | CommandError::OutOfRange { name, .. } => {
                Some(name)
            }
            _ => None,
        }
    }

    /// Builds an `InvalidParam`.
    pub fn invalid_param(name: impl Into<String>, reason: impl Into<String>) -> Self {
        CommandError::InvalidParam {
            name: name.into(),
            reason: reason.into(),
        }
    }
}

/// Tool categories (docs/05 §1 "Category").
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum CommandCategory {
    /// Drawing tools.
    Draw,
    /// Editing tools.
    Edit,
    /// Groups, components, tags, materials.
    Organize,
    /// Dimensions, text, section planes.
    Annotate,
    /// Guides (tape measure, protractor, axes).
    Guide,
    /// Solid tools.
    Solid,
    /// View-only commands (not undoable).
    View,
    /// File commands (`save`, `purge-unused`, …).
    File,
}

/// One interactive step of a tool, described declaratively so the CLI can print
/// it (`planyrith tool <name> --steps`, docs/01 §3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolStep {
    /// Status-bar prompt.
    pub prompt: String,
    /// What the value field accepts at this step (docs/05 §1), empty if nothing.
    pub value_field: String,
    /// Inference kinds listened to.
    pub inference: Vec<InferenceKind>,
}

/// Frontend-facing metadata of a command (docs/01 §3.1 `CommandMeta`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommandMeta {
    /// Stable kebab-case name.
    pub name: String,
    /// Category.
    pub category: CommandCategory,
    /// Undo-history label.
    pub label: String,
    /// `true` for commands that do not mutate the document (not recorded in
    /// history, e.g. `select`, `activate-scene` camera part).
    pub view_only: bool,
    /// Interactive steps.
    pub steps: Vec<ToolStep>,
}

/// Fields every tool's `Params` carries (docs/05 §1 "Common Params fields");
/// flatten it with `#[serde(flatten)]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct CommonParams {
    /// Edit context for this command; `None` = the engine's current context.
    #[serde(default)]
    pub context: Option<EntityPath>,
    /// Validate and report only.
    #[serde(default)]
    pub dry_run: bool,
}

/// A unit of document mutation (docs/01 §3.1). Implementations are stateless;
/// all inputs are in `Params`. Commands never do I/O, read clocks or use
/// randomness; file commands receive a `&dyn Storage` through `Transaction`.
pub trait Command: Send + Sync + 'static {
    /// Serde-typed parameters; every field documented; units explicit.
    type Params: Serialize + DeserializeOwned + JsonSchema + Clone + Send + Sync + 'static;
    /// Serde-typed result (new pids, measured values, warnings).
    type Output: Serialize + JsonSchema;

    /// Stable kebab-case name, e.g. `"push-pull"`. Used by CLI, RPC, scripts, docs.
    fn name(&self) -> &'static str;

    /// Validates `params` against the document without mutating (live preview,
    /// CLI `--dry-run`).
    fn validate(&self, doc: &Document, params: &Self::Params) -> Result<(), CommandError>;

    /// Applies inside a transaction; mutates only through `tx`.
    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        params: Self::Params,
    ) -> Result<Self::Output, CommandError>;

    /// Frontend metadata.
    fn meta(&self) -> CommandMeta;
}

/// Object-safe view of a [`Command`] working on JSON values; what the registry
/// stores and what RPC/CLI dispatch through (docs/01 §3.2).
pub trait ErasedCommand: Send + Sync {
    /// Stable name.
    fn name(&self) -> &'static str;
    /// Metadata.
    fn meta(&self) -> CommandMeta;
    /// JSON schema of `Params`.
    fn params_schema(&self) -> schemars::Schema;
    /// JSON schema of `Output`.
    fn output_schema(&self) -> schemars::Schema;
    /// Deserializes and validates.
    fn validate_json(&self, doc: &Document, params: &serde_json::Value)
    -> Result<(), CommandError>;
    /// Deserializes, applies, serializes the output.
    fn apply_json(
        &self,
        tx: &mut Transaction<'_>,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CommandError>;
}

impl<C: Command> ErasedCommand for C {
    fn name(&self) -> &'static str {
        Command::name(self)
    }

    fn meta(&self) -> CommandMeta {
        Command::meta(self)
    }

    fn params_schema(&self) -> schemars::Schema {
        schemars::schema_for!(C::Params)
    }

    fn output_schema(&self) -> schemars::Schema {
        schemars::schema_for!(C::Output)
    }

    fn validate_json(
        &self,
        doc: &Document,
        params: &serde_json::Value,
    ) -> Result<(), CommandError> {
        let params: C::Params = serde_json::from_value(params.clone())
            .map_err(|e| CommandError::invalid_param("params", e.to_string()))?;
        Command::validate(self, doc, &params)
    }

    fn apply_json(
        &self,
        tx: &mut Transaction<'_>,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CommandError> {
        let params: C::Params = serde_json::from_value(params)
            .map_err(|e| CommandError::invalid_param("params", e.to_string()))?;
        let output = Command::apply(self, tx, params)?;
        serde_json::to_value(output).map_err(|e| CommandError::Internal(e.to_string()))
    }
}

/// Summary row for `command.list` (docs/08 §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommandInfo {
    /// Name.
    pub name: String,
    /// Category.
    pub category: CommandCategory,
    /// Label.
    pub label: String,
    /// Registry schema version.
    pub schema_version: u32,
}

/// Name → command, sorted by name for deterministic listing (docs/01 §3.2).
#[derive(Default)]
pub struct CommandRegistry {
    commands: BTreeMap<&'static str, Box<dyn ErasedCommand>>,
}

impl fmt::Debug for CommandRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandRegistry")
            .field("names", &self.names().collect::<Vec<_>>())
            .finish()
    }
}

impl CommandRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a command; returns the previous one registered under the same
    /// name, if any.
    pub fn register<C: Command>(&mut self, command: C) -> Option<Box<dyn ErasedCommand>> {
        let name = Command::name(&command);
        self.commands.insert(name, Box::new(command))
    }

    /// Looks a command up by name.
    pub fn get(&self, name: &str) -> Option<&dyn ErasedCommand> {
        self.commands.get(name).map(|b| b.as_ref())
    }

    /// `true` when `name` is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.commands.contains_key(name)
    }

    /// Registered names in sorted order.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.commands.keys().copied()
    }

    /// Number of commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// `true` when empty.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Rows for `command.list`.
    pub fn list(&self) -> Vec<CommandInfo> {
        self.commands
            .values()
            .map(|c| {
                let meta = c.meta();
                CommandInfo {
                    name: meta.name,
                    category: meta.category,
                    label: meta.label,
                    schema_version: SCHEMA_VERSION,
                }
            })
            .collect()
    }

    /// Registers every built-in command of the current milestone
    /// (docs/13 0.1: `line`, `rectangle`, `circle`, …).
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        draw::register(&mut registry);
        edit::register(&mut registry);
        organize::register(&mut registry);
        annotate::register(&mut registry);
        view::register(&mut registry);
        file::register(&mut registry);
        registry
    }
}

/// `E1101` for an edit-context path that does not resolve to a container.
fn path_error(path: &InstancePath, err: PathError) -> CommandError {
    match err {
        PathError::NotFound(key) | PathError::NotAContainer(key) => CommandError::NotFound(
            EntityRef::Path(EntityPath::new(path.parent().unwrap_or_default(), key)),
        ),
        PathError::TooDeep => CommandError::NotApplicable(err.to_string()),
    }
}

/// Token returned by [`Engine::begin_group`] and consumed by [`Engine::end_group`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupToken(u64);

/// Events emitted by the engine (docs/01 §8). Payloads carry deltas, never
/// full snapshots.
#[derive(Debug, Clone)]
pub enum Event {
    /// A transaction was committed, undone or redone.
    DocumentChanged {
        /// Undo label.
        label: String,
        /// The deltas applied (inverse deltas when undoing).
        deltas: Arc<[Delta]>,
        /// Document generation after the change.
        generation: u64,
    },
    /// The selection changed.
    SelectionChanged(Selection),
    /// The edit context changed (entered/exited a group).
    ActiveContextChanged(EditContext),
    /// A scene was activated.
    SceneActivated(SceneId),
    /// The camera moved (GUI-local; not part of the document history).
    CameraChanged(Camera),
    /// The active interactive tool changed (GUI).
    ToolChanged(String),
    /// Progress of a long operation.
    Progress {
        /// `0..=1`.
        fraction: f32,
        /// What is happening.
        message: String,
    },
    /// A non-blocking warning.
    Warning(Warning),
    /// Undo/redo availability changed.
    HistoryChanged {
        /// Can undo.
        can_undo: bool,
        /// Can redo.
        can_redo: bool,
    },
}

/// Listener callback type.
pub type EventListener = Box<dyn FnMut(&Event)>;

/// Handle to a subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubscriptionId(u64);

/// Synchronous in-process event bus (docs/01 §8). Frontends and the RPC server
/// fan events out to their own subscribers.
#[derive(Default)]
pub struct EventBus {
    listeners: Vec<(SubscriptionId, EventListener)>,
    next_id: u64,
}

impl fmt::Debug for EventBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventBus")
            .field("listeners", &self.listeners.len())
            .finish()
    }
}

impl EventBus {
    /// Empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a listener.
    pub fn subscribe(&mut self, listener: EventListener) -> SubscriptionId {
        let id = SubscriptionId(self.next_id);
        self.next_id += 1;
        self.listeners.push((id, listener));
        id
    }

    /// Removes a listener; `true` if it existed.
    pub fn unsubscribe(&mut self, id: SubscriptionId) -> bool {
        let before = self.listeners.len();
        self.listeners.retain(|(l, _)| *l != id);
        self.listeners.len() != before
    }

    /// Delivers an event to every listener in subscription order.
    pub fn emit(&mut self, event: &Event) {
        for (_, listener) in &mut self.listeners {
            listener(event);
        }
    }

    /// Number of listeners.
    pub fn len(&self) -> usize {
        self.listeners.len()
    }

    /// `true` when nobody listens.
    pub fn is_empty(&self) -> bool {
        self.listeners.is_empty()
    }
}

/// The single-writer owner of a document, its history and its registry
/// (docs/01 §3.2, §6). Selection and edit context are engine state, not file
/// content (docs/04 §11).
pub struct Engine {
    doc: Document,
    history: UndoStack,
    registry: Arc<CommandRegistry>,
    events: EventBus,
    selection: Selection,
    context: EditContext,
    last_created: Vec<Pid>,
    generation: u64,
    next_group_token: u64,
    /// Open undo groups, innermost last.
    open_groups: Vec<GroupToken>,
    /// Edge/face id aliases kept for the whole session (undo/redo replay).
    aliases: MeshAliases,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("generation", &self.generation)
            .field("history", &self.history)
            .field("selection", &self.selection)
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Wraps a document.
    pub fn new(doc: Document, registry: Arc<CommandRegistry>) -> Self {
        Engine {
            doc,
            history: UndoStack::default(),
            registry,
            events: EventBus::new(),
            selection: Selection::new(),
            context: EditContext::model(),
            last_created: Vec::new(),
            generation: 0,
            next_group_token: 1,
            open_groups: Vec::new(),
            aliases: MeshAliases::new(),
        }
    }

    /// The document (read-only; mutate through commands).
    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// The registry.
    pub fn registry(&self) -> &CommandRegistry {
        &self.registry
    }

    /// The undo history.
    pub fn history(&self) -> &UndoStack {
        &self.history
    }

    /// The event bus.
    pub fn events(&self) -> &EventBus {
        &self.events
    }

    /// Mutable event bus (subscribe/unsubscribe).
    pub fn events_mut(&mut self) -> &mut EventBus {
        &mut self.events
    }

    /// Current selection.
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Replaces the selection and emits `SelectionChanged` (not undoable).
    pub fn set_selection(&mut self, selection: Selection) {
        self.selection = selection;
        let snapshot = self.selection.clone();
        self.events.emit(&Event::SelectionChanged(snapshot));
    }

    /// Current edit context.
    pub fn context(&self) -> &EditContext {
        &self.context
    }

    /// Opens a container for editing after validating the path (`E1101`);
    /// emits `ActiveContextChanged`.
    pub fn set_context(&mut self, context: EditContext) -> Result<(), CommandError> {
        self.doc
            .resolve_container(&context.path)
            .map_err(|e| path_error(&context.path, e))?;
        if let Some(step) = self.doc.locked_step(&context.path) {
            return Err(CommandError::NotApplicable(format!(
                "{step} is locked; unlock it before editing inside it"
            )));
        }
        self.context = context;
        let snapshot = self.context.clone();
        self.events.emit(&Event::ActiveContextChanged(snapshot));
        Ok(())
    }

    /// Pids created by the last executed command (`last` in queries, docs/08 §2).
    pub fn last_created(&self) -> &[Pid] {
        &self.last_created
    }

    /// Restores `last` for a resumed session (the CLI reads it from the
    /// journal of the file it opens).
    pub fn set_last_created(&mut self, pids: Vec<Pid>) {
        self.last_created = pids;
    }

    /// Monotonic counter bumped on every committed, undone or redone transaction
    /// (docs/08 §7 `document.generation`).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Executes a stateless command by type (docs/01 §3.2).
    pub fn execute<C: Command + Default>(
        &mut self,
        params: C::Params,
    ) -> Result<C::Output, CommandError> {
        let command = C::default();
        self.execute_with(&command, params)
    }

    /// Executes a command instance: validate → transaction → apply → commit →
    /// history → events. On error nothing is committed.
    pub fn execute_with<C: Command>(
        &mut self,
        command: &C,
        params: C::Params,
    ) -> Result<C::Output, CommandError> {
        command.validate(&self.doc, &params)?;
        let meta = command.meta();
        self.run(&meta, |tx| command.apply(tx, params))
    }

    /// Executes a registered command by name with JSON params (CLI, RPC, scripts).
    /// With `"dry_run": true` the params are only validated and
    /// `{"dry_run": true, "valid": true}` is returned.
    pub fn execute_erased(
        &mut self,
        name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CommandError> {
        let registry = Arc::clone(&self.registry);
        let command = registry.get(name).ok_or_else(|| {
            CommandError::invalid_param("command", format!("unknown command `{name}`"))
        })?;
        command.validate_json(&self.doc, &params)?;
        if params.get("dry_run").and_then(serde_json::Value::as_bool) == Some(true) {
            return Ok(serde_json::json!({ "dry_run": true, "valid": true }));
        }
        let meta = command.meta();
        self.run(&meta, |tx| command.apply_json(tx, params))
    }

    /// Transaction → apply → commit (or rollback) → history → events.
    fn run<R>(
        &mut self,
        meta: &CommandMeta,
        apply: impl FnOnce(&mut Transaction<'_>) -> Result<R, CommandError>,
    ) -> Result<R, CommandError> {
        let mut tx = Transaction::new(&mut self.doc, meta.label.clone()).with_view(
            self.selection.clone(),
            self.last_created.clone(),
            self.context.clone(),
        );
        let output = match apply(&mut tx) {
            Ok(output) => output,
            Err(err) => {
                tx.rollback(&mut self.aliases).map_err(|e| {
                    CommandError::Internal(format!("rollback after `{err}` failed: {e}"))
                })?;
                return Err(err);
            }
        };
        let (selection, context) = tx.take_view_changes();
        let committed = tx.commit();
        for warning in &committed.warnings {
            self.events.emit(&Event::Warning(warning.clone()));
        }
        if !meta.view_only {
            self.last_created = committed.created.clone();
        }
        if !committed.deltas.is_empty() {
            self.generation += 1;
            self.events.emit(&Event::DocumentChanged {
                label: committed.label.clone(),
                deltas: Arc::from(committed.deltas.clone()),
                generation: self.generation,
            });
            if !meta.view_only {
                self.history.push(committed);
                self.emit_history();
            }
        }
        if let Some(selection) = selection {
            self.set_selection(selection);
        }
        if let Some(context) = context {
            self.set_context(context)?;
        }
        Ok(output)
    }

    fn emit_history(&mut self) {
        let event = Event::HistoryChanged {
            can_undo: self.history.can_undo(),
            can_redo: self.history.can_redo(),
        };
        self.events.emit(&event);
    }

    /// Applies an undo entry's deltas (inverted and reversed for undo). All
    /// or nothing: when a delta fails, the ones already applied are reverted
    /// so the document is back where it was.
    fn replay_entry(&mut self, entry: &UndoEntry, undo: bool) -> Result<Vec<Delta>, CommandError> {
        let what = if undo { "undo" } else { "redo" };
        let deltas: Vec<Delta> = if undo {
            entry
                .transactions
                .iter()
                .rev()
                .flat_map(|tx| tx.deltas.iter().rev().map(Delta::inverse))
                .collect()
        } else {
            entry
                .transactions
                .iter()
                .flat_map(|tx| tx.deltas.iter().cloned())
                .collect()
        };
        let mut applied: Vec<Delta> = Vec::with_capacity(deltas.len());
        for delta in deltas {
            match transaction::apply_delta(&mut self.doc, &delta, &mut self.aliases) {
                Ok(live) => applied.push(live),
                Err(e) => {
                    for done in applied.iter().rev() {
                        transaction::apply_delta(&mut self.doc, &done.inverse(), &mut self.aliases)
                            .map_err(|e2| {
                                CommandError::Internal(format!(
                                    "{what} of `{}` failed ({e}) and could not be reverted: {e2}",
                                    entry.label
                                ))
                            })?;
                    }
                    return Err(CommandError::Internal(format!(
                        "{what} of `{}` failed: {e}",
                        entry.label
                    )));
                }
            }
        }
        Ok(applied)
    }

    /// Validates without applying (`--dry-run`).
    pub fn validate_erased(
        &self,
        name: &str,
        params: &serde_json::Value,
    ) -> Result<(), CommandError> {
        let command = self.registry.get(name).ok_or_else(|| {
            CommandError::invalid_param("command", format!("unknown command `{name}`"))
        })?;
        command.validate_json(&self.doc, params)
    }

    /// Undoes the last undo entry (docs/01 §3.1: inverse deltas, O(change)).
    pub fn undo(&mut self) -> Result<(), CommandError> {
        let entry = self
            .history
            .undo_entry()
            .cloned()
            .ok_or_else(|| CommandError::NotApplicable("nothing to undo".into()))?;
        let applied = self.replay_entry(&entry, true)?;
        self.history.step_back();
        self.after_history_move(entry.label, applied);
        Ok(())
    }

    /// Redoes the last undone entry.
    pub fn redo(&mut self) -> Result<(), CommandError> {
        let entry = self
            .history
            .redo_entry()
            .cloned()
            .ok_or_else(|| CommandError::NotApplicable("nothing to redo".into()))?;
        let applied = self.replay_entry(&entry, false)?;
        self.history.step_forward();
        self.after_history_move(entry.label.clone(), applied);
        // `last` names what the redone step created (docs/08 §2).
        self.last_created = entry
            .transactions
            .iter()
            .flat_map(|t| t.created.iter().copied())
            .collect();
        Ok(())
    }

    fn after_history_move(&mut self, label: String, applied: Vec<Delta>) {
        self.generation += 1;
        self.last_created.clear();
        self.events.emit(&Event::DocumentChanged {
            label,
            deltas: Arc::from(applied),
            generation: self.generation,
        });
        self.emit_history();
    }

    /// Starts grouping subsequent commands into one undo step (batches,
    /// `group()` in scripts). Groups nest; only the outermost one creates the
    /// undo entry.
    pub fn begin_group(&mut self, label: &str) -> GroupToken {
        let token = GroupToken(self.next_group_token);
        self.next_group_token += 1;
        if self.open_groups.is_empty() {
            self.history.begin_group(label);
        }
        self.open_groups.push(token);
        token
    }

    /// Ends the group started with `token` (must be the innermost open one).
    pub fn end_group(&mut self, token: GroupToken) -> Result<(), CommandError> {
        if self.open_groups.last() != Some(&token) {
            return Err(CommandError::invalid_param(
                "group",
                "the token does not match the innermost open group",
            ));
        }
        self.open_groups.pop();
        if self.open_groups.is_empty() {
            self.history.end_group();
            self.emit_history();
        }
        Ok(())
    }

    /// Replaces the document (open/revert), clearing history and selection.
    pub fn replace_document(&mut self, doc: Document) {
        self.doc = doc;
        self.history = UndoStack::default();
        self.open_groups.clear();
        self.aliases.clear();
        self.selection = Selection::new();
        self.context = EditContext::model();
        self.last_created.clear();
        self.generation += 1;
    }

    /// Consumes the engine, returning the document (save paths).
    pub fn into_document(self) -> Document {
        self.doc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Noop;

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct NoopParams {
        #[serde(flatten)]
        common: CommonParams,
        value: u32,
    }

    #[derive(Debug, Serialize, JsonSchema)]
    struct NoopOutput {
        echoed: u32,
    }

    impl Command for Noop {
        type Params = NoopParams;
        type Output = NoopOutput;

        fn name(&self) -> &'static str {
            "noop"
        }

        fn validate(&self, _doc: &Document, params: &NoopParams) -> Result<(), CommandError> {
            if params.value > 10 {
                return Err(CommandError::OutOfRange {
                    name: "value".into(),
                    reason: "> 10".into(),
                });
            }
            Ok(())
        }

        fn apply(
            &self,
            _tx: &mut Transaction<'_>,
            params: NoopParams,
        ) -> Result<NoopOutput, CommandError> {
            Ok(NoopOutput {
                echoed: params.value,
            })
        }

        fn meta(&self) -> CommandMeta {
            CommandMeta {
                name: "noop".into(),
                category: CommandCategory::View,
                label: "No-op".into(),
                view_only: true,
                steps: Vec::new(),
            }
        }
    }

    #[test]
    fn registry_lists_sorted_and_exposes_schemas() {
        let mut reg = CommandRegistry::new();
        assert!(reg.register(Noop).is_none());
        assert!(reg.register(Noop).is_some());
        assert_eq!(reg.names().collect::<Vec<_>>(), vec!["noop"]);
        let cmd = reg.get("noop").unwrap();
        let schema = serde_json::to_value(cmd.params_schema()).unwrap();
        assert!(schema["properties"]["value"].is_object());
        assert!(schema["properties"]["dry_run"].is_object());
        assert_eq!(reg.list()[0].schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn erased_validation_maps_errors_to_codes() {
        let mut reg = CommandRegistry::new();
        reg.register(Noop);
        let engine = Engine::new(Document::new(), Arc::new(reg));
        let err = engine
            .validate_erased("noop", &serde_json::json!({ "value": 11 }))
            .unwrap_err();
        assert_eq!(err.code(), "E1002");
        assert_eq!(err.param(), Some("value"));
        let err = engine
            .validate_erased("noop", &serde_json::json!({ "value": "x" }))
            .unwrap_err();
        assert_eq!(err.code(), "E1001");
        let err = engine
            .validate_erased("missing", &serde_json::json!({}))
            .unwrap_err();
        assert_eq!(err.code(), "E1001");
        assert!(
            engine
                .validate_erased("noop", &serde_json::json!({ "value": 3 }))
                .is_ok()
        );
    }

    #[test]
    fn event_bus_delivers_in_order_and_unsubscribes() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut bus = EventBus::new();
        let s1 = seen.clone();
        let id = bus.subscribe(Box::new(move |e| {
            if let Event::ToolChanged(t) = e {
                s1.borrow_mut().push(t.clone());
            }
        }));
        bus.emit(&Event::ToolChanged("line".into()));
        assert!(bus.unsubscribe(id));
        assert!(!bus.unsubscribe(id));
        bus.emit(&Event::ToolChanged("move".into()));
        assert_eq!(*seen.borrow(), vec!["line".to_string()]);
    }

    // ----- engine round trips with real kernel operations -----------------------

    use crate::document::{ContainerRef, Tag};
    use crate::geom::{HalfEdgeMesh, NoDeltas};
    use crate::ids::EntityId;
    use glam::DVec3;

    /// Draws a closed polyline in the model (test-only command).
    #[derive(Default)]
    struct Polyline;

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct PolylineParams {
        #[schemars(with = "Vec<[f64; 3]>")]
        points: Vec<DVec3>,
        closed: bool,
    }

    impl Command for Polyline {
        type Params = PolylineParams;
        type Output = Vec<Pid>;

        fn name(&self) -> &'static str {
            "test-polyline"
        }

        fn validate(&self, _doc: &Document, p: &PolylineParams) -> Result<(), CommandError> {
            if p.points.len() < 2 {
                return Err(CommandError::invalid_param("points", "need two points"));
            }
            Ok(())
        }

        fn apply(
            &self,
            tx: &mut Transaction<'_>,
            p: PolylineParams,
        ) -> Result<Vec<Pid>, CommandError> {
            let n = p.points.len();
            let count = if p.closed { n } else { n - 1 };
            for i in 0..count {
                let (a, b) = (p.points[i], p.points[(i + 1) % n]);
                tx.with_mesh(ContainerRef::Model, |m, tol, d| m.add_edge(a, b, tol, d))?;
            }
            Ok(tx.created().to_vec())
        }

        fn meta(&self) -> CommandMeta {
            CommandMeta {
                name: "test-polyline".into(),
                category: CommandCategory::Draw,
                label: "Polyline".into(),
                view_only: false,
                steps: Vec::new(),
            }
        }
    }

    /// Creates a tag and assigns it to every face of the model (test-only).
    #[derive(Default)]
    struct TagFaces;

    impl Command for TagFaces {
        type Params = String;
        type Output = ();

        fn name(&self) -> &'static str {
            "test-tag-faces"
        }

        fn validate(&self, _doc: &Document, _p: &String) -> Result<(), CommandError> {
            Ok(())
        }

        fn apply(&self, tx: &mut Transaction<'_>, name: String) -> Result<(), CommandError> {
            let tag = tx.insert_tag(Tag::new(name, crate::document::Rgba8::BLACK))?;
            let faces: Vec<EntityId> = tx
                .document()
                .model
                .mesh()
                .faces()
                .map(|(f, _)| EntityId::Face(f))
                .collect();
            for f in faces {
                tx.set_tag(ContainerRef::Model, f, Some(tag))?;
            }
            Ok(())
        }

        fn meta(&self) -> CommandMeta {
            CommandMeta {
                name: "test-tag-faces".into(),
                category: CommandCategory::Organize,
                label: "Tag faces".into(),
                view_only: false,
                steps: Vec::new(),
            }
        }
    }

    /// A command that draws an edge and then fails (rollback test).
    #[derive(Default)]
    struct DrawThenFail;

    impl Command for DrawThenFail {
        type Params = ();
        type Output = ();

        fn name(&self) -> &'static str {
            "test-fail"
        }

        fn validate(&self, _doc: &Document, _p: &()) -> Result<(), CommandError> {
            Ok(())
        }

        fn apply(&self, tx: &mut Transaction<'_>, _p: ()) -> Result<(), CommandError> {
            tx.with_mesh(ContainerRef::Model, |m, tol, d| {
                m.add_edge(DVec3::ZERO, DVec3::new(0.0, 0.0, 50.0), tol, d)
            })?;
            Err(CommandError::NotApplicable("on purpose".into()))
        }

        fn meta(&self) -> CommandMeta {
            CommandMeta {
                name: "test-fail".into(),
                category: CommandCategory::Draw,
                label: "Fail".into(),
                view_only: false,
                steps: Vec::new(),
            }
        }
    }

    /// Id-independent view of the model: each ordered entity as
    /// `pid kind geometry tag hidden`.
    fn signature(doc: &Document) -> Vec<String> {
        let model = &doc.model;
        let mesh: &HalfEdgeMesh = model.mesh();
        model
            .iter()
            .map(|id| {
                let pid = model.pid_of(id).unwrap();
                let geometry = match id {
                    EntityId::Edge(e) => {
                        let s = mesh.edge_segment(e).unwrap();
                        let (a, b) = (format!("{:.3?}", s.a), format!("{:.3?}", s.b));
                        if a < b {
                            format!("{a}-{b}")
                        } else {
                            format!("{b}-{a}")
                        }
                    }
                    EntityId::Face(f) => format!(
                        "area {:.3} n {:.3?}",
                        mesh.face_area(f).unwrap(),
                        mesh.face_normal(f).unwrap()
                    ),
                    other => format!("{other:?}"),
                };
                let tag = model
                    .tag_of(id)
                    .and_then(|t| doc.tags.get(t))
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                format!("{pid} {geometry} [{tag}] {}", model.is_hidden(id))
            })
            .collect()
    }

    fn engine() -> Engine {
        let mut reg = CommandRegistry::new();
        reg.register(Polyline);
        reg.register(TagFaces);
        reg.register(DrawThenFail);
        Engine::new(Document::new(), Arc::new(reg))
    }

    fn rect() -> PolylineParams {
        PolylineParams {
            points: vec![
                DVec3::ZERO,
                DVec3::new(100.0, 0.0, 0.0),
                DVec3::new(100.0, 50.0, 0.0),
                DVec3::new(0.0, 50.0, 0.0),
            ],
            closed: true,
        }
    }

    #[test]
    fn mesh_commands_bind_pids_and_undo_redo_restores_them() {
        let mut e = engine();
        let created = e.execute::<Polyline>(rect()).unwrap();
        assert_eq!(created.len(), 5, "4 edges + 1 face");
        assert_eq!(e.last_created(), created.as_slice());
        let after_rect = signature(e.document());

        e.execute::<TagFaces>("Floor".into()).unwrap();
        // A chord splits the tagged face: the new piece inherits the tag.
        e.execute::<Polyline>(PolylineParams {
            points: vec![DVec3::new(40.0, 0.0, 0.0), DVec3::new(40.0, 50.0, 0.0)],
            closed: false,
        })
        .unwrap();
        let end = signature(e.document());
        let tagged = end.iter().filter(|s| s.contains("[Floor]")).count();
        assert_eq!(tagged, 2, "{end:#?}");
        assert_eq!(e.document().model.mesh().face_count(), 2);
        assert_eq!(e.generation(), 3);

        e.undo().unwrap();
        e.undo().unwrap();
        assert_eq!(signature(e.document()), after_rect);
        assert!(e.document().tags.by_name("Floor").is_none());
        e.undo().unwrap();
        assert!(e.document().model.is_empty());
        assert!(e.document().model.mesh().is_empty());
        assert!(e.undo().is_err());

        e.redo().unwrap();
        assert_eq!(signature(e.document()), after_rect);
        e.redo().unwrap();
        e.redo().unwrap();
        assert_eq!(signature(e.document()), end);
        assert!(e.redo().is_err());

        // Twice more around the loop: aliases keep resolving.
        for _ in 0..2 {
            for _ in 0..3 {
                e.undo().unwrap();
            }
            for _ in 0..3 {
                e.redo().unwrap();
            }
            assert_eq!(signature(e.document()), end);
        }
        assert!(
            e.document()
                .model
                .mesh()
                .validate(e.document().tolerances())
                .is_empty()
        );
    }

    #[test]
    fn failed_commands_roll_back_and_leave_no_history() {
        let mut e = engine();
        e.execute::<Polyline>(rect()).unwrap();
        let before = signature(e.document());
        let err = e.execute::<DrawThenFail>(()).unwrap_err();
        assert_eq!(err.code(), "E1201");
        assert_eq!(signature(e.document()), before);
        assert_eq!(e.history().applied(), 1);
        // Pids are never reused, even after a rollback.
        let next = e.execute::<Polyline>(PolylineParams {
            points: vec![DVec3::new(0.0, 0.0, 10.0), DVec3::new(5.0, 0.0, 10.0)],
            closed: false,
        });
        let pids = next.unwrap();
        assert!(pids.iter().all(|p| p.0 > 5));
    }

    #[test]
    fn groups_make_one_undo_step_and_erased_commands_run() {
        let mut e = engine();
        let token = e.begin_group("Two squares");
        let inner = e.begin_group("ignored");
        e.execute_erased("test-polyline", serde_json::to_value(rect()).unwrap())
            .unwrap();
        e.end_group(inner).unwrap();
        e.execute_erased("test-tag-faces", serde_json::json!("Walls"))
            .unwrap();
        assert!(e.end_group(GroupToken(999)).is_err());
        e.end_group(token).unwrap();
        assert_eq!(e.history().applied(), 1);
        assert_eq!(e.history().undo_label(), Some("Two squares"));
        let dry = e
            .execute_erased(
                "test-polyline",
                serde_json::json!({ "points": [[0.0, 0.0, 0.0]], "closed": false, "dry_run": true }),
            )
            .unwrap_err();
        assert_eq!(dry.code(), "E1001");
        e.undo().unwrap();
        assert!(e.document().model.is_empty());
    }

    #[test]
    fn edit_context_must_resolve() {
        let mut e = engine();
        let bad = EditContext {
            path: InstancePath::root().child(crate::document::EntityKey::Pid(Pid(42))),
        };
        assert_eq!(e.set_context(bad).unwrap_err().code(), "E1101");
        assert!(e.set_context(EditContext::model()).is_ok());
        let _ = NoDeltas;
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(CommandError::Cancelled.code(), "E1701");
        assert_eq!(
            CommandError::Geometry(GeometryError::OutOfRange).code(),
            "E1002"
        );
        assert_eq!(
            CommandError::Io(IoError::UnsupportedVersion { found: 9, max: 1 }).code(),
            "E1401"
        );
    }
}
