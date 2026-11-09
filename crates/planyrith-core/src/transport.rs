//! Implements docs/08 §3.2 (encrypted connections): a byte stream between a
//! client and `planyrith serve`, encrypted and authenticated with the Noise
//! protocol (`Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s`). The server's token is
//! the pre-shared key (through a key derivation), so only someone who knows
//! it completes the handshake, and each connection gets fresh keys.
//!
//! Every message on the wire is a big-endian `u16` length and that many
//! bytes: two handshake messages, then encrypted chunks of at most
//! [`MAX_PLAIN`] plaintext bytes. The streams work on any `Read`/`Write`
//! (they never open sockets themselves).

use std::io::{self, Read, Write};
use std::sync::Arc;

use snow::StatelessTransportState;

/// The Noise protocol used.
pub const PATTERN: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";
/// Largest plaintext in one encrypted message (a Noise message is at most
/// 65535 bytes, 16 of them the tag).
pub const MAX_PLAIN: usize = 65535 - 16;

fn psk(token: &str) -> [u8; 32] {
    blake3::derive_key("planyrith serve token 2026-11-09 v1", token.as_bytes())
}

fn noise_error(e: snow::Error) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("the encrypted handshake failed (a wrong token?): {e}"),
    )
}

fn send(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let len = u16::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "message too long"))?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(bytes)?;
    w.flush()
}

fn receive(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len)?;
    let mut buf = vec![0u8; u16::from_be_bytes(len) as usize];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// The keys of one connection, shared by its reading and writing halves.
#[derive(Clone)]
pub struct Session {
    state: Arc<StatelessTransportState>,
}

impl Session {
    /// Runs the client's side of the handshake on `stream`.
    pub fn connect(stream: &mut (impl Read + Write), token: &str) -> io::Result<Session> {
        let params = PATTERN
            .parse()
            .map_err(|e: snow::Error| io::Error::other(e.to_string()))?;
        let mut hs = snow::Builder::new(params)
            .psk(0, &psk(token))
            .and_then(|b| b.build_initiator())
            .map_err(noise_error)?;
        let mut buf = vec![0u8; 1024];
        let n = hs.write_message(&[], &mut buf).map_err(noise_error)?;
        send(stream, &buf[..n])?;
        let reply = receive(stream)?;
        hs.read_message(&reply, &mut buf).map_err(noise_error)?;
        Ok(Session {
            state: Arc::new(hs.into_stateless_transport_mode().map_err(noise_error)?),
        })
    }

    /// Runs the server's side of the handshake on `stream`.
    pub fn accept(stream: &mut (impl Read + Write), token: &str) -> io::Result<Session> {
        let params = PATTERN
            .parse()
            .map_err(|e: snow::Error| io::Error::other(e.to_string()))?;
        let mut hs = snow::Builder::new(params)
            .psk(0, &psk(token))
            .and_then(|b| b.build_responder())
            .map_err(noise_error)?;
        let mut buf = vec![0u8; 1024];
        let hello = receive(stream)?;
        hs.read_message(&hello, &mut buf).map_err(noise_error)?;
        let n = hs.write_message(&[], &mut buf).map_err(noise_error)?;
        send(stream, &buf[..n])?;
        Ok(Session {
            state: Arc::new(hs.into_stateless_transport_mode().map_err(noise_error)?),
        })
    }

    /// The writing half over `inner`.
    pub fn writer<W: Write>(&self, inner: W) -> SecureWriter<W> {
        SecureWriter {
            inner,
            state: Arc::clone(&self.state),
            nonce: 0,
            buf: vec![0u8; MAX_PLAIN + 16],
        }
    }

    /// The reading half over `inner`.
    pub fn reader<R: Read>(&self, inner: R) -> SecureReader<R> {
        SecureReader {
            inner,
            state: Arc::clone(&self.state),
            nonce: 0,
            plain: Vec::new(),
            at: 0,
        }
    }
}

