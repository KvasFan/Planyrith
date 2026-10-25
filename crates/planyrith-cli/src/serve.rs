//! `planyrith serve` (docs/08 §3): a JSON-RPC 2.0 server over standard input
//! and output, one JSON frame per line. Milestone 0.1 serves the stdio
//! transport and every method that does not need the renderer; the socket
//! transport, `command.preview` and `document.render_mesh` arrive with the GUI
//! attach mode (0.2, docs/08 Decisions).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::path::PathBuf;
use std::rc::Rc;

use planyrith_core::commands::{CommandError, CommandRegistry, Event, GroupToken};
use planyrith_core::document::{Document, EditContext, EntityPath, EntityRef, Query, QueryEnv};
use planyrith_core::ids::Pid;
use serde_json::{Map, Value, json};

use crate::out;
use crate::session::Session;
use crate::verbs::{document_info, entity_row};
use crate::{CliError, Global};

/// JSON-RPC 2.0 reserved error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// Every field `document.get` returns (the `query --fields` names).
const ALL_FIELDS: [&str; 15] = [
    "pid",
    "kind",
    "path",
    "tag",
    "material",
    "bbox",
    "length",
    "area",
    "volume",
    "name",
    "definition",
    "transform",
    "is_solid",
    "hidden",
    "locked",
];

/// Engine events of every open document, oldest first: `(doc_id, kind, data)`.
type EventQueue = Rc<RefCell<Vec<(String, &'static str, Value)>>>;

/// Event kinds `events.subscribe` accepts (`*` = all).
const EVENT_KINDS: [&str; 9] = [
    "document_changed",
    "selection_changed",
    "context_changed",
    "scene_activated",
    "camera_changed",
    "tool_changed",
    "progress",
    "warning",
    "history_changed",
];

/// Runs the server until standard input closes.
pub fn serve(
    global: &Global,
    socket: Option<&str>,
    token: Option<&str>,
    readonly: bool,
) -> Result<(), CliError> {
    if socket.is_some() || token.is_some() {
        return Err(CliError::usage(
            "the socket transport (and its --token) arrives with the GUI attach mode in 0.2; \
             0.1 serves standard input and output (--stdio)",
        ));
    }
    let mut server = Server {
        global: global.clone(),
        readonly,
        registry: CommandRegistry::with_builtins(),
        docs: BTreeMap::new(),
        next_doc: 1,
        kinds: BTreeSet::new(),
        events: Rc::default(),
    };
    if global.file.is_some() {
        let session = Session::open(global, false)?;
        server.adopt(session);
    }
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| {
            CliError::io(planyrith_core::io::IoError::storage(
                "standard input",
                e.to_string(),
            ))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = server.frame(&line);
        // Notifications first: when a reply arrives, the client has seen
        // every event its request caused.
        for event in server.drain_events() {
            send(&event)?;
        }
        if let Some(reply) = reply {
            send(&reply)?;
        }
    }
    Ok(())
}

/// Writes one frame; a closed output ends the server quietly.
fn send(frame: &Value) -> Result<(), CliError> {
    out::line(frame)
}

/// A JSON-RPC error.
struct RpcError {
    code: i64,
    message: String,
    data: Value,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
            data: Value::Null,
        }
    }

    fn params(message: impl Into<String>) -> Self {
        RpcError::new(INVALID_PARAMS, message)
    }

    /// A §9 error: the JSON-RPC code is the number of the `E` code
    /// (`E1101` → 1101), `data` carries the code itself and the parameter.
    fn from_cli(e: CliError) -> Self {
        match &e.code {
            Some(code) => RpcError {
                code: code
                    .trim_start_matches('E')
                    .parse()
                    .unwrap_or(INTERNAL_ERROR),
                message: e.message,
                data: json!({ "code": code, "param": e.param, "details": Value::Null }),
            },
            None => RpcError {
                code: INVALID_PARAMS,
                message: e.message,
                data: Value::Null,
            },
        }
    }

    fn from_command(e: CommandError) -> Self {
        RpcError::from_cli(CliError::from_command(e))
    }

    /// `E1501`: the server is read-only.
    fn readonly(what: &str) -> Self {
        RpcError::from_cli(CliError::io(planyrith_core::io::IoError::ReadOnly(
            format!("the server is read-only (--readonly): {what} is refused"),
        )))
    }

    fn to_json(&self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if !self.data.is_null() {
            error["data"] = self.data.clone();
        }
        error
    }
}

