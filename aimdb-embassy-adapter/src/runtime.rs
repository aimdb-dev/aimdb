//! Embassy Runtime Adapter for AimDB
//!
//! This module provides the Embassy-specific implementation of AimDB's runtime traits,
//! enabling async task execution in embedded environments using Embassy.

use aimdb_core::{DbError, DbResult};
use aimdb_executor::{ExecutorResult, RuntimeAdapter};

#[cfg(feature = "tracing")]
use tracing::debug;

#[cfg(feature = "embassy-net-support")]
use embassy_net::Stack;

/// Trait for accessing Embassy network stack
///
/// This trait provides connectors with access to the network stack for
/// creating TCP/UDP connections. It's implemented by EmbassyAdapter when
/// the `embassy-net-support` feature is enabled.
///
/// # Example
/// ```rust,ignore
/// use aimdb_embassy_adapter::EmbassyNetwork;
///
/// fn create_mqtt_connector<R: EmbassyNetwork>(runtime: &R) {
///     let network = runtime.network_stack();
///     // Use network to create TCP connection
/// }
/// ```
#[cfg(feature = "embassy-net-support")]
pub trait EmbassyNetwork {
    /// Get a reference to the Embassy network stack
    ///
    /// Returns a reference to the static network stack that can be used
    /// for creating network connections (TCP, UDP, etc.).
    fn network_stack(&self) -> &'static Stack<'static>;
}

/// Embassy runtime adapter for async task execution in embedded systems.
///
/// When the `embassy-net-support` feature is enabled it holds a `&'static Stack<'static>`
/// for connector use; otherwise it is effectively a unit type. All futures are
/// driven by the `AimDbRunner` returned from `AimDbBuilder::build()`, which is
/// awaited inside the Embassy main task.
///
/// # Safety
///
/// `embassy_net::Stack` contains a `RefCell` and is `!Sync`. Embassy executors
/// run cooperatively on a single core with no preemption or thread migration,
/// so `unsafe impl Send/Sync` is sound here. This is the only `unsafe` left in
/// the adapter after issue #88 removed the `Spawn` impl and task pool.
///
/// # Example
/// ```rust,no_run
/// # #[cfg(not(feature = "std"))]
/// # {
/// use aimdb_embassy_adapter::EmbassyAdapter;
/// use aimdb_core::RuntimeAdapter;
///
/// # async fn example() -> aimdb_core::DbResult<()> {
/// let adapter = EmbassyAdapter::new()?;
/// # Ok(())
/// # }
/// # }
/// ```
#[derive(Clone)]
pub struct EmbassyAdapter {
    #[cfg(feature = "embassy-net-support")]
    network: Option<&'static Stack<'static>>,
    #[cfg(not(feature = "embassy-net-support"))]
    _phantom: core::marker::PhantomData<()>,
}

// SAFETY: Embassy executors run cooperatively on a single core. The
// `embassy_net::Stack` (when present) is `!Sync` only because of its internal
// `RefCell`; in a single-threaded executor no concurrent access is possible.
unsafe impl Send for EmbassyAdapter {}
unsafe impl Sync for EmbassyAdapter {}

impl core::fmt::Debug for EmbassyAdapter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut s = f.debug_struct("EmbassyAdapter");
        #[cfg(feature = "embassy-net-support")]
        s.field("network", &self.network.is_some());
        #[cfg(not(feature = "embassy-net-support"))]
        s.field("_phantom", &"no features enabled");
        s.finish()
    }
}

impl EmbassyAdapter {
    /// Creates a new EmbassyAdapter without a network stack.
    ///
    /// # Returns
    /// `Ok(EmbassyAdapter)` — always succeeds.
    pub fn new() -> ExecutorResult<Self> {
        #[cfg(feature = "tracing")]
        debug!("Creating EmbassyAdapter (no network)");

        Ok(Self {
            #[cfg(feature = "embassy-net-support")]
            network: None,
            #[cfg(not(feature = "embassy-net-support"))]
            _phantom: core::marker::PhantomData,
        })
    }

    /// Creates a new EmbassyAdapter returning `DbResult` for backward compatibility.
    pub fn new_db_result() -> DbResult<Self> {
        Self::new().map_err(DbError::from)
    }

    /// Creates a new EmbassyAdapter with a network stack
    #[cfg(feature = "embassy-net-support")]
    pub fn new_with_network(network: &'static Stack<'static>) -> Self {
        #[cfg(feature = "tracing")]
        debug!("Creating EmbassyAdapter with network");

        Self {
            network: Some(network),
        }
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

impl Default for EmbassyAdapter {
    fn default() -> Self {
        Self::new().expect("EmbassyAdapter::default() should not fail")
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
        let duration = embassy_time::Duration::from_micros(d.as_micros().min(u64::MAX as u128) as u64);
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

// Host-run contract test for the dyn-safe RuntimeOps surface. Gated like the
// join-queue tests: `embassy-sync` (not `embassy-runtime`) so the cortex-m
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
        let ops: Arc<dyn RuntimeOps> = Arc::new(EmbassyAdapter::new().unwrap());
        block_on(aimdb_executor::test_support::assert_runtime_ops_contract(
            ops.as_ref(),
        ));
    }
}

// Implement EmbassyNetwork trait for accessing network stack
#[cfg(feature = "embassy-net-support")]
impl EmbassyNetwork for EmbassyAdapter {
    fn network_stack(&self) -> &'static Stack<'static> {
        self.network.expect(
            "Network stack not available - connectors requiring network access need the 'embassy-net-support' feature enabled and must use EmbassyAdapter::new_with_network() to provide a network stack",
        )
    }
}
