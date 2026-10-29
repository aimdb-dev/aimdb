//! Tokio Runtime Adapter for AimDB
//!
//! Implements [`RuntimeOps`] — the one trait a runtime adapter provides —
//! on top of `std::time` and `tokio::time`.

use aimdb_core::{DbResult, LogLevel, RuntimeOps};

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

/// The instant `now_nanos` counts from in the current runtime: its first
/// reading there, or `now` if this is the first.
///
/// Per runtime, because a paused clock belongs to its runtime: one process-wide
/// anchor taken after a paused runtime advanced 60 s would sit in the future of
/// every later runtime, pinning their `now_nanos` at zero (#333). Readings
/// outside any runtime share one anchor on the real clock.
///
/// One entry per runtime that ever reads the clock, kept for the process'
/// lifetime (tokio offers no hook on runtime drop); the thread-local cache
/// keeps the lock off the hot path.
#[cfg(feature = "tokio-runtime")]
fn clock_anchor(now: tokio::time::Instant) -> tokio::time::Instant {
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, PoisonError};
    use tokio::runtime::{Handle, Id};
    use tokio::time::Instant;

    type Key = Option<Id>;
    thread_local! {
        static CACHED: Cell<Option<(Key, Instant)>> = const { Cell::new(None) };
    }
    static ANCHORS: OnceLock<Mutex<HashMap<Key, Instant>>> = OnceLock::new();

    let key = Handle::try_current().ok().map(|h| h.id());
    if let Some((cached_key, anchor)) = CACHED.get() {
        if cached_key == key {
            return anchor;
        }
    }
    let anchor = *ANCHORS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(key)
        .or_insert(now);
    CACHED.set(Some((key, anchor)));
    anchor
}

#[cfg(feature = "tokio-runtime")]
impl RuntimeOps for TokioAdapter {
    fn name(&self) -> &'static str {
        "tokio"
    }

    fn now_nanos(&self) -> u64 {
        // Tokio's clock, so paused time (`tokio::time::pause`, `test-util`)
        // advances it the same way it advances `sleep`; outside paused time it
        // is the system's monotonic clock. Nanoseconds since the current
        // runtime's anchor (monotonic per runtime, arbitrary epoch).
        let now = tokio::time::Instant::now();
        now.saturating_duration_since(clock_anchor(now))
            .as_nanos()
            .min(u64::MAX as u128) as u64
    }

    fn unix_time(&self) -> Option<(u64, u32)> {
        // The OS wall clock. `now_nanos()` is monotonic (for durations); this
        // is the absolute time AimX metadata timestamps report.
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .ok()
            .map(|d| (d.as_secs(), d.subsec_nanos()))
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
        let before = ops.now_nanos();
        ops.sleep(Duration::from_millis(10)).await;
        let after = ops.now_nanos();
        assert!(
            after - before >= 10_000_000,
            "10ms sleep advanced now_nanos by only {}ns",
            after - before
        );
    }

    /// #333: under paused time `sleep` advances Tokio's virtual clock
    /// instantly; `now_nanos` must follow it, not the real clock.
    #[tokio::test(start_paused = true)]
    async fn now_nanos_follows_paused_tokio_time() {
        let ops: Arc<dyn RuntimeOps> = Arc::new(TokioAdapter);
        let before = ops.now_nanos();
        ops.sleep(Duration::from_secs(10)).await;
        let after = ops.now_nanos();
        assert!(
            after - before >= 10_000_000_000,
            "a paused 10s sleep advanced now_nanos by only {}ns",
            after - before
        );
    }

    /// #333: a paused runtime that advanced before its first reading must not
    /// move the epoch of a runtime after it. With one process-wide anchor the
    /// second runtime read zero before and after its 10 s sleep.
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
            tokio::time::sleep(Duration::from_secs(60)).await;
            TokioAdapter.now_nanos();
        });

        paused().block_on(async {
            let before = TokioAdapter.now_nanos();
            tokio::time::sleep(Duration::from_secs(10)).await;
            let after = TokioAdapter.now_nanos();
            assert!(
                after - before >= 10_000_000_000,
                "a paused 10s sleep in the second runtime advanced now_nanos by only {}ns",
                after - before
            );
        });
    }
}
