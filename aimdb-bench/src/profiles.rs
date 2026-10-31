//! Canonical workload profiles — Tokio buffer constructors over the shared
//! [`crate::payloads`] (payload types/factories moved there
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
//! Buffers are constructed from a `BufferCfg` via the `Buffer<T>` trait and
//! hold `Stamped<T>`, as a record's buffer does, so the bench code tests
//! exactly the same code path and slot type that production uses.

use aimdb_core::buffer::{Buffer, BufferCfg};
use aimdb_core::Stamped;
use aimdb_tokio_adapter::TokioBuffer;

pub use crate::payloads::{
    command_msg, stamped, state_msg, telemetry_msg, CommandMsg, StateMsg, TelemetryMsg, BATCH_SIZE,
    WARMUP_ITERS,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Ring capacity for the Telemetry profile.
///
/// Must be ≥ `BATCH_SIZE` so B2 fan-out measurements never trigger
/// `BufferLagged` within a single iteration.
pub const TELEMETRY_CAPACITY: usize = 1024;

// ── Buffer constructors ───────────────────────────────────────────────────────

/// Build a `TokioBuffer<Stamped<TelemetryMsg>>` backed by `SpmcRing` (`broadcast`).
pub fn telemetry_buffer() -> TokioBuffer<Stamped<TelemetryMsg>> {
    TokioBuffer::new(&BufferCfg::SpmcRing {
        capacity: TELEMETRY_CAPACITY,
    })
}

/// Build a `TokioBuffer<Stamped<StateMsg>>` backed by `SingleLatest` (`watch`).
pub fn state_buffer() -> TokioBuffer<Stamped<StateMsg>> {
    TokioBuffer::new(&BufferCfg::SingleLatest)
}

/// Build a `TokioBuffer<Stamped<CommandMsg>>` backed by `Mailbox` (`Mutex` slot + waker list).
pub fn command_buffer() -> TokioBuffer<Stamped<CommandMsg>> {
    TokioBuffer::new(&BufferCfg::Mailbox)
}
