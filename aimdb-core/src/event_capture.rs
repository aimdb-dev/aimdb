//! Test-only observation of a crate's operational events.
//!
//! Every `events.rs` has a `capture` module that lets a test see the events
//! `report()` is handed while a closure or future runs on the current thread.
//! Only the event type differs between crates, so it is generated here:
//!
//! ```ignore
//! #[cfg(test)]
//! aimdb_core::__event_capture!(super::MqttEvent<'_>);
//! ```
//!
//! The caller owns the `cfg` gate, so the expansion never reaches a production
//! build, and `std` is only needed by the expanding crate's tests.

/// Generates `mod capture` for `$event`; no stability guarantee.
///
/// - `capture::with(observer, f)` observes events reported while `f` runs.
/// - `capture::future(observer, fut)` observes them across each poll of `fut`.
/// - `capture::record(&event)` is what `report()` calls.
///
/// Observation is poll-scoped and thread-local: work spawned onto other tasks
/// or threads must be wrapped explicitly.
#[doc(hidden)]
#[macro_export]
macro_rules! __event_capture {
    ($event:ty) => {
        /// Test-only poll-scoped observation. Spawned work must be wrapped explicitly.
        #[allow(dead_code)]
        pub(crate) mod capture {
            extern crate std;
            use ::core::future::{poll_fn, Future};
            use std::{boxed::Box, cell::RefCell, sync::Arc};
            pub(crate) type Observer = Arc<dyn Fn(&$event) + Send + Sync>;
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
            pub(crate) fn record(event: &$event) {
                let observer = CURRENT.with(|slot| slot.borrow().clone());
                if let Some(observer) = observer {
                    observer(event);
                }
            }
        }
    };
}
