//! Embassy Runtime Adapter for AimDB
//!
//! This module provides the Embassy-specific implementation of AimDB's runtime traits,
//! enabling async task execution in embedded environments using Embassy.

use aimdb_executor::RuntimeAdapter;

#[cfg(feature = "tracing")]
use tracing::debug;

/// Embassy runtime adapter for async task execution in embedded systems.
///
/// A unit type: the runtime travels as `Arc<dyn RuntimeOps>` (issue #131) and
/// network connectors take the `embassy_net::Stack` at construction (wrapped in
/// [`NetStack`](crate::connectors::NetStack)), so the adapter carries no state
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

    /// Anchors wall-clock time so [`TimeOps::unix_time`](aimdb_executor::TimeOps::unix_time)
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

// Trait implementations for the simplified core adapter interfaces

impl RuntimeAdapter for EmbassyAdapter {
    fn runtime_name() -> &'static str {
        "embassy"
    }
}

/// Wall-clock anchor for [`TimeOps::unix_time`](aimdb_executor::TimeOps::unix_time):
/// Unix **seconds** at Embassy uptime 0 (boot). `0` = unset (Embassy has no RTC),
/// so `unix_time` returns `None` until [`EmbassyAdapter::set_unix_time`] is called.
/// A `u32` keeps the anchor in a natively-atomic word on Cortex-M (no
/// `portable-atomic` / critical-section needed); sub-second precision comes from
/// uptime.
#[cfg(feature = "embassy-time")]
static BOOT_UNIX_SECS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

// Implement TimeOps trait for time operations
#[cfg(feature = "embassy-time")]
impl aimdb_executor::TimeOps for EmbassyAdapter {
    type Instant = embassy_time::Instant;
    type Duration = embassy_time::Duration;

    fn now(&self) -> Self::Instant {
        embassy_time::Instant::now()
    }

    fn duration_since(
        &self,
        later: Self::Instant,
        earlier: Self::Instant,
    ) -> Option<Self::Duration> {
        if later >= earlier {
            Some(later - earlier)
        } else {
            None
        }
    }

    fn millis(&self, millis: u64) -> Self::Duration {
        embassy_time::Duration::from_millis(millis)
    }

    fn secs(&self, secs: u64) -> Self::Duration {
        embassy_time::Duration::from_secs(secs)
    }

    fn micros(&self, micros: u64) -> Self::Duration {
        embassy_time::Duration::from_micros(micros)
    }

    fn sleep(&self, duration: Self::Duration) -> impl core::future::Future<Output = ()> + Send {
        embassy_time::Timer::after(duration)
    }

    fn duration_as_nanos(&self, duration: Self::Duration) -> u64 {
        // `embassy_time::Duration` resolution depends on the configured tick rate;
        // microsecond granularity is the portable lower bound.
        duration.as_micros().saturating_mul(1_000)
    }

    /// Wall-clock time, derived from the [`set_unix_time`](EmbassyAdapter::set_unix_time)
    /// anchor plus monotonic uptime. `None` until the anchor is set (Embassy has
    /// no RTC). Sub-second precision is taken from uptime, so it carries a
    /// sub-second phase offset from the true wall clock — fine for timestamps.
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
impl aimdb_executor::RuntimeOps for EmbassyAdapter {
    fn name(&self) -> &'static str {
        <Self as RuntimeAdapter>::runtime_name()
    }

    fn now_nanos(&self) -> u64 {
        // Boot-anchored monotonic uptime; microsecond granularity is the
        // portable lower bound (see `duration_as_nanos`).
        embassy_time::Instant::now()
            .as_micros()
            .saturating_mul(1_000)
    }

    fn unix_time(&self) -> Option<(u64, u32)> {
        <Self as aimdb_executor::TimeOps>::unix_time(self)
    }

    fn sleep(&self, d: core::time::Duration) -> aimdb_executor::BoxFuture {
        let duration =
            embassy_time::Duration::from_micros(d.as_micros().min(u64::MAX as u128) as u64);
        alloc::boxed::Box::pin(embassy_time::Timer::after(duration))
    }

    fn log(&self, level: aimdb_executor::LogLevel, msg: &str) {
        use aimdb_executor::{LogLevel, Logger};
        match level {
            LogLevel::Debug => Logger::debug(self, msg),
            LogLevel::Info => Logger::info(self, msg),
            LogLevel::Warn => Logger::warn(self, msg),
            LogLevel::Error => Logger::error(self, msg),
        }
    }
}

// Implement Logger trait
impl aimdb_executor::Logger for EmbassyAdapter {
    fn info(&self, message: &str) {
        defmt::info!("{}", message);
    }

    fn debug(&self, message: &str) {
        defmt::debug!("{}", message);
    }

    fn warn(&self, message: &str) {
        defmt::warn!("{}", message);
    }

    fn error(&self, message: &str) {
        defmt::error!("{}", message);
    }
}

// Runtime trait is auto-implemented when RuntimeAdapter + TimeOps + Logger are all implemented

// Host-run contract test for the dyn-safe RuntimeOps surface. Gated on
// `embassy-sync` (not `embassy-runtime`) so the cortex-m
// executor never enters the host build; the defmt/time-driver stubs shared by
// the lib test binary live in `buffer.rs`'s test module. The stub clock is
// pinned at 0, so only `Duration::ZERO` sleeps complete — see
// `assert_runtime_ops_contract`'s docs.
#[cfg(all(test, feature = "embassy-sync", feature = "embassy-time"))]
mod runtime_ops_tests {
    use super::*;
    use aimdb_executor::RuntimeOps;
    use alloc::sync::Arc;
    use futures::executor::block_on;

    #[test]
    fn runtime_ops_contract() {
        // `Arc<dyn RuntimeOps>` must be constructible from the adapter.
        let ops: Arc<dyn RuntimeOps> = Arc::new(EmbassyAdapter::new());
        block_on(aimdb_executor::test_support::assert_runtime_ops_contract(
            ops.as_ref(),
        ));
    }
}
