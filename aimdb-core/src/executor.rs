//! Runtime capability surface consumed by the database engine.
//!
//! [`RuntimeOps`] is the single trait a runtime adapter implements: identity,
//! a clock factory, sleep, and logging, all on concrete types
//! (`core::time::Duration`, a boxed sleep future, `Arc<dyn InstanceClock>`) so the
//! runtime travels as a *value* — `Arc<dyn RuntimeOps>` — rather than a type
//! parameter. `AimDb`/`RuntimeContext` hold the runtime this way, and adapters
//! hand one to `AimDbBuilder::runtime`.
//!
//! The boxed `sleep` future costs one allocation per call, consistent with the
//! engine's cost model: every service future is already boxed, and `alloc` is
//! mandatory on all supported targets.

/// Heap-pinned, `Send`, `'static` future — the unit of work AimDB drives.
pub type BoxFuture =
    core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = ()> + Send + 'static>>;

/// Log severity for [`RuntimeOps::log`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Object-safe runtime capabilities: identity, clock, sleep, and logging.
///
/// Time is read from the [`InstanceClock`] this trait creates, never from the
/// runtime itself (design 058 §3.1).
///
/// Implemented by every runtime adapter (`TokioAdapter`, `EmbassyAdapter`,
/// `WasmAdapter`) so a runtime can travel as `Arc<dyn RuntimeOps>` instead of
/// a generic parameter.
pub trait RuntimeOps: Send + Sync {
    /// Short adapter identity, e.g. `"tokio"`.
    fn name(&self) -> &'static str;

    /// Completes after at least `d` has elapsed.
    fn sleep(&self, d: core::time::Duration) -> BoxFuture;

    /// Emits `msg` at `level` through the adapter's logging backend.
    fn log(&self, level: LogLevel, msg: &str);

    /// Creates a new clock whose monotonic origin is the moment of this call.
    ///
    /// Called once by `AimDbBuilder::build()`; every write of the instance is
    /// stamped from the returned clock, whichever thread, task or runtime
    /// performs it (design 058 §3). Code outside an instance (a standalone
    /// client engine) creates its own through `RuntimeContext::new`.
    fn new_clock(&self) -> alloc::sync::Arc<dyn InstanceClock>;
}

use crate::clock::InstanceClock;

pub type ExecutorResult<T> = Result<T, ExecutorError>;

/// Errors from the engine's task-coordination plumbing.
#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    /// A join pipeline's trigger queue closed because every sender was dropped.
    #[error("Join queue closed")]
    QueueClosed,
}

/// Shared behavioral contract for [`RuntimeOps`] implementations.
///
/// Each adapter crate calls [`assert_runtime_ops_contract`] from a test running
/// under its own executor (`#[tokio::test]`, `block_on`, `#[wasm_bindgen_test]`),
/// so the one contract is exercised against every runtime implementation.
///
/// [`assert_runtime_ops_contract`]: test_support::assert_runtime_ops_contract
#[doc(hidden)]
pub mod test_support {
    use super::{LogLevel, RuntimeOps};
    use crate::clock::{InstanceClock, Stamp};
    use alloc::sync::Arc;
    #[cfg(target_has_atomic = "64")]
    use portable_atomic::{AtomicU64, Ordering};

    /// An [`InstanceClock`] pinned at zero.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct ZeroClock;

    impl InstanceClock for ZeroClock {
        fn now(&self) -> Stamp {
            Stamp::from_nanos(0)
        }
        fn unix_time(&self) -> Option<(u64, u32)> {
            None
        }
    }

    /// An [`InstanceClock`] that stands still until [`advance`](Self::advance)d.
    /// Host targets only (needs 64-bit atomics).
    #[cfg(target_has_atomic = "64")]
    #[derive(Debug, Default)]
    pub struct ManualClock(AtomicU64);

    #[cfg(target_has_atomic = "64")]
    impl ManualClock {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn advance(&self, d: core::time::Duration) {
            let ns = crate::clock::stamp_from_duration(d).as_nanos();
            self.0.fetch_add(ns, Ordering::SeqCst);
        }
    }

    #[cfg(target_has_atomic = "64")]
    impl InstanceClock for ManualClock {
        fn now(&self) -> Stamp {
            Stamp::from_nanos(self.0.load(Ordering::SeqCst))
        }
        fn unix_time(&self) -> Option<(u64, u32)> {
            None
        }
    }

    /// [`NoopRuntimeOps`], except that its instance clock is one shared
    /// [`ManualClock`] the test holds and advances.
    #[cfg(target_has_atomic = "64")]
    #[derive(Clone, Debug, Default)]
    pub struct ManualClockRuntimeOps {
        pub clock: Arc<ManualClock>,
    }

    #[cfg(target_has_atomic = "64")]
    impl RuntimeOps for ManualClockRuntimeOps {
        fn name(&self) -> &'static str {
            "manual-clock"
        }
        fn sleep(&self, _d: core::time::Duration) -> super::BoxFuture {
            alloc::boxed::Box::pin(core::future::ready(()))
        }
        fn log(&self, _level: LogLevel, _msg: &str) {}
        fn new_clock(&self) -> Arc<dyn InstanceClock> {
            self.clock.clone()
        }
    }

    /// A no-op [`RuntimeOps`] for tests that need a runtime value (a
    /// `RuntimeContext`, an engine clock) but no real runtime behind it: the
    /// clock (and each instance clock) is pinned at `0`, sleeps complete immediately, logs are discarded.
    ///
    /// One shared stub instead of a hand-rolled copy per test module, so a
    /// `RuntimeOps` change is fixed here and in the real adapters only.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct NoopRuntimeOps;

    impl RuntimeOps for NoopRuntimeOps {
        fn name(&self) -> &'static str {
            "noop"
        }
        fn sleep(&self, _d: core::time::Duration) -> super::BoxFuture {
            alloc::boxed::Box::pin(core::future::ready(()))
        }
        fn log(&self, _level: LogLevel, _msg: &str) {}
        fn new_clock(&self) -> Arc<dyn InstanceClock> {
            Arc::new(ZeroClock)
        }
    }

    /// Asserts the behavioral contract every `RuntimeOps` implementation must hold.
    ///
    /// Uses `Duration::ZERO` for the sleep check: the Embassy host-test time
    /// driver reports `now() == 0` with a no-op `schedule_wake`, so any non-zero
    /// sleep would hang forever on the host. Adapters with a real clock should
    /// additionally assert that a non-zero sleep advances their clock in their
    /// own tests.
    pub async fn assert_runtime_ops_contract(ops: &dyn RuntimeOps) {
        assert!(!ops.name().is_empty(), "name() must be non-empty");

        let clock = ops.new_clock();
        let s0 = clock.now();
        let s1 = clock.now();
        assert!(
            s1 >= s0,
            "instance clock must be nondecreasing ({s1:?} < {s0:?})"
        );

        ops.log(LogLevel::Debug, "runtime-ops contract: debug");
        ops.log(LogLevel::Info, "runtime-ops contract: info");
        ops.log(LogLevel::Warn, "runtime-ops contract: warn");
        ops.log(LogLevel::Error, "runtime-ops contract: error");

        ops.sleep(core::time::Duration::ZERO).await;

        if let Some((secs, nanos)) = clock.unix_time() {
            assert!(
                nanos < 1_000_000_000,
                "unix_time nanos out of range: {nanos}"
            );
            // Sanity: any real wall clock reads after 2020-09-13 (1.6e9).
            assert!(
                secs > 1_600_000_000,
                "unix_time seconds implausible: {secs}"
            );
        }
    }
}
