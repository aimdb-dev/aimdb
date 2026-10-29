//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const ROUTES: &str = "aimdb_core::outbound::routes";
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum OutboundEvent<'a> {
    Lagged {
        topic: &'a str,
        skipped: u64,
    },
    TopicOverflow {
        topic: &'a str,
        capacity: usize,
    },
    SerializeFailed {
        topic: &'a str,
        error: &'a crate::outbound::SerializeFailure,
    },
    Closed {
        topic: &'a str,
        error: &'a crate::DbError,
    },
}

pub(crate) fn report(event: OutboundEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        OutboundEvent::Lagged { topic, skipped } => {
            log_warn!(target: target::ROUTES, "outbound route '{}' lagged by {}", topic, skipped)
        }
        OutboundEvent::TopicOverflow { topic, capacity } => log_warn!(
            target: target::ROUTES,
            "outbound route '{}': topic does not fit in {} bytes, value skipped",
            topic,
            capacity
        ),
        OutboundEvent::SerializeFailed { topic, error } => {
            log_error!(
                target: target::ROUTES,
                "outbound route '{}': {}, value skipped",
                topic,
                error
            )
        }
        OutboundEvent::Closed { topic, error } => {
            log_info!(target: target::ROUTES, "outbound route '{}' closed: {:?}", topic, error)
        }
    }
}

/// Test-only poll-scoped observation. Spawned work must be wrapped explicitly.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod capture {
    extern crate std;
    use super::OutboundEvent;
    use core::future::{poll_fn, Future};
    use std::{boxed::Box, cell::RefCell, sync::Arc};
    pub(crate) type Observer = Arc<dyn Fn(&OutboundEvent<'_>) + Send + Sync>;
    std::thread_local! {
        static CURRENT: RefCell<Option<Observer>> = const { RefCell::new(None) };
    }
    struct Guard(Option<Observer>);
    impl Drop for Guard {
        fn drop(&mut self) {
            CURRENT.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    pub(crate) fn with<T>(observer: Observer, f: impl FnOnce() -> T) -> T {
        let _guard = Guard(CURRENT.with(|slot| slot.replace(Some(observer))));
        f()
    }
    pub(crate) async fn future<F: Future>(observer: Observer, future: F) -> F::Output {
        let mut future = Box::pin(future);
        poll_fn(|cx| with(observer.clone(), || future.as_mut().poll(cx))).await
    }
    pub(super) fn record(event: &OutboundEvent<'_>) {
        let observer = CURRENT.with(|slot| slot.borrow().clone());
        if let Some(observer) = observer {
            observer(event);
        }
    }
}
