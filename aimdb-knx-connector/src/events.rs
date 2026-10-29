//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const CLIENT: &str = "aimdb_knx_connector::client";
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum KnxEvent<'a> {
    SendFailed,
    AckTimedOut {
        sequence: u8,
    },
    OutboundRejected {
        route: &'a str,
        error: aimdb_core::transport::PublishError,
    },
    BinderBusy,
    BindFailed,
}

pub(crate) fn report(event: KnxEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        KnxEvent::SendFailed => {
            aimdb_core::log_error!(target: target::CLIENT, "KNX send failed");
        }
        KnxEvent::AckTimedOut { sequence } => {
            aimdb_core::log_warn!(
                target: target::CLIENT,
                "KNX outbound: no ACK for sequence {}",
                sequence
            )
        }
        KnxEvent::OutboundRejected { route, error } => {
            aimdb_core::log_warn!(
                target: target::CLIENT,
                "KNX outbound: skipping a value for route '{}': {:?}",
                route,
                error
            );
        }
        KnxEvent::BinderBusy => {
            aimdb_core::log_error!(
                target: target::CLIENT,
                "KNX bind failed: the binder's socket is held by another handle \
                     (a clone of a single-socket binder?); retrying"
            );
        }
        KnxEvent::BindFailed => {
            aimdb_core::log_error!(target: target::CLIENT, "KNX bind failed; retrying");
        }
    }
}

/// Test-only poll-scoped observation. Spawned work must be wrapped explicitly.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod capture {
    extern crate std;
    use super::KnxEvent;
    use core::future::{poll_fn, Future};
    use std::{boxed::Box, cell::RefCell, sync::Arc};
    pub(crate) type Observer = Arc<dyn Fn(&KnxEvent<'_>) + Send + Sync>;
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
    pub(super) fn record(event: &KnxEvent<'_>) {
        let observer = CURRENT.with(|slot| slot.borrow().clone());
        if let Some(observer) = observer {
            observer(event);
        }
    }
}
