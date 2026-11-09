//! Implements docs/08-cli-specification.md §3.3 (joining from the browser):
//! a session joined to a `planyrith serve` document without ever waiting
//! for an answer, as the browser requires. Requests go out at once; replies
//! are taken when they arrive, a frame at a time.
//!
//! [`Link`] is the protocol without input or output: the frames of §3
//! (a little-endian `u32` length, then JSON), inside the encrypted messages
//! of §3.2 when the server listens on an address. [`Joined`] keeps a joined
//! document current over any [`Socket`] (a browser's WebSocket; a native
//! one in the tests), as the desktop's blocking attachment does: news every
//! half second, the document reloaded when it changed, commands, one's own
//! undo and saving on the server.

use std::collections::BTreeMap;

use planyrith_core::document::{EditContext, Selection};
use planyrith_core::ids::Pid;
use planyrith_core::transport::{Initiator, Opener, Sealer, take_message};
use serde::Deserialize;
use serde_json::{Value, json};

/// The largest frame accepted (as the server's).
const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Someone connected to the server.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Participant {
    /// The server's number for them.
    pub client: u64,
    /// Their name.
    pub name: String,
    /// Their color (sRGB).
    pub color: [u8; 3],
    /// This is the session's own connection.
    pub you: bool,
    /// What they selected: paths and world bounds.
    pub selection: Vec<Selected>,
}

/// One selected entity of a participant.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Selected {
    /// Its path.
    pub path: String,
    /// Its world bounds, when it has any.
    pub bounds: Option<[[f64; 3]; 2]>,
}

/// Authoritative document and view state returned by `document.snapshot`.
#[derive(Deserialize)]
pub struct Snapshot {
    /// The document as native bytes.
    pub bytes: Vec<u8>,
    /// Its generation.
    pub generation: u64,
    /// The client's selection on the server.
    pub selection: Selection,
    /// The client's edit context on the server.
    pub context: EditContext,
    /// What the client's last command created.
    pub last: Vec<Pid>,
    /// Where the server saves the document.
    pub path: Option<std::path::PathBuf>,
    /// The first save asks for a native working copy.
    #[serde(default)]
    pub native_copy_pending: bool,
    /// The shared undo label.
    pub undo: Option<String>,
    /// The shared redo label.
    pub redo: Option<String>,
}

/// What `session.status` answers.
#[derive(Deserialize)]
pub(crate) struct Status {
    pub generation: u64,
    pub undo: Option<String>,
    pub redo: Option<String>,
    /// The labels of one's own undo and redo (servers from 0.12 on).
    #[serde(default)]
    pub mine: Option<Labels>,
    pub participants: Vec<Participant>,
}

/// Undo and redo labels.
#[derive(Deserialize, Default)]
pub(crate) struct Labels {
    pub undo: Option<String>,
    pub redo: Option<String>,
}

/// The JSON-RPC protocol of `serve` without input or output.
pub struct Link {
    handshake: Option<Initiator>,
    keys: Option<(Sealer, Opener)>,
    received: Vec<u8>,
    plain: Vec<u8>,
    /// Frames written before the handshake finished.
    waiting: Vec<Vec<u8>>,
    outbox: Vec<Vec<u8>>,
    next: u64,
    replies: BTreeMap<u64, Result<Value, String>>,
}

impl Link {
    /// A link; with a token, it starts the encrypted handshake.
    pub fn new(token: Option<&str>) -> Result<Link, String> {
        let mut link = Link {
            handshake: None,
            keys: None,
            received: Vec::new(),
            plain: Vec::new(),
            waiting: Vec::new(),
            outbox: Vec::new(),
            next: 0,
            replies: BTreeMap::new(),
        };
        if let Some(token) = token {
            let (initiator, hello) = Initiator::start(token).map_err(|e| e.to_string())?;
            link.handshake = Some(initiator);
            link.outbox.push(hello);
        }
        Ok(link)
    }

    /// Queues a request; returns its id.
    pub fn request(&mut self, method: &str, params: Value) -> Result<u64, String> {
        self.next += 1;
        let body = serde_json::to_vec(
            &json!({ "jsonrpc": "2.0", "id": self.next, "method": method, "params": params }),
        )
        .map_err(|e| e.to_string())?;
        if body.len() > MAX_FRAME {
            return Err("RPC request exceeds 64 MiB".into());
        }
        let mut frame = (body.len() as u32).to_le_bytes().to_vec();
        frame.extend(body);
        if self.handshake.is_some() {
            self.waiting.push(frame);
        } else {
            self.send(frame)?;
        }
        Ok(self.next)
    }

