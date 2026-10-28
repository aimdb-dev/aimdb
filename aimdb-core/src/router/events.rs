//! Typed operational reporting; payloads live only for the synchronous call.

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum RouterEvent<'a> {
    IngestFailed { topic: &'a str, reason: &'a str },
}

pub(crate) fn report(event: RouterEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        RouterEvent::IngestFailed { topic, reason } => {
            log_warn!("Failed to ingest message on '{}': {}", topic, reason)
        }
    }
}

/// Test-only poll-scoped observation. Spawned work must be wrapped explicitly.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod capture {
    extern crate std;
    use super::RouterEvent;
    use core::future::{poll_fn, Future};
    use std::{boxed::Box, cell::RefCell, sync::Arc};
    pub(crate) type Observer = Arc<dyn Fn(&RouterEvent<'_>) + Send + Sync>;
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
    pub(super) fn record(event: &RouterEvent<'_>) {
        let observer = CURRENT.with(|slot| slot.borrow().clone());
        if let Some(observer) = observer {
            observer(event);
        }
    }
}
