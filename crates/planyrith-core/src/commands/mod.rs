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
pub mod draw;
pub mod edit;
pub mod file;
pub mod organize;
pub mod transaction;
pub mod view;

pub use transaction::{CommittedTransaction, Delta, Transaction, UndoEntry, UndoStack};

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::diagnostics::Warning;
use crate::document::{Camera, Document, EditContext, EntityPath, EntityRef, Query, Selection};
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
        todo!("docs/13 milestone 0.1 built-in command set")
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
        todo!("docs/04 §11 set edit context")
    }

    /// Pids created by the last executed command (`last` in queries, docs/08 §2).
    pub fn last_created(&self) -> &[Pid] {
        &self.last_created
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
        todo!("docs/01 §3.2 Engine::execute")
    }

    /// Executes a registered command by name with JSON params (CLI, RPC, scripts).
    pub fn execute_erased(
        &mut self,
        name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CommandError> {
        todo!("docs/01 §3.2 Engine::execute_erased")
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
        todo!("docs/01 §3.2 Engine::undo")
    }

    /// Redoes the last undone entry.
    pub fn redo(&mut self) -> Result<(), CommandError> {
        todo!("docs/01 §3.2 Engine::redo")
    }

    /// Starts grouping subsequent commands into one undo step (batches,
    /// `group()` in scripts).
    pub fn begin_group(&mut self, label: &str) -> GroupToken {
        todo!("docs/01 §3.2 Engine::begin_group")
    }

    /// Ends the group started with `token`.
    pub fn end_group(&mut self, token: GroupToken) -> Result<(), CommandError> {
        todo!("docs/01 §3.2 Engine::end_group")
    }

    /// Replaces the document (open/revert), clearing history and selection.
    pub fn replace_document(&mut self, doc: Document) {
        self.doc = doc;
        self.history = UndoStack::default();
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