    fn send(&mut self, frame: Vec<u8>) -> Result<(), String> {
        let message = match &mut self.keys {
            Some((sealer, _)) => sealer.seal(&frame).map_err(|e| e.to_string())?,
            None => frame,
        };
        self.outbox.push(message);
        Ok(())
    }

    /// Takes in bytes the server sent (any pieces).
    pub fn received(&mut self, bytes: &[u8]) -> Result<(), String> {
        if let Some(initiator) = self.handshake.take() {
            self.received.extend_from_slice(bytes);
            let Some(reply) = take_message(&mut self.received) else {
                self.handshake = Some(initiator);
                return Ok(());
            };
            let session = initiator
                .finish(&reply)
                .map_err(|_| "The token is not the server's".to_string())?;
            let mut opener = session.opener();
            let rest = std::mem::take(&mut self.received);
            self.plain
                .extend(opener.push(&rest).map_err(|e| e.to_string())?);
            self.keys = Some((session.sealer(), opener));
            for frame in std::mem::take(&mut self.waiting) {
                self.send(frame)?;
            }
        } else {
            match &mut self.keys {
                Some((_, opener)) => {
                    let plain = opener.push(bytes).map_err(|e| e.to_string())?;
                    self.plain.extend(plain);
                }
                None => self.plain.extend_from_slice(bytes),
            }
        }
        self.frames()
    }

    fn frames(&mut self) -> Result<(), String> {
        while self.plain.len() >= 4 {
            let len =
                u32::from_le_bytes([self.plain[0], self.plain[1], self.plain[2], self.plain[3]])
                    as usize;
            if len > MAX_FRAME {
                return Err("RPC response exceeds 64 MiB".into());
            }
            if self.plain.len() < 4 + len {
                break;
            }
            let reply: Value =
                serde_json::from_slice(&self.plain[4..4 + len]).map_err(|e| e.to_string())?;
            self.plain.drain(..4 + len);
            // Events (no id) are not used: news come from `session.status`.
            if let Some(id) = reply.get("id").and_then(Value::as_u64) {
                let result = match reply.get("error") {
                    Some(error) => Err(error["message"]
                        .as_str()
                        .unwrap_or("Remote command failed")
                        .to_string()),
                    None => Ok(reply.get("result").cloned().unwrap_or(Value::Null)),
                };
                self.replies.insert(id, result);
            }
        }
        Ok(())
    }

    /// The reply to request `id`, once it arrived.
    pub fn reply(&mut self, id: u64) -> Option<Result<Value, String>> {
        self.replies.remove(&id)
    }

    /// The messages to send now, in order.
    pub fn outgoing(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outbox)
    }
}

/// A message channel to the server (a WebSocket).
pub trait Socket {
    /// Sends one message.
    fn send(&mut self, bytes: Vec<u8>);
    /// The messages received since the last call.
    fn receive(&mut self) -> Vec<Vec<u8>>;
    /// Whether it is open: `Ok(false)` while connecting, an error once
    /// closed.
    fn open(&self) -> Result<bool, String>;
}

/// What a request in flight is for.
enum Pending {
    Auth,
    Name,
    Status,
    Snapshot,
    /// A command, undo, redo or save: errors and warnings are reported.
    Change,
}

/// A document joined without waiting (docs/08 §3.3); see the module
/// documentation.
pub struct Joined {
    socket: Box<dyn Socket>,
    link: Option<Link>,
    token: String,
    encrypted: bool,
    doc_id: String,
    name: String,
    pending: BTreeMap<u64, Pending>,
    /// The generation of the document last loaded.
    pub generation: Option<u64>,
    /// One's own undo label.
    pub undo: Option<String>,
    /// One's own redo label.
    pub redo: Option<String>,
    /// The people working on the document.
    pub participants: Vec<Participant>,
    /// Authenticated and named: commands may be sent.
    pub ready: bool,
    polled: f64,
    /// Errors of commands since the last call to [`Joined::take_messages`].
    errors: Vec<String>,
    /// Warnings of commands since then.
    warnings: Vec<String>,
}