/// An open document.
struct Doc {
    session: Session,
    /// The undo group consecutive `command.execute` calls with the same
    /// `group` share.
    group: Option<(String, GroupToken)>,
}

struct Server {
    global: Global,
    readonly: bool,
    registry: CommandRegistry,
    docs: BTreeMap<String, Doc>,
    next_doc: u64,
    /// Subscribed event kinds.
    kinds: BTreeSet<String>,
    /// Pending events.
    events: EventQueue,
}

impl Server {
    /// Handles one frame (a request, a notification or a batch); `None` when
    /// nothing is to be answered.
    fn frame(&mut self, text: &str) -> Option<Value> {
        let value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                return Some(reply_error(
                    Value::Null,
                    RpcError::new(PARSE_ERROR, e.to_string()),
                ));
            }
        };
        match value {
            Value::Array(items) if !items.is_empty() => {
                let replies: Vec<Value> =
                    items.into_iter().filter_map(|r| self.request(r)).collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            other => self.request(other),
        }
    }

    fn request(&mut self, request: Value) -> Option<Value> {
        let Value::Object(mut map) = request else {
            return Some(reply_error(
                Value::Null,
                RpcError::new(INVALID_REQUEST, "a request is a JSON object"),
            ));
        };
        let id = map.remove("id");
        let reply_id = id.clone().unwrap_or(Value::Null);
        if map.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(reply_error(
                reply_id,
                RpcError::new(INVALID_REQUEST, "`jsonrpc` must be \"2.0\""),
            ));
        }
        let Some(method) = map
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return Some(reply_error(
                reply_id,
                RpcError::new(INVALID_REQUEST, "`method` must be a string"),
            ));
        };
        let params = match map.remove("params") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(p)) => p,
            Some(_) => {
                return Some(reply_error(
                    reply_id,
                    RpcError::params("`params` must be an object"),
                ));
            }
        };
        let result = self.call(&method, &params);
        // A request without an id is a notification: no reply, even on error.
        id.map(|id| match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "result": result, "id": id }),
            Err(e) => reply_error(id, e),
        })
    }

    fn call(&mut self, method: &str, p: &Map<String, Value>) -> Result<Value, RpcError> {
        match method {
            "server.capabilities" => Ok(self.capabilities()),
            "session.new" => self.session_new(p),
            "session.open" => self.session_open(p),
            "session.save" => self.session_save(p),
            "session.close" => self.session_close(p),
            "command.list" => Ok(serde_json::to_value(self.registry.list()).unwrap_or(Value::Null)),
            "command.schema" => self.command_schema(p),
            "command.execute" => self.command_execute(p),
            "history.undo" | "history.redo" => self.history_step(p, method == "history.undo"),
            "history.list" => self.history_list(p),
            "document.query" => self.document_query(p),
            "document.get" => self.document_get(p),
            "selection.get" => {
                let doc = self.doc(p)?;
                Ok(selection_json(&doc.session))
            }
            "selection.set" => self.selection_set(p),
            "context.get" => {
                let doc = self.doc(p)?;
                Ok(context_json(&doc.session))
            }
            "context.set" => self.context_set(p),
            "events.subscribe" => self.subscribe(p),
            "command.preview" | "document.render_mesh" => {
                Err(RpcError::from_command(CommandError::NotApplicable(
                    format!("`{method}` needs the renderer, which arrives in 0.2"),
                )))
            }
            other => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("unknown method `{other}`"),
            )),
        }
    }

    fn capabilities(&self) -> Value {
        let schema_version = self
            .registry
            .list()
            .first()
            .map(|c| c.schema_version)
            .unwrap_or(1);
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "schema_version": schema_version,
            "formats": { "open": ["plyr"], "save": ["plyr"], "export": ["json"] },
            "features": {
                "transports": ["stdio"],
                "readonly": self.readonly,
                "events": EVENT_KINDS,
                "methods": [
                    "server.capabilities", "session.new", "session.open", "session.save",
                    "session.close", "command.list", "command.schema", "command.execute",
                    "history.undo", "history.redo", "history.list", "document.query",
                    "document.get", "selection.get", "selection.set", "context.get",
                    "context.set", "events.subscribe",
                ],
            },
        })
    }

    /// Registers a session under a new id and forwards its events.
    fn adopt(&mut self, mut session: Session) -> String {
        let id = format!("d{}", self.next_doc);
        self.next_doc += 1;
        let queue = Rc::clone(&self.events);
        let doc_id = id.clone();
        session
            .engine
            .events_mut()
            .subscribe(Box::new(move |event| {
                if let Some((kind, data)) = event_json(event) {
                    queue.borrow_mut().push((doc_id.clone(), kind, data));
                }
            }));
        self.docs.insert(
            id.clone(),
            Doc {
                session,
                group: None,
            },
        );
        id
    }

    fn opened(&mut self, session: Session) -> Value {
        let info = document_info(&session);
        let id = self.adopt(session);
        json!({ "doc_id": id, "info": info })
    }

    fn session_new(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let path = optional_str(p, "path")?.map(PathBuf::from);
        Ok(self.opened(Session::create(Document::new(), path)))
    }

    fn session_open(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let path = required_str(p, "path")?;
        let global = Global {
            file: Some(PathBuf::from(path)),
            ..self.global.clone()
        };
        let session = Session::open(&global, false).map_err(RpcError::from_cli)?;
        Ok(self.opened(session))
    }

    fn session_save(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        if self.readonly {
            return Err(RpcError::readonly("saving"));
        }
        let out = optional_str(p, "path")?.map(PathBuf::from);
        let global = Global {
            out,
            no_save: false,
            dry_run: false,
            ..self.global.clone()
        };
        let doc = self.doc_mut(p)?;
        close_group(doc)?;
        let saved = doc.session.save(&global).map_err(RpcError::from_cli)?;
        Ok(json!({ "path": saved }))
    }

    fn session_close(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let id = required_str(p, "doc_id")?;
        let doc = self
            .docs
            .remove(id)
            .ok_or_else(|| RpcError::params(format!("no open document `{id}`")))?;
        drop(doc);
        self.events.borrow_mut().retain(|(d, ..)| d != id);
        Ok(json!({ "closed": id }))
    }

    fn command_schema(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let name = required_str(p, "name")?;
        let command = self
            .registry
            .get(name)
            .ok_or_else(|| RpcError::params(format!("unknown command `{name}`")))?;
        Ok(json!({
            "params": serde_json::to_value(command.params_schema()).unwrap_or(Value::Null),
            "output": serde_json::to_value(command.output_schema()).unwrap_or(Value::Null),
            "meta": serde_json::to_value(command.meta()).unwrap_or(Value::Null),
        }))
    }

    fn command_execute(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let name = required_str(p, "name")?.to_string();
        let params = match p.get("params") {
            None | Some(Value::Null) => json!({}),
            Some(v @ Value::Object(_)) => v.clone(),
            Some(_) => return Err(RpcError::params("`params` must be an object")),
        };
        let group = optional_str(p, "group")?.map(str::to_string);
        let view_only = self
            .registry
            .get(&name)
            .map(|c| c.meta().view_only)
            .ok_or_else(|| RpcError::params(format!("unknown command `{name}`")))?;
        if self.readonly && !view_only {
            return Err(RpcError::readonly(&format!("`{name}`")));
        }
        let global = self.global.clone();
        let doc = self.doc_mut(p)?;
        // Consecutive calls with the same group form one undo step.
        if doc.group.as_ref().map(|(g, _)| g) != group.as_ref() {
            close_group(doc)?;
            if let Some(label) = &group {
                let token = doc.session.engine.begin_group(label);
                doc.group = Some((label.clone(), token));
            }
        }
        let warned = doc.session.warnings.borrow().len();
        let before = doc.session.engine.generation();
        let output = doc
            .session
            .engine
            .execute_erased(&name, params.clone())
            .map_err(RpcError::from_command)?;
        if !view_only {
            doc.session
                .journal(&name, &params, group.as_deref(), &global);
        }
        let warnings: Vec<Value> = doc
            .session
            .warnings
            .borrow()
            .iter()
            .skip(warned)
            .map(|w| json!({ "code": w.code.as_str(), "message": w.message }))
            .collect();
        let engine = &doc.session.engine;
        Ok(json!({
            "output": output,
            "warnings": warnings,
            "deltas_summary": {
                "changed": engine.generation() != before,
                "generation": engine.generation(),
                "created": engine.last_created().iter().map(|p| p.0).collect::<Vec<_>>(),
            },
        }))
    }

    fn history_step(&mut self, p: &Map<String, Value>, undo: bool) -> Result<Value, RpcError> {
        if self.readonly {
            return Err(RpcError::readonly(if undo { "undo" } else { "redo" }));
        }
        let doc = self.doc_mut(p)?;
        close_group(doc)?;
        let engine = &mut doc.session.engine;
        if undo { engine.undo() } else { engine.redo() }.map_err(RpcError::from_command)?;
        Ok(history_json(&doc.session))
    }

    fn history_list(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let doc = self.doc(p)?;
        Ok(history_json(&doc.session))
    }

    fn document_query(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let query = required_str(p, "query")?.to_string();
        let fields: Vec<String> = match p.get("fields") {
            None | Some(Value::Null) => vec!["pid".into(), "kind".into(), "path".into()],
            Some(Value::Array(list)) => list
                .iter()
                .map(|f| {
                    f.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| RpcError::params("`fields` lists field names"))
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(RpcError::params("`fields` must be an array of names")),
        };
        let doc = self.doc(p)?;
        rows(&doc.session, &query, &fields).map(Value::Array)
    }

    fn document_get(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let pid = p
            .get("pid")
            .and_then(Value::as_u64)
            .ok_or_else(|| RpcError::params("`pid` must be a positive integer"))?;
        let fields: Vec<String> = ALL_FIELDS.iter().map(|f| f.to_string()).collect();
        let doc = self.doc(p)?;
        let mut found = rows(&doc.session, &format!("@{pid}"), &fields)?;
        match found.len() {
            0 => Err(RpcError::from_command(CommandError::NotFound(
                EntityRef::Pid(Pid(pid)),
            ))),
            _ => Ok(found.swap_remove(0)),
        }
    }

    fn selection_set(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let query = required_str(p, "query")?.to_string();
        let doc = self.doc_mut(p)?;
        doc.session
            .engine
            .execute_erased("select", json!({ "query": query }))
            .map_err(RpcError::from_command)?;
        Ok(selection_json(&doc.session))
    }

    fn context_set(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let path = required_str(p, "path")?.to_string();
        let doc = self.doc_mut(p)?;
        let context = if path.is_empty() || path == "#Model" {
            EditContext::model()
        } else {
            let parsed = EntityPath::parse(&path, doc.session.engine.document())
                .map_err(|e| RpcError::params(format!("path: {e}")))?;
            EditContext {
                path: parsed.instances.child(parsed.target),
            }
        };
        close_group(doc)?;
        doc.session
            .engine
            .set_context(context)
            .map_err(RpcError::from_command)?;
        Ok(context_json(&doc.session))
    }

    fn subscribe(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let kinds = match p.get("kinds") {
            Some(Value::Array(list)) => list,
            _ => return Err(RpcError::params("`kinds` must be an array of event kinds")),
        };
        let mut set = BTreeSet::new();
        for k in kinds {
            let k = k
                .as_str()
                .ok_or_else(|| RpcError::params("`kinds` lists event kinds"))?;
            if k == "*" {
                set.extend(EVENT_KINDS.iter().map(|s| s.to_string()));
            } else if EVENT_KINDS.contains(&k) {
                set.insert(k.to_string());
            } else {
                return Err(RpcError::params(format!(
                    "unknown event kind `{k}` (one of {}, or `*`)",
                    EVENT_KINDS.join(", ")
                )));
            }
        }
        self.kinds = set;
        Ok(json!({ "kinds": self.kinds }))
    }

    /// The pending events as `event` notifications, filtered by the
    /// subscription.
    fn drain_events(&mut self) -> Vec<Value> {
        let pending = std::mem::take(&mut *self.events.borrow_mut());
        pending
            .into_iter()
            .filter(|(_, kind, _)| self.kinds.contains(*kind))
            .map(|(doc_id, kind, data)| {
                json!({
                    "jsonrpc": "2.0",
                    "method": "event",
                    "params": { "doc_id": doc_id, "kind": kind, "data": data },
                })
            })
            .collect()
    }

    fn doc(&self, p: &Map<String, Value>) -> Result<&Doc, RpcError> {
        let id = required_str(p, "doc_id")?;
        self.docs
            .get(id)
            .ok_or_else(|| RpcError::params(format!("no open document `{id}`")))
    }

    fn doc_mut(&mut self, p: &Map<String, Value>) -> Result<&mut Doc, RpcError> {
        let id = required_str(p, "doc_id")?;
        self.docs
            .get_mut(id)
            .ok_or_else(|| RpcError::params(format!("no open document `{id}`")))
    }
}

