//! Bounded length-prefix framing for TCP.
//!
//! One logical frame is:
//!
//! ```text
//! u32 big-endian payload length
//! payload bytes
//! ```
//!
//! The declared length is payload bytes only. Oversized frames are fatal because
//! length-prefix TCP has no delimiter that would let the receiver safely resync.

use aimdb_core::session::FrameFault;
use alloc::vec::Vec;

/// Number of bytes in the fixed frame header.
pub const HEADER_LEN: usize = 4;

/// Default maximum payload size.
pub const DEFAULT_MAX_FRAME: usize = 64 * 1024;

/// Framing failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Payload length exceeded the configured maximum.
    TooLarge,
    /// Header plus payload length overflowed the local pointer width.
    LengthOverflow,
}

/// Append one length-prefixed frame to `out`.
pub fn encode_frame(frame: &[u8], out: &mut Vec<u8>) -> Result<(), FrameError> {
    let len = u32::try_from(frame.len()).map_err(|_| FrameError::LengthOverflow)?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(frame);
    Ok(())
}

/// Reassembles length-prefixed frames from arbitrary byte chunks.
pub struct FrameAccumulator {
    buf: Vec<u8>,
    max_frame: usize,
}

impl Default for FrameAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameAccumulator {
    /// A fresh accumulator with [`DEFAULT_MAX_FRAME`] cap.
    pub fn new() -> Self {
        Self::with_max_frame(DEFAULT_MAX_FRAME)
    }

    /// A fresh accumulator with a caller-provided maximum payload size.
    pub fn with_max_frame(max_frame: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_frame,
        }
    }

    /// Append newly read bytes.
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Pop the next complete frame, if buffered.
    pub fn next_frame(&mut self) -> Option<Result<Vec<u8>, FrameError>> {
        if self.buf.len() < HEADER_LEN {
            return None;
        }

        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
        let len = len as usize;
        if len > self.max_frame {
            self.buf.clear();
            return Some(Err(FrameError::TooLarge));
        }

        let Some(total) = HEADER_LEN.checked_add(len) else {
            self.buf.clear();
            return Some(Err(FrameError::LengthOverflow));
        };
        if self.buf.len() < total {
            return None;
        }

        self.buf.drain(..HEADER_LEN);
        Some(Ok(self.buf.drain(..len).collect()))
    }
}

/// Length-prefix framing against core's [`Framer`](aimdb_core::session::Framer),
/// so one framer serves both runtimes.
///
/// Unlike a self-synchronizing format, a length prefix has no delimiter to
/// resync on, so a framing error is fatal: `next_frame` reports it once and the
/// accumulator is left empty rather than pretending the stream is still
/// aligned.
#[cfg(feature = "connector")]
pub struct LengthFramer {
    acc: FrameAccumulator,
    max_frame: usize,
}

#[cfg(feature = "connector")]
impl LengthFramer {
    /// A framer bounded by [`DEFAULT_MAX_FRAME`].
    pub fn new() -> Self {
        Self::with_max_frame(DEFAULT_MAX_FRAME)
    }

    /// A framer bounded by `max_frame` payload bytes.
    pub fn with_max_frame(max_frame: usize) -> Self {
        Self {
            acc: FrameAccumulator::with_max_frame(max_frame),
            max_frame,
        }
    }
}

#[cfg(feature = "connector")]
impl Default for LengthFramer {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds one [`LengthFramer`] per connection, bounded by `max_frame`.
///
/// A `fn() -> LengthFramer` is nameable but stateless, so it can only ever
/// produce [`DEFAULT_MAX_FRAME`]. Carrying the bound in a factory keeps the
/// framed type aliases nameable *and* lets a deployment choose the cap — on a
/// constrained target it is a memory bound, on an exposed port a limit on what
/// a peer can make the receiver buffer.
#[cfg(feature = "connector")]
#[derive(Debug, Clone, Copy)]
pub struct LengthFramers {
    max_frame: usize,
}

#[cfg(feature = "connector")]
impl LengthFramers {
    /// Framers bounded by `max_frame` payload bytes.
    pub fn new(max_frame: usize) -> Self {
        Self { max_frame }
    }
}

#[cfg(feature = "connector")]
impl Default for LengthFramers {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_FRAME)
    }
}

#[cfg(feature = "connector")]
impl aimdb_core::session::FramerFactory for LengthFramers {
    type Framer = LengthFramer;

    fn framer(&self) -> LengthFramer {
        LengthFramer::with_max_frame(self.max_frame)
    }
}

#[cfg(feature = "connector")]
impl aimdb_core::session::Framer for LengthFramer {
    fn encode(&self, frame: &[u8], out: &mut Vec<u8>) -> Result<(), FrameFault> {
        // An oversized frame is dropped whole rather than written half-encoded:
        // the peer would read a length prefix with no payload behind it and
        // desync permanently. The link itself is untouched, so the fault is
        // recoverable and the caller decides what to do with the connection.
        if frame.len() > self.max_frame {
            return Err(FrameFault::Recoverable);
        }
        encode_frame(frame, out).map_err(|_| FrameFault::Recoverable)
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        self.acc.push_bytes(bytes);
    }

    fn next_frame(&mut self) -> Option<Result<Vec<u8>, FrameFault>> {
        // A length prefix has no delimiter to resync on, so a bad header is
        // fatal: nothing downstream tells payload bytes from the next header.
        self.acc
            .next_frame()
            .map(|r| r.map_err(|_| FrameFault::Fatal))
    }
}
