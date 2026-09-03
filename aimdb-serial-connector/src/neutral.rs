//! The serial connector reduced to framing: [`CobsFramer`] plus core's
//! `FramedConnection` serve both runtimes.
//!
//! The byte sources come from the adapters — `TokioByteStream` and
//! `EmbassyUart` — so this crate contributes only the framer and names no
//! socket or UART type of its own.

use aimdb_core::session::Framer;
use alloc::vec::Vec;

use crate::framing::{encode_frame, FrameAccumulator};

/// Per-`read` chunk, matching the UART ring size.
pub const READ_CHUNK: usize = 64;
/// Per-`write_all` chunk: some HAL `BufferedUart::write` rejects a single write
/// larger than its TX ring.
pub const WRITE_CHUNK: usize = 64;

/// COBS framing against core's [`Framer`], so one framer serves both runtimes.
///
/// `encode` COBS-encodes a frame and appends the `0x00` sentinel; the
/// accumulator yields one frame per sentinel, skipping a malformed run (COBS is
/// self-synchronizing).
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
        // `FrameError` collapses to `()`: the connection only distinguishes
        // "got a frame" from "skip and resync".
        self.acc.next_frame().map(|r| r.map_err(|_| ()))
    }
}

/// A framed connection over the Embassy adapter's UART halves.
#[cfg(feature = "embassy-runtime")]
pub type EmbassyFramed<Rd, Wr> = aimdb_core::session::FramedConnection<
    aimdb_embassy_adapter::net::EmbassyUart<Rd, Wr>,
    CobsFramer,
    READ_CHUNK,
    WRITE_CHUNK,
>;

/// A framed connection over any Tokio byte source — a `tokio_serial::SerialStream`
/// in production, a `tokio::io::duplex()` pipe in tests.
#[cfg(feature = "tokio-runtime")]
pub type TokioFramed<S> = aimdb_core::session::FramedConnection<
    aimdb_tokio_adapter::net::TokioByteStream<S>,
    CobsFramer,
    READ_CHUNK,
    WRITE_CHUNK,
>;

/// The same framer and the same core connection over the Embassy UART, boxed as
/// the runner takes it.
///
/// Type-checking this on `thumbv7em` is what "one connector module, no runtime
/// `cfg` on the code path" means concretely: if the two paths diverge, the
/// embedded check fails here rather than in an example.
#[cfg(feature = "embassy-runtime")]
#[allow(dead_code)]
fn _same_framed_connection_serves_the_uart<Rd, Wr>(rx: Rd, tx: Wr)
where
    Rd: embedded_io_async::Read + Send + 'static,
    Wr: embedded_io_async::Write + Send + 'static,
{
    use aimdb_core::session::Connection;
    use aimdb_embassy_adapter::net::EmbassyUart;
    use alloc::boxed::Box;

    let conn: EmbassyFramed<Rd, Wr> =
        EmbassyFramed::new(EmbassyUart::new(rx, tx), CobsFramer::new());
    let _boxed: Box<dyn Connection> = Box::new(conn);
}
