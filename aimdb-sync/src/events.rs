//! Typed operational reporting; payloads live only for the synchronous call.

/// Borrowed foreign diagnostic, retaining the original Display/Debug policy.
pub(crate) enum Detail<'a> {
    Display(&'a dyn core::fmt::Display),
}
impl<'a> Detail<'a> {
    pub(crate) fn display(value: &'a impl core::fmt::Display) -> Self {
        Self::Display(value)
    }
}
impl core::fmt::Display for Detail<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Display(value) => core::fmt::Display::fmt(value, f),
        }
    }
}
impl core::fmt::Debug for Detail<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self, f)
    }
}

#[derive(Debug)]
pub(crate) enum SyncEvent<'a> {
    RuntimeStartFailed { error: Detail<'a> },
    RuntimeHandoffFailed,
    DatabaseBuildFailed { error: Detail<'a> },
    DatabaseHandoffFailed,
    ImplicitShutdown,
    ShutdownSignalled,
}

pub(crate) fn report(event: SyncEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        SyncEvent::RuntimeStartFailed { error } => {
            aimdb_core::log_error!("Failed to create Tokio runtime: {}", error)
        }
        SyncEvent::RuntimeHandoffFailed => {
            aimdb_core::log_error!("Failed to send runtime handle to main thread")
        }
        SyncEvent::DatabaseBuildFailed { error } => {
            aimdb_core::log_error!("Failed to build database: {}", error)
        }
        SyncEvent::DatabaseHandoffFailed => {
            aimdb_core::log_error!("Failed to send database to main thread")
        }
        SyncEvent::ImplicitShutdown => {
            aimdb_core::log_warn!("AimDbHandle dropped without calling shutdown()")
        }
        SyncEvent::ShutdownSignalled => {
            aimdb_core::log_warn!("Shutdown was signalled; the runtime thread stops on its own")
        }
    }
}

/// Test-only poll-scoped observation. Spawned work must be wrapped explicitly.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod capture {
    extern crate std;
    use super::SyncEvent;
    use core::future::{poll_fn, Future};
    use std::{boxed::Box, cell::RefCell, sync::Arc};
    pub(crate) type Observer = Arc<dyn Fn(&SyncEvent<'_>) + Send + Sync>;
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
    pub(super) fn record(event: &SyncEvent<'_>) {
        let observer = CURRENT.with(|slot| slot.borrow().clone());
        if let Some(observer) = observer {
            observer(event);
        }
    }
}
