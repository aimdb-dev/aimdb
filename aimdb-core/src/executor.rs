//! Runtime capability surface consumed by the database engine.
//!
//! [`RuntimeOps`] is the single trait a runtime adapter implements: identity,
//! monotonic + wall-clock time, sleep, and logging, all on concrete types
//! (`u64` nanoseconds, `core::time::Duration`, a boxed sleep future) so the
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

/// Object-safe runtime capabilities: identity, time, sleep, and logging.
///
/// Implemented by every runtime adapter (`TokioAdapter`, `EmbassyAdapter`,
/// `WasmAdapter`) so a runtime can travel as `Arc<dyn RuntimeOps>` instead of
/// a generic parameter.
pub trait RuntimeOps: Send + Sync {
    /// Short adapter identity, e.g. `"tokio"`.
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
    /// no RTC anchor), where only monotonic time is available.
    fn unix_time(&self) -> Option<(u64, u32)>;

    /// Completes after at least `d` has elapsed.
    fn sleep(&self, d: core::time::Duration) -> BoxFuture;

    /// Emits `msg` at `level` through the adapter's logging backend.
    fn log(&self, level: LogLevel, msg: &str);
}

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

    /// A no-op [`RuntimeOps`] for tests that need a runtime value (a
    /// `RuntimeContext`, an engine clock) but no real runtime behind it: the
    /// clock is pinned at `0`, sleeps complete immediately, logs are discarded.
    ///
    /// One shared stub instead of a hand-rolled copy per test module, so a
    /// `RuntimeOps` change is fixed here and in the real adapters only.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct NoopRuntimeOps;

    impl RuntimeOps for NoopRuntimeOps {
        fn name(&self) -> &'static str {
            "noop"
        }
        fn now_nanos(&self) -> u64 {
            0
        }
        fn unix_time(&self) -> Option<(u64, u32)> {
            None
        }
        fn sleep(&self, _d: core::time::Duration) -> super::BoxFuture {
            alloc::boxed::Box::pin(core::future::ready(()))
        }
        fn log(&self, _level: LogLevel, _msg: &str) {}
    }

    /// Asserts the behavioral contract every `RuntimeOps` implementation must hold.
    ///
    /// Uses `Duration::ZERO` for the sleep check: the Embassy host-test time
    /// driver reports `now() == 0` with a no-op `schedule_wake`, so any non-zero
    /// sleep would hang forever on the host. Adapters with a real clock should
    /// additionally assert that a non-zero sleep advances `now_nanos` in their
    /// own tests.
    pub async fn assert_runtime_ops_contract(ops: &dyn RuntimeOps) {
        assert!(!ops.name().is_empty(), "name() must be non-empty");

        let t0 = ops.now_nanos();
        let t1 = ops.now_nanos();
        assert!(t1 >= t0, "now_nanos() must be monotonic ({t1} < {t0})");

        ops.log(LogLevel::Debug, "runtime-ops contract: debug");
        ops.log(LogLevel::Info, "runtime-ops contract: info");
        ops.log(LogLevel::Warn, "runtime-ops contract: warn");
        ops.log(LogLevel::Error, "runtime-ops contract: error");

        ops.sleep(core::time::Duration::ZERO).await;

        if let Some((secs, nanos)) = ops.unix_time() {
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
