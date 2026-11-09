//! Implements docs/01 §1 and docs/08 §3: native GUI attachment to `serve`.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::time::Duration;

trait Transport: Read + Write {}
impl<T: Read + Write> Transport for T {}

pub(crate) use crate::link::{Snapshot, Status};

/// One local authenticated framed JSON-RPC connection.
pub(crate) struct Remote {
    transport: Box<dyn Transport>,
    serial: u64,
    doc_id: String,
    pub undo: Option<String>,
    pub redo: Option<String>,
    pub generation: u64,
    /// Undo and redo take back one's own changes (a joined session,
    /// docs/08 §3.1), not the last change whoever made it.
    pub own_history: bool,
}
impl Remote {
    pub fn connect(address: &str, token: &str, doc_id: &str) -> Result<Self, String> {
        // A port (this machine), `host:port` (another one, docs/08 §3.1) or
        // a Unix socket path.
        let network = if let Ok(port) = address.parse::<u16>() {
            Some(std::net::SocketAddr::from(([127, 0, 0, 1], port)))
        } else if !address.starts_with('/') && address.contains(':') {
            use std::net::ToSocketAddrs;
            Some(
                address
                    .to_socket_addrs()
                    .map_err(|e| e.to_string())?
                    .next()
                    .ok_or_else(|| format!("{address} has no address"))?,
            )
        } else {
            None
        };
        let transport: Box<dyn Transport> = if let Some(socket) = network {
            let stream = std::net::TcpStream::connect_timeout(&socket, Duration::from_secs(5))
                .map_err(|e| e.to_string())?;
            let _ = stream.set_nodelay(true);
            stream
                .set_read_timeout(Some(Duration::from_secs(65)))
                .map_err(|e| e.to_string())?;
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .map_err(|e| e.to_string())?;
            if address.parse::<u16>().is_ok() {
                Box::new(stream)
            } else {
                // An address (`host:port`) means the server takes clients
                // from the network: the connection is encrypted with the
                // token (docs/08 §3.2).
                Box::new(
                    planyrith_core::transport::SecureStream::connect(stream, token)
                        .map_err(|e| e.to_string())?,
                )
            }
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
                return Err(crate::i18n::t("Use the local TCP port on this platform"));
            }
        };
        let mut remote = Self {
            transport,
            serial: 0,
            doc_id: doc_id.into(),
            undo: None,
            redo: None,
            generation: u64::MAX,
            own_history: false,
        };
        remote.call("server.auth", json!({"token":token}))?;
        let capabilities = remote.call("server.capabilities", json!({}))?;
        if !capabilities["features"]["methods"]
            .as_array()
            .is_some_and(|methods| methods.iter().any(|m| m == "document.snapshot"))
        {
            return Err(crate::i18n::t(
                "The server needs Planyrith 0.8 or newer for GUI attachment",
            ));
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
    /// The document's generation, history labels and the people working on
    /// it (`session.status`, docs/08 §3.1).
    pub fn status(&mut self) -> Result<Status, String> {
        let status: Status = serde_json::from_value(self.call("session.status", json!({}))?)
            .map_err(|e| e.to_string())?;
        let labels = match (&status.mine, self.own_history) {
            (Some(mine), true) => (mine.undo.clone(), mine.redo.clone()),
            _ => (status.undo.clone(), status.redo.clone()),
        };
        (self.undo, self.redo) = labels;
        Ok(status)
    }

    /// The name the others see.
    pub fn set_name(&mut self, name: &str) -> Result<(), String> {
        self.call("presence.set", json!({ "name": name }))
            .map(|_| ())
    }

    pub fn snapshot(&mut self) -> Result<Snapshot, String> {
        let snapshot: Snapshot = serde_json::from_value(self.call("document.snapshot", json!({}))?)
            .map_err(|e| e.to_string())?;
        if !self.own_history {
            self.undo = snapshot.undo.clone();
            self.redo = snapshot.redo.clone();
        }
        Ok(snapshot)
    }
}
