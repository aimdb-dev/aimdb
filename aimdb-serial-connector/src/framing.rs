//! COBS frame boundaries over an unframed serial byte stream.
//!
//! The AimX codec emits raw JSON bytes; **framing is the transport's job** (the
//! UDS transport delimits with `\n`; serial uses COBS + a `0x00` sentinel). COBS
//! (Consistent Overhead Byte Stuffing) rewrites a payload so it never contains a
//! `0x00`, then a single `0x00` marks the frame boundary — so a receiver that
//! joins mid-stream resynchronizes on the next sentinel. AimX JSON never contains
//! a raw `0x00`, so the encoding is overhead-minimal (one byte per ~254).
//!
//! This module is shared by both runtime halves and is pure `no_std + alloc`, so
//! the round-trip is unit-tested on the host without any transport.
//!
//! Two layers live here. [`encode_frame`] and [`FrameAccumulator`] are the COBS
//! codec itself, with no dependency on the session substrate. [`CobsFramer`]
//! below is that codec behind core's `Framer` trait, plus the
//! `FramedConnection` aliases it forms with each adapter's byte source — the
//! whole of what this crate contributes to a session, since the byte sources
//! come from the adapters and this crate names no socket or UART type of its
//! own. That half needs `aimdb_core::session`, so it is gated on the runtime
//! features that enable core's `connector-session`.

use alloc::vec::Vec;

/// A frame could not be recovered — line noise, a truncated frame, a mid-stream
/// join before the next sentinel, or an un-delimited run that overflowed the frame
/// cap. The transports skip it and resync on the next sentinel rather than tearing
/// down the session. Self-contained so the framing round-trip is testable without
/// the `connector-session` substrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameError;

/// Frame sentinel — the byte COBS guarantees never appears inside an encoded
/// frame, so it cleanly delimits one frame from the next.
pub const DELIM: u8 = 0x00;

/// Default cap on un-delimited buffered bytes ([`FrameAccumulator::new`]): once a
/// run with no sentinel exceeds this, it's treated as a desync and dropped rather
/// than buffered without bound (an OOM risk on a small embedded heap). Generous
/// versus any AimX control frame and an order of magnitude over typical embedded
/// frames; only ever approached by a pathological stream, since steady-state
/// buffering is a fraction of this.
pub const DEFAULT_MAX_FRAME: usize = 8 * 1024;

/// COBS-encode `payload` and append it (plus the trailing [`DELIM`]) to `out`.
///
/// The dual of [`FrameAccumulator::next_frame`]: `decode(strip_delim(encode(p))) == p`.
pub fn encode_frame(payload: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&cobs::encode_vec(payload));
    out.push(DELIM);
}

/// Reassembles whole COBS frames from arbitrarily-chunked reads.
///
/// A serial `read()` returns whatever bytes happen to be buffered — a partial
/// frame, exactly one frame, or several. Push raw bytes in with
/// [`push_bytes`](Self::push_bytes); pull each complete frame out with
/// [`next_frame`](Self::next_frame) until it returns `None` (need more bytes).
pub struct FrameAccumulator {
    buf: Vec<u8>,
    /// Set after an oversized (un-delimited) run is dropped: the bytes still
    /// arriving are that frame's tail, so skip up to and including the next
    /// sentinel before framing resumes.
    resyncing: bool,
    /// Cap on bytes buffered without a sentinel before the run is declared a desync
    /// and dropped — bounds memory on a stream that never delimits.
    max_frame: usize,
}

impl Default for FrameAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameAccumulator {
    /// A fresh, empty accumulator with the [`DEFAULT_MAX_FRAME`] cap.
    pub fn new() -> Self {
        Self::with_max_frame(DEFAULT_MAX_FRAME)
    }

    /// A fresh accumulator that drops any un-delimited run longer than `max_frame`
    /// (and resyncs on the next sentinel) instead of buffering it without bound.
    pub fn with_max_frame(max_frame: usize) -> Self {
        Self {
            buf: Vec::new(),
            resyncing: false,
            max_frame,
        }
    }

    /// Append freshly-read bytes to the pending buffer.
    pub fn push_bytes(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Pop the next complete frame, COBS-decoded.
    ///
    /// - `None` — no `DELIM` buffered yet; read more bytes and retry.
    /// - `Some(Ok(frame))` — one decoded payload (the `DELIM` is consumed).
    /// - `Some(Err(FrameError))` — either the delimited chunk was not valid COBS
    ///   (line noise / a desync), or an un-delimited run exceeded `max_frame`; the
    ///   offending bytes are dropped so the next call resynchronizes on the
    ///   following sentinel.
    pub fn next_frame(&mut self) -> Option<Result<Vec<u8>, FrameError>> {
        loop {
            // Resync after an overflow: the buffered bytes are the tail of an
            // abandoned oversized frame, so drop up to and including the next
            // sentinel before resuming (don't decode that tail as a frame).
            if self.resyncing {
                match self.buf.iter().position(|&b| b == DELIM) {
                    Some(pos) => {
                        self.buf.drain(..=pos);
                        self.resyncing = false;
                    }
                    // Still no boundary — the whole buffer is tail garbage. Drop it
                    // (releasing capacity) and wait for more bytes.
                    None => {
                        self.buf = Vec::new();
                        return None;
                    }
                }
            }

            let Some(pos) = self.buf.iter().position(|&b| b == DELIM) else {
                // No frame boundary buffered. Bound memory against a stream that
                // never delimits: once an un-delimited run exceeds `max_frame`,
                // treat it as a desync — drop it (one `FrameError`) and resync on
                // the next sentinel rather than buffering toward an OOM.
                if self.buf.len() > self.max_frame {
                    self.buf = Vec::new();
                    self.resyncing = true;
                    return Some(Err(FrameError));
                }
                return None;
            };
            // Take the chunk up to and including the sentinel; the encoded frame is
            // everything before it.
            let chunk: Vec<u8> = self.buf.drain(..=pos).collect();
            let encoded = &chunk[..pos];
            // A leading/duplicate sentinel yields an empty chunk — not a frame;
            // swallow it and look for the next one (resync).
            if encoded.is_empty() {
                continue;
            }
            return Some(cobs::decode_vec(encoded).map_err(|_| FrameError));
        }
    }
}

// ===========================================================================
// The codec above behind core's `Framer`.
//
// Gated on the runtime features: these items name `aimdb_core::session`, which
// core gates on `connector-session`, and both runtime features turn that on.
// ===========================================================================

/// Per-`read` chunk, matching the UART ring size.
#[cfg(any(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub const READ_CHUNK: usize = 64;
/// Per-`write_all` chunk: some HAL `BufferedUart::write` rejects a single write
/// larger than its TX ring.
#[cfg(any(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub const WRITE_CHUNK: usize = 64;

/// COBS framing against core's [`Framer`](aimdb_core::session::Framer), so one
/// framer serves both runtimes.
///
/// `encode` COBS-encodes a frame and appends the [`DELIM`] sentinel; the
/// accumulator yields one frame per sentinel, skipping a malformed run (COBS is
/// self-synchronizing).
#[cfg(any(feature = "tokio-runtime", feature = "embassy-runtime"))]
#[derive(Default)]
pub struct CobsFramer {
    acc: FrameAccumulator,
}

#[cfg(any(feature = "tokio-runtime", feature = "embassy-runtime"))]
impl CobsFramer {
    /// A fresh COBS framer.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(any(feature = "tokio-runtime", feature = "embassy-runtime"))]
impl aimdb_core::session::Framer for CobsFramer {
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
