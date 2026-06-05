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

use alloc::vec::Vec;

/// A delimited chunk was not valid COBS — line noise, a truncated frame, or a
/// mid-stream join before the next sentinel. The transports map this to a
/// `TransportError`. Self-contained so the framing round-trip is testable without
/// the `connector-session` substrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameError;

/// Frame sentinel — the byte COBS guarantees never appears inside an encoded
/// frame, so it cleanly delimits one frame from the next.
pub const DELIM: u8 = 0x00;

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
#[derive(Default)]
pub struct FrameAccumulator {
    buf: Vec<u8>,
}

impl FrameAccumulator {
    /// A fresh, empty accumulator.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Append freshly-read bytes to the pending buffer.
    pub fn push_bytes(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Pop the next complete frame, COBS-decoded.
    ///
    /// - `None` — no `DELIM` buffered yet; read more bytes and retry.
    /// - `Some(Ok(frame))` — one decoded payload (the `DELIM` is consumed).
    /// - `Some(Err(FrameError))` — the delimited chunk was not valid COBS (line
    ///   noise / a desync); the chunk is consumed so the next call resynchronizes
    ///   on the following sentinel.
    pub fn next_frame(&mut self) -> Option<Result<Vec<u8>, FrameError>> {
        loop {
            let pos = self.buf.iter().position(|&b| b == DELIM)?;
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
