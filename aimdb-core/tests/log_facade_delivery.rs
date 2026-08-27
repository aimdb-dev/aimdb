//! Design 050, criteria 3 and 4: an event reaches an installed `log`
//! destination exactly once, carrying the emitting module as its target — and
//! with `tracing` alongside, each destination sees it once.
#![cfg(feature = "log")]

mod log_support;

use std::sync::Arc;

use aimdb_core::executor::{BoxFuture, LogLevel, RuntimeOps};
use log_support::Capture;

static CAPTURE: Capture = Capture::new();

/// `build()` refuses without a runtime, and the call sites are past that check.
/// Nothing here is exercised beyond being present.
struct StubRuntime;

impl RuntimeOps for StubRuntime {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn now_nanos(&self) -> u64 {
        0
    }

    fn unix_time(&self) -> Option<(u64, u32)> {
        None
    }

    fn sleep(&self, _d: core::time::Duration) -> BoxFuture {
        Box::pin(core::future::pending())
    }

    fn log(&self, _level: LogLevel, _msg: &str) {}
}

/// Counts what it is handed, so criterion 4 can check "once each". Hand-written
/// rather than `tracing-subscriber`, since not needing that is the point.
#[cfg(feature = "tracing")]
mod trace_sink {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracing::span;

    pub static BUILDER_EVENTS: AtomicUsize = AtomicUsize::new(0);

    pub struct CountBuilderEvents;

    impl tracing::Subscriber for CountBuilderEvents {
        fn enabled(&self, _m: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _a: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(1)
        }
        fn record(&self, _s: &span::Id, _v: &span::Record<'_>) {}
        fn record_follows_from(&self, _s: &span::Id, _f: &span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            if event.metadata().target() == "aimdb_core::builder" {
                BUILDER_EVENTS.fetch_add(1, Ordering::Relaxed);
            }
        }
        fn enter(&self, _s: &span::Id) {}
        fn exit(&self, _s: &span::Id) {}
    }
}

#[tokio::test]
async fn a_builder_event_arrives_once_with_its_module_as_target() {
    log::set_logger(&CAPTURE).expect("no other logger may be installed in this binary");
    log::set_max_level(log::LevelFilter::Trace);

    #[cfg(feature = "tracing")]
    tracing::subscriber::set_global_default(trace_sink::CountBuilderEvents)
        .expect("no other subscriber may be installed in this binary");

    let (_db, _runner) = aimdb_core::AimDbBuilder::new()
        .runtime(Arc::new(StubRuntime))
        .build()
        .await
        .expect("an empty builder with a runtime builds");

    // Criterion 3: the target is the *expansion site's* module path, so it stays
    // the string a `tracing` subscriber has always seen.
    let builder_events: Vec<_> = CAPTURE
        .taken()
        .into_iter()
        .filter(|r| r.target == "aimdb_core::builder")
        .collect();
    assert!(
        !builder_events.is_empty(),
        "no event arrived with target aimdb_core::builder; got {:?}",
        CAPTURE.taken()
    );

    // ...and exactly once: two arms compile in, only one may reach `log`.
    let collecting = CAPTURE.with_message("Collecting futures for 0 records");
    assert_eq!(
        collecting.len(),
        1,
        "expected one delivery of the builder's collect event, got {collecting:?}"
    );
    assert_eq!(collecting[0].level, log::Level::Info);
    assert_eq!(collecting[0].target, "aimdb_core::builder");

    // Criterion 4: both arms run, once each — not twice into either.
    #[cfg(feature = "tracing")]
    {
        use std::sync::atomic::Ordering;
        assert!(
            trace_sink::BUILDER_EVENTS.load(Ordering::Relaxed) > 0,
            "the tracing arm stopped delivering when the log arm was added"
        );
        assert_eq!(
            CAPTURE
                .with_message("Record future collection complete")
                .len(),
            1,
            "the log arm delivered more than once with tracing also enabled"
        );
    }
}
