//! Tokio Runtime Adapter for AimDB
//!
//! Implements [`RuntimeOps`] — the one trait a runtime adapter provides —
//! on top of `std::time` and `tokio::time`.

use aimdb_core::{DbResult, LogLevel, RuntimeOps};
use std::time::Instant;

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

#[cfg(feature = "tokio-runtime")]
impl RuntimeOps for TokioAdapter {
    fn name(&self) -> &'static str {
        "tokio"
    }

    fn now_nanos(&self) -> u64 {
        // std `Instant` has no public epoch, so anchor the first reading and
        // report nanoseconds elapsed since it (monotonic, arbitrary epoch).
        static ANCHOR: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        ANCHOR
            .get_or_init(Instant::now)
            .elapsed()
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
}
