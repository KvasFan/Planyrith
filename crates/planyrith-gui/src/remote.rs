//! Implements docs/01 §1 and docs/08 §3: native GUI attachment to `serve`.

use planyrith_core::document::{EditContext, Selection};
use planyrith_core::ids::Pid;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::time::Duration;

trait Transport: Read + Write {}
impl<T: Read + Write> Transport for T {}

/// Authoritative document and view state returned by `document.snapshot`.
#[derive(Deserialize)]
pub(crate) struct Snapshot {
    pub bytes: Vec<u8>,
    pub generation: u64,
    pub selection: Selection,
    pub context: EditContext,
    pub last: Vec<Pid>,
    pub path: Option<std::path::PathBuf>,
    #[serde(default)]
    pub native_copy_pending: bool,
    pub undo: Option<String>,
    pub redo: Option<String>,
}

/// One local authenticated framed JSON-RPC connection.
pub(crate) struct Remote {
    transport: Box<dyn Transport>,
    serial: u64,
    doc_id: String,
    pub undo: Option<String>,
    pub redo: Option<String>,
    pub generation: u64,
}
impl Remote {
    pub fn connect(address: &str, token: &str, doc_id: &str) -> Result<Self, String> {
        let transport: Box<dyn Transport> = if let Ok(port) = address.parse::<u16>() {
            let socket = std::net::SocketAddr::from(([127, 0, 0, 1], port));
            let stream = std::net::TcpStream::connect_timeout(&socket, Duration::from_secs(3))
                .map_err(|e| e.to_string())?;
            stream
                .set_read_timeout(Some(Duration::from_secs(65)))
                .map_err(|e| e.to_string())?;
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .map_err(|e| e.to_string())?;
            Box::new(stream)
        } else {
            #[cfg(unix)]
            {
                let stream =
                    std::os::unix::net::UnixStream::connect(address).map_err(|e| e.to_string())?;
                stream
                    .set_read_timeout(Some(Duration::from_secs(65)))
                    .map_err(|e| e.to_string())?;
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .map_err(|e| e.to_string())?;
                Box::new(stream)
            }
            #[cfg(not(unix))]
            {
                return Err("Use the local TCP port on this platform".into());
            }
        };
        let mut remote = Self {
            transport,
            serial: 0,
            doc_id: doc_id.into(),
            undo: None,
            redo: None,
            generation: u64::MAX,
        };
        remote.call("server.auth", json!({"token":token}))?;
        let capabilities = remote.call("server.capabilities", json!({}))?;
        if !capabilities["features"]["methods"]
            .as_array()
            .is_some_and(|methods| methods.iter().any(|m| m == "document.snapshot"))
        {
            return Err("The server needs Planyrith 0.8 or newer for GUI attachment".into());
        }
        if doc_id.is_empty() {
            let opened = remote.call("session.new", json!({}))?;
            remote.doc_id = opened["doc_id"]
                .as_str()
                .ok_or("Missing remote document id")?
                .into();
        }
        Ok(remote)
    }
    pub fn call(&mut self, method: &str, mut params: Value) -> Result<Value, String> {
        self.serial += 1;
        if !self.doc_id.is_empty() {
            params["doc_id"] = json!(self.doc_id);
        }
        let bytes = serde_json::to_vec(
            &json!({"jsonrpc":"2.0","id":self.serial,"method":method,"params":params}),
        )
        .map_err(|e| e.to_string())?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err("RPC request exceeds 64 MiB".into());
        }
        self.transport
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .and_then(|_| self.transport.write_all(&bytes))
            .and_then(|_| self.transport.flush())
            .map_err(|e| e.to_string())?;
        loop {
            let mut header = [0; 4];
            self.transport
                .read_exact(&mut header)
                .map_err(|e| e.to_string())?;
            let length = u32::from_le_bytes(header) as usize;
            if length > 64 * 1024 * 1024 {
                return Err("RPC response exceeds 64 MiB".into());
            }
            let mut bytes = vec![0; length];
            self.transport
                .read_exact(&mut bytes)
                .map_err(|e| e.to_string())?;
            let reply: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if reply.get("id").is_none() {
                continue;
            }
            if reply["id"] != self.serial {
                return Err("Unexpected RPC response id".into());
            }
            if let Some(error) = reply.get("error") {
                return Err(error["message"]
                    .as_str()
                    .unwrap_or("Remote command failed")
                    .into());
            }
            return reply
                .get("result")
                .cloned()
                .ok_or_else(|| "RPC response has no result".into());
        }
    }
    pub fn snapshot(&mut self) -> Result<Snapshot, String> {
        let snapshot: Snapshot = serde_json::from_value(self.call("document.snapshot", json!({}))?)
            .map_err(|e| e.to_string())?;
        self.undo = snapshot.undo.clone();
        self.redo = snapshot.redo.clone();
        Ok(snapshot)
    }
}
