//! Embassy buffer constructors for the host-driven B0/B1/B2 suites.
//!
//! These reuse the same payload types and message factories as the Tokio
//! profiles ([`crate::profiles`]) so the two adapters are measured against
//! identical workloads. Only the buffer backend differs: here the buffers are
//! [`EmbassyBuffer`]s built on embassy-sync primitives, driven on the host via
//! `futures::executor::block_on`.
//!
//! # Const-generic sizing
//!
//! Unlike Tokio's runtime-sized buffers, Embassy buffers are sized at
//! compile time (`EmbassyBuffer<T, CAP, SUBS, PUBS, WATCH_N>`). The aliases
//! below fix those parameters per profile:
//!
//! | Profile   | Backend        | CAP | SUBS | PUBS | WATCH_N | Notes                       |
//! |-----------|----------------|-----|------|------|---------|-----------------------------|
//! | Telemetry | `SpmcRing`     | 16  | 4    | 1    | 1       | SUBS=4 covers 1→4 fan-out   |
//! | State     | `SingleLatest` | 1   | 1    | 1    | 4       | only WATCH_N is used        |
//! | Command   | `Mailbox`      | 1   | 1    | 1    | 1       | Channel capacity is fixed=1 |
//!
//! The lockstep push→recv loops in the benches keep at most one message in
//! flight, so `CAP=16` for Telemetry is far more than enough to avoid lagging.
//!
//! # Lazy SpmcRing subscriber (important)
//!
//! An [`EmbassyBuffer`] `SpmcRing` reader registers its underlying embassy
//! `Subscriber` **lazily, on its first poll** — not at `subscribe()` time. A
//! message published before that first poll is therefore missed, and a
//! subsequent `recv()` would block forever. Benches must call
//! [`prime`] on each reader *before* the first `push`, which forces subscriber
//! registration via `try_recv`. This is a no-op for Watch/Mailbox readers, so
//! it is safe (and clearer) to prime every reader uniformly.

use aimdb_core::buffer::Reader;
use aimdb_embassy_adapter::EmbassyBuffer;

use crate::profiles::{CommandMsg, StateMsg, TelemetryMsg};

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

/// Force lazy subscriber registration on an Embassy reader before the first
/// `push`.
///
/// For an `SpmcRing` reader this registers the embassy `Subscriber` at the
/// current queue position so it does not miss the first published message (see
/// the module docs). For Watch/Mailbox readers it is a harmless empty read.
/// Must be called *outside* the measured window — registration may allocate.
#[inline]
pub fn prime<T: Clone + Send>(reader: &mut Reader<T>) {
    // `Reader<T>` exposes `try_recv`; the only expected error here is
    // `BufferEmpty`, which we deliberately ignore — the point is the side
    // effect of creating the subscriber, not the (absent) value.
    let _ = reader.try_recv();
}
