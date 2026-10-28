// Included only in the unit-test build: the test harness owns the logger.
use super::*;

#[cfg(feature = "tracing")]
struct TraceSink(std::sync::Arc<std::sync::atomic::AtomicUsize>);
#[cfg(feature = "tracing")]
impl tracing::Subscriber for TraceSink {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        assert_eq!(event.metadata().target(), "aimdb_core::session::events");
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}
#[test]
fn log_backend_preserves_text_and_levels_under_the_events_target() {
    // Scope collection to this thread so parallel engine tests cannot contaminate it.
    std::thread_local! {
        static RECORDS: std::cell::RefCell<Option<Vec<(log::Level, alloc::string::String, alloc::string::String)>>> = const { std::cell::RefCell::new(None) };
    }
    struct Logger;
    impl log::Log for Logger {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            RECORDS.with(|records| {
                if let Some(records) = records.borrow_mut().as_mut() {
                    records.push((
                        record.level(),
                        record.target().into(),
                        alloc::format!("{}", record.args()),
                    ));
                }
            });
        }
        fn flush(&self) {}
    }
    static LOGGER: Logger = Logger;
    log::set_logger(&LOGGER).expect("no other unit test installs a logger");
    log::set_max_level(log::LevelFilter::Trace);
    RECORDS.with(|records| {
        records.replace(Some(Vec::new()));
    });
    #[cfg(feature = "tracing")]
    let trace_seen = Arc::new(core::sync::atomic::AtomicUsize::new(0));
    let emit = || {
        report(SessionEvent::AcceptFailed {
            error: TransportError::Io,
        });
        report(SessionEvent::AcceptStillFailing {
            attempts: 10,
            error: TransportError::Busy,
        });
        report(SessionEvent::AcceptRecovered { after: 12 });
    };
    // A real subscriber prevents tracing's optional log fallback from
    // duplicating the facade's explicit log destination.
    #[cfg(feature = "tracing")]
    tracing::subscriber::with_default(TraceSink(trace_seen.clone()), emit);
    #[cfg(not(feature = "tracing"))]
    emit();
    #[cfg(feature = "tracing")]
    assert_eq!(trace_seen.load(core::sync::atomic::Ordering::Relaxed), 3);
    let records = RECORDS.with(|records| records.take().unwrap());
    assert_eq!(
        records,
        [
            (
                log::Level::Error,
                "aimdb_core::session::events".into(),
                "accept failed: Io; retrying, further failures are summarized".into()
            ),
            (
                log::Level::Warn,
                "aimdb_core::session::events".into(),
                "accept still failing: 10 attempts in a row, last: Busy".into()
            ),
            (
                log::Level::Info,
                "aimdb_core::session::events".into(),
                "accept recovered after 12 failed attempt(s)".into()
            ),
        ]
    );
}
