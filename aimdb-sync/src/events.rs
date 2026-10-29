//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const HANDLE: &str = "aimdb_sync::handle";
}

#[derive(Debug)]
pub(crate) enum SyncEvent<'a> {
    RuntimeStartFailed { error: &'a std::io::Error },
    RuntimeHandoffFailed,
    DatabaseBuildFailed { error: &'a aimdb_core::DbError },
    DatabaseHandoffFailed,
    ImplicitShutdown,
    ShutdownSignalled,
}

pub(crate) fn report(event: SyncEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        SyncEvent::RuntimeStartFailed { error } => {
            aimdb_core::log_error!(
                target: target::HANDLE,
                "Failed to create Tokio runtime: {}",
                error
            )
        }
        SyncEvent::RuntimeHandoffFailed => {
            aimdb_core::log_error!(
                target: target::HANDLE,
                "Failed to send runtime handle to main thread"
            )
        }
        SyncEvent::DatabaseBuildFailed { error } => {
            aimdb_core::log_error!(target: target::HANDLE, "Failed to build database: {}", error)
        }
        SyncEvent::DatabaseHandoffFailed => {
            aimdb_core::log_error!(target: target::HANDLE, "Failed to send database to main thread")
        }
        SyncEvent::ImplicitShutdown => {
            aimdb_core::log_warn!(
                target: target::HANDLE,
                "AimDbHandle dropped without calling shutdown()"
            )
        }
        SyncEvent::ShutdownSignalled => {
            aimdb_core::log_warn!(
                target: target::HANDLE,
                "Shutdown was signalled; the runtime thread stops on its own"
            )
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
