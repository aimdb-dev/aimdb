//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const BUFFER: &str = "aimdb_embassy_adapter::buffer";
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum AdapterEvent {
    SubscriberSlotsExhausted { capacity: usize },
}

pub(crate) fn report(event: AdapterEvent) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        AdapterEvent::SubscriberSlotsExhausted { capacity } => {
            aimdb_core::log_error!(
                target: target::BUFFER,
                "AimDB: SpmcRing subscriber slot exhausted (max SUBS={}). \
             Increase the CONSUMERS const generic on buffer_sized<CAP, CONSUMERS>. \
             Count one slot per .tap(), .link_to() connector, and each transform_join input.",
                capacity
            );
        }
    }
}

/// Test-only poll-scoped observation. Spawned work must be wrapped explicitly.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod capture {
    extern crate std;
    use super::AdapterEvent;
    use core::future::{poll_fn, Future};
    use std::{boxed::Box, cell::RefCell, sync::Arc};
    pub(crate) type Observer = Arc<dyn Fn(&AdapterEvent) + Send + Sync>;
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
    pub(super) fn record(event: &AdapterEvent) {
        let observer = CURRENT.with(|slot| slot.borrow().clone());
        if let Some(observer) = observer {
            observer(event);
        }
    }
}
