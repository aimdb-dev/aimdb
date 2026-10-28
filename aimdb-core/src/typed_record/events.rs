//! Typed operational reporting; payloads live only for the synchronous call.

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum RemoteWriteEvent<'a> {
    Rejected {
        operation: &'a str,
        record_type: &'a str,
    },
}

pub(crate) fn report(event: RemoteWriteEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        RemoteWriteEvent::Rejected {
            operation,
            record_type,
        } => log_warn!(
            "Rejected {} for '{}': has active producer or transform",
            operation,
            record_type
        ),
    }
}

/// Test-only poll-scoped observation. Spawned work must be wrapped explicitly.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod capture {
    extern crate std;
    use super::RemoteWriteEvent;
    use core::future::{poll_fn, Future};
    use std::{boxed::Box, cell::RefCell, sync::Arc};
    pub(crate) type Observer = Arc<dyn Fn(&RemoteWriteEvent<'_>) + Send + Sync>;
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
    pub(super) fn record(event: &RemoteWriteEvent<'_>) {
        let observer = CURRENT.with(|slot| slot.borrow().clone());
        if let Some(observer) = observer {
            observer(event);
        }
    }
}
