//! AimX UDS transport (Phase 3, std-only) — a [`Connection`] over a Unix-domain
//! socket with NDJSON framing in the transport: one line == one logical frame.
//!
//! Client-first scope: this ships the dialing half ([`UdsDialer`] +
//! [`UdsConnection`]) that the proactive `run_client` engine drives. The
//! accepting half (`UdsListener`) lands with the server port — the substrate is
//! role-neutral, so the same [`UdsConnection`] will serve both sides.

use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

use crate::session::{BoxFut, Connection, Dialer, PeerInfo, TransportError, TransportResult};

/// A framed bidirectional pipe over a Unix-domain socket. Framing lives in the
/// transport: [`recv`](Connection::recv) returns one newline-delimited frame
/// (newline stripped); [`send`](Connection::send) appends the newline.
pub struct UdsConnection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    peer: PeerInfo,
}

impl UdsConnection {
    /// Wrap an already-connected [`UnixStream`] (used by both the dialer here and
    /// the future server-side listener).
    pub fn new(stream: UnixStream) -> Self {
        let (read_half, write_half) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer: write_half,
            peer: PeerInfo::default(),
        }
    }
}

impl Connection for UdsConnection {
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(async move {
            let mut line = String::new();
            match self.reader.read_line(&mut line).await {
                Ok(0) => Ok(None), // EOF — peer closed
                Ok(_) => {
                    // Strip the trailing '\n' (and a stray '\r' if present); the
                    // frame is the line content, the codec owns the rest.
                    while matches!(line.as_bytes().last(), Some(b'\n' | b'\r')) {
                        line.pop();
                    }
                    Ok(Some(line.into_bytes()))
                }
                Err(_) => Err(TransportError::Io),
            }
        })
    }

    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        Box::pin(async move {
            self.writer
                .write_all(frame)
                .await
                .map_err(|_| TransportError::Closed)?;
            self.writer
                .write_all(b"\n")
                .await
                .map_err(|_| TransportError::Closed)?;
            self.writer
                .flush()
                .await
                .map_err(|_| TransportError::Closed)
        })
    }

    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}

/// The initiating (client) side: dials a Unix-domain socket and yields a
/// [`UdsConnection`]. Cheap to clone the path, so `run_client` can redial on
/// reconnect.
pub struct UdsDialer {
    socket_path: PathBuf,
}

impl UdsDialer {
    /// Dial the socket at `socket_path` on each [`connect`](Dialer::connect).
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }
}

impl Dialer for UdsDialer {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            let stream = UnixStream::connect(&self.socket_path)
                .await
                .map_err(|_| TransportError::Io)?;
            Ok(Box::new(UdsConnection::new(stream)) as Box<dyn Connection>)
        })
    }
}
