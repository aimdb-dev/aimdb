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
pub(crate) enum SqliteEvent<'a> {
    CorruptJson { error: Detail<'a> },
}

pub(crate) fn report(event: SqliteEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        SqliteEvent::CorruptJson { error } => aimdb_core::log_warn!(
            "SQLite: corrupted JSON in record_history row, substituting null: {}",
            error
        ),
    }
}

/// Test-only poll-scoped observation. Spawned work must be wrapped explicitly.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod capture {
    extern crate std;
    use super::SqliteEvent;
    use core::future::{poll_fn, Future};
    use std::{boxed::Box, cell::RefCell, sync::Arc};
    pub(crate) type Observer = Arc<dyn Fn(&SqliteEvent<'_>) + Send + Sync>;
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
    pub(super) fn record(event: &SqliteEvent<'_>) {
        let observer = CURRENT.with(|slot| slot.borrow().clone());
        if let Some(observer) = observer {
            observer(event);
        }
    }
}