impl Joined {
    /// Joins `doc_id` as `name` over `socket` (which may still be
    /// connecting); `encrypted` when the server listens on an address.
    pub fn new(
        socket: Box<dyn Socket>,
        token: &str,
        encrypted: bool,
        doc_id: &str,
        name: &str,
    ) -> Joined {
        Joined {
            socket,
            link: None,
            token: token.to_string(),
            encrypted,
            doc_id: doc_id.to_string(),
            name: name.to_string(),
            pending: BTreeMap::new(),
            generation: None,
            undo: None,
            redo: None,
            participants: Vec::new(),
            ready: false,
            polled: f64::NEG_INFINITY,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn call(&mut self, method: &str, mut params: Value, what: Pending) -> Result<(), String> {
        let link = self.link.as_mut().ok_or("Not connected yet")?;
        if method != "server.auth" && method != "presence.set" {
            params["doc_id"] = json!(self.doc_id);
        }
        let id = link.request(method, params)?;
        self.pending.insert(id, what);
        Ok(())
    }

    /// Exchanges messages and returns a newer document when one arrived.
    /// `now` is in seconds; the server is asked for news every half second.
    pub fn poll(&mut self, now: f64) -> Result<Option<Snapshot>, String> {
        if !self.socket.open()? {
            return Ok(None);
        }
        if self.link.is_none() {
            self.link = Some(Link::new(self.encrypted.then_some(self.token.as_str()))?);
            self.call("server.auth", json!({ "token": self.token }), Pending::Auth)?;
            let name = self.name.clone();
            self.call("presence.set", json!({ "name": name }), Pending::Name)?;
            self.call("session.status", json!({}), Pending::Status)?;
            self.polled = now;
        }
        let link = self.link.as_mut().ok_or("Not connected")?;
        for message in self.socket.receive() {
            link.received(&message)?;
        }
        let mut snapshot = None;
        let ids: Vec<u64> = self.pending.keys().copied().collect();
        for id in ids {
            let Some(reply) = self.link.as_mut().and_then(|l| l.reply(id)) else {
                continue;
            };
            let Some(what) = self.pending.remove(&id) else {
                continue;
            };
            match what {
                Pending::Auth => {
                    reply.map_err(|e| format!("The server refused to join: {e}"))?;
                }
                Pending::Name => {
                    reply?;
                    self.ready = true;
                }
                Pending::Status => {
                    let status: Status =
                        serde_json::from_value(reply?).map_err(|e| e.to_string())?;
                    let mine = status.mine.unwrap_or(Labels {
                        undo: status.undo,
                        redo: status.redo,
                    });
                    self.undo = mine.undo;
                    self.redo = mine.redo;
                    self.participants = status.participants;
                    let loading = self
                        .pending
                        .values()
                        .any(|p| matches!(p, Pending::Snapshot));
                    if self.generation != Some(status.generation) && !loading {
                        self.call("document.snapshot", json!({}), Pending::Snapshot)?;
                    }
                }
                Pending::Snapshot => {
                    let loaded: Snapshot =
                        serde_json::from_value(reply?).map_err(|e| e.to_string())?;
                    self.generation = Some(loaded.generation);
                    snapshot = Some(loaded);
                }
                Pending::Change => match reply {
                    Ok(result) => {
                        if let Some(list) = result["warnings"].as_array() {
                            self.warnings.extend(
                                list.iter()
                                    .filter_map(|w| w["message"].as_str().map(str::to_string)),
                            );
                        }
                        // The change shows at once, not at the next poll.
                        self.polled = f64::NEG_INFINITY;
                    }
                    Err(e) => self.errors.push(e),
                },
            }
        }
        let asking = self.pending.values().any(|p| matches!(p, Pending::Status));
        if now - self.polled >= 0.5 && !asking {
            self.polled = now;
            self.call("session.status", json!({}), Pending::Status)?;
        }
        self.flush();
        Ok(snapshot)
    }

    fn flush(&mut self) {
        if let Some(link) = self.link.as_mut() {
            for message in link.outgoing() {
                self.socket.send(message);
            }
        }
    }

    /// Runs a command on the server with this session's selection (a query)
    /// and edit context (a path).
    pub fn execute(
        &mut self,
        name: &str,
        params: Value,
        selection: &str,
        context: &str,
    ) -> Result<(), String> {
        if !self.ready {
            return Err("Still joining the document".into());
        }
        self.call("context.set", json!({ "path": context }), Pending::Change)?;
        self.call(
            "selection.set",
            json!({ "query": selection }),
            Pending::Change,
        )?;
        self.call(
            "command.execute",
            json!({ "name": name, "params": params }),
            Pending::Change,
        )?;
        self.flush();
        Ok(())
    }

    /// Takes back (or re-applies) one's own last change.
    pub fn history(&mut self, undo: bool) -> Result<(), String> {
        if !self.ready {
            return Err("Still joining the document".into());
        }
        let method = if undo { "history.undo" } else { "history.redo" };
        self.call(method, json!({ "mine": true }), Pending::Change)?;
        self.flush();
        Ok(())
    }

    /// Saves the document where the server keeps it.
    pub fn save(&mut self) -> Result<(), String> {
        if !self.ready {
            return Err("Still joining the document".into());
        }
        self.call("session.save", json!({}), Pending::Change)?;
        self.flush();
        Ok(())
    }

    /// Errors and warnings of changes since the last call.
    pub fn take_messages(&mut self) -> (Vec<String>, Vec<String>) {
        (
            std::mem::take(&mut self.errors),
            std::mem::take(&mut self.warnings),
        )
    }
}

/// A handler the browser calls with an event.
#[cfg(target_arch = "wasm32")]
type Handler<E> = wasm_bindgen::closure::Closure<dyn FnMut(E)>;

/// A browser's WebSocket as a [`Socket`] (docs/08 §3.3).
#[cfg(target_arch = "wasm32")]
pub struct BrowserSocket {
    ws: web_sys::WebSocket,
    inbox: std::rc::Rc<std::cell::RefCell<Vec<Vec<u8>>>>,
    state: std::rc::Rc<std::cell::RefCell<Result<bool, String>>>,
    _handlers: (
        Handler<web_sys::Event>,
        Handler<web_sys::MessageEvent>,
        Handler<web_sys::Event>,
        Handler<web_sys::CloseEvent>,
    ),
}

#[cfg(target_arch = "wasm32")]
impl BrowserSocket {
    /// Opens `ws://<address>/rpc`; `address` is `host:port`.
    pub fn connect(address: &str) -> Result<BrowserSocket, String> {
        use std::cell::RefCell;
        use std::rc::Rc;
        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;

        let ws = web_sys::WebSocket::new(&format!("ws://{address}/rpc"))
            .map_err(|e| format!("{e:?}"))?;
        ws.set_binary_type(web_sys::BinaryType::Arraybuffer);
        let inbox: Rc<RefCell<Vec<Vec<u8>>>> = Rc::default();
        let state: Rc<RefCell<Result<bool, String>>> = Rc::new(RefCell::new(Ok(false)));
        let opened = {
            let state = Rc::clone(&state);
            Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                *state.borrow_mut() = Ok(true);
            })
        };
        let message = {
            let inbox = Rc::clone(&inbox);
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
                if let Ok(buffer) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                    inbox
                        .borrow_mut()
                        .push(js_sys::Uint8Array::new(&buffer).to_vec());
                }
            })
        };
        let failed = {
            let state = Rc::clone(&state);
            Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                *state.borrow_mut() = Err("Could not reach the server".into());
            })
        };
        let closed = {
            let state = Rc::clone(&state);
            Closure::<dyn FnMut(web_sys::CloseEvent)>::new(move |_| {
                let mut state = state.borrow_mut();
                if state.is_ok() {
                    *state = Err("The server closed the connection".into());
                }
            })
        };
        ws.set_onopen(Some(opened.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(message.as_ref().unchecked_ref()));
        ws.set_onerror(Some(failed.as_ref().unchecked_ref()));
        ws.set_onclose(Some(closed.as_ref().unchecked_ref()));
        Ok(BrowserSocket {
            ws,
            inbox,
            state,
            _handlers: (opened, message, failed, closed),
        })
    }
}

#[cfg(target_arch = "wasm32")]
impl Socket for BrowserSocket {
    fn send(&mut self, bytes: Vec<u8>) {
        if self.ws.send_with_u8_array(&bytes).is_err() {
            *self.state.borrow_mut() = Err("The connection to the server failed".into());
        }
    }

    fn receive(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut *self.inbox.borrow_mut())
    }

    fn open(&self) -> Result<bool, String> {
        self.state.borrow().clone()
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for BrowserSocket {
    fn drop(&mut self) {
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onerror(None);
        self.ws.set_onclose(None);
        let _ = self.ws.close();
    }
}
