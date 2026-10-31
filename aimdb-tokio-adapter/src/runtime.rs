//! Tokio Runtime Adapter for AimDB
//!
//! Implements [`RuntimeOps`] — the one trait a runtime adapter provides —
//! on top of `std::time` and `tokio::time`.

use aimdb_core::{DbResult, InstanceClock, LogLevel, RuntimeOps, Stamp};

/// Tokio runtime adapter for std environments
///
/// Hand an `Arc<TokioAdapter>` to `AimDbBuilder::runtime` to run AimDB on
/// the Tokio executor.
///
/// # Example
/// ```rust,no_run
/// use aimdb_tokio_adapter::TokioAdapter;
/// use std::sync::Arc;
///
/// #[tokio::main]
/// async fn main() -> aimdb_core::DbResult<()> {
///     let runtime = Arc::new(TokioAdapter::new()?);
///     let builder = aimdb_core::AimDbBuilder::new().runtime(runtime);
///     Ok(())
/// }
/// ```
#[cfg(feature = "tokio-runtime")]
#[derive(Debug, Clone, Copy)]
pub struct TokioAdapter;

#[cfg(feature = "tokio-runtime")]
impl TokioAdapter {
    /// Creates a new TokioAdapter instance
    ///
    /// Always succeeds since TokioAdapter is a zero-sized type; the `Result`
    /// return is kept so adapter construction has one signature everywhere.
    pub fn new() -> DbResult<Self> {
        Ok(Self)
    }
}

#[cfg(feature = "tokio-runtime")]
impl Default for TokioAdapter {
    fn default() -> Self {
        Self::new().expect("TokioAdapter::default() should not fail")
    }
}

/// The instance clock (design 058 §3.2): Tokio's clock of the runtime that
/// was current at `build()`, measured from that moment.
///
/// A write from outside that runtime (an `aimdb-sync` producer on its own
/// thread, another runtime) enters the captured runtime's context to read the
/// clock, so under `tokio::time::pause` every write follows the instance
/// runtime's paused time wherever it comes from.
#[cfg(feature = "tokio-runtime")]
struct TokioClock {
    /// `None` only when `build()` was driven outside any Tokio runtime; then
    /// every reading is taken in the caller's context.
    handle: Option<tokio::runtime::Handle>,
    origin: tokio::time::Instant,
}

#[cfg(feature = "tokio-runtime")]
impl TokioClock {
    fn new() -> Self {
        let handle = tokio::runtime::Handle::try_current().ok();
        let origin = tokio::time::Instant::now();
        Self { handle, origin }
    }
}

#[cfg(feature = "tokio-runtime")]
impl InstanceClock for TokioClock {
    fn now(&self) -> Stamp {
        use tokio::runtime::Handle;
        use tokio::time::Instant;

        let now = match &self.handle {
            Some(handle) if Handle::try_current().map(|h| h.id()).ok() != Some(handle.id()) => {
                // Selects the captured runtime's (possibly paused) clock.
                let _guard = handle.enter();
                Instant::now()
            }
            _ => Instant::now(),
        };
        aimdb_core::clock::stamp_from_duration(now.saturating_duration_since(self.origin))
    }

    fn unix_time(&self) -> Option<(u64, u32)> {
        // The OS wall clock, independent of the monotonic origin.
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .ok()
            .map(|d| (d.as_secs(), d.subsec_nanos()))
    }
}

#[cfg(feature = "tokio-runtime")]
impl RuntimeOps for TokioAdapter {
    fn name(&self) -> &'static str {
        "tokio"
    }

    fn sleep(&self, d: core::time::Duration) -> aimdb_core::BoxFuture {
        Box::pin(tokio::time::sleep(d))
    }

    fn log(&self, level: LogLevel, msg: &str) {
        // Forward to the `log` facade so the binary chooses the backend
        // (env_logger, tracing-log, …). When no backend is configured,
        // `log::max_level()` is still `Off`; fall back to plain stdout so
        // demos and quickstarts show output without any logger setup. (The
        // fallback cannot distinguish "no backend" from a backend configured
        // with everything filtered off — configure a backend if you need
        // full silence.)
        if log::max_level() == log::LevelFilter::Off {
            match level {
                LogLevel::Debug => {}
                LogLevel::Info => println!("[info] {}", msg),
                LogLevel::Warn => println!("[warn] {}", msg),
                LogLevel::Error => eprintln!("[error] {}", msg),
            }
            return;
        }
        match level {
            LogLevel::Debug => log::debug!(target: "aimdb", "{msg}"),
            LogLevel::Info => log::info!(target: "aimdb", "{msg}"),
            LogLevel::Warn => log::warn!(target: "aimdb", "{msg}"),
            LogLevel::Error => log::error!(target: "aimdb", "{msg}"),
        }
    }

    /// The clock of the runtime current at this call, measured from now: one
    /// clock for every reader, whatever thread or runtime it runs on.
    fn new_clock(&self) -> std::sync::Arc<dyn InstanceClock> {
        std::sync::Arc::new(TokioClock::new())
    }
}

