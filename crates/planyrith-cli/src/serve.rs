//! `planyrith serve` (docs/08 §3): a JSON-RPC 2.0 server over standard input
//! and output (one JSON frame per line) or over a Unix socket or a localhost
//! TCP port (frames prefixed by their length as a little-endian `u32`). A TCP
//! server requires `--token`: clients authenticate with `server.auth` before
//! any other method (docs/08 Decisions).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use planyrith_core::commands::{CommandError, CommandRegistry, Engine, Event, GroupToken};
use planyrith_core::document::{
    ContainerRef, Document, EditContext, EntityPath, EntityRef, FrontOrBack, Query, QueryEnv,
};
use planyrith_core::ids::{EntityId, Pid};
use planyrith_core::io::render_mesh::{ExtractOptions, RenderMesh, extract};
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
/// Server-defined: authenticate first (`server.auth`).
const UNAUTHORIZED: i64 = -32001;
/// Largest socket frame accepted (64 MiB).
const MAX_FRAME: usize = 64 << 20;

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

/// Runs the server until standard input closes (stdio) or forever (socket).
pub fn serve(
    global: &Global,
    socket: Option<&str>,
    token: Option<&str>,
    readonly: bool,
) -> Result<(), CliError> {
    if socket.is_none() && token.is_some() {
        return Err(CliError::usage("--token applies to --socket"));
    }
    let mut server = Server {
        global: global.clone(),
        readonly,
        registry: Arc::new(crate::extensions::registry()?),
        docs: BTreeMap::new(),
        next_doc: 1,
        kinds: BTreeSet::new(),
        events: Rc::default(),
        token: token.map(str::to_string),
        authenticated: token.is_none(),
    };
    if global.file.is_some() {
        let session = Session::open(global, false)?;
        server.adopt(session);
    }
    if let Some(spec) = socket {
        return serve_socket(&mut server, spec);
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

fn io_error(what: &str, e: &std::io::Error) -> CliError {
    CliError::io(planyrith_core::io::IoError::storage(what, e.to_string()))
}

/// Listens on a localhost TCP port (a number; 0 picks a free one) or a Unix
/// socket path, and serves one client at a time. The documents stay open
/// across connections.
fn serve_socket(server: &mut Server, spec: &str) -> Result<(), CliError> {
    if let Ok(port) = spec.parse::<u16>() {
        if server.token.is_none() {
            return Err(CliError::usage(
                "a TCP socket needs --token <secret>: any local user can connect to it",
            ));
        }
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| io_error("socket", &e))?;
        let addr = listener.local_addr().map_err(|e| io_error("socket", &e))?;
        out::diagnostic(format!("listening on {addr}"));
        for stream in listener.incoming() {
            let stream = stream.map_err(|e| io_error("socket", &e))?;
            let _ = stream.set_nodelay(true);
            let reader = stream.try_clone().map_err(|e| io_error("socket", &e))?;
            if let Err(e) = server.connection(reader, stream) {
                out::diagnostic(format!("connection closed: {e}"));
            }
        }
        return Ok(());
    }
    serve_unix(server, spec)
}

#[cfg(unix)]
fn serve_unix(server: &mut Server, path: &str) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    let path = std::path::Path::new(path);
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(CliError::usage(format!(
                "{} is in use by another server",
                path.display()
            )));
        }
        // A stale socket from a server that did not exit cleanly.
        std::fs::remove_file(path).map_err(|e| io_error("socket", &e))?;
    }
    let listener = UnixListener::bind(path).map_err(|e| io_error("socket", &e))?;
    // Only this user may connect (docs/08 Decisions).
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| io_error("socket", &e))?;
    out::diagnostic(format!("listening on {}", path.display()));
    for stream in listener.incoming() {
        let stream = stream.map_err(|e| io_error("socket", &e))?;
        let reader = stream.try_clone().map_err(|e| io_error("socket", &e))?;
        if let Err(e) = server.connection(reader, stream) {
            out::diagnostic(format!("connection closed: {e}"));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn serve_unix(_server: &mut Server, path: &str) -> Result<(), CliError> {
    Err(CliError::usage(format!(
        "`{path}` is not a port number, and Unix sockets are not available on this system"
    )))
}

fn write_frame(writer: &mut impl Write, frame: &Value) -> std::io::Result<()> {
    let bytes = frame.to_string().into_bytes();
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
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
    registry: Arc<CommandRegistry>,
    /// The shared secret socket clients present with `server.auth`.
    token: Option<String>,
    /// The current client presented it (always true without a token).
    authenticated: bool,
    docs: BTreeMap<String, Doc>,
    next_doc: u64,
    /// Subscribed event kinds.
    kinds: BTreeSet<String>,
    /// Pending events.
    events: EventQueue,
}

impl Server {
    /// Serves one socket client until it disconnects.
    fn connection(&mut self, mut reader: impl Read, mut writer: impl Write) -> std::io::Result<()> {
        self.authenticated = self.token.is_none();
        self.kinds.clear();
        self.events.borrow_mut().clear();
        loop {
            let mut len = [0u8; 4];
            match reader.read_exact(&mut len) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e),
            }
            let len = u32::from_le_bytes(len) as usize;
            if len > MAX_FRAME {
                let error = RpcError::new(
                    INVALID_REQUEST,
                    format!("frame of {len} bytes is too large"),
                );
                write_frame(&mut writer, &reply_error(Value::Null, error))?;
                return Ok(());
            }
            let mut frame = vec![0u8; len];
            reader.read_exact(&mut frame)?;
            let reply = match String::from_utf8(frame) {
                Ok(text) => self.frame(&text),
                Err(e) => Some(reply_error(
                    Value::Null,
                    RpcError::new(PARSE_ERROR, format!("frames are UTF-8 JSON: {e}")),
                )),
            };
            for event in self.drain_events() {
                write_frame(&mut writer, &event)?;
            }
            if let Some(reply) = reply {
                write_frame(&mut writer, &reply)?;
            }
        }
    }

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
        if method == "server.auth" {
            return self.auth(p);
        }
        if !self.authenticated {
            return Err(RpcError::new(
                UNAUTHORIZED,
                "authenticate first: server.auth {token}",
            ));
        }
        match method {
            "server.capabilities" => Ok(self.capabilities()),
            "session.new" => self.session_new(p),
            "session.open" => self.session_open(p),
            "session.save" => self.session_save(p),
            "session.close" => self.session_close(p),
            "command.list" => Ok(serde_json::to_value(self.registry.list()).unwrap_or(Value::Null)),
            "command.schema" => self.command_schema(p),
            "command.execute" => self.command_execute(p),
            "script.eval" => {
                let mut command = p.clone();
                command.insert("name".into(), json!("script-eval"));
                command.insert("params".into(),json!({"source":required_str(p,"source")?,"args":p.get("args").cloned().unwrap_or(json!({}))}));
                self.command_execute(&command)
            }
            "history.undo" | "history.redo" => self.history_step(p, method == "history.undo"),
            "history.list" => self.history_list(p),
            "document.query" => self.document_query(p),
            "document.get" => self.document_get(p),
            "document.snapshot" => {
                let doc = self.doc(p)?;
                let engine = &doc.session.engine;
                let options = planyrith_core::io::native::WriteOptions {
                    deterministic: true,
                    include_history: false,
                    ..Default::default()
                };
                let bytes = planyrith_core::io::native::write_document(
                    engine.document(),
                    &[],
                    &options,
                    &planyrith_core::io::NoProgress,
                )
                .map_err(|e| RpcError::from_cli(CliError::io(e)))?;
                Ok(
                    json!({"bytes":bytes,"generation":engine.generation(),"selection":engine.selection().normalized(engine.document()),"context":EditContext {path:engine.context().path.canonical(engine.document())},"last":engine.last_created(),"path":doc.session.path,"undo":engine.history().undo_label(),"redo":engine.history().redo_label()}),
                )
            }
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
            "command.preview" => self.command_preview(p),
            "document.render_mesh" => self.document_render_mesh(p),
            other => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("unknown method `{other}`"),
            )),
        }
    }

    /// `server.auth {token}`: compares in constant time.
    fn auth(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let given = required_str(p, "token")?.as_bytes();
        let ok = self.token.as_ref().is_none_or(|t| {
            let t = t.as_bytes();
            t.len() == given.len()
                && t.iter().zip(given).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
        });
        if !ok {
            self.authenticated = false;
            return Err(RpcError::new(UNAUTHORIZED, "wrong token"));
        }
        self.authenticated = true;
        Ok(json!({ "authenticated": true }))
    }

    /// `document.render_mesh {doc_id, path?}`: the render buffers of the
    /// model, or of the definition a group or instance path opens.
    fn document_render_mesh(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let path = optional_str(p, "path")?.map(str::to_string);
        let doc = self.doc(p)?;
        let document = doc.session.engine.document();
        let container = match path {
            None => ContainerRef::Model,
            Some(text) => {
                let path = EntityPath::parse(&text, document)
                    .map_err(|e| RpcError::params(format!("path: {e}")))?;
                let resolved = document
                    .resolve(&path)
                    .map_err(|e| RpcError::params(format!("path: {e}")))?;
                let def = document
                    .container(resolved.container)
                    .and_then(|e| e.definition_of(resolved.entity))
                    .ok_or_else(|| RpcError::params("path must name a group or component"))?;
                ContainerRef::Definition(def)
            }
        };
        let mesh = extract(document, container, ExtractOptions::default())
            .map_err(|e| RpcError::from_command(CommandError::Geometry(e)))?;
        Ok(mesh_json(document, container, &mesh))
    }

    /// `command.preview {doc_id, name, params}`: runs the command on a copy
    /// of the document and returns its output and the render buffers of the
    /// containers it changed (the document itself is untouched).
    fn command_preview(&mut self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let name = required_str(p, "name")?.to_string();
        let params = match p.get("params") {
            None | Some(Value::Null) => json!({}),
            Some(v @ Value::Object(_)) => v.clone(),
            Some(_) => return Err(RpcError::params("`params` must be an object")),
        };
        let registry = Arc::clone(&self.registry);
        let doc = self.doc(p)?;
        let source = &doc.session.engine;
        let mut fork = Engine::new(source.document().fork_for_preview(), registry);
        fork.set_selection(source.selection().clone());
        fork.set_last_created(source.last_created().to_vec());
        fork.set_context(source.context().clone())
            .map_err(RpcError::from_command)?;
        let touched: Rc<RefCell<BTreeSet<ContainerRef>>> = Rc::default();
        let sink = Rc::clone(&touched);
        fork.events_mut().subscribe(Box::new(move |event| {
            if let Event::DocumentChanged { deltas, .. } = event {
                sink.borrow_mut()
                    .extend(deltas.iter().filter_map(|d| d.container()));
            }
        }));
        let output = fork
            .execute_erased(&name, params)
            .map_err(RpcError::from_command)?;
        let document = fork.document();
        let containers: Vec<Value> = touched
            .borrow()
            .iter()
            .filter_map(|&c| {
                let mesh = extract(document, c, ExtractOptions::default()).ok()?;
                Some(mesh_json(document, c, &mesh))
            })
            .collect();
        Ok(json!({ "output": output, "containers": containers }))
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
            "formats": {
                "open": ["plyr"],
                "save": ["plyr"],
                "import": planyrith_core::io::import::ImportFormat::supported(),
                "export": planyrith_core::io::export::ExportFormat::supported(),
            },
            "features": {
                "transports": if cfg!(unix) {
                    json!(["stdio", "tcp", "unix"])
                } else {
                    json!(["stdio", "tcp"])
                },
                "readonly": self.readonly,
                "events": EVENT_KINDS,
                "methods": [
                    "server.capabilities", "session.new", "session.open", "session.save",
                    "session.close", "command.list", "command.schema", "command.execute",
                    "history.undo", "history.redo", "history.list", "document.query",
                    "document.get", "selection.get", "selection.set", "context.get",
                    "context.set", "events.subscribe", "command.preview",
                    "document.render_mesh", "document.snapshot", "script.eval", "server.auth",
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
        Ok(self.opened(Session::create(Document::new(), path).map_err(RpcError::from_cli)?))
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
        if let Some(path) = &saved {
            doc.session.path = Some(PathBuf::from(path));
        }
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
            .execute(&name, params.clone())
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

/// Standard base64 with padding (the binary attachments of docs/08 §3).
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn f32_bytes<const N: usize>(values: &[[f32; N]]) -> String {
    let mut bytes = Vec::with_capacity(values.len() * N * 4);
    for v in values {
        for x in v {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
    }
    base64(&bytes)
}

fn u32_bytes(values: impl IntoIterator<Item = u32>) -> String {
    let bytes: Vec<u8> = values.into_iter().flat_map(u32::to_le_bytes).collect();
    base64(&bytes)
}

/// A render mesh as JSON: buffers as base64 little-endian arrays (`f32`
/// positions, normals and UVs; `u32` face ids, indices and edge faces; `u8`
/// edge flags) plus material ranges, bounds and the pids of the faces and
/// edges by local index (docs/06 §2, docs/08 §3).
fn mesh_json(doc: &Document, container: ContainerRef, mesh: &RenderMesh) -> Value {
    let entities = doc.container(container);
    let pid = |id: EntityId| entities.and_then(|e| e.pid_of(id)).map(|p| p.0);
    let container = match container {
        ContainerRef::Model => json!("model"),
        ContainerRef::Definition(d) => doc.definition(d).map_or(
            Value::Null,
            |def| json!({ "definition": def.uuid.to_string(), "name": def.name }),
        ),
    };
    let ranges: Vec<Value> = mesh
        .indices_by_material
        .iter()
        .map(|(key, range)| {
            json!({
                "material": key.material.and_then(|m| doc.materials.get(m)).map(|m| m.name.clone()),
                "side": if key.side == FrontOrBack::Front { "front" } else { "back" },
                "start": range.start,
                "end": range.end,
            })
        })
        .collect();
    let bounds = if mesh.bounds.is_empty() {
        Value::Null
    } else {
        json!({ "min": mesh.bounds.min.to_array(), "max": mesh.bounds.max.to_array() })
    };
    json!({
        "container": container,
        "vertex_count": mesh.positions.len(),
        "positions": f32_bytes(&mesh.positions),
        "normals": f32_bytes(&mesh.normals),
        "uvs": f32_bytes(&mesh.uvs),
        "face_ids": u32_bytes(mesh.face_ids.iter().copied()),
        "indices": u32_bytes(mesh.indices.iter().copied()),
        "ranges": ranges,
        "edges": {
            "count": mesh.edge_lookup.len(),
            "positions": f32_bytes(&mesh.edges.positions),
            "flags": base64(&mesh.edges.flags),
            "faces": u32_bytes(mesh.edges.faces.iter().flat_map(|f| *f)),
        },
        "bounds": bounds,
        "face_pids": mesh.face_lookup.iter().map(|f| pid(EntityId::Face(*f))).collect::<Vec<_>>(),
        "edge_pids": mesh.edge_lookup.iter().map(|e| pid(EntityId::Edge(*e))).collect::<Vec<_>>(),
    })
}
