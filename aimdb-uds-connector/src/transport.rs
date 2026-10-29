//! AimX UDS transport — a [`Connection`] over a Unix-domain socket with NDJSON
//! framing in the transport: one line == one logical frame.
//!
//! Both roles ride the same role-neutral [`UdsConnection`]: the dialing half
//! ([`UdsDialer`], driven by `run_client`) and the accepting half
//! ([`UdsListener`], driven by `serve`).

use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

use aimdb_core::session::{
    BoxFut, Connection, Dialer, Listener, PeerInfo, TransportError, TransportResult,
};

/// A framed bidirectional pipe over a Unix-domain socket. Framing lives in the
/// transport: [`recv`](Connection::recv) returns one newline-delimited frame
/// (newline stripped); [`send`](Connection::send) appends the newline.
pub struct UdsConnection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    /// Partial line, kept here so it survives a dropped `recv` future.
    pending: Vec<u8>,
    peer: PeerInfo,
}

impl UdsConnection {
    /// Wrap an already-connected [`UnixStream`] (used by both the dialer and the
    /// server-side listener).
    pub fn new(stream: UnixStream) -> Self {
        let (read_half, write_half) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer: write_half,
            pending: Vec::new(),
            peer: PeerInfo::default(),
        }
    }
}

impl Connection for UdsConnection {
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(async move {
            // `read_until` resumes after a drop; `read_line` would lose the line.
            match self.reader.read_until(b'\n', &mut self.pending).await {
                // EOF with nothing buffered — peer closed.
                Ok(_) if self.pending.is_empty() => Ok(None),
                Ok(_) => {
                    let mut line = core::mem::take(&mut self.pending);
                    // Strip the trailing '\n' (and a stray '\r' if present); the
                    // frame is the line content, the codec owns the rest.
                    while matches!(line.last(), Some(b'\n' | b'\r')) {
                        line.pop();
                    }
                    Ok(Some(line))
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
/// [`UdsConnection`]. Cheap to clone (just the path), so `run_client` can redial
/// on reconnect and the generic `SessionClientConnector` can hold it.
#[derive(Clone)]
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

/// The accepting (server) side: wraps an already-bound [`UnixListener`] and
/// yields a [`UdsConnection`] per accepted client. The dual of [`UdsDialer`];
/// `serve` drives it. Socket setup (remove-stale / `bind` / `set_permissions`)
/// happens once in [`UdsServer`](crate::UdsServer)'s bind step before the
/// listener is handed here.
pub struct UdsListener {
    inner: UnixListener,
}

impl UdsListener {
    /// Wrap an already-bound [`UnixListener`].
    pub fn new(inner: UnixListener) -> Self {
        Self { inner }
    }
}

impl Listener for UdsListener {
    fn accept(&mut self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async move {
            let (stream, _addr) = self.inner.accept().await.map_err(|_| TransportError::Io)?;
            Ok(Box::new(UdsConnection::new(stream)) as Box<dyn Connection>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A `recv` dropped mid-line must not lose the bytes it already read.
    #[tokio::test]
    async fn a_dropped_recv_keeps_the_partial_line() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let mut conn = UdsConnection::new(ours);

        theirs.write_all(b"{\"id\":1,").await.unwrap();
        let dropped = tokio::time::timeout(Duration::from_millis(50), conn.recv()).await;
        assert!(
            dropped.is_err(),
            "no full line yet, so recv must still be pending"
        );

        theirs.write_all(b"\"m\":\"ping\"}\r\n").await.unwrap();
        let frame = conn.recv().await.unwrap().unwrap();
        assert_eq!(frame, b"{\"id\":1,\"m\":\"ping\"}");
    }

    /// One frame per line; an unterminated last line is a frame, then EOF.
    #[tokio::test]
    async fn frames_split_on_newlines_until_eof() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let mut conn = UdsConnection::new(ours);

        theirs.write_all(b"a\nb\nc").await.unwrap();
        drop(theirs);
        assert_eq!(conn.recv().await.unwrap().unwrap(), b"a");
        assert_eq!(conn.recv().await.unwrap().unwrap(), b"b");
        assert_eq!(conn.recv().await.unwrap().unwrap(), b"c");
        assert_eq!(conn.recv().await.unwrap(), None);
    }
}