/// Ends the document's open undo group, if any.
fn close_group(doc: &mut Doc) -> Result<(), RpcError> {
    if let Some((_, token)) = doc.group.take() {
        doc.session
            .engine
            .end_group(token)
            .map_err(RpcError::from_command)?;
    }
    Ok(())
}

fn reply_error(id: Value, e: RpcError) -> Value {
    json!({ "jsonrpc": "2.0", "error": e.to_json(), "id": id })
}

fn required_str<'a>(p: &'a Map<String, Value>, key: &str) -> Result<&'a str, RpcError> {
    p.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::params(format!("`{key}` must be a string")))
}

fn optional_str<'a>(p: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, RpcError> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(RpcError::params(format!("`{key}` must be a string"))),
    }
}

/// Query rows, as `query --json` prints them.
fn rows(session: &Session, query: &str, fields: &[String]) -> Result<Vec<Value>, RpcError> {
    let engine = &session.engine;
    let expr = Query::new(query)
        .parse()
        .map_err(|e| RpcError::from_cli(CliError::invalid("query", e.to_string())))?;
    let env = QueryEnv {
        selection: engine.selection(),
        last: engine.last_created(),
        context: engine.context(),
    };
    let selection = expr
        .eval(engine.document(), &env)
        .map_err(|e| RpcError::from_cli(CliError::invalid("query", e.to_string())))?;
    selection
        .iter()
        .map(|path| {
            entity_row(engine.document(), &env, path, fields)
                .map(Value::Object)
                .map_err(RpcError::from_cli)
        })
        .collect()
}

