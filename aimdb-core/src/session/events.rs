//! Internal operational events. Payloads are borrowed only for synchronous reporting.
//! Logging policy lives here; metrics and public observation remain separate decisions.

use super::{AuthError, CodecError, FrameFault, TransportError};

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum SessionEvent<'a> {
    AuthenticationRejected {
        error: &'a AuthError,
    },
    HandshakeReset {
        ended_subscriptions: usize,
    },
    SubscribeRefused {
        id: &'a str,
    },
    SubscriptionEnded {
        id: &'a str,
    },
    SnapshotEncodeFailed {
        id: &'a str,
        topic: &'a str,
        error: &'a CodecError,
    },
    AcceptFailed {
        error: TransportError,
    },
    AcceptStillFailing {
        attempts: u64,
        error: TransportError,
    },
    AcceptRecovered {
        after: u64,
    },
    ConnectionLimitReached {
        limit: usize,
    },
    ListenerClosed {
        open_connections: usize,
    },
    DialFailed {
        error: &'a TransportError,
    },
    ReconnectExhausted {
        attempts: usize,
    },
    MirrorGap {
        topic: &'a str,
        skipped: u64,
    },
    ReceiveFramingFailed,
    SendFrameRejected {
        bytes: usize,
        fault: FrameFault,
    },
    #[cfg(feature = "remote")]
    WildcardSubscribeFailed {
        pattern: &'a str,
        record: &'a str,
        error: &'a crate::DbError,
    },
    #[cfg(feature = "remote")]
    HandshakeVersionRejected {
        client_version: &'a str,
        server_version: &'a str,
    },
}

pub(crate) fn report(event: SessionEvent<'_>) {
    #[cfg(all(test, feature = "std"))]
    capture::record(&event);
    match event {
        SessionEvent::AuthenticationRejected { error } => {
            log_warn!("session authenticate rejected: {:?}", error)
        }
        SessionEvent::HandshakeReset {
            ended_subscriptions,
        } => log_info!(
            "new handshake on a live connection: ending {} subscription(s) of the previous client",
            ended_subscriptions
        ),
        SessionEvent::SubscribeRefused { id } => {
            log_warn!("subscribe refused: id {} is still live", id)
        }
        SessionEvent::SubscriptionEnded { id } => {
            log_warn!("subscription {} ended: its source closed", id)
        }
        SessionEvent::SnapshotEncodeFailed { id, topic, error } => log_warn!(
            "snapshot encode failed, skipping: sub={} topic={} err={:?}",
            id,
            topic,
            error
        ),
        SessionEvent::AcceptFailed { error } => log_error!(
            "accept failed: {:?}; retrying, further failures are summarized",
            error
        ),
        SessionEvent::AcceptStillFailing { attempts, error } => log_warn!(
            "accept still failing: {} attempts in a row, last: {:?}",
            attempts,
            error
        ),
        SessionEvent::AcceptRecovered { after } => {
            log_info!("accept recovered after {} failed attempt(s)", after)
        }
        SessionEvent::ConnectionLimitReached { limit } => {
            log_warn!("max_connections={} reached, refusing client", limit)
        }
        SessionEvent::ListenerClosed { open_connections } => log_info!(
            "listener closed; serving {} open connection(s) to their end",
            open_connections
        ),
        SessionEvent::DialFailed { error } => log_warn!("client dial failed: {:?}", error),
        SessionEvent::ReconnectExhausted { attempts } => {
            log_warn!("client giving up after {} reconnect attempts", attempts)
        }
        SessionEvent::MirrorGap { topic, skipped } => log_warn!(
            "mirror gap on '{}': {} update(s) lost (latest-state, no resync)",
            topic,
            skipped
        ),
        SessionEvent::ReceiveFramingFailed => {
            log_warn!("framed recv: unrecoverable framing error, closing connection")
        }
        SessionEvent::SendFrameRejected { bytes, fault } => log_warn!(
            "framed send: framer rejected a {}-byte frame ({:?}), closing connection",
            bytes,
            fault
        ),
        #[cfg(feature = "remote")]
        SessionEvent::WildcardSubscribeFailed {
            pattern,
            record,
            error,
        } => log_warn!(
            "wildcard subscribe '{}': skipping '{}': {:?}",
            pattern,
            record,
            error
        ),
        #[cfg(feature = "remote")]
        SessionEvent::HandshakeVersionRejected {
            client_version,
            server_version,
        } => log_warn!(
            "refusing handshake: client protocol version {:?} incompatible with server {}",
            client_version,
            server_version
        ),
    }
}

/// Poll-scoped capture: no thread-local state survives an await. Wrap spawned
/// futures individually; unrelated tasks and parallel tests do not inherit it.
#[cfg(all(test, feature = "std"))]
pub(super) mod capture {
    use super::SessionEvent;
    use core::future::{poll_fn, Future};
    use std::{cell::RefCell, sync::Arc};

    pub(in crate::session) type Observer = Arc<dyn Fn(&SessionEvent<'_>) + Send + Sync>;
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

    pub(in crate::session) fn with<T>(observer: Observer, f: impl FnOnce() -> T) -> T {
        let _guard = Guard(CURRENT.with(|slot| slot.replace(Some(observer))));
        f()
    }

    pub(in crate::session) async fn future<F: Future>(observer: Observer, future: F) -> F::Output {
        let mut future = alloc::boxed::Box::pin(future);
        poll_fn(|cx| with(observer.clone(), || future.as_mut().poll(cx))).await
    }

    pub(super) fn record(event: &SessionEvent<'_>) {
        let observer = CURRENT.with(|slot| slot.borrow().clone());
        if let Some(observer) = observer {
            observer(event);
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests;
