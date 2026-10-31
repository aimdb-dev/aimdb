//! Embassy Runtime Adapter for AimDB
//!
//! Implements [`RuntimeOps`](aimdb_core::RuntimeOps) — the one trait a runtime
//! adapter provides — on top of `embassy_time` and `defmt`.

#[cfg(feature = "tracing")]
use tracing::debug;

/// Embassy runtime adapter for async task execution in embedded systems.
///
/// A unit type: the runtime travels as `Arc<dyn RuntimeOps>` and network
/// connectors take the `embassy_net::Stack` at construction (wrapped in
/// `NetStack`, feature `connectors`), so the adapter carries no state
/// and no `unsafe`. All futures are driven by the `AimDbRunner` returned from
/// `AimDbBuilder::build()`, which is awaited inside the Embassy main task.
///
/// # Example
/// ```rust,no_run
/// # #[cfg(not(feature = "std"))]
/// # {
/// use aimdb_embassy_adapter::EmbassyAdapter;
///
/// let adapter = EmbassyAdapter::new();
/// # }
/// ```
#[derive(Clone, Debug, Default)]
pub struct EmbassyAdapter;

impl EmbassyAdapter {
    /// Creates a new EmbassyAdapter. Infallible — the adapter is a stateless
    /// unit type.
    pub fn new() -> Self {
        #[cfg(feature = "tracing")]
        debug!("Creating EmbassyAdapter");

        Self
    }

    /// Anchors wall-clock time so [`InstanceClock::unix_time`](aimdb_core::InstanceClock::unix_time)
    /// can derive absolute timestamps from Embassy's monotonic uptime.
    ///
    /// Embassy has no real-time clock, so `unix_time` returns `None` until this is
    /// called — typically once, after the device learns the real time (NTP / GPS /
    /// a host handshake). `now_unix_secs` is the current Unix time in **seconds**;
    /// the anchor is at second granularity (sub-second precision then comes from
    /// uptime). The anchor is process-global (shared by all adapter clones).
    #[cfg(feature = "embassy-time")]
    pub fn set_unix_time(now_unix_secs: u64) {
        use core::sync::atomic::Ordering;
        let uptime_secs = embassy_time::Instant::now().as_secs();
        let boot = now_unix_secs.saturating_sub(uptime_secs) as u32;
        BOOT_UNIX_SECS.store(boot, Ordering::Relaxed);
    }
}

/// Wall-clock anchor for [`InstanceClock::unix_time`](aimdb_core::InstanceClock::unix_time):
/// Unix **seconds** at Embassy uptime 0 (boot). `0` = unset (Embassy has no RTC),
/// so `unix_time` returns `None` until [`EmbassyAdapter::set_unix_time`] is called.
/// A `u32` keeps the anchor in a natively-atomic word on Cortex-M (no
/// `portable-atomic` / critical-section needed); sub-second precision comes from
/// uptime.
#[cfg(feature = "embassy-time")]
static BOOT_UNIX_SECS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The instance clock (design 058 §3.2): `embassy_time::Instant` is already
/// one global clock, so this only moves the origin from boot to `build()`.
#[cfg(feature = "embassy-time")]
struct EmbassyClock {
    origin: embassy_time::Instant,
}

#[cfg(feature = "embassy-time")]
impl aimdb_core::InstanceClock for EmbassyClock {
    fn now(&self) -> aimdb_core::Stamp {
        let micros = embassy_time::Instant::now()
            .as_micros()
            .saturating_sub(self.origin.as_micros());
        aimdb_core::Stamp::from_nanos(micros.saturating_mul(1_000))
    }

    /// Wall-clock time, derived from the [`set_unix_time`](EmbassyAdapter::set_unix_time)
    /// anchor plus monotonic uptime, independent of the clock's origin. `None`
    /// until the anchor is set (Embassy has no RTC). Sub-second precision is
    /// taken from uptime, so it carries a sub-second phase offset from the true
    /// wall clock — fine for timestamps.
    fn unix_time(&self) -> Option<(u64, u32)> {
        use core::sync::atomic::Ordering;
        let boot = BOOT_UNIX_SECS.load(Ordering::Relaxed);
        if boot == 0 {
            return None;
        }
        let now = embassy_time::Instant::now();
        let secs = boot as u64 + now.as_secs();
        let sub_nanos = ((now.as_micros() % 1_000_000) as u32).saturating_mul(1_000);
        Some((secs, sub_nanos))
    }
}

#[cfg(feature = "embassy-time")]
impl aimdb_core::RuntimeOps for EmbassyAdapter {
    fn name(&self) -> &'static str {
        "embassy"
    }

    fn sleep(&self, d: core::time::Duration) -> aimdb_core::BoxFuture {
        let duration =
            embassy_time::Duration::from_micros(d.as_micros().min(u64::MAX as u128) as u64);
        alloc::boxed::Box::pin(embassy_time::Timer::after(duration))
    }

    fn log(&self, level: aimdb_core::LogLevel, msg: &str) {
        use aimdb_core::LogLevel;
        match level {
            LogLevel::Debug => defmt::debug!("{}", msg),
            LogLevel::Info => defmt::info!("{}", msg),
            LogLevel::Warn => defmt::warn!("{}", msg),
            LogLevel::Error => defmt::error!("{}", msg),
        }
    }

    fn new_clock(&self) -> alloc::sync::Arc<dyn aimdb_core::InstanceClock> {
        alloc::sync::Arc::new(EmbassyClock {
            origin: embassy_time::Instant::now(),
        })
    }
}

// Host-run contract test for the dyn-safe RuntimeOps surface. Gated on
// `embassy-sync` (not `embassy-runtime`) so the cortex-m
// executor never enters the host build; the defmt/time-driver stubs shared by
// the lib test binary live in `buffer.rs`'s test module. The stub clock is
// pinned at 0, so only `Duration::ZERO` sleeps complete — see
// `assert_runtime_ops_contract`'s docs.
#[cfg(all(test, feature = "embassy-sync", feature = "embassy-time"))]
mod runtime_ops_tests {
    use super::*;
    use aimdb_core::RuntimeOps;
    use alloc::sync::Arc;
    use futures::executor::block_on;

    #[test]
    fn runtime_ops_contract() {
        // `Arc<dyn RuntimeOps>` must be constructible from the adapter.
        let ops: Arc<dyn RuntimeOps> = Arc::new(EmbassyAdapter::new());
        block_on(aimdb_core::executor::test_support::assert_runtime_ops_contract(ops.as_ref()));
    }
}
