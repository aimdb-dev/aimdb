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
