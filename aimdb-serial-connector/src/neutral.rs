//! Design 052 §3.3 / acceptance 3, on real code: the serial connector reduced
//! to **framing plus an open helper**, with no runtime module in the path.
//!
//! The verification raised one doubt about acceptance 3 ("no `tokio`
//! dependency except the `tokio-serial` open helper"): implementing
//! [`ByteStream`] over a `tokio_serial::SerialStream` seems to need either
//! `tokio`'s io traits or a dependency on `aimdb-tokio-adapter`, which this
//! crate's manifest deliberately avoids.
//!
//! It needs neither of those to be a problem. `tokio` is already an optional
//! dependency here for `io-util` alone, and one small generic impl over
//! `AsyncRead + AsyncWrite` covers `SerialStream` and the `tokio::io::duplex()`
//! pipe the tests use alike. No adapter dependency, no `tokio/net`, and the
//! same [`CobsFramer`] serves both runtimes through core's
//! [`FramedConnection`].

use aimdb_core::session::{ByteStream, Framer, TransportError, TransportResult};
use alloc::vec::Vec;

use crate::framing::{encode_frame, FrameAccumulator};

/// Per-`read` chunk, matching the Embassy half's UART ring size.
pub const READ_CHUNK: usize = 64;
/// Per-`write_all` chunk: some HAL `BufferedUart::write` rejects a single
/// write larger than its TX ring.
pub const WRITE_CHUNK: usize = 64;

/// COBS framing — the only serial-specific transport bit, now written against
/// **core's** [`Framer`] rather than the Embassy adapter's, so one framer
/// serves the Tokio and Embassy paths.
///
/// `encode` COBS-encodes a frame and appends the `0x00` sentinel; the
/// accumulator yields one frame per sentinel, skipping a malformed run (COBS
/// is self-synchronizing).
#[derive(Default)]
pub struct CobsFramer {
    acc: FrameAccumulator,
}

impl CobsFramer {
    /// A fresh COBS framer.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Framer for CobsFramer {
    fn encode(&self, frame: &[u8], out: &mut Vec<u8>) {
        encode_frame(frame, out);
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        self.acc.push_bytes(bytes);
    }

    fn next_frame(&mut self) -> Option<Result<Vec<u8>, ()>> {
        // The accumulator's `FrameError` collapses to `()`: the connection only
        // distinguishes "got a frame" from "skip and resync".
        self.acc.next_frame().map(|r| r.map_err(|_| ()))
    }
}

/// A [`ByteStream`] over any Tokio async byte stream — a
/// `tokio_serial::SerialStream` in production, a `tokio::io::duplex()` pipe in
/// tests.
///
/// The futures are plain `async fn`s, so the compiler proves them `Send` and
/// the `+ Send` core's trait declares costs nothing. This is what acceptance 3
/// actually requires: `tokio` for `io-util`, and no adapter dependency.
#[cfg(feature = "tokio-runtime")]
pub struct SerialByteStream<S>(pub S);

#[cfg(feature = "tokio-runtime")]
impl<S> ByteStream for SerialByteStream<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    async fn read(&mut self, buf: &mut [u8]) -> TransportResult<usize> {
        use tokio::io::AsyncReadExt;
        self.0.read(buf).await.map_err(|_| TransportError::Io)
    }

    async fn write_all(&mut self, buf: &[u8]) -> TransportResult<()> {
        use tokio::io::AsyncWriteExt;
        self.0
            .write_all(buf)
            .await
            .map_err(|_| TransportError::Closed)
    }

    async fn flush(&mut self) -> TransportResult<()> {
        use tokio::io::AsyncWriteExt;
        self.0.flush().await.map_err(|_| TransportError::Closed)
    }
}

/// A [`ByteStream`] over `embedded-io-async` halves — the Embassy UART.
///
/// Split halves rather than one stream, because that is what
/// `embassy_stm32::usart::BufferedUart::split()` yields. The adapter would
/// normally own this; it lives here to show the *same* [`CobsFramer`] and the
/// *same* `FramedConnection` serve both sides.
#[cfg(feature = "embassy-runtime")]
pub struct UartByteStream<Rd, Wr> {
    rx: Rd,
    tx: Wr,
}

#[cfg(feature = "embassy-runtime")]
impl<Rd, Wr> UartByteStream<Rd, Wr> {
    /// Join the split halves into one bidirectional stream.
    pub fn new(rx: Rd, tx: Wr) -> Self {
        Self { rx, tx }
    }
}

#[cfg(feature = "embassy-runtime")]
impl<Rd, Wr> ByteStream for UartByteStream<Rd, Wr>
where
    Rd: embedded_io_async::Read,
    Wr: embedded_io_async::Write,
{
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl core::future::Future<Output = TransportResult<usize>> + Send + 'a {
        aimdb_embassy_adapter::SendFutureWrapper(async move {
            self.rx.read(buf).await.map_err(|_| TransportError::Io)
        })
    }

    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl core::future::Future<Output = TransportResult<()>> + Send + 'a {
        aimdb_embassy_adapter::SendFutureWrapper(async move {
            self.tx
                .write_all(buf)
                .await
                .map_err(|_| TransportError::Closed)
        })
    }

    fn flush(&mut self) -> impl core::future::Future<Output = TransportResult<()>> + Send + '_ {
        aimdb_embassy_adapter::SendFutureWrapper(async move {
            self.tx.flush().await.map_err(|_| TransportError::Closed)
        })
    }
}

/// The Embassy dual of the Tokio type `tests/neutral_framed.rs` exercises:
/// the **same** [`CobsFramer`] and the **same** core `FramedConnection`, over
/// `embedded-io-async` UART halves instead of a Tokio stream.
///
/// Type-checking this is the point — it is what "one connector module, no
/// runtime `cfg` on the code path" means concretely. If the two ever diverge,
/// the thumbv7em check fails here rather than in an example.
#[cfg(feature = "embassy-runtime")]
#[allow(dead_code)]
fn _same_framed_connection_serves_the_uart<Rd, Wr>(rx: Rd, tx: Wr)
where
    Rd: embedded_io_async::Read + Send + 'static,
    Wr: embedded_io_async::Write + Send + 'static,
{
    use aimdb_core::session::{Connection, FramedConnection};
    use alloc::boxed::Box;

    let conn: FramedConnection<UartByteStream<Rd, Wr>, CobsFramer, READ_CHUNK, WRITE_CHUNK> =
        FramedConnection::new(UartByteStream::new(rx, tx), CobsFramer::new());
    // The `+ Send` bounds are what make this coercion legal for a `!Send`
    // socket wrapped by the adapter's newtype.
    let _boxed: Box<dyn Connection> = Box::new(conn);
}
