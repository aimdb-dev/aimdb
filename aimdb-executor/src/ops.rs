//! Dyn-safe runtime capability surface
//!
//! [`RuntimeOps`] is the object-safe counterpart to the generic trait family
//! ([`RuntimeAdapter`](crate::RuntimeAdapter) + [`TimeOps`](crate::TimeOps) +
//! [`Logger`](crate::Logger)). Those traits use associated types and
//! `impl Future` returns, so a runtime can only be held as a type parameter
//! `R`. `RuntimeOps` flattens the same capabilities onto concrete types
//! (`u64` nanoseconds, `core::time::Duration`, a boxed sleep future) so the
//! runtime can be passed as a *value* — `Arc<dyn RuntimeOps>` — wherever
//! threading `R` through a signature is not worth it.
//!
//! Design notes (issue #130, design doc 034 §3.2/§3.3):
//!
//! - **Concrete time types instead of associated types.** Adapters convert
//!   internally; `aimdb-core`'s profiling clock already flattens instants to
//!   nanoseconds the same way, proving the shape works.
//! - **Boxed `sleep` future**: one allocation per call, consistent with the
//!   existing cost model — every service future is already boxed, and `alloc`
//!   is mandatory on all supported targets.
//! - **Per-adapter implementations are intentional.** A blanket impl over
//!   [`TimeOps`](crate::TimeOps) is impossible: `now_nanos` needs an epoch
//!   anchor, and `TimeOps::Instant` is opaque with no place to store one.
//!
//! This trait is groundwork: `aimdb-core` does not consume it yet (that is
//! the follow-up de-genericization, issue #131).

/// Heap-pinned, `Send`, `'static` future — the unit of work AimDB drives.
///
/// Canonical definition; `aimdb_core::BoxFuture` re-exports this alias.
pub type BoxFuture =
    core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = ()> + Send + 'static>>;

/// Log severity for [`RuntimeOps::log`].
///
/// Mirrors the four discrete [`Logger`](crate::Logger) methods so adapters
/// can forward to their existing logging backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Object-safe runtime capabilities: identity, time, sleep, and logging.
///
/// Implemented by every runtime adapter (`TokioAdapter`, `EmbassyAdapter`,
/// `WasmAdapter`) so a runtime can travel as `Arc<dyn RuntimeOps>` instead of
/// a generic parameter.
pub trait RuntimeOps: Send + Sync {
    /// Short adapter identity, e.g. `"tokio"` — matches
    /// [`RuntimeAdapter::runtime_name`](crate::RuntimeAdapter::runtime_name).
    fn name(&self) -> &'static str;

    /// Monotonic clock reading in nanoseconds from an arbitrary epoch.
    ///
    /// Only differences between two readings are meaningful. Never decreases;
    /// saturates at `u64::MAX` rather than wrapping.
    fn now_nanos(&self) -> u64;

    /// Wall-clock time as `(seconds, nanoseconds)` since the Unix epoch, if
    /// the runtime has a real-time clock.
    ///
    /// Returns `None` on platforms without a wall clock (e.g. a bare MCU with
    /// no RTC anchor) — same contract as
    /// [`TimeOps::unix_time`](crate::TimeOps::unix_time).
    fn unix_time(&self) -> Option<(u64, u32)>;

    /// Completes after at least `d` has elapsed.
    fn sleep(&self, d: core::time::Duration) -> BoxFuture;

    /// Emits `msg` at `level` through the adapter's logging backend.
    fn log(&self, level: LogLevel, msg: &str);
}
