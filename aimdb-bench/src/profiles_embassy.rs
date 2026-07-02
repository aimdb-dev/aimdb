//! Embassy buffer constructors for the host-driven B0/B1/B2 suites.
//!
//! Reuse the same payload types and message factories as the Tokio profiles
//! ([`crate::payloads`]) so both adapters are measured against identical
//! workloads; only the backend differs. These are [`EmbassyBuffer`]s on
//! embassy-sync primitives, driven on the host via
//! `futures::executor::block_on`.
//!
//! # Const-generic sizing
//!
//! Embassy buffers are sized at compile time
//! (`EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>`). The aliases below fix those
//! parameters per profile:
//!
//! | Profile   | Backend        | CAP | SUBS | PUBS | WATCH_N | Notes                       |
//! |-----------|----------------|-----|------|------|---------|-----------------------------|
//! | Telemetry | `SpmcRing`     | 16  | 4    | 1    | 1       | SUBS=4 covers 1→4 fan-out   |
//! | State     | `SingleLatest` | 1   | 1    | 1    | 4       | only WATCH_N is used        |
//! | Command   | `Mailbox`      | 1   | 1    | 1    | 1       | Channel capacity is fixed=1 |
//!
//! Lockstep push→recv keeps at most one message in flight, so `CAP=16` for
//! Telemetry never lags.
//!
//! # Eager SpmcRing subscriber (design 039 F8/F9)
//!
//! An `SpmcRing` reader registers its embassy `Subscriber` **eagerly, at
//! `subscribe()` time** — not lazily on first poll. Earlier versions of this
//! module required benches to call a `prime()` helper on each reader before
//! the first `push` to force registration early; that helper is gone, and no
//! priming step is needed anymore.

use aimdb_embassy_adapter::EmbassyBuffer;

use crate::payloads::{CommandMsg, StateMsg, TelemetryMsg};

/// SpmcRing capacity for the Telemetry profile (compile-time const generic).
pub const TELEMETRY_CAP: usize = 16;

/// Telemetry buffer: `SpmcRing` with room for the 1→4 fan-out (SUBS=4).
pub type TelemetryBuffer = EmbassyBuffer<TelemetryMsg, TELEMETRY_CAP, 4, 1, 1>;

/// State buffer: `SingleLatest` (`Watch`); only `WATCH_N` is meaningful.
pub type StateBuffer = EmbassyBuffer<StateMsg, 1, 1, 1, 4>;

/// Command buffer: `Mailbox` (single-slot `Channel`).
pub type CommandBuffer = EmbassyBuffer<CommandMsg, 1, 1, 1, 1>;

/// Build a Telemetry `SpmcRing` Embassy buffer.
pub fn telemetry_buffer() -> TelemetryBuffer {
    EmbassyBuffer::new_spmc()
}

/// Build a State `SingleLatest` Embassy buffer.
pub fn state_buffer() -> StateBuffer {
    EmbassyBuffer::new_watch()
}

/// Build a Command `Mailbox` Embassy buffer.
pub fn command_buffer() -> CommandBuffer {
    EmbassyBuffer::new_mailbox()
}