fn selection_json(session: &Session) -> Value {
    let doc = session.engine.document();
    let paths: Vec<String> = session
        .engine
        .selection()
        .iter()
        .map(|p| p.display_with(doc))
        .collect();
    json!({ "selection": paths })
}

/// The open container as an entity path (`#Model` at the root), the form
/// `context.set` and `--context` accept.
fn context_json(session: &Session) -> Value {
    let path = &session.engine.context().path;
    let text = match (path.parent(), path.steps.last()) {
        (Some(instances), Some(&target)) => {
            EntityPath { instances, target }.display_with(session.engine.document())
        }
        _ => "#Model".to_string(),
    };
    json!({ "context": text })
}

fn history_json(session: &Session) -> Value {
    let history = session.engine.history();
    let labels: Vec<&str> = history.entries().iter().map(|e| e.label.as_str()).collect();
    json!({
        "entries": labels,
        "applied": history.applied(),
        "undo": history.undo_label(),
        "redo": history.redo_label(),
        "generation": session.engine.generation(),
    })
}

/// The kind and payload of an engine event (docs/01 §8).
fn event_json(event: &Event) -> Option<(&'static str, Value)> {
    fn to<T: serde::Serialize + ?Sized>(v: &T) -> Value {
        serde_json::to_value(v).unwrap_or(Value::Null)
    }
    Some(match event {
        Event::DocumentChanged {
            label,
            deltas,
            generation,
        } => (
            "document_changed",
            json!({ "label": label, "generation": generation, "deltas": to(&**deltas) }),
        ),
        Event::SelectionChanged(selection) => ("selection_changed", to(selection)),
        Event::ActiveContextChanged(context) => ("context_changed", to(context)),
        Event::SceneActivated(scene) => ("scene_activated", to(scene)),
        Event::CameraChanged(camera) => ("camera_changed", to(camera)),
        Event::ToolChanged(tool) => ("tool_changed", json!(tool)),
        Event::Progress { fraction, message } => (
            "progress",
            json!({ "fraction": fraction, "message": message }),
        ),
        Event::Warning(w) => (
            "warning",
            json!({ "code": w.code.as_str(), "message": w.message }),
        ),
        Event::HistoryChanged { can_undo, can_redo } => (
            "history_changed",
            json!({ "can_undo": can_undo, "can_redo": can_redo }),
        ),
    })
}