#[cfg(all(test, feature = "tokio-runtime"))]
mod runtime_ops_tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn runtime_ops_contract() {
        // `Arc<dyn RuntimeOps>` must be constructible from the adapter.
        let ops: Arc<dyn RuntimeOps> = Arc::new(TokioAdapter);
        aimdb_core::executor::test_support::assert_runtime_ops_contract(ops.as_ref()).await;
    }

    #[tokio::test]
    async fn runtime_ops_sleep_advances_clock() {
        let ops: Arc<dyn RuntimeOps> = Arc::new(TokioAdapter);
        let clock = ops.new_clock();
        let before = clock.now();
        ops.sleep(Duration::from_millis(10)).await;
        let elapsed = clock.now().since(before);
        assert!(
            elapsed >= Duration::from_millis(10),
            "10ms sleep advanced the clock by only {elapsed:?}"
        );
    }

    /// Under paused time `sleep` advances Tokio's virtual clock instantly;
    /// the clock must follow it, not the real clock (#333).
    #[tokio::test(start_paused = true)]
    async fn clock_follows_paused_sleep() {
        let ops: Arc<dyn RuntimeOps> = Arc::new(TokioAdapter);
        let clock = ops.new_clock();
        let before = clock.now();
        ops.sleep(Duration::from_secs(10)).await;
        assert!(clock.now().since(before) >= Duration::from_secs(10));
    }

    /// #333: a paused runtime that advanced before must not move the origin of
    /// a clock created in a later runtime: each clock starts at its creation.
    #[test]
    fn a_runtime_that_advanced_first_does_not_pin_the_next_one() {
        let paused = || {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .start_paused(true)
                .build()
                .unwrap()
        };

        paused().block_on(async {
            let clock = TokioAdapter.new_clock();
            tokio::time::sleep(Duration::from_secs(60)).await;
            clock.now();
        });

        paused().block_on(async {
            let clock = TokioAdapter.new_clock();
            let before = clock.now();
            tokio::time::sleep(Duration::from_secs(10)).await;
            assert!(clock.now().since(before) >= Duration::from_secs(10));
        });
    }

    /// 058 R5: the instance clock starts at its creation and follows paused
    /// time exactly.
    #[tokio::test(start_paused = true)]
    async fn instance_clock_follows_paused_time_exactly() {
        let clock = TokioAdapter.new_clock();
        assert_eq!(clock.now(), Stamp::from_nanos(0));
        assert_eq!(clock.now(), Stamp::from_nanos(0));
        let d = Duration::from_millis(1234);
        tokio::time::advance(d).await;
        assert_eq!(clock.now().since(Stamp::from_nanos(0)), d);
    }

    /// 058 R2: a reading from a plain thread, or from inside another runtime,
    /// is taken on the instance runtime's (paused) clock.
    #[tokio::test(start_paused = true)]
    async fn instance_clock_is_the_same_from_any_thread_or_runtime() {
        let clock = TokioAdapter.new_clock();
        tokio::time::advance(Duration::from_secs(5)).await;
        let expected = clock.now();
        assert_eq!(expected.as_nanos(), 5_000_000_000);

        let c = clock.clone();
        let from_thread = std::thread::spawn(move || c.now()).join().unwrap();
        assert_eq!(from_thread, expected);

        let c = clock.clone();
        let from_other_runtime = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(async move { c.now() })
        })
        .join()
        .unwrap();
        assert_eq!(from_other_runtime, expected);
    }
}
