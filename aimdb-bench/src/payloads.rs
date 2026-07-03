//! Canonical workload payloads and deterministic message factories.
//!
//! `no_std`-clean so both the host `aimdb-bench` suites and
//! the `no_std` `examples/embassy-bench-stm32h5` B3 rig share one definition
//! instead of the stm32h5 example hand-forking its own copy "kept in sync by
//! hand." Three profiles match the three buffer types:
//!
//! | Profile       | Buffer type    | Payload |
//! |---------------|----------------|---------|
//! | **Telemetry** | `SpmcRing`     | small   |
//! | **State**     | `SingleLatest` | medium  |
//! | **Command**   | `Mailbox`      | small   |

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
