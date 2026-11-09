//! Implements docs/08-cli-specification.md §3.3 (joining from the browser):
//! `planyrith serve --web <port|address>` answers browsers on one port. A
//! WebSocket on `/rpc` carries exactly the bytes a socket client exchanges:
//! length-prefixed frames inside the encrypted handshake and messages of
//! §3.2, always (a browser cannot tell how the server listens). Any other
//! `GET` serves the web build from `--web-root`.
//!
//! Each WebSocket gets a thread that owns it, moving binary messages to and
//! from a [`WsPipe`], which the server reads and writes like a socket.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tungstenite::{Message, WebSocket};

/// The longest request head read (bytes).
const MAX_HEAD: usize = 16 * 1024;

/// One WebSocket connection as a byte stream: what is written is sent as
/// one binary message per flush; what is read are the received messages'
/// bytes in order. End of stream when the browser leaves.
pub(crate) struct WsPipe {
    incoming: Arc<Mutex<Receiver<Vec<u8>>>>,
    buf: Vec<u8>,
    at: usize,
    outgoing: Sender<Vec<u8>>,
    out: Vec<u8>,
}

impl WsPipe {
    /// Another handle on the same connection (one reads, one writes).
    pub(crate) fn duplicate(&self) -> WsPipe {
        WsPipe {
            incoming: Arc::clone(&self.incoming),
            buf: Vec::new(),
            at: 0,
            outgoing: self.outgoing.clone(),
            out: Vec::new(),
        }
    }
}

impl Read for WsPipe {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at == self.buf.len() {
            let next = self
                .incoming
                .lock()
                .map_err(|_| io::Error::other("poisoned"))?
                .recv();
            match next {
                Ok(bytes) => {
                    self.buf = bytes;
                    self.at = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl Write for WsPipe {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.out.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.out.is_empty() {
            self.outgoing
                .send(std::mem::take(&mut self.out))
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the browser left"))?;
        }
        Ok(())
    }
}

/// Binds `address` and hands every WebSocket on `/rpc` to `accepted` as a
/// pipe; other requests get the files of `root`. Returns the bound address.
pub(crate) fn listen(
    address: SocketAddr,
    root: Option<PathBuf>,
    accepted: impl Fn(WsPipe) -> bool + Send + Sync + 'static,
) -> io::Result<SocketAddr> {
    let listener = TcpListener::bind(address)?;
    let bound = listener.local_addr()?;
    let accepted = Arc::new(accepted);
    let root = Arc::new(root);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let accepted = Arc::clone(&accepted);
            let root = Arc::clone(&root);
            std::thread::spawn(move || {
                let _ = handle(stream, root.as_deref(), accepted.as_ref());
            });
        }
    });
    Ok(bound)
}

/// The request head (through the empty line), read without consuming it.
fn peek_head(stream: &TcpStream) -> io::Result<String> {
    let mut buf = vec![0u8; MAX_HEAD];
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    loop {
        let n = stream.peek(&mut buf)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if let Some(end) = buf[..n].windows(4).position(|w| w == b"\r\n\r\n") {
            return Ok(String::from_utf8_lossy(&buf[..end + 4]).into_owned());
        }
        if n == buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head too long",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn handle(
    mut stream: TcpStream,
    root: Option<&Path>,
    accepted: &(dyn Fn(WsPipe) -> bool + Send + Sync),
) -> io::Result<()> {
    let head = peek_head(&stream)?;
    let mut lines = head.lines();
    let request = lines.next().unwrap_or_default().to_string();
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or("/");
    let path = target.split(['?', '#']).next().unwrap_or("/");
    let upgrade = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("upgrade:") && l.contains("websocket")
    });
    if upgrade && path == "/rpc" {
        let ws = tungstenite::accept(stream).map_err(io::Error::other)?;
        let (to_server, incoming) = std::sync::mpsc::channel();
        let (outgoing, from_server) = std::sync::mpsc::channel();
        let pipe = WsPipe {
            incoming: Arc::new(Mutex::new(incoming)),
            buf: Vec::new(),
            at: 0,
            outgoing,
            out: Vec::new(),
        };
        if accepted(pipe) {
            pump(ws, &to_server, &from_server);
        }
        return Ok(());
    }
    // Consume the head, then answer.
    let mut head_bytes = vec![0u8; head.len()];
    stream.read_exact(&mut head_bytes)?;
    if method != "GET" && method != "HEAD" {
        return respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"GET only\n",
            method,
        );
    }
    match file(root, path) {
        Some((body, kind)) => respond(&mut stream, "200 OK", kind, &body, method),
        None if root.is_none() && path == "/" => respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            NO_WEB_BUILD.as_bytes(),
            method,
        ),
        None => respond(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"not found\n",
            method,
        ),
    }
}

const NO_WEB_BUILD: &str = "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Planyrith server</title></head>\
<body><p>This Planyrith server takes browsers on <code>/rpc</code>, but has no web build to \
serve: start it with <code>--web-root &lt;directory&gt;</code> (the output of \
<code>tools/build-web.sh</code>).</p></body></html>\n";

/// Moves messages between the WebSocket and the server until either side
/// ends.
fn pump(
    mut ws: WebSocket<TcpStream>,
    to_server: &Sender<Vec<u8>>,
    from_server: &Receiver<Vec<u8>>,
) {
    // Short reads leave time to send what the server wrote.
    if ws
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(15)))
        .is_err()
    {
        return;
    }
    loop {
        match ws.read() {
            Ok(Message::Binary(bytes)) => {
                if to_server.send(bytes.to_vec()).is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
        loop {
            match from_server.try_recv() {
                Ok(bytes) => {
                    if ws.send(Message::binary(bytes)).is_err() {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return;
                }
            }
        }
    }
}

/// A file of the web build: its bytes and media type. Paths stay inside
/// `root`.
fn file(root: Option<&Path>, path: &str) -> Option<(Vec<u8>, &'static str)> {
    let root = root?;
    let relative = path.trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    let relative = Path::new(relative);
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    let full = root.join(relative);
    let body = std::fs::read(&full).ok()?;
    let kind = match full
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "wasm" => "application/wasm",
        "json" | "webmanifest" => "application/json",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "br" => "application/octet-stream",
        _ => "application/octet-stream",
    };
    Some((body, kind))
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    kind: &str,
    body: &[u8],
    method: &str,
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Cache-Control: no-cache\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if method != "HEAD" {
        stream.write_all(body)?;
    }
    stream.flush()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn files_stay_inside_the_root() {
        let dir = std::env::temp_dir().join(format!("planyrith-web-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<p>hi</p>").unwrap();
        std::fs::write(dir.join("app.wasm"), [0u8, 97, 115, 109]).unwrap();
        assert_eq!(file(Some(&dir), "/").unwrap().1, "text/html; charset=utf-8");
        assert_eq!(file(Some(&dir), "/app.wasm").unwrap().1, "application/wasm");
        assert!(file(Some(&dir), "/../secret").is_none());
        assert!(file(Some(&dir), "/missing.js").is_none());
        assert!(file(None, "/").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
