//! Canonical workload profiles and deterministic message factories.
//!
//! Three profiles match the three buffer types:
//!
//! | Profile      | Buffer type   | Tokio primitive     | Payload   |
//! |--------------|---------------|---------------------|-----------|
//! | **Telemetry**| `SpmcRing`    | `broadcast`         | small     |
//! | **State**    | `SingleLatest`| `watch`             | medium    |
//! | **Command**  | `Mailbox`     | `Mutex + Notify`    | small     |
//!
//! Buffers are constructed from a `BufferCfg` via the `Buffer<T>` trait so
//! the bench code tests exactly the same code path that production uses.

use aimdb_core::buffer::{Buffer, BufferCfg};
use aimdb_tokio_adapter::TokioBuffer;

// ── Payload types ─────────────────────────────────────────────────────────────

/// Small telemetry reading pushed at high frequency (Telemetry profile).
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryMsg {
    pub sensor_id: u32,
    pub value: f64,
    pub sequence: u64,
}

/// Medium state snapshot with several fields (State profile).
#[derive(Debug, Clone, PartialEq)]
pub struct StateMsg {
    pub device_id: u32,
    pub temperature: f64,
    pub humidity: f64,
    pub pressure: f64,
    pub sequence: u64,
}

/// Control command payload (Command / Mailbox profile).
#[derive(Debug, Clone, PartialEq)]
pub struct CommandMsg {
    pub command_id: u32,
    pub target: u32,
    pub value: f64,
    pub sequence: u64,
}

// ── Constants ─────────────────────────────────────────────────────────────────

/// Ring capacity for the Telemetry profile.
///
/// Must be ≥ `BATCH_SIZE` so B2 fan-out measurements never trigger
/// `BufferLagged` within a single iteration.
pub const TELEMETRY_CAPACITY: usize = 1024;

/// Number of messages in the B0 measured batch window.
pub const BATCH_SIZE: usize = 512;

/// Warmup iterations excluded from the B0 measurement window.
pub const WARMUP_ITERS: usize = 200;

// ── Deterministic message factories ──────────────────────────────────────────

/// Produce a deterministic `TelemetryMsg` for iteration `i`.
#[inline]
pub fn telemetry_msg(i: u64) -> TelemetryMsg {
    TelemetryMsg {
        sensor_id: (i % 16) as u32,
        value: i as f64 * 0.1,
        sequence: i,
    }
}

/// Produce a deterministic `StateMsg` for iteration `i`.
#[inline]
pub fn state_msg(i: u64) -> StateMsg {
    StateMsg {
        device_id: (i % 8) as u32,
        temperature: 20.0 + i as f64 * 0.01,
        humidity: 50.0 + i as f64 * 0.005,
        pressure: 1013.25 + i as f64 * 0.001,
        sequence: i,
    }
}

/// Produce a deterministic `CommandMsg` for iteration `i`.
#[inline]
pub fn command_msg(i: u64) -> CommandMsg {
    CommandMsg {
        command_id: (i % 256) as u32,
        target: (i % 4) as u32,
        value: i as f64,
        sequence: i,
    }
}

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

/// Build a `TokioBuffer<CommandMsg>` backed by `Mailbox` (`Mutex + Notify`).
pub fn command_buffer() -> TokioBuffer<CommandMsg> {
    TokioBuffer::new(&BufferCfg::Mailbox)
}
