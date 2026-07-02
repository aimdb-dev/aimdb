//! Canonical workload profiles — Tokio buffer constructors over the shared
//! [`crate::payloads`] (design 039 F12: payload types/factories moved there
//! so they're `no_std`-shareable with the stm32h5 B3 example).
//!
//! Three profiles match the three buffer types:
//!
//! | Profile       | Buffer type    | Tokio primitive           | Payload |
//! |---------------|----------------|---------------------------|---------|
//! | **Telemetry** | `SpmcRing`     | `broadcast`               | small   |
//! | **State**     | `SingleLatest` | `watch`                   | medium  |
//! | **Command**   | `Mailbox`      | `Mutex` slot + waker list | small   |
//!
//! Buffers are constructed from a `BufferCfg` via the `Buffer<T>` trait so
//! the bench code tests exactly the same code path that production uses.

use aimdb_core::buffer::{Buffer, BufferCfg};
use aimdb_tokio_adapter::TokioBuffer;

pub use crate::payloads::{
    command_msg, state_msg, telemetry_msg, CommandMsg, StateMsg, TelemetryMsg, BATCH_SIZE,
    WARMUP_ITERS,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Ring capacity for the Telemetry profile.
///
/// Must be ≥ `BATCH_SIZE` so B2 fan-out measurements never trigger
/// `BufferLagged` within a single iteration.
pub const TELEMETRY_CAPACITY: usize = 1024;

// ── Buffer constructors ───────────────────────────────────────────────────────

/// Build a `TokioBuffer<TelemetryMsg>` backed by `SpmcRing` (`broadcast`).
pub fn telemetry_buffer() -> TokioBuffer<TelemetryMsg> {
    TokioBuffer::new(&BufferCfg::SpmcRing {
        capacity: TELEMETRY_CAPACITY,
    })
}

/// Build a `TokioBuffer<StateMsg>` backed by `SingleLatest` (`watch`).
pub fn state_buffer() -> TokioBuffer<StateMsg> {
    TokioBuffer::new(&BufferCfg::SingleLatest)
}

/// Build a `TokioBuffer<CommandMsg>` backed by `Mailbox` (`Mutex` slot + waker list).
pub fn command_buffer() -> TokioBuffer<CommandMsg> {
    TokioBuffer::new(&BufferCfg::Mailbox)
}
