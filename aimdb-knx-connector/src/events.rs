//! Typed operational reporting; payloads live only for the synchronous call.

/// Borrowed foreign diagnostic, retaining the original Display/Debug policy.
pub(crate) enum Detail<'a> {
    Debug(&'a dyn core::fmt::Debug),
}
impl<'a> Detail<'a> {
    pub(crate) fn debug(value: &'a impl core::fmt::Debug) -> Self {
        Self::Debug(value)
    }
}
impl core::fmt::Display for Detail<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Debug(value) => core::fmt::Debug::fmt(value, f),
        }
    }
}
impl core::fmt::Debug for Detail<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self, f)
    }
}
#[cfg(feature = "defmt")]
impl defmt::Format for Detail<'_> {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "{}", defmt::Display2Format(self));
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum KnxEvent<'a> {
    SendFailed,
    AckTimedOut { sequence: u8 },
    OutboundRejected { route: &'a str, error: Detail<'a> },
    BinderBusy,
    BindFailed,
}

pub(crate) fn report(event: KnxEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        KnxEvent::SendFailed => aimdb_core::log_error!("KNX send failed"),
        KnxEvent::AckTimedOut { sequence } => {
            aimdb_core::log_warn!("KNX outbound: no ACK for sequence {}", sequence)
        }
        KnxEvent::OutboundRejected { route, error } => aimdb_core::log_warn!(
            "KNX outbound: skipping a value for route '{}': {:?}",
            route,
            error
        ),
        KnxEvent::BinderBusy => aimdb_core::log_error!(
            "KNX bind failed: the binder's socket is held by another handle \
                     (a clone of a single-socket binder?); retrying"
        ),
        KnxEvent::BindFailed => aimdb_core::log_error!("KNX bind failed; retrying"),
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
