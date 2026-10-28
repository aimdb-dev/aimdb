//! Typed operational reporting; payloads live only for the synchronous call.

#[derive(Debug)]
pub(crate) enum WebSocketEvent<'a> {
    QueryHandlerFailed {
        error: &'a str,
    },
    PersistenceQueryFailed {
        error: &'a str,
    },
    MalformedQueryResult {
        error: &'a serde_json::Error,
    },
    BindFailed {
        address: core::net::SocketAddr,
        error: &'a std::io::Error,
    },
    Listening {
        address: core::net::SocketAddr,
    },
    ServerFailed {
        error: &'a std::io::Error,
    },
    ConnectionLimitReached {
        peer: core::net::SocketAddr,
        limit: u64,
    },
    VersionRejected {
        peer: core::net::SocketAddr,
        client_version: &'a str,
        server_version: &'a str,
    },
    AuthenticationRejected {
        peer: core::net::SocketAddr,
        reason: &'a str,
    },
}

pub(crate) fn report(event: WebSocketEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        WebSocketEvent::QueryHandlerFailed { error } => {
            aimdb_core::log_warn!("record.query handler failed: {}", error);
        }
        WebSocketEvent::PersistenceQueryFailed { error } => {
            aimdb_core::log_warn!("record.query persistence handler failed: {}", error);
        }
        WebSocketEvent::MalformedQueryResult { error } => {
            aimdb_core::log_warn!("record.query handler returned malformed records: {}", error);
        }
        WebSocketEvent::BindFailed { address, error } => {
            aimdb_core::log_error!("WebSocket connector failed to bind {}: {}", address, error);
        }
        WebSocketEvent::Listening { address } => {
            aimdb_core::log_info!("WebSocket connector listening on {}", address);
        }
        WebSocketEvent::ServerFailed { error } => {
            aimdb_core::log_error!("WebSocket server error: {}", error);
        }
        WebSocketEvent::ConnectionLimitReached { peer, limit } => {
            aimdb_core::log_warn!(
                "WebSocket upgrade from {} refused: {} clients connected",
                peer,
                limit
            );
        }
        WebSocketEvent::VersionRejected {
            peer,
            client_version,
            server_version,
        } => {
            aimdb_core::log_warn!("WebSocket upgrade from {} refused: incompatible protocol version {:?} (server speaks {})", peer, client_version, server_version);
        }
        WebSocketEvent::AuthenticationRejected { peer, reason } => {
            aimdb_core::log_warn!("WebSocket auth rejected from {}: {}", peer, reason);
        }
    }
}

/// Test-only poll-scoped observation. Spawned work must be wrapped explicitly.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod capture {
    extern crate std;
    use super::WebSocketEvent;
    use core::future::{poll_fn, Future};
    use std::{boxed::Box, cell::RefCell, sync::Arc};
    pub(crate) type Observer = Arc<dyn Fn(&WebSocketEvent<'_>) + Send + Sync>;
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
    pub(super) fn record(event: &WebSocketEvent<'_>) {
        let observer = CURRENT.with(|slot| slot.borrow().clone());
        if let Some(observer) = observer {
            observer(event);
        }
    }
}

#[cfg(all(test, feature = "log"))]
#[path = "../tests/support/event_backend.rs"]
mod backend_tests;