/// Encrypts what is written to it.
pub struct SecureWriter<W: Write> {
    inner: W,
    state: Arc<StatelessTransportState>,
    nonce: u64,
    buf: Vec<u8>,
}

impl<W: Write> Write for SecureWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let chunk = &data[..data.len().min(MAX_PLAIN)];
        if chunk.is_empty() {
            return Ok(0);
        }
        let n = self
            .state
            .write_message(self.nonce, chunk, &mut self.buf)
            .map_err(|e| io::Error::other(e.to_string()))?;
        self.nonce += 1;
        let len = (n as u16).to_be_bytes();
        self.inner.write_all(&len)?;
        self.inner.write_all(&self.buf[..n])?;
        Ok(chunk.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Decrypts what is read from it; a forged or replayed message is an error.
pub struct SecureReader<R: Read> {
    inner: R,
    state: Arc<StatelessTransportState>,
    nonce: u64,
    plain: Vec<u8>,
    at: usize,
}

impl<R: Read> Read for SecureReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.at == self.plain.len() {
            let sealed = match receive(&mut self.inner) {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(0),
                Err(e) => return Err(e),
            };
            self.plain.resize(sealed.len(), 0);
            let n = self
                .state
                .read_message(self.nonce, &sealed, &mut self.plain)
                .map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("an encrypted message did not verify: {e}"),
                    )
                })?;
            self.nonce += 1;
            self.plain.truncate(n);
            self.at = 0;
        }
        let n = out.len().min(self.plain.len() - self.at);
        out[..n].copy_from_slice(&self.plain[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// Both halves over one stream that can be read and written (a client's
/// socket, used from one thread).
pub struct SecureStream<S: Read + Write> {
    reader: SecureReader<Shared<S>>,
    writer: SecureWriter<Shared<S>>,
}

/// One stream shared by the two halves of a [`SecureStream`].
pub struct Shared<S>(Arc<std::sync::Mutex<S>>);

impl<S: Read> Read for Shared<S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .read(out)
    }
}

impl<S: Write> Write for Shared<S> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .flush()
    }
}

impl<S: Read + Write> SecureStream<S> {
    /// Connects as a client over `stream` with `token`.
    pub fn connect(mut stream: S, token: &str) -> io::Result<Self> {
        let session = Session::connect(&mut stream, token)?;
        let shared = Arc::new(std::sync::Mutex::new(stream));
        Ok(SecureStream {
            reader: session.reader(Shared(Arc::clone(&shared))),
            writer: session.writer(Shared(shared)),
        })
    }
}

impl<S: Read + Write> Read for SecureStream<S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.reader.read(out)
    }
}

impl<S: Read + Write> Write for SecureStream<S> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.writer.write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn messages_cross_encrypted_and_a_wrong_token_is_refused() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let session = Session::accept(&mut stream, "s3cret").unwrap();
            let mut reader = session.reader(stream.try_clone().unwrap());
            let mut writer = session.writer(stream);
            let mut got = vec![0u8; 200_000];
            reader.read_exact(&mut got).unwrap();
            writer.write_all(&got).unwrap();
            writer.flush().unwrap();
            // A second client with the wrong token fails its handshake.
        });
        let mut client =
            SecureStream::connect(TcpStream::connect(addr).unwrap(), "s3cret").unwrap();
        // Larger than one message: split and joined again.
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        client.write_all(&data).unwrap();
        client.flush().unwrap();
        let mut back = vec![0u8; data.len()];
        client.read_exact(&mut back).unwrap();
        assert_eq!(back, data);
        server.join().unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            Session::accept(&mut stream, "s3cret").is_err()
        });
        let client = SecureStream::connect(TcpStream::connect(addr).unwrap(), "wrong");
        let refused_by_server = server.join().unwrap();
        assert!(client.is_err() || refused_by_server);
    }
}
